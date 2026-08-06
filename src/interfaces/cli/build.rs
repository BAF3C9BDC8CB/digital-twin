//! `dt build` 和 `dt search` 命令的 CLI 处理器。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::application::pipeline::config::PipelineConfig;
use crate::application::pipeline::engine::ProcessorEngine;
use crate::application::pipeline::infer_client::{
    ChatClient, SiliconFlowChatClient, XInferenceChatClient,
};
use crate::application::pipeline::processors::{
    ChunkProcessor, LlmClientProcessor, StoreProcessor, TreeSitterProcessor,
};
use crate::application::pipeline::prompt::PromptRegistry;
use crate::application::pipeline::registry::ProcessorRegistry;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, HealthStatus};
use crate::infrastructure::parser::ParserRegistry;
use sha2::{Digest, Sha256};

/// 处理 `dt build`——将项目索引到知识图谱。
///
/// 所有后端连接（Memgraph、Qdrant、embed、SQLite）必须由调用方
/// 建立，并以 `Option<Arc<...>>` 传入。
pub async fn handle_build(
    path: PathBuf,
    name: Option<String>,
    file: Option<PathBuf>,
    full: bool,
    pipeline: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    batch_config: BatchConfig,
) -> anyhow::Result<()> {
    // 确定项目名
    let project_name = name.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    });

    if let Some(f) = &file {
        tracing::info!(
            "构建: project={}, path={}, file={} (全量构建; 单文件提示未使用)",
            project_name,
            path.display(),
            f.display(),
        );
    } else {
        tracing::info!("构建: project={}, path={}", project_name, path.display(),);
    }

    // 加载流水线配置以获取 embed 设置
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    let skip_embed = !pipeline_config.processors.embed;

    // 通过 BuildCommand 执行构建
    let cmd = crate::application::build::builder::BuildCommand {
        project_path: path.clone(),
        project_name: project_name.clone(),
        full,
        verbose: true,
        skip_embed,
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

    // 使用配置的 llm_provider 创建第二阶段 LLM 客户端。
    // 从 pipeline.yaml 解析 provider（XInference / SiliconFlow）与模型名，
    // 与 run_pipeline_analysis 中的逻辑保持一致。
    let siliconflow = {
        use crate::infrastructure::siliconflow::SiliconFlowClient;

        let llm_provider = pipeline_config
            .providers
            .as_ref()
            .map(|p| p.llm_provider.clone())
            .unwrap_or_else(|| "siliconflow".to_string());

        match llm_provider.as_str() {
            "xinference" => {
                let xi_cfg = pipeline_config
                    .providers
                    .as_ref()
                    .and_then(|p| p.xinference.as_ref());

                let base_url = xi_cfg
                    .map(|c| c.url.as_str())
                    .unwrap_or("http://localhost:9997/v1")
                    .to_string();
                let api_key = xi_cfg.map(|c| c.api_key.clone()).unwrap_or_default();
                let llm_model = xi_cfg
                    .map(|c| c.model_llm.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("qwen3.5")
                    .to_string();

                let client = SiliconFlowClient::new(
                    base_url,
                    api_key,
                    String::new(), // embed 模型——聊天不需要
                    String::new(), // reranker 模型——聊天不需要
                    llm_model,
                );
                Some(Arc::new(client))
            }
            _ => {
                // 默认：SiliconFlow
                let base_url = pipeline_config.inference_server.url.clone();
                let api_key = load_siliconflow_api_key();
                let llm_model = load_siliconflow_llm_model()
                    .or_else(|| std::env::var("SILICONFLOW_LLM_MODEL").ok())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default();

                let client = SiliconFlowClient::new(
                    base_url,
                    api_key,
                    String::new(), // embed 模型——聊天不需要
                    String::new(), // reranker 模型——聊天不需要
                    llm_model,
                );
                Some(Arc::new(client))
            }
        }
    };

    let deps = crate::application::build::builder::BuildDependencies {
        graph,
        vector,
        snapshot,
        embed,
        siliconflow,
        batch_config: Some(batch_config),
        skip_embed,
    };

    cmd.run(deps).await?;

    // ── 可选的流水线分析（增强而非替代）────
    if pipeline {
        if let Err(e) = run_pipeline_analysis(
            &path,
            &project_name,
            pipeline_graph,
            pipeline_vector,
            pipeline_embed,
            pipeline_snapshot,
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
/// 跳过隐藏文件/目录（以 `.` 开头的名称）、二进制文件
/// 以及常见的非文本扩展名。最多返回 `MAX_PIPELINE_FILES`
/// 个条目，以避免压垮流水线引擎。
fn collect_project_files(root: &Path) -> Vec<(PathBuf, String)> {
    use walkdir::WalkDir;

    const MAX_PIPELINE_FILES: usize = 500;

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let walk = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // 跳过隐藏文件/目录。
            e.file_name()
                .to_str()
                .map(|s| !s.starts_with('.'))
                .unwrap_or(false)
        });

    for entry in walk.filter_map(|e| e.ok()) {
        if files.len() >= MAX_PIPELINE_FILES {
            tracing::info!("已达到流水线文件限制 ({MAX_PIPELINE_FILES}) — 截断");
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        // 跳过常见的二进制/非文本扩展名。
        let skip_ext = [
            "png", "jpg", "jpeg", "gif", "svg", "ico", "woff2", "ttf", "eot", "pdf", "zip", "jar",
            "class", "o", "so", "dylib", "dll", "exe", "bin", "db", "sqlite",
        ];
        if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
            if skip_ext.contains(&ext) {
                continue;
            }
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            files.push((entry.path().to_path_buf(), content));
        }
    }

    tracing::debug!("从 {} 收集了 {} 个文本文件", files.len(), root.display());
    files
}

/// 为搜索创建 embed 客户端，从 `config/pipeline.yaml` 读取配置。
/// 使用 provider 路由同时支持 SiliconFlow 与 XInference。
fn provider_config_from_pipeline() -> crate::infrastructure::embedder::ProviderConfig {
    use crate::infrastructure::embedder::ProviderConfig;

    let pipeline_cfg = match PipelineConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("无法加载 pipeline.yaml ({e})，使用默认配置");
            return ProviderConfig::default_siliconflow();
        }
    };

    let pcfg = match pipeline_cfg.providers {
        Some(p) => p,
        None => {
            tracing::warn!("pipeline.yaml 中无 providers 配置，使用默认配置");
            return ProviderConfig::default_siliconflow();
        }
    };

    let sf = pcfg.siliconflow.as_ref();
    let xi = pcfg.xinference.as_ref();

    let api_key_fallback = || std::env::var("SILICONFLOW_API_KEY").unwrap_or_default();

    ProviderConfig {
        siliconflow_url: sf
            .map(|s| s.url.as_str())
            .unwrap_or("https://api.siliconflow.cn/v1")
            .to_string(),
        siliconflow_api_key: sf
            .and_then(|s| {
                if s.api_key.is_empty() {
                    None
                } else {
                    Some(s.api_key.clone())
                }
            })
            .unwrap_or_else(api_key_fallback),
        siliconflow_model_embed: sf.map(|s| s.model_embed.clone()).unwrap_or_default(),
        siliconflow_model_reranker: sf.map(|s| s.model_reranker.clone()).unwrap_or_default(),
        siliconflow_model_llm: sf.map(|s| s.model_llm.clone()).unwrap_or_default(),
        xinference_url: xi.map(|s| s.url.as_str()).unwrap_or("").to_string(),
        xinference_api_key: xi.map(|s| s.api_key.clone()).unwrap_or_default(),
        xinference_model_embed: xi.map(|s| s.model_embed.clone()).unwrap_or_default(),
        xinference_model_reranker: xi.map(|s| s.model_reranker.clone()).unwrap_or_default(),
        xinference_model_llm: xi.map(|s| s.model_llm.clone()).unwrap_or_default(),
        embed_provider: pcfg.embed_provider.clone(),
        rerank_provider: pcfg.rerank_provider.clone(),
        llm_provider: pcfg.llm_provider.clone(),
    }
}

fn create_search_embed_client() -> Arc<dyn EmbedService> {
    crate::infrastructure::embedder::create_embed_router(provider_config_from_pipeline())
}

fn create_search_rerank_client() -> Arc<dyn crate::domain::traits::RerankService> {
    crate::infrastructure::embedder::create_rerank_router(provider_config_from_pipeline())
}

/// 从 `config/pipeline.yaml` 读取 SiliconFlow LLM 模型名。
fn load_siliconflow_llm_model() -> Option<String> {
    if let Ok(cfg) = PipelineConfig::load() {
        if let Some(providers) = cfg.providers {
            if let Some(sf) = providers.siliconflow {
                if !sf.model_llm.is_empty() {
                    return Some(sf.model_llm);
                }
            }
        }
    }
    None
}

/// 从 `config/pipeline.yaml` 读取 SiliconFlow API key，
/// 未配置时回退到 `SILICONFLOW_API_KEY` 环境变量。
fn load_siliconflow_api_key() -> String {
    // 优先尝试环境变量
    if let Ok(key) = std::env::var("SILICONFLOW_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // 再尝试 pipeline.yaml
    if let Ok(cfg) = PipelineConfig::load() {
        if let Some(providers) = cfg.providers {
            if let Some(sf) = providers.siliconflow {
                if !sf.api_key.is_empty() {
                    return sf.api_key;
                }
            }
        }
    }
    String::new()
}

/// 构建完成后对项目运行流水线分析。
///
/// 这是纯粹的附加步骤——任何错误仅记录为警告，
/// **不会**导致整个构建失败。
async fn run_pipeline_analysis(
    project_path: &Path,
    project_name: &str,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
) -> anyhow::Result<()> {
    // ── 1. Load pipeline config — skip if disabled ────────────────
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    if !pipeline_config.enabled {
        tracing::info!("流水线已禁用 (config/pipeline.yaml enabled=false)");
        return Ok(());
    }
    tracing::info!("正在为 {project_name} 启动流水线分析...");

    // ── 2. 根据 llm_provider 配置连接推理服务器 ────────────────
    let infer_max_concurrent = pipeline_config.inference_server.max_concurrent;

    // 从配置中获取 LLM provider
    let llm_provider = pipeline_config
        .providers
        .as_ref()
        .map(|p| p.llm_provider.clone())
        .unwrap_or_else(|| "siliconflow".to_string());

    let (infer_client, infer_model): (Arc<dyn ChatClient>, String) = match llm_provider.as_str() {
        "xinference" => {
            let xi_cfg = pipeline_config
                .providers
                .as_ref()
                .and_then(|p| p.xinference.as_ref());

            let base_url = xi_cfg
                .map(|c| c.url.as_str())
                .unwrap_or("http://localhost:9997/v1")
                .to_string();
            let api_key = xi_cfg.map(|c| c.api_key.clone()).unwrap_or_default();
            let model = xi_cfg
                .map(|c| c.model_llm.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("qwen3.5")
                .to_string();

            tracing::info!("使用 XInference LLM: {} @ {}", model, base_url);
            let client = Arc::new(XInferenceChatClient::new(
                base_url.to_string(),
                api_key,
                infer_max_concurrent,
            ));
            (client as Arc<dyn ChatClient>, model.to_string())
        }
        _ => {
            let api_key = load_siliconflow_api_key();
            let model = load_siliconflow_llm_model()
                .or_else(|| std::env::var("SILICONFLOW_LLM_MODEL").ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Qwen3-14B".to_string());

            // R4：使用 SiliconFlow provider 的 URL——而非本地的
            // inference_server.url。为空时回退到客户端的默认值。
            let sf_url = pipeline_config
                .providers
                .as_ref()
                .and_then(|p| p.siliconflow.as_ref())
                .map(|s| s.url.clone())
                .unwrap_or_default();
            tracing::info!("使用 SiliconFlow LLM: {} @ {}", model, sf_url);
            let client = Arc::new(SiliconFlowChatClient::new(sf_url, infer_max_concurrent));
            (client as Arc<dyn ChatClient>, model)
        }
    };

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
                let llm_config = pipeline_config.llm.unwrap_or_default();
                registry.register(Box::new(LlmClientProcessor::new(
                    infer_client.clone(),
                    infer_model.clone(),
                    Arc::new(prompts),
                    llm_config,
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
    let engine = ProcessorEngine::new(registry, pipeline_config.inference_server.max_concurrent);

    let all_files = collect_project_files(project_path);
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
                        .is_step_done(project_name, &rel_path, step, &file_hash)
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
                    // 项目相对路径——引擎接收的是相对
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
            "{project_name} 增量跳过 {skipped_count} 个完全未变更文件, \
             {partial_count} 个文件有步骤待执行",
        );
    }

    tracing::info!("流水线正在分析 {} 个文件...", files_to_process.len());

    if files_to_process.is_empty() {
        tracing::info!("{project_name} 流水线分析完成: 所有文件均为最新 (已跳过 {total_count} 个)");
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
        .analyze_batch(engine_input, project_name.to_string(), engine_skip)
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
                        .mark_step_done(project_name, rel_path, step, file_hash)
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
        "{project_name} 流水线分析完成: \
         分析了 {} 个文件, {} 个成功, {} 个有错误 (跳过 {} 个未变更)",
        analyses.len(),
        success_count,
        error_count,
        skipped_count,
    );

    Ok(())
}

/// 处理 `dt build --all`——按顺序构建多个项目。
///
/// 遍历 (project_name, project_path) 元组列表，为每个项目调用
/// `handle_build()`。错误按项目捕获，不会中断整个批次。
/// 最后打印汇总。
pub async fn handle_build_all(
    projects: Vec<(String, PathBuf)>,
    full: bool,
    pipeline: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    batch_config: BatchConfig,
) -> anyhow::Result<()> {
    let total = projects.len();
    let mut succeeded = 0u32;
    let mut failed = 0u32;

    println!("正在构建 {} 个项目...", total);

    for (i, (name, path)) in projects.into_iter().enumerate() {
        let idx = i + 1; // 从 1 开始的显示序号
        println!("[{idx}/{total}] 正在构建 {name}，路径 {}", path.display());

        match handle_build(
            path,
            Some(name.clone()),
            None,
            full,
            pipeline,
            graph.clone(),
            vector.clone(),
            embed.clone(),
            snapshot.clone(),
            batch_config.clone(),
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

        // 项目之间短暂暂停，让日志落盘
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("完成。{succeeded} 个成功, {failed} 个失败。");

    Ok(())
}

/// 从可能混合的字符串中提取内嵌的 ASCII 单词序列

/// 处理 `dt search`——统一检索渲染壳（U-D3：默认 world=all；--json 输出纯 JSON）。
pub async fn handle_search(
    query: String,
    world: String,
    limit: usize,
    json: bool,
    project: Option<String>,
    file_type: Option<String>,
    content_type: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "搜索: query={query} world={world} limit={limit} json={json} project={project:?}"
    );

    if !json {
        println!("搜索: query=\"{query}\" world={world} limit={limit}");
    }

    use crate::application::context::search_mcp::CrossWorldSearchTrait;

    let embed: Option<Arc<dyn EmbedService>> = Some(create_search_embed_client());
    let rerank = Some(create_search_rerank_client());
    let cws = crate::application::context::search_mcp::CrossWorldSearch::new(
        graph, vector, embed, rerank,
    );
    let req = crate::application::context::search_mcp::SearchRequest {
        query: query.clone(),
        world: Some(world),
        limit: Some(limit),
        project,
        max_hops: None,
        with_evidence: None,
        origin: None,
        doc_id: None,
        file_type,
        entity_type_filter: content_type,
    };
    let result = cws.search(&req).await?;

    if json {
        // U-D4：--json 时 stdout 仅含 JSON（header 行已抑制，日志走 stderr）
        println!(
            "{}",
            crate::interfaces::cli::search_render::render_json(&result)
        );
    } else {
        print!(
            "{}",
            crate::interfaces::cli::search_render::render_human(&result)
        );
    }
    Ok(())
}
