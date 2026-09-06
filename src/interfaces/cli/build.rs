//! `dt build` 和 `dt search` 命令的 CLI 处理器。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::application::pipeline::config::PipelineConfig;
use crate::application::pipeline::engine::ProcessorEngine;
use crate::application::pipeline::infer_client::{ChatClient, SiliconFlowChatClient};
use crate::application::pipeline::processors::{
    ChunkProcessor, LlmClientProcessor, StoreProcessor, TreeSitterProcessor,
};
use crate::application::pipeline::prompt::PromptRegistry;
use crate::application::pipeline::registry::ProcessorRegistry;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, ScanConfig};
use crate::infrastructure::parser::ParserRegistry;
use sha2::{Digest, Sha256};

/// 处理 `dt build`——将代码根索引到知识图谱。
///
/// 所有后端连接（Memgraph、Qdrant、embed、SQLite）必须由调用方
/// 建立，并以 `Option<Arc<...>>` 传入。
pub async fn handle_build(
    path: PathBuf,
    name: Option<String>,
    file: Option<PathBuf>,
    full: bool,
    pipeline: bool,
    llm_backfill: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    batch_config: BatchConfig,
    scan_config: crate::domain::types::ScanConfig,
) -> anyhow::Result<()> {
    // 确定根别名（root alias：无显式 name 时取路径最后一段目录名）
    let root_alias = name.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    });
    let target_file = file.as_ref().map(|f| {
        if f.is_absolute() {
            f.clone()
        } else {
            path.join(f)
        }
    });

    if let Some(f) = &file {
        tracing::info!(
            "构建: project={}, path={}, file={} (全量构建; 单文件提示未使用)",
            root_alias,
            path.display(),
            f.display(),
        );
    } else {
        tracing::info!("构建: project={}, path={}", root_alias, path.display(),);
    }

    // 加载流水线配置以获取 embed 设置
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    let skip_embed = !pipeline_config.processors.embed;

    // 通过 BuildCommand 执行构建（字段 project_name = 存储协议层叫法，
    // 此处传入的 root_alias 即其语义：代码根别名）。
    let cmd = crate::application::build::builder::BuildCommand {
        project_path: path.clone(),
        project_name: root_alias.clone(),
        full,
        verbose: true,
        skip_embed,
        llm_backfill,
    };

    // 为流水线使用而克隆，因为 BuildDependencies 会消耗原始值。
    let pipeline_graph = graph
        .as_ref()
        .map(|g| Arc::clone(g) as Arc<dyn GraphRepository>);
    let pipeline_vector = vector
        .as_ref()
        .map(|v| Arc::clone(v) as Arc<dyn VectorRepository>);
    let pipeline_embed = embed
        .as_ref()
        .map(|e| Arc::clone(e) as Arc<dyn EmbedService>);

    // 为流水线使用而克隆快照（会被 BuildDependencies 消耗）
    let pipeline_snapshot = snapshot
        .as_ref()
        .map(|s| Arc::clone(s) as Arc<dyn SnapshotRepository>);

    // 使用统一路由，确保普通构建与增强 pipeline 使用同一 provider。
    // 并发读 providers.llm 池（各端点之和）；单次回复上限读 llm.max_tokens。
    let (llm_client, llm_model, llm_max_tokens) = build_llm_client(&pipeline_config);

    let deps = crate::application::build::builder::BuildDependencies {
        graph,
        vector,
        snapshot,
        embed,
        llm_client: Some(llm_client),
        llm_model,
        target_file,
        batch_config: Some(batch_config),
        skip_embed,
        scan_config: scan_config.clone(),
        // Phase 2 方法级并发 = LLM 池各端点 max_concurrent 之和（多 key 并行）
        llm_concurrency: pipeline_config.inference_max_concurrent(),
        llm_max_tokens,
    };

    cmd.run(deps).await?;

    // ── 可选的流水线分析（增强而非替代）────
    if pipeline {
        if let Err(e) = run_pipeline_analysis(
            &path,
            &root_alias,
            pipeline_graph,
            pipeline_vector,
            pipeline_embed,
            pipeline_snapshot,
            &scan_config,
        )
        .await
        {
            tracing::warn!("流水线分析失败 (非致命): {e}");
        }
    }

    Ok(())
}

/// 递归收集目录中所有含文本的文件。
///
/// 遵循 `ScanConfig` 统一忽略规则（ignore_names / ignore_globs，覆盖
/// 目录、文件名、扩展名与 glob 通配），并跳过隐藏目录以及常见的
/// 二进制扩展名。最多返回 `MAX_PIPELINE_FILES` 个条目，以避免压垮流水线引擎。
fn collect_project_files(root: &Path, scan_config: &ScanConfig) -> Vec<(PathBuf, String)> {
    use walkdir::WalkDir;

    const MAX_PIPELINE_FILES: usize = 500;

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let walk = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                // 跳过隐藏目录。
                let name = e.file_name().to_str().unwrap_or("");
                if name.starts_with('.') {
                    return false;
                }
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(e.path())
                    .to_string_lossy()
                    .to_string();
                return !scan_config.is_ignored(&rel);
            }
            true
        });

    for entry in walk.filter_map(|e| e.ok()) {
        if files.len() >= MAX_PIPELINE_FILES {
            tracing::info!("已达到流水线文件限制 ({MAX_PIPELINE_FILES}) — 截断");
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        // 统一忽略规则（文件名精确 / 扩展名 / 通配）。
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();
        if scan_config.is_ignored(&rel) {
            continue;
        }
        // 按大小过滤（与 scanner.rs collect_files 的 max_file_size 对齐；
        // 缺失此过滤时超大打包文件（如 webpack bundle）会整体进入 LLM 流水线，
        // 导致 opencode.go 等上游返回 HTTP 400。实测 public/bpmnjs/index.js 7.9MB。
        match entry.metadata() {
            Ok(m) => {
                if m.len() > scan_config.max_file_size {
                    tracing::debug!(
                        "跳过超大文件 {} ({} bytes > max {})",
                        entry.path().display(),
                        m.len(),
                        scan_config.max_file_size
                    );
                    continue;
                }
            }
            Err(_) => continue,
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            files.push((entry.path().to_path_buf(), content));
        }
    }

    tracing::debug!("从 {} 收集了 {} 个文本文件", files.len(), root.display());
    files
}

/// 为搜索创建 embed/rerank 客户端——由 pipeline.yaml providers 端点池构建。
///
/// 2026-09-06 起：embed/rerank 各走 `providers.embed` / `providers.rerank`
/// 端点池（多厂商 × 多模型，失败自动顺延）；旧 `providers.siliconflow` 单块
/// 与 `ProviderConfig` 已移除。返回 (embed, rerank, llm) 池化服务。
pub(crate) fn build_search_services() -> (
    Arc<dyn EmbedService>,
    Arc<dyn crate::domain::traits::RerankService>,
    Arc<dyn crate::domain::traits::LlmService>,
) {
    let cfg = PipelineConfig::load().unwrap_or_default();
    let svcs = crate::infrastructure::embedder::build_pooled_services(&cfg);
    (
        svcs.embed.unwrap_or_else(|| {
            Arc::new(crate::infrastructure::embedder::NoopEmbedService::default())
        }),
        svcs.rerank.unwrap_or_else(|| {
            Arc::new(crate::infrastructure::provider_router::NoopRerankService)
        }),
        svcs.llm.unwrap_or_else(|| {
            Arc::new(crate::infrastructure::embedder::NoopLlmService)
        }),
    )
}

pub(crate) fn create_search_embed_client() -> Arc<dyn EmbedService> {
    build_search_services().0
}

pub(crate) fn create_search_rerank_client() -> Arc<dyn crate::domain::traits::RerankService> {
    build_search_services().1
}

pub(crate) fn create_search_llm_client() -> Arc<dyn crate::domain::traits::LlmService> {
    build_search_services().2
}

/// 构建 LLM 对话客户端——由 `providers.llm` 端点池构建（多端点多 key 并行）。
///
/// 模型名/并发已收敛进端点池（端点级 model 覆盖 > `llm.model`）；并发取
/// `inference_max_concurrent()`（池内各端点之和）；单次回复上限读 `llm.max_tokens`。
///
/// 返回 `(客户端, 默认模型名, max_tokens)`。两个调用方（普通文件管线与
/// Nacos/Jenkins 远程源管线）共用同一路由逻辑，保证模型配置一处生效。
fn build_llm_client(pipeline_config: &PipelineConfig) -> (Arc<dyn ChatClient>, String, u32) {
    // 旧 SILICONFLOW_API_KEY env 优先逻辑已废弃——端点密钥一律按端点配置
    // （api_key / api_key_env）解析，解析期校验（config::validate）。
    let model = pipeline_config.llm_model();
    let model = if model.is_empty() {
        "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B".to_string()
    } else {
        model
    };

    let sf_max_tokens = pipeline_config
        .llm
        .as_ref()
        .map(|c| c.max_tokens)
        .unwrap_or(512);

    let client = crate::application::pipeline::infer_client::PooledChatClient::from_pipeline(
        pipeline_config,
    );
    tracing::info!(
        "使用 LLM 端点池 ({} 端点, model={}, max_tokens={})",
        client.pool_len(),
        model,
        sf_max_tokens
    );
    (client.to_arc(), model, sf_max_tokens)
}

/// 构建完成后对代码根运行流水线分析。
///
/// 这是纯粹的附加步骤——任何错误仅记录为警告，
/// **不会**导致整个构建失败。
async fn run_pipeline_analysis(
    project_path: &Path,
    root_alias: &str,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    scan_config: &crate::domain::types::ScanConfig,
) -> anyhow::Result<()> {
    // ── 1. Load pipeline config ───────────────────────────────────
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!("正在为 {root_alias} 启动流水线分析...");

    // ── 2. 连接推理服务器（siliconflow）────
    let (infer_client, infer_model, _infer_max_tokens): (Arc<dyn ChatClient>, String, u32) =
        build_llm_client(&pipeline_config);

    let inference_available = match infer_client.health_check().await {
        Ok(true) => {
            tracing::info!("推理服务器可用");
            true
        }
        Ok(false) => {
            tracing::info!("推理服务器不可达 — 跳过 GPU 处理器");
            false
        }
        Err(e) => {
            tracing::warn!("推理服务器健康检查失败: {e} — 跳过 GPU 处理器");
            false
        }
    };

    // ── 4. 构建处理器注册表 ───────────────────────────────
    let mut registry = ProcessorRegistry::new();

    if pipeline_config.processors.tree_sitter {
        let parser_registry = Arc::new(ParserRegistry::new());
        registry.register(Box::new(TreeSitterProcessor::new(parser_registry)));
        tracing::info!("  处理器: TreeSitter");
    }
    if pipeline_config.processors.chunk {
        registry.register(Box::new(ChunkProcessor::default()));
        tracing::info!("  处理器: Chunk");
    }
    if pipeline_config.processors.llm && inference_available {
        match PromptRegistry::load_default() {
            Ok(prompts) => {
                let llm_config = pipeline_config.llm.as_ref().cloned().unwrap_or_default();
                let doc_gate = pipeline_config.doc_gate.as_ref().cloned();
                registry.register(Box::new(LlmClientProcessor::with_doc_gate(
                    infer_client.clone(),
                    infer_model.clone(),
                    Arc::new(prompts),
                    llm_config,
                    doc_gate,
                )));
                tracing::info!("  处理器: LlmClient");
            }
            Err(e) => {
                tracing::warn!("  提示词注册表不可用: {e} — 跳过 LLM 处理器");
            }
        }
    }
    if pipeline_config.processors.store {
        registry.register(Box::new(StoreProcessor::new(graph, vector, embed)));
        tracing::info!("  处理器: Store");
    }

    if registry.is_empty() {
        tracing::info!("没有注册的流水线处理器 — 跳过分析");
        return Ok(());
    }

    // ── 4. 运行流水线 ───────────────────────────────────────────
    let registry = Arc::new(registry);
    let engine = ProcessorEngine::new(registry, pipeline_config.inference_max_concurrent());

    let all_files = collect_project_files(project_path, scan_config);
    let total_count = all_files.len();

    // ── 增量跳过：计算文件哈希并构建逐文件、逐步骤的跳过映射。
    // 所有步骤都已完成的文件被完全排除。
    // 部分（而非全部）步骤已完成的文件获得跳过映射，使
    // 引擎只执行缺失的处理器。
    // 活动步骤名与上面注册的处理器一一对应。
    let active_steps: Vec<&str> = {
        let mut steps = Vec::new();
        if pipeline_config.processors.tree_sitter {
            steps.push("tree_sitter");
        }
        if pipeline_config.processors.chunk {
            steps.push("chunk");
        }
        if pipeline_config.processors.llm && inference_available {
            steps.push("llm");
        }
        if pipeline_config.processors.store {
            steps.push("store");
        }
        steps
    };

    // (路径、文本、相对路径、文件哈希、待跳过步骤)
    type FileEntry = (PathBuf, String, String, String, HashSet<String>);

    let (files_to_process, skip_map, skipped_count): (Vec<FileEntry>, _, usize) =
        if let Some(ref snap) = snapshot {
            let mut pending: Vec<FileEntry> = Vec::new();
            let mut skipped = 0usize;
            let mut skip: HashMap<PathBuf, HashSet<String>> = HashMap::new();

            for (path, text) in all_files {
                let mut hasher = Sha256::new();
                hasher.update(text.as_bytes());
                let file_hash = format!("{:x}", hasher.finalize());

                let rel_path = path
                    .strip_prefix(project_path)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                let mut steps_to_skip: HashSet<String> = HashSet::new();
                let mut all_done = true;

                for step in &active_steps {
                    match snap
                        .is_step_done(root_alias, &rel_path, step, &file_hash)
                        .await
                    {
                        Ok(true) => {
                            steps_to_skip.insert(step.to_string());
                        }
                        Ok(false) => {
                            all_done = false;
                        }
                        Err(e) => {
                            tracing::warn!("对 {} 的 is_step_done 检查失败: {e}", rel_path);
                            all_done = false;
                        }
                    }
                }

                // ── 依赖级联：汇点步骤（store）依赖上游生产者。
                // 若下游步骤需要运行，其上游生产者也必须运行。
                // 链条：store → llm → chunk → tree_sitter
                {
                    let active_set: HashSet<&str> = active_steps.iter().copied().collect();
                    if active_set.contains("store") && !steps_to_skip.contains("store") {
                        steps_to_skip.remove("llm");
                    }
                    if active_set.contains("llm") && !steps_to_skip.contains("llm") {
                        steps_to_skip.remove("chunk");
                    }
                    if active_set.contains("chunk") && !steps_to_skip.contains("chunk") {
                        steps_to_skip.remove("tree_sitter");
                    }
                }

                if all_done {
                    skipped += 1;
                } else {
                    // 映射中只记录非空的跳过集合。键是
                    // 相对代码根的路径——引擎接收的是相对
                    // 路径（见下方的 engine_input）。
                    if !steps_to_skip.is_empty() {
                        skip.insert(PathBuf::from(&rel_path), steps_to_skip.clone());
                    }
                    pending.push((path, text, rel_path, file_hash, steps_to_skip));
                }
            }
            (pending, skip, skipped)
        } else {
            // 无快照 → 无增量跟踪；处理所有文件
            let pending: Vec<FileEntry> = all_files
                .into_iter()
                .map(|(p, t)| {
                    let rel = p
                        .strip_prefix(project_path)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .to_string();
                    let mut hasher = Sha256::new();
                    hasher.update(t.as_bytes());
                    let hash = format!("{:x}", hasher.finalize());
                    (p, t, rel, hash, HashSet::new())
                })
                .collect();
            (pending, HashMap::new(), 0)
        };

    if skipped_count > 0 {
        let partial_count = files_to_process.len();
        tracing::info!(
            "{root_alias} 增量跳过 {skipped_count} 个完全未变更文件, \
             {partial_count} 个文件有步骤待执行",
        );
    }

    tracing::info!("流水线正在分析 {} 个文件...", files_to_process.len());

    if files_to_process.is_empty() {
        tracing::info!("{root_alias} 流水线分析完成: 所有文件均为最新 (已跳过 {total_count} 个)");
        return Ok(());
    }

    // 向引擎输入（相对路径、文本）对。相对路径是流水线中
    // 文件的规范标识：chunk 处理器以
    // `dt://doc/{project}/{rel_path}` 派生 doc_id（domain::id::make_document_id），
    // 与早期构建写入的 Document 节点以及构建编排层
    // 消费的 deleted_paths（§6.5）保持一致。
    let engine_input: Vec<(PathBuf, String)> = files_to_process
        .iter()
        .map(|(_, t, rel, _, _)| (PathBuf::from(rel), t.clone()))
        .collect();

    // 为引擎构建跳过映射：仅传入非空的跳过集合
    let engine_skip: Option<Arc<HashMap<PathBuf, HashSet<String>>>> = if skip_map.is_empty() {
        None
    } else {
        Some(Arc::new(skip_map))
    };

    let analyses = engine
        .analyze_batch(engine_input, root_alias.to_string(), engine_skip)
        .await;
    let success_count = analyses.iter().filter(|a| a.success).count();
    let error_count = analyses.len() - success_count;

    // ── 将成功处理文件的新执行步骤标记为完成。
    // 我们只标记不在逐文件跳过集合中的步骤（即引擎实际
    // 执行的步骤），从而让之前跳过的步骤保持正确记录。
    // 注意：analyses 携带相对路径（上方 engine 输入约定）。
    if let Some(ref snap) = snapshot {
        for (_path, _text, rel_path, file_hash, steps_to_skip) in &files_to_process {
            let was_success = analyses
                .iter()
                .any(|a| a.file_path == Path::new(rel_path) && a.success);
            if was_success {
                for step in &active_steps {
                    if steps_to_skip.contains(*step) {
                        continue; // 之前运行时已标记
                    }
                    if let Err(e) = snap
                        .mark_step_done(root_alias, rel_path, step, file_hash)
                        .await
                    {
                        tracing::warn!(
                            "对 {} 的 mark_step_done 失败, 步骤 {}: {e}",
                            rel_path,
                            step,
                        );
                    }
                }
            }
        }
    }

    // 在 debug 级别记录逐文件错误
    for analysis in &analyses {
        if !analysis.errors.is_empty() {
            let path_display = analysis.file_path.display();
            for err in &analysis.errors {
                tracing::debug!("  [{path_display}] {err}");
            }
        }
    }

    tracing::info!(
        "{root_alias} 流水线分析完成: \
         分析了 {} 个文件, {} 个成功, {} 个有错误 (跳过 {} 个未变更)",
        analyses.len(),
        success_count,
        error_count,
        skipped_count,
    );

    Ok(())
}

/// 处理 `dt build --all`——按顺序构建多个 root（代码根）。
///
/// 遍历 (root_alias, root_path) 元组列表，为每个 root 调用
/// `handle_build()`。错误按 root 捕获，不会中断整个批次。
/// 最后打印汇总。
pub async fn handle_build_all(
    roots: Vec<(String, PathBuf)>,
    full: bool,
    pipeline: bool,
    llm_backfill: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    batch_config: BatchConfig,
    scan_config: crate::domain::types::ScanConfig,
) -> anyhow::Result<()> {
    let total = roots.len();
    let mut succeeded = 0u32;
    let mut failed = 0u32;

    println!("正在构建 {} 个 root...", total);

    for (i, (name, path)) in roots.into_iter().enumerate() {
        let idx = i + 1; // 从 1 开始的显示序号
        println!(
            "[{idx}/{total}] 正在构建 {name}，路径 {path}",
            path = path.display()
        );

        match handle_build(
            path,
            Some(name.clone()),
            None,
            full,
            pipeline,
            llm_backfill,
            graph.clone(),
            vector.clone(),
            embed.clone(),
            snapshot.clone(),
            batch_config.clone(),
            scan_config.clone(),
        )
        .await
        {
            Ok(()) => {
                succeeded += 1;
                println!("[{idx}/{total}] ✓ {name}");
            }
            Err(err) => {
                failed += 1;
                eprintln!("[{idx}/{total}] ✗ {name}: {err}");
            }
        }

        // root 之间短暂暂停，让日志落盘
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("完成。{succeeded} 个成功, {failed} 个失败。");

    Ok(())
}

/// 从 config.yaml 的 `roots` 段构造"根别名 → 磁盘绝对路径"表。
///
/// 供 `ProjectPathResolver` 使用，把 `dt://doc/{根别名}/{相对路径}` 来源
/// 解析为磁盘全路径（仅 CLI 人类渲染；加载失败返回空表，来源保持原样）。
///
/// 复用 [`crate::runtime::resolve_roots`]，与 `dt build` 的解析保持单一实现
/// （旧版在此手写解析 `projects` 段，2026-09-05 root 化后删除）。
pub fn project_roots_from_config() -> Vec<(String, String)> {
    let Some(cfg) = crate::runtime::load_config() else {
        return Vec::new();
    };
    crate::runtime::resolve_roots(&cfg)
        .into_iter()
        .map(|(alias, path)| (alias, path.display().to_string()))
        .collect()
}
