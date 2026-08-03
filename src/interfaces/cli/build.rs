//! CLI handlers for `dt build` and `dt search` commands.
//!
//! Extracted from main.rs to keep the entrypoint lean.

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
    ChunkProcessor, HanlpClientProcessor, LlmClientProcessor, StoreProcessor, TreeSitterProcessor,
};
use crate::application::pipeline::prompt::PromptRegistry;
use crate::application::pipeline::registry::ProcessorRegistry;
use crate::application::search::fusion::RankedItem;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, HealthStatus};
use crate::infrastructure::hanlp::HanlpClient;
use crate::infrastructure::parser::ParserRegistry;
use sha2::{Digest, Sha256};

/// Handle `dt build` — index a project into the knowledge graph.
///
/// All backend connections (Memgraph, Qdrant, embed, SQLite) must be established
/// by the caller and passed as `Option<Arc<...>>`.
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
    hanlp: Option<Arc<HanlpClient>>,
) -> anyhow::Result<()> {
    // Determine project name
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

    // Load pipeline config for embed settings
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    let skip_embed = !pipeline_config.processors.embed;

    // Execute build via BuildCommand
    let cmd = crate::application::build::builder::BuildCommand {
        project_path: path.clone(),
        project_name: project_name.clone(),
        full,
        verbose: true,
        skip_embed,
    };

    // Clone for pipeline use since BuildDependencies consumes the originals.
    let pipeline_graph = graph
        .as_ref()
        .map(|g| Arc::clone(g) as Arc<dyn GraphRepository>);
    let pipeline_vector = vector
        .as_ref()
        .map(|v| Arc::clone(v) as Arc<dyn VectorRepository>);
    let pipeline_embed = embed
        .as_ref()
        .map(|e| Arc::clone(e) as Arc<dyn EmbedService>);

    // Clone snapshots for pipeline use (consumed by BuildDependencies)
    let pipeline_snapshot = snapshot
        .as_ref()
        .map(|s| Arc::clone(s) as Arc<dyn SnapshotRepository>);

    // Create Phase 2 LLM client using the configured llm_provider.
    // Resolves provider (XInference / SiliconFlow) and model name from
    // pipeline.yaml, matching the same logic in run_pipeline_analysis.
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
                    String::new(), // embed model — not needed for chat
                    String::new(), // reranker model — not needed for chat
                    llm_model,
                );
                Some(Arc::new(client))
            }
            _ => {
                // Default: SiliconFlow
                let base_url = pipeline_config.inference_server.url.clone();
                let api_key = load_siliconflow_api_key();
                let llm_model = load_siliconflow_llm_model()
                    .or_else(|| std::env::var("SILICONFLOW_LLM_MODEL").ok())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default();

                let client = SiliconFlowClient::new(
                    base_url,
                    api_key,
                    String::new(), // embed model — not needed for chat
                    String::new(), // reranker model — not needed for chat
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

    // ── Optional pipeline analysis (enhancement, not replacement) ────
    if pipeline {
        if let Err(e) = run_pipeline_analysis(
            &path,
            &project_name,
            pipeline_graph,
            pipeline_vector,
            pipeline_embed,
            hanlp.clone(),
            pipeline_snapshot,
        )
        .await
        {
            tracing::warn!("流水线分析失败 (非致命): {e}");
        }
    }

    Ok(())
}

/// Collect all text-bearing files from a directory recursively.
///
/// Skips hidden files/directories (names starting with `.`), binary files,
/// and common non-text extensions.  Returns up to `MAX_PIPELINE_FILES`
/// entries to avoid overwhelming the pipeline engine.
fn collect_project_files(root: &Path) -> Vec<(PathBuf, String)> {
    use walkdir::WalkDir;

    const MAX_PIPELINE_FILES: usize = 500;

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let walk = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden files / directories.
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
        // Skip common binary / non-text extensions.
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

/// Create an embed client for search, reading config from `config/pipeline.yaml`.
/// Uses the provider router to support both SiliconFlow and XInference.
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

/// Read the SiliconFlow LLM model name from `config/pipeline.yaml`.
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

/// Read the SiliconFlow API key from `config/pipeline.yaml`,
/// falling back to `SILICONFLOW_API_KEY` env var.
fn load_siliconflow_api_key() -> String {
    // Try env var first
    if let Ok(key) = std::env::var("SILICONFLOW_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // Try pipeline.yaml
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

/// Run pipeline analysis on a project after the build completes.
///
/// This is a purely additive step — any error is logged as a warning and
/// does **not** fail the overall build.
async fn run_pipeline_analysis(
    project_path: &Path,
    project_name: &str,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    hanlp: Option<Arc<HanlpClient>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
) -> anyhow::Result<()> {
    // ── 1. Load pipeline config — skip if disabled ────────────────
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    if !pipeline_config.enabled {
        tracing::info!("流水线已禁用 (config/pipeline.yaml enabled=false)");
        return Ok(());
    }
    tracing::info!("正在为 {project_name} 启动流水线分析...");

    // ── 2. Check HanLP availability ──────────────────────────────
    let hanlp_available = if let Some(ref hanlp_client) = hanlp {
        match hanlp_client.health_check().await {
            Ok(HealthStatus::Healthy) => {
                tracing::info!("HanLP 服务器可用");
                true
            }
            _ => {
                tracing::info!("HanLP 服务器不可达 — 跳过 NLP 处理器");
                false
            }
        }
    } else {
        false
    };

    // ── 3. Connect to inference server based on llm_provider config ───────
    let infer_max_concurrent = pipeline_config.inference_server.max_concurrent;

    // Get the LLM provider from config
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

            // R4: use the SiliconFlow provider URL — not the local
            // inference_server.url. Empty falls back to the client's default.
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

    // ── 4. Build processor registry ───────────────────────────────
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
    if pipeline_config.processors.hanlp && hanlp_available {
        if let Some(ref hanlp_client) = hanlp {
            registry.register(Box::new(HanlpClientProcessor::new(hanlp_client.clone())));
            tracing::info!("  处理器: Hanlp");
        }
    }
    if pipeline_config.processors.llm && inference_available {
        match PromptRegistry::load(Path::new("config/prompts")) {
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

    // ── 4. Run pipeline ───────────────────────────────────────────
    let registry = Arc::new(registry);
    let engine = ProcessorEngine::new(registry, pipeline_config.inference_server.max_concurrent);

    let all_files = collect_project_files(project_path);
    let total_count = all_files.len();

    // ── Incremental skip: compute file hashes and build per-file, per-step skip map.
    // Files where ALL steps are already done are excluded entirely.
    // Files where SOME (but not all) steps are done get a skip map so the
    // engine only executes the missing processors.
    // Active step names correspond to the processors we registered above.
    let active_steps: Vec<&str> = {
        let mut steps = Vec::new();
        if pipeline_config.processors.tree_sitter {
            steps.push("tree_sitter");
        }
        if pipeline_config.processors.chunk {
            steps.push("chunk");
        }
        if pipeline_config.processors.hanlp && hanlp_available {
            steps.push("hanlp");
        }
        if pipeline_config.processors.llm && inference_available {
            steps.push("llm");
        }
        if pipeline_config.processors.store {
            steps.push("store");
        }
        steps
    };

    // (path, text, rel_path, file_hash, steps_to_skip)
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
                            tracing::warn!("is_step_done failed for {}: {e}", rel_path);
                            all_done = false;
                        }
                    }
                }

                // ── Dependency cascade: sink steps (store) depend on upstream producers.
                // If a downstream step needs to run, its upstream producers must also run.
                // Chain: store → {hanlp, llm} → chunk → tree_sitter
                {
                    let active_set: HashSet<&str> = active_steps.iter().copied().collect();
                    if active_set.contains("store") && !steps_to_skip.contains("store") {
                        steps_to_skip.remove("hanlp");
                        steps_to_skip.remove("llm");
                    }
                    if active_set.contains("hanlp") && !steps_to_skip.contains("hanlp") {
                        steps_to_skip.remove("chunk");
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
                    // Only record non-empty skip sets in the map. Keys are
                    // project-relative paths — the engine is fed relative
                    // paths (see engine_input below).
                    if !steps_to_skip.is_empty() {
                        skip.insert(PathBuf::from(&rel_path), steps_to_skip.clone());
                    }
                    pending.push((path, text, rel_path, file_hash, steps_to_skip));
                }
            }
            (pending, skip, skipped)
        } else {
            // No snapshot → no incremental tracking; process all files
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

    // Feed the engine (relative path, text) pairs. Relative paths are the
    // canonical file identity in the pipeline: the chunk processor derives
    // doc_id as `dt://doc/{project}/{rel_path}` (domain::id::make_document_id),
    // matching the Document nodes written by earlier builds and the
    // deleted_paths consumed by the build orchestration layer (§6.5).
    let engine_input: Vec<(PathBuf, String)> = files_to_process
        .iter()
        .map(|(_, t, rel, _, _)| (PathBuf::from(rel), t.clone()))
        .collect();

    // Build skip map for the engine: only pass non-empty skip sets
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

    // ── Mark successfully processed files' NEWLY EXECUTED steps as done.
    // We only mark steps that were NOT in the per-file skip set (i.e. were
    // actually executed by the engine), so that previously-skipped steps
    // remain correctly recorded.
    // Note: analyses carry relative paths (engine input contract above).
    if let Some(ref snap) = snapshot {
        for (_path, _text, rel_path, file_hash, steps_to_skip) in &files_to_process {
            let was_success = analyses
                .iter()
                .any(|a| a.file_path == Path::new(rel_path) && a.success);
            if was_success {
                for step in &active_steps {
                    if steps_to_skip.contains(*step) {
                        continue; // already marked from a previous run
                    }
                    if let Err(e) = snap
                        .mark_step_done(project_name, rel_path, step, file_hash)
                        .await
                    {
                        tracing::warn!("mark_step_done failed for {} step {}: {e}", rel_path, step,);
                    }
                }
            }
        }
    }

    // Log per-file errors at debug level
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

/// Handle `dt build --all` — build multiple projects in sequence.
///
/// Iterates over a list of (project_name, project_path) tuples, calling
/// `handle_build()` for each one. Errors are caught per-project and do
/// not abort the batch. A summary is printed at the end.
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

    println!("Building {} projects...", total);

    for (i, (name, path)) in projects.into_iter().enumerate() {
        let idx = i + 1; // 1-based display index
        println!("[{idx}/{total}] Building {name} at {}", path.display());

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
            None, // hanlp — not passed in build-all context
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

        // Brief pause between projects to let logs flush
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("Done. {succeeded} succeeded, {failed} failed.");

    Ok(())
}

/// Extract embedded ASCII word sequences from a string that may mix

/// Handle `dt search` — 统一检索渲染壳（U-D3：默认 world=all；--json 输出纯 JSON）。
pub async fn handle_search(
    query: String,
    world: String,
    limit: usize,
    json: bool,
    project: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!("搜索: query={query} world={world} limit={limit} json={json} project={project:?}");

    if !json {
        println!("Search: query=\"{query}\" world={world} limit={limit}");
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
    };
    let result = cws.search(&req).await?;

    if json {
        // U-D4：--json 时 stdout 仅含 JSON（header 行已抑制，日志走 stderr）
        println!("{}", crate::interfaces::cli::search_render::render_json(&result));
    } else {
        print!("{}", crate::interfaces::cli::search_render::render_human(&result));
    }
    Ok(())
}

/// Print config chunk search results with full text.
fn print_config_chunk_results(items: &[RankedItem]) {
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if !seen.insert(item.title.clone()) {
            continue;
        }
        println!("  [{:.4}] {}", item.score, item.title);
        for line in item.snippet.lines().take(20) {
            println!("         {}", line);
        }
        if item.snippet.lines().count() > 20 {
            println!(
                "         ... ({} more lines)",
                item.snippet.lines().count() - 20
            );
        }
    }
}

/// Handle `dt search-kg` — hybrid KG search (vector + keyword).
///
/// Uses multi-query expansion (Chinese + English) with Reciprocal Rank
/// Fusion for vector search on `kg_nodes`, combined with CONTAINS
/// keyword search on business labels. This hybrid approach bridges the
/// language gap between Chinese queries and English config property names.
pub async fn handle_search_kg(
    query: String,
    limit: usize,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!("搜索-KG: \"{query}\" --limit {limit}");

    println!("Search-KG: query=\"{query}\" limit={limit}");

    use crate::application::context::fusion::{reciprocal_rank_fusion, RankedItem};
    use crate::application::context::search_config::extract_ascii_words;

    let mut all_rank_lists: Vec<Vec<RankedItem>> = Vec::new();

    // ── 1. Keyword search on business labels ─────────────────
    if let Some(graph_ref) = &graph {
        // Extract English keywords from query
        let rewriter = crate::application::search::rewrite::QueryRewriter::with_defaults();
        let candidates = rewriter.rewrite(&query);
        let mut keywords: Vec<String> = Vec::new();
        for w in extract_ascii_words(&query) {
            if !keywords.contains(&w) {
                keywords.push(w);
            }
        }
        for c in candidates.iter().skip(1) {
            if c.chars().all(|ch| ch.is_ascii()) {
                for word in c.split_whitespace() {
                    let w = word.to_lowercase();
                    // Skip short/noisy expanded terms (e.g. "db" matches too much)
                    if w.len() >= 3 && !keywords.contains(&w) {
                        keywords.push(w);
                    }
                }
            }
        }
        keywords.truncate(5);

        if !keywords.is_empty() {
            let orig_ascii: Vec<String> = extract_ascii_words(&query);
            // Strategy: if query has ASCII words, use only them (expanded terms
            // like "url"/"host" are too broad). Chinese-only queries use expanded.
            let must_have = if !orig_ascii.is_empty() {
                format!(
                    "({})",
                    orig_ascii
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            } else {
                format!(
                    "({})",
                    keywords
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            };
            let cypher = format!(
                "MATCH (n) WHERE (n:ConfigKey OR n:ConfigSection OR n:Server OR n:Database \
                 OR n:NacosConfig OR n:NacosService OR n:K8sDeployment OR n:K8sService \
                 OR n:Knowledge OR n:Concept OR n:Domain OR n:Playbook) \
                 AND {} \
                 RETURN n.elementId AS id, labels(n)[0] AS type, \
                        coalesce(n.name, '') AS name, \
                        coalesce(n.value, n.summary, n.description, '') AS snippet \
                 ORDER BY size(n.name) \
                 LIMIT {}",
                must_have,
                (limit * 3).max(30)
            );
            let mut params: HashMap<String, serde_json::Value> = HashMap::new();
            for (i, k) in keywords.iter().enumerate() {
                params.insert(format!("kw{}", i), serde_json::Value::String(k.clone()));
            }
            match graph_ref.read_query(&cypher, params).await {
                Ok(result) => {
                    if let Some(rows) = result.as_array() {
                        let list: Vec<RankedItem> = rows
                            .iter()
                            .map(|row| RankedItem {
                                id: row
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                title: row
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                snippet: row
                                    .get("snippet")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                source_world: "graph".into(),
                                entity_type: row
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .to_string(),
                                score: 0.0,
                            })
                            .collect();
                        if !list.is_empty() {
                            all_rank_lists.push(list);
                        }
                    }
                }
                Err(e) => tracing::warn!("Search-KG graph query failed: {e}"),
            }
        }
    }

    // ── 2. Vector search on kg_nodes + _semantic collections ─────────
    if let Some(vec_repo) = &vector {
        {
            let embed = create_search_embed_client();
            let rewriter = crate::application::search::rewrite::QueryRewriter::with_defaults();
            let candidates = rewriter.rewrite(&query);
            let mut queries_to_embed = vec![query.clone()];
            for c in candidates.into_iter().skip(1) {
                if c != query && queries_to_embed.len() < 3 {
                    queries_to_embed.push(c);
                }
            }
            if let Ok(all_vectors) = embed.embed_batch(&queries_to_embed).await {
                if !all_vectors.is_empty() {
                    // Collect vector collections: kg_nodes (business nodes) + doc_chunks (global docs)
                    // Phase 5+6: only search global collections to avoid noise from legacy _semantic
                    let vector_collections = vec![
                        "kg_nodes".to_string(),
                        crate::shared::collections::DOC_CHUNKS.to_string(),
                    ];
                    for col in &vector_collections {
                        for qvec in &all_vectors {
                            if let Ok(results) =
                                vec_repo.search(col, qvec.clone(), (limit * 3) as u64).await
                            {
                                let mut rank_list: Vec<RankedItem> = Vec::new();
                                for r in results {
                                    let score =
                                        r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                    if score <= 0.0 {
                                        continue;
                                    }
                                    let payload =
                                        r.get("payload").or(r.get("result")).unwrap_or(&r);
                                    let id = r.get("id").map(|v| v.to_string()).unwrap_or_default();
                                    // For doc_chunks: extract file path from doc_id; for kg_nodes: use name as title
                                    let text =
                                        payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                    // Unified entity_type: prefer labels[] (kg_nodes), then doc_type, then infer from collection
                                    let label = payload
                                        .get("labels")
                                        .and_then(|v| v.as_array())
                                        .and_then(|arr| arr.first())
                                        .and_then(|v| v.as_str())
                                        .or_else(|| payload.get("label").and_then(|v| v.as_str()))
                                        .or_else(|| {
                                            payload.get("doc_type").and_then(|v| v.as_str())
                                        })
                                        .unwrap_or_else(|| {
                                            if col == "doc_chunks"
                                                || col == crate::shared::collections::DOC_CHUNKS
                                            {
                                                "Doc"
                                            } else if col == "kg_nodes" {
                                                "KG"
                                            } else {
                                                "?"
                                            }
                                        })
                                        .to_string();

                                    // Build title and snippet based on entity type
                                    let (name, desc) = if col == "doc_chunks"
                                        || col == crate::shared::collections::DOC_CHUNKS
                                    {
                                        // Doc chunks: extract file path from doc_id, show path + first line
                                        let doc_id = payload
                                            .get("doc_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let chunk_path = doc_id
                                            .strip_prefix("dt://doc/")
                                            .and_then(|s| s.rsplit_once('#'))
                                            .map(|(path, _)| path.to_string())
                                            .unwrap_or_default();
                                        let first_line: String = text
                                            .lines()
                                            .next()
                                            .unwrap_or("")
                                            .chars()
                                            .take(80)
                                            .collect();
                                        let title = if first_line.is_empty() {
                                            chunk_path.clone()
                                        } else {
                                            first_line.clone()
                                        };
                                        (title, format!("{}  {}", chunk_path, first_line))
                                    } else if col == "kg_nodes" {
                                        // KG nodes: name as title, description/summary as snippet
                                        let n = payload
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .filter(|s| !s.is_empty())
                                            .or_else(|| {
                                                payload.get("title").and_then(|v| v.as_str())
                                            })
                                            .unwrap_or("?")
                                            .to_string();
                                        let d = payload
                                            .get("description")
                                            .or(payload.get("summary"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        (n, d)
                                    } else {
                                        // Legacy _semantic: use text as title
                                        let t = if !text.is_empty() {
                                            text.chars().take(120).collect()
                                        } else {
                                            payload
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .filter(|s| !s.is_empty())
                                                .or_else(|| {
                                                    payload.get("key").and_then(|v| v.as_str())
                                                })
                                                .unwrap_or("?")
                                                .to_string()
                                        };
                                        let d = if !text.is_empty() {
                                            text.to_string()
                                        } else {
                                            payload
                                                .get("description")
                                                .or(payload.get("summary"))
                                                .or(payload.get("value"))
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string()
                                        };
                                        (t, d)
                                    };
                                    rank_list.push(RankedItem {
                                        id,
                                        title: name,
                                        snippet: desc,
                                        source_world: format!("vector/{}", col),
                                        entity_type: label,
                                        score,
                                    });
                                }
                                if !rank_list.is_empty() {
                                    all_rank_lists.push(rank_list);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── 3. Fuse with RRF and print ─────────────────────────────────
    if all_rank_lists.is_empty() {
        println!("  (no results)");
    } else {
        let mut fused = reciprocal_rank_fusion(all_rank_lists, 60.0, limit);
        // Filter out Document nodes — they're noisy for config/infra search
        fused.retain(|item| item.entity_type != "Document");
        if fused.is_empty() {
            println!("  (no results)");
        } else {
            // Deduplicate and display with type-aware formatting
            let mut seen_titles = std::collections::HashSet::new();
            for item in &fused {
                if !seen_titles.insert(item.title.clone()) {
                    continue;
                }
                println!(
                    "  [{:.4}] [{}] {}",
                    item.score, item.entity_type, item.title
                );

                // Type-aware snippet display:
                // - Doc: snippet is "file_path  first_line" — show as path reference
                // - Concept/Knowledge/Experience/Domain: show description (no file path)
                // - ConfigKey/NacosConfig/Server: show content (no local file)
                // - Method: snippet is "file_path  Class::sig  Lline-line"
                let snippet = &item.snippet;
                if !snippet.is_empty() {
                    let short = if snippet.chars().count() > 100 {
                        let truncated: String = snippet.chars().take(100).collect();
                        format!("{}…", truncated)
                    } else {
                        snippet.clone()
                    };
                    println!("         {}", short);
                }
            }
        }
    }

    Ok(())
}
