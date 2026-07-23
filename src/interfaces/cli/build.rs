//! CLI handlers for `dt build` and `dt search` commands.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::application::pipeline::config::PipelineConfig;
use crate::application::pipeline::engine::ProcessorEngine;
use crate::application::pipeline::infer_client::InferClient;
use crate::application::pipeline::processors::{
    ChunkProcessor, HanlpClientProcessor, LlmClientProcessor, StoreProcessor, TreeSitterProcessor,
};
use crate::application::pipeline::prompt::PromptRegistry;
use crate::application::pipeline::registry::ProcessorRegistry;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::BatchConfig;
use crate::application::search::fusion::RankedItem;
use crate::infrastructure::parser::ParserRegistry;
use crate::infrastructure::siliconflow::SiliconFlowClient;

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
) -> anyhow::Result<()> {
    // Determine project name
    let project_name = name.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    });

    if let Some(f) = &file {
        tracing::info!(
            "Build: project={}, path={}, file={} (full build; single-file hint unused)",
            project_name,
            path.display(),
            f.display(),
        );
    } else {
        tracing::info!(
            "Build: project={}, path={}",
            project_name,
            path.display(),
        );
    }

    // Execute build via BuildCommand
    let cmd = crate::application::build::builder::BuildCommand {
        project_path: path.clone(),
        project_name: project_name.clone(),
        full,
        verbose: true,
    };

    // Clone for pipeline use since BuildDependencies consumes the originals.
    let pipeline_graph = graph.as_ref().map(|g| Arc::clone(g) as Arc<dyn GraphRepository>);
    let pipeline_vector = vector.as_ref().map(|v| Arc::clone(v) as Arc<dyn VectorRepository>);
    let pipeline_embed = embed.as_ref().map(|e| Arc::clone(e) as Arc<dyn EmbedService>);

    // Create SiliconFlow client for Phase 2 (LLM analysis)
    let siliconflow = {
        use crate::infrastructure::siliconflow::SiliconFlowClient;
        let api_key = load_siliconflow_api_key();
        let base_url = load_siliconflow_config_str("url")
            .or_else(|| std::env::var("SILICONFLOW_BASE_URL").ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.siliconflow.cn/v1".to_string());
        let llm_model = load_siliconflow_config_str("model_llm")
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
    };

    let deps = crate::application::build::builder::BuildDependencies {
        graph,
        vector,
        snapshot,
        embed,
        siliconflow,
        batch_config: Some(batch_config),
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
        )
        .await
        {
            tracing::warn!("Pipeline analysis failed (non-fatal): {e}");
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
            tracing::info!("Reached pipeline file limit ({MAX_PIPELINE_FILES}) — truncating");
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        // Skip common binary / non-text extensions.
        let skip_ext = ["png", "jpg", "jpeg", "gif", "svg", "ico", "woff2",
                        "ttf", "eot", "pdf", "zip", "jar", "class", "o", "so",
                        "dylib", "dll", "exe", "bin", "db", "sqlite"];
        if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
            if skip_ext.contains(&ext) {
                continue;
            }
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            files.push((entry.path().to_path_buf(), content));
        }
    }

    tracing::debug!("Collected {} text files from {}", files.len(), root.display());
    files
}

/// Read the SiliconFlow API key from `~/.config/digital-twin/config.yaml`,
/// falling back to `SILICONFLOW_API_KEY` env var.
fn load_siliconflow_api_key() -> String {
    // Try env var first
    if let Ok(key) = std::env::var("SILICONFLOW_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // Try config file
    load_siliconflow_config_str("api_key").unwrap_or_default()
}

/// Read a field from `services.siliconflow.<field>` in config.yaml.
fn load_siliconflow_config_str(field: &str) -> Option<String> {
    let config_paths = [
        std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".config").join("digital-twin").join("config.yaml")),
        Some(std::path::PathBuf::from("config/config.yaml")),
    ];
    for config_path in config_paths.iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(config_path) {
            use serde::Deserialize;
            #[derive(Deserialize)]
            struct SfCfg {
                #[serde(default)]
                url: Option<String>,
                #[serde(default)]
                api_key: Option<String>,
                #[serde(default)]
                model_embed: Option<String>,
                #[serde(default)]
                model_reranker: Option<String>,
                #[serde(default)]
                model_llm: Option<String>,
            }
            #[derive(Deserialize)]
            struct Services { siliconflow: Option<SfCfg> }
            #[derive(Deserialize)]
            struct Cfg { services: Option<Services> }
            if let Ok(cfg) = serde_yaml::from_str::<Cfg>(&content) {
                let sf = cfg.services.and_then(|s| s.siliconflow)?;
                let val = match field {
                    "url" => sf.url,
                    "api_key" => sf.api_key,
                    "model_embed" => sf.model_embed,
                    "model_reranker" => sf.model_reranker,
                    "model_llm" => sf.model_llm,
                    _ => None,
                };
                if let Some(v) = val {
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
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
) -> anyhow::Result<()> {
    // ── 1. Load pipeline config — skip if disabled ────────────────
    let pipeline_config = PipelineConfig::load().map_err(|e| anyhow::anyhow!("{e}"))?;
    if !pipeline_config.enabled {
        tracing::info!("Pipeline disabled (config/pipeline.yaml enabled=false)");
        return Ok(());
    }
    tracing::info!("Pipeline analysis starting for {project_name}...");

    // ── 2. Connect to SiliconFlow cloud API ─────────────────────
    let infer_client = Arc::new(InferClient::new(
        "https://api.siliconflow.cn/v1".to_string(),
        2, // max concurrent — conservatively low for cloud API
    ));
    let api_key = std::env::var("SILICONFLOW_API_KEY").unwrap_or_default();

    let inference_available = match infer_client.health_check().await {
        Ok(true) => {
            tracing::info!("Inference server available");
            true
        }
        Ok(false) => {
            tracing::info!("Inference server not reachable — skipping GPU processors");
            false
        }
        Err(e) => {
            tracing::warn!("Inference server health check failed: {e} — skipping GPU processors");
            false
        }
    };

    // ── 3. Build processor registry ───────────────────────────────
    let mut registry = ProcessorRegistry::new();

    if pipeline_config.processors.tree_sitter {
        let parser_registry = Arc::new(ParserRegistry::new());
        registry.register(Box::new(TreeSitterProcessor::new(parser_registry)));
        tracing::info!("  Processor: TreeSitter");
    }
    if pipeline_config.processors.chunk {
        registry.register(Box::new(ChunkProcessor::default()));
        tracing::info!("  Processor: Chunk");
    }
    if pipeline_config.processors.hanlp && inference_available {
        registry.register(Box::new(HanlpClientProcessor::with_client(infer_client.clone())));
        tracing::info!("  Processor: Hanlp");
    }
    if pipeline_config.processors.llm && inference_available {
        match PromptRegistry::load(Path::new("config/prompts")) {
            Ok(prompts) => {
                let llm_config = pipeline_config.llm.unwrap_or_default();
                registry.register(Box::new(LlmClientProcessor::new(
                    infer_client.clone(),
                    Arc::new(prompts),
                    llm_config,
                )));
                tracing::info!("  Processor: LlmClient");
            }
            Err(e) => {
                tracing::warn!("  Prompt registry unavailable: {e} — skipping LLM processor");
            }
        }
    }
    if pipeline_config.processors.store {
        registry.register(Box::new(StoreProcessor::new(graph, vector, embed)));
        tracing::info!("  Processor: Store");
    }

    if registry.is_empty() {
        tracing::info!("No pipeline processors registered — skipping analysis");
        return Ok(());
    }

    // ── 4. Run pipeline ───────────────────────────────────────────
    let registry = Arc::new(registry);
    let engine = ProcessorEngine::new(registry, pipeline_config.inference_server.max_concurrent);

    let files = collect_project_files(project_path);
    tracing::info!("Pipeline analyzing {} files...", files.len());

    let analyses = engine.analyze_batch(files, project_name.to_string()).await;
    let success_count = analyses.iter().filter(|a| a.success).count();
    let error_count = analyses.len() - success_count;

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
        "Pipeline analysis complete for {project_name}: \
         {} files analyzed, {} OK, {} with errors",
        analyses.len(),
        success_count,
        error_count,
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
/// Chinese and English (e.g. "Redis集群配置信息" → ["Redis"],
/// "我所有的MySQL数据库地址" → ["MySQL"]).
fn extract_ascii_words(s: &str) -> Vec<String> {
    let re = regex::Regex::new(r"[a-zA-Z0-9_.-]+").unwrap();
    re.find_iter(s)
        .map(|m| m.as_str().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Handle `dt search` — semantic code search across worlds.
///
/// For "code" world: vector search across `*_methods` collections, falls
/// back to CONTAINS text search.
/// For "config"/"all" worlds: **hybrid search** — Qdrant vector search on
/// `kg_nodes` + keyword CONTAINS search on config labels, fused
/// with Reciprocal Rank Fusion. Multi-query expansion (Chinese + English)
/// bridges the language gap between user queries and config property names.
/// For "knowledge" / "memory" / etc.: Cypher text search.
pub async fn handle_search(
    query: String,
    world: String,
    limit: usize,
    path: Option<PathBuf>,
    project: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: search --query {query} --world {world} --limit {limit} --project {:?} --path {:?}",
        project, path.as_ref().map(|p| p.display().to_string()),
    );

    println!("Search: query=\"{query}\" world={world} limit={limit}");
    if let Some(ref p) = project {
        println!("  project: {p}");
    }
    if let Some(ref p) = path {
        println!("  scope: {}", p.display());
    }

    // ── Helper: get English keyword terms from query ────────────────
    let get_keywords = |q: &str| -> Vec<String> {
        let rewriter = crate::application::search::rewrite::QueryRewriter::with_defaults();
        let candidates = rewriter.rewrite(q);
        let mut terms: Vec<String> = Vec::new();
        // Take original query words (extract embedded English terms like "Redis" from "Redis集群")
        for w in extract_ascii_words(q) {
            if !terms.contains(&w) { terms.push(w); }
        }
        // Take English-expanded terms (skip short/noisy ones like "db")
        for c in candidates.iter().skip(1) {
            if c.chars().all(|ch| ch.is_ascii()) {
                for word in c.split_whitespace() {
                    let w = word.to_lowercase();
                    if w.len() >= 3 && !terms.contains(&w) {
                        terms.push(w);
                    }
                }
            }
        }
        terms.truncate(5);
        terms
    };

    // ── Config world: vector search on config_chunks (Qdrant) ────────────
    // Uses full-chunk text embeddings to bridge the Chinese→English gap
    // that prevented effective vector search on individual ConfigKey names.
    // Falls back to keyword search when dt-embed is unavailable.
    if world == "config" {
        // Attempt vector search on config_chunks
        if let Some(vec_repo) = &vector {
            let embed = Arc::new(SiliconFlowClient::new(
                crate::infrastructure::siliconflow::base_url_from_env(),
                crate::infrastructure::siliconflow::api_key_from_env(),
                crate::infrastructure::siliconflow::embed_model_from_env(),
                crate::infrastructure::siliconflow::reranker_model_from_env(),
                crate::infrastructure::siliconflow::llm_model_from_env(),
            )) as Arc<dyn EmbedService>;
            // Build query variants for multi-vector fusion
            let queries: Vec<String> = {
                let mut qs = vec![query.clone()];
                let ascii_terms: Vec<String> = extract_ascii_words(&query);
                for t in &ascii_terms {
                    if *t != query && !qs.contains(t) { qs.push(t.clone()); }
                }
                if !query.to_lowercase().contains("config") {
                    qs.push(format!("{} config", query));
                }
                qs.truncate(3);
                qs
            };

            if let Ok(all_vectors) = embed.embed_batch(&queries).await {
                use crate::application::search::fusion::{RankedItem, reciprocal_rank_fusion};
                let mut rank_lists: Vec<Vec<RankedItem>> = Vec::new();
                for qvec in &all_vectors {
                    if let Ok(results) = vec_repo.search(
                        "config_chunks", qvec.clone(), (limit * 2) as u64,
                    ).await {
                        let list: Vec<RankedItem> = results.iter().map(|r| {
                            let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let payload = r.get("payload").unwrap_or(r);
                            let section = payload.get("section_name").and_then(|v| v.as_str()).unwrap_or("");
                            let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let data_id = payload.get("data_id").and_then(|v| v.as_str()).unwrap_or("");
                            let ns = payload.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
                            RankedItem {
                                id: r.get("id").map(|v| v.to_string()).unwrap_or_default(),
                                title: format!("[{}:{}] ({} keys)",
                                    data_id, section,
                                    payload.get("key_count").and_then(|v| v.as_u64()).unwrap_or(0)),
                                snippet: text.to_string(),
                                source_world: "vector/config_chunks".into(),
                                entity_type: "ConfigChunk".into(),
                                score,
                            }
                        }).collect();
                        if !list.is_empty() { rank_lists.push(list); }
                    }
                }
                if !rank_lists.is_empty() {
                    let fused = reciprocal_rank_fusion(rank_lists, 60.0, limit);
                    print_config_chunk_results(&fused);
                    return Ok(());
                }
            }
        }
        // Fallback: CONTAINS keyword search on ConfigKey nodes
        println!("  (vector search unavailable — falling back to keyword search)");
        if let Some(graph_ref) = &graph {
            let keywords = get_keywords(&query);
            if !keywords.is_empty() {
                // Split keywords: original ASCII terms (must-have) vs expanded terms
                let orig_ascii: Vec<String> = query.split_whitespace()
                    .filter(|w| w.chars().all(|c| c.is_ascii()))
                    .map(|w| w.to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect();
                // Strategy:
                // - For queries with ASCII words: use ONLY the original ASCII terms
                //   (expanded English e.g. "url"/"host" are too broad and match noise)
                // - For Chinese-only queries: use all expanded English terms
                let must_have = if !orig_ascii.is_empty() {
                    // Use original ASCII terms only (e.g. "redis" → spring.redis.*)
                    format!("({})", orig_ascii.iter().enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>().join(" OR "))
                } else {
                    // Chinese-only query: use all expanded English terms
                    format!("({})", keywords.iter().enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>().join(" OR "))
                };
                let display_limit = if limit > 200 { limit } else { limit.max(50) };
                let cypher = format!(
                    "MATCH (n) WHERE (n:ConfigKey OR n:Server \
                     OR n:Database OR n:NacosConfig OR n:NacosService) \
                     AND {} \
                     RETURN labels(n)[0] AS type, coalesce(n.name, '') AS name, \
                            coalesce(n.value, n.summary, n.description, '') AS snippet, \
                            coalesce(n.namespace, n.environment, n.project, '') AS source \
                     ORDER BY size(n.name), n.name \
                     LIMIT {}",
                    must_have, display_limit
                );
                let mut params: HashMap<String, serde_json::Value> = HashMap::new();
                for (i, k) in keywords.iter().enumerate() {
                    params.insert(format!("kw{}", i), serde_json::Value::String(k.clone()));
                }
                match graph_ref.read_query(&cypher, params).await {
                    Ok(result) => {
                        if let Some(rows) = result.as_array() {
                            let mut seen_names = std::collections::HashSet::new();
                            for row in rows {
                                let ty = row.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                // Deduplicate: only show first occurrence of each config name
                                if !seen_names.insert(name.to_string()) { continue; }
                                let snippet = row.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                                let display = if snippet.is_empty() { String::new() } else { format!(": {}", snippet) };
                                println!("  [{ty}] {name}{display}");
                            }
                            return Ok(());
                        }
                    }
                    Err(e) => tracing::warn!("Config search failed: {e}"),
                }
            }
        }
        println!("  (no results)");
        return Ok(());
    }

    // ── Shared: RankedItem collection from vector search + keyword ────
    use crate::application::search::fusion::{RankedItem, reciprocal_rank_fusion};
    let mut all_rank_lists: Vec<Vec<RankedItem>> = Vec::new();

    // ── Vector search path: code / all worlds ───────────────────────
    let did_vector_search = if (world == "code" || world == "all") && vector.is_some() {
        let vec_repo = vector.as_ref().unwrap();

        let embed: Option<Arc<dyn EmbedService>> = {
            tracing::info!("Xinference client created for vector search");
            Some(Arc::new(SiliconFlowClient::new(
                crate::infrastructure::siliconflow::base_url_from_env(),
                crate::infrastructure::siliconflow::api_key_from_env(),
                crate::infrastructure::siliconflow::embed_model_from_env(),
                crate::infrastructure::siliconflow::reranker_model_from_env(),
                crate::infrastructure::siliconflow::llm_model_from_env(),
            )) as Arc<dyn EmbedService>)
        };

        if let Some(embed_svc) = embed {
            let queries_to_embed: Vec<String> = if world == "code" {
                vec![query.clone()]
            } else {
                let rewriter = crate::application::search::rewrite::QueryRewriter::with_defaults();
                let candidates = rewriter.rewrite(&query);
                let mut qs = vec![query.clone()];
                for c in candidates.into_iter().skip(1) {
                    if c != query && qs.len() < 2 {
                        qs.push(c);
                    }
                }
                qs
            };

            let all_query_vectors = embed_svc.embed_batch(&queries_to_embed).await
                .map_err(|e| anyhow::anyhow!("embed failed: {e}"))?;

            if !all_query_vectors.is_empty() {
                let collections_to_search: Vec<String> = match world.as_str() {
                    "code" => {
                        if let Some(ref proj) = project {
                            vec![format!("{}_methods", proj)]
                        } else {
                            let collections = match vec_repo.list_collections().await {
                                Ok(cols) => cols,
                                Err(e) => {
                                    tracing::warn!("Failed to list Qdrant collections: {e}");
                                    vec![]
                                }
                            };
                            collections.into_iter()
                                .filter(|c| c.ends_with("_methods"))
                                .collect()
                        }
                    }
                    "all" => {
                        let mut cols = vec!["kg_nodes".to_string()];
                        if let Some(ref proj) = project {
                            cols.push(format!("{}_methods", proj));
                            cols.push(format!("{}_semantic", proj));
                        } else if let Ok(all_cols) = vec_repo.list_collections().await {
                            cols.extend(all_cols.into_iter().filter(|c| c.ends_with("_methods") || c.ends_with("_semantic")));
                        }
                        cols
                    }
                    _ => vec![],
                };

                for qvec in &all_query_vectors {
                    for col in &collections_to_search {
                        if let Ok(results) = vec_repo.search(col, qvec.clone(), (limit * 3) as u64).await {
                            let mut rank_list: Vec<RankedItem> = Vec::new();
                            for r in results {
                                let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                if score <= 0.0 { continue; }
                                let payload = r.get("payload").or(r.get("result")).unwrap_or(&r);
                                let id = r.get("id").map(|v| v.to_string()).unwrap_or_default();
                                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let entity_type = payload.get("labels")
                                    .and_then(|v| v.as_array()).and_then(|arr| arr.first()).and_then(|v| v.as_str())
                                    .or_else(|| payload.get("label").and_then(|v| v.as_str()))
                                    .or_else(|| {
                                        // Infer from collection name for code search
                                        if col.ends_with("_methods") { Some("Method") }
                                        else if col.ends_with("_semantic") { Some("Code") }
                                        else { None }
                                    })
                                    .unwrap_or("?").to_string();
                                let desc = if col.ends_with("_methods") {
                                    let file = payload.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
                                    let start = payload.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let end = payload.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let sig = payload.get("signature").and_then(|v| v.as_str()).unwrap_or("");
                                    let cls = payload.get("class_name").and_then(|v| v.as_str()).unwrap_or("");
                                    format!("{}  {}::{}  L{}-{}", file, cls, sig, start, end)
                                } else {
                                    payload.get("description").or(payload.get("summary"))
                                        .or(payload.get("value")).and_then(|v| v.as_str()).unwrap_or("").to_string()
                                };
                                rank_list.push(RankedItem { id, title: name, snippet: desc, source_world: format!("vector/{}", col), entity_type, score });
                            }
                            if !rank_list.is_empty() { all_rank_lists.push(rank_list); }
                        }
                    }
                }
            }
        }
        true
    } else {
        false
    };

    // ── All world: also add keyword search on config labels ──
    if world == "all" {
        if let Some(graph_ref) = &graph {
            let keywords = get_keywords(&query);
            if !keywords.is_empty() {
                let orig_ascii: Vec<String> = query.split_whitespace()
                    .filter(|w| w.chars().all(|c| c.is_ascii()))
                    .map(|w| w.to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect();
                let must_have = if !orig_ascii.is_empty() {
                    format!("({})", orig_ascii.iter().enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>().join(" OR "))
                } else {
                    format!("({})", keywords.iter().enumerate()
                        .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                        .collect::<Vec<_>>().join(" OR "))
                };
                let cypher = format!(
                    "MATCH (n) WHERE (n:ConfigKey OR n:ConfigSection OR n:Server \
                     OR n:Database OR n:NacosConfig OR n:NacosService) \
                     AND {} \
                     RETURN n.elementId AS id, labels(n)[0] AS type, \
                            coalesce(n.name, '') AS name, \
                            coalesce(n.value, n.summary, n.description, '') AS snippet \
                     LIMIT {}",
                    must_have, limit
                );
                let mut params: HashMap<String, serde_json::Value> = HashMap::new();
                for (i, k) in keywords.iter().enumerate() {
                    params.insert(format!("kw{}", i), serde_json::Value::String(k.clone()));
                }
                if let Ok(result) = graph_ref.read_query(&cypher, params).await {
                    if let Some(rows) = result.as_array() {
                        let list: Vec<RankedItem> = rows.iter().map(|row| {
                            RankedItem {
                                id: row.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                title: row.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                snippet: row.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                source_world: "graph/config".into(),
                                entity_type: row.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                                score: 0.0,
                            }
                        }).collect();
                        if !list.is_empty() { all_rank_lists.push(list); }
                    }
                }
            }
        }
    }

    // ── Fuse vector + keyword with RRF ─────────────────────────────
    if !all_rank_lists.is_empty() {
        let mut fused = reciprocal_rank_fusion(all_rank_lists, 60.0, limit);
        if world != "code" {
            fused.retain(|item| item.entity_type != "Document");
        }
        if !fused.is_empty() {
            let mut seen_titles = std::collections::HashSet::new();
            for item in &fused {
                if !seen_titles.insert(item.title.clone()) { continue; }
                println!("  [{:.4}] [{}] {}", item.score, item.entity_type, item.title);
                if world == "code" && !item.snippet.is_empty() {
                    // snippet format: "file_path  Class::signature  Lline-line"
                    for line in item.snippet.lines() {
                        println!("         {}", line);
                    }
                } else {
                    let short_snippet = if item.snippet.chars().count() > 80 {
                        let truncated: String = item.snippet.chars().take(80).collect();
                        format!("{}…", truncated)
                    } else {
                        item.snippet.clone()
                    };
                    if !short_snippet.is_empty() {
                        println!("         {}", short_snippet);
                    }
                }
            }
            return Ok(());
        }
    }

    // Fall through: code world still needs keyword fallback
    if world == "code" && did_vector_search {
        println!("  (vector search unavailable — falling back to keyword text search)");
    }

    // ── Cypher text search for code/knowledge/memory/other ──
    // (Also serves as fallback for code when vector is down)
    match graph {
        Some(graph_ref) => {
            let cypher = match world.as_str() {
                "code" | "reality" => {
                    let project_filter = project.as_ref()
                        .map(|p| format!(" AND n.project = '{}' ", p.replace('\'', "\\'")))
                        .unwrap_or_default();
                    format!(
                        "MATCH (n) WHERE (n:Method OR n:Class OR n:Interface) \
                         AND (n.name CONTAINS $q OR n.file_path CONTAINS $q){project_filter}\
                         RETURN labels(n)[0] AS type, coalesce(n.name, n.method_name, n.class_name, '') AS name, \
                                coalesce(n.file_path, '') AS source \
                         LIMIT {limit}"
                    )
                },
                "knowledge" => format!(
                    "MATCH (n) WHERE (n:Knowledge OR n:Experience OR n:Concept OR n:Domain OR n:Playbook) \
                     AND (n.name CONTAINS $q OR n.title CONTAINS $q OR n.summary CONTAINS $q) \
                     RETURN labels(n)[0] AS type, coalesce(n.name, n.title, '') AS name, \
                            coalesce(n.summary, n.description, '') AS desc \
                     LIMIT {limit}"
                ),
                "memory" => format!(
                    "MATCH (n) WHERE (n:Modification OR n:Deployment OR n:ConfigChange \
                     OR n:BugFix OR n:Decision OR n:Conversation OR n:Session) \
                     AND (n.details CONTAINS $q OR coalesce(n.summary, '') CONTAINS $q) \
                     RETURN labels(n)[0] AS type, coalesce(n.name, n.entity_id, n.session_id, '') AS name, \
                            coalesce(n.details, n.summary, '') AS desc \
                     LIMIT {limit}"
                ),
                _ => format!(
                    "MATCH (n) WHERE n.name CONTAINS $q OR n.title CONTAINS $q \
                     OR n.details CONTAINS $q OR n.summary CONTAINS $q \
                     RETURN labels(n)[0] AS type, coalesce(n.name, n.title, '') AS name, \
                            coalesce(n.summary, n.details, '') AS desc \
                     LIMIT {limit}"
                ),
            };

            let mut params = HashMap::new();
            params.insert("q".into(), serde_json::Value::String(query.clone()));
            match graph_ref.read_query(&cypher, params).await {
                Ok(result) => {
                    if let Some(rows) = result.as_array() {
                        if rows.is_empty() {
                            println!("  (no results)");
                        } else {
                            for row in rows {
                                let ty = row.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let desc = row.get("desc").or(row.get("source"))
                                    .and_then(|v| v.as_str()).unwrap_or("");
                                println!("  [{ty}] {name}: {desc}");
                            }
                        }
                    } else {
                        println!("  (no results)");
                    }
                }
                Err(e) => eprintln!("  Search error: {e}"),
            }
        }
        None => {
            if vector.is_none() {
                tracing::warn!("Neither graph database nor Qdrant available — no search results");
                println!("  (No search backend available)");
            }
        }
    }

    Ok(())
}

/// Print config chunk search results with full text.
fn print_config_chunk_results(items: &[RankedItem]) {
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if !seen.insert(item.title.clone()) { continue; }
        println!("  [{:.4}] {}", item.score, item.title);
        for line in item.snippet.lines().take(20) {
            println!("         {}", line);
        }
        if item.snippet.lines().count() > 20 {
            println!("         ... ({} more lines)", item.snippet.lines().count() - 20);
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
    tracing::info!("dt-daemon CLI: search-kg \"{query}\" --limit {limit}");

    println!("Search-KG: query=\"{query}\" limit={limit}");

    use crate::application::search::fusion::{RankedItem, reciprocal_rank_fusion};

    let mut all_rank_lists: Vec<Vec<RankedItem>> = Vec::new();

    // ── 1. Keyword search on business labels ─────────────────
    if let Some(graph_ref) = &graph {
        // Extract English keywords from query
        let rewriter = crate::application::search::rewrite::QueryRewriter::with_defaults();
        let candidates = rewriter.rewrite(&query);
        let mut keywords: Vec<String> = Vec::new();
        for w in extract_ascii_words(&query) {
            if !keywords.contains(&w) { keywords.push(w); }
        }
        for c in candidates.iter().skip(1) {
            if c.chars().all(|ch| ch.is_ascii()) {
                for word in c.split_whitespace() {
                    let w = word.to_lowercase();
                    // Skip short/noisy expanded terms (e.g. "db" matches too much)
                    if w.len() >= 3 && !keywords.contains(&w) { keywords.push(w); }
                }
            }
        }
        keywords.truncate(5);

        if !keywords.is_empty() {
                let orig_ascii: Vec<String> = extract_ascii_words(&query);
            // Strategy: if query has ASCII words, use only them (expanded terms
            // like "url"/"host" are too broad). Chinese-only queries use expanded.
            let must_have = if !orig_ascii.is_empty() {
                format!("({})", orig_ascii.iter().enumerate()
                    .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                    .collect::<Vec<_>>().join(" OR "))
            } else {
                format!("({})", keywords.iter().enumerate()
                    .map(|(i, _)| format!("toLower(n.name) CONTAINS toLower($kw{})", i))
                    .collect::<Vec<_>>().join(" OR "))
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
                must_have, (limit * 3).max(30)
            );
            let mut params: HashMap<String, serde_json::Value> = HashMap::new();
            for (i, k) in keywords.iter().enumerate() {
                params.insert(format!("kw{}", i), serde_json::Value::String(k.clone()));
            }
            match graph_ref.read_query(&cypher, params).await {
                Ok(result) => {
                    if let Some(rows) = result.as_array() {
                        let list: Vec<RankedItem> = rows.iter().map(|row| {
                            RankedItem {
                                id: row.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                title: row.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                snippet: row.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                source_world: "graph".into(),
                                entity_type: row.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                                score: 0.0,
                            }
                        }).collect();
                        if !list.is_empty() { all_rank_lists.push(list); }
                    }
                }
                Err(e) => tracing::warn!("Search-KG graph query failed: {e}"),
            }
        }
    }

    // ── 2. Vector search on kg_nodes + _semantic collections ─────────
    if let Some(vec_repo) = &vector {
        {
            let embed = Arc::new(SiliconFlowClient::new(
                crate::infrastructure::siliconflow::base_url_from_env(),
                crate::infrastructure::siliconflow::api_key_from_env(),
                crate::infrastructure::siliconflow::embed_model_from_env(),
                crate::infrastructure::siliconflow::reranker_model_from_env(),
                crate::infrastructure::siliconflow::llm_model_from_env(),
            )) as Arc<dyn EmbedService>;
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
                    // Collect all vector collections to search: kg_nodes + all *_semantic
                    let mut vector_collections = vec!["kg_nodes".to_string()];
                    if let Ok(cols) = vec_repo.list_collections().await {
                        vector_collections.extend(cols.into_iter().filter(|c| c.ends_with("_semantic")));
                    }
                    for col in &vector_collections {
                        for qvec in &all_vectors {
                            if let Ok(results) = vec_repo.search(col, qvec.clone(), (limit * 3) as u64).await {
                                let mut rank_list: Vec<RankedItem> = Vec::new();
                                for r in results {
                                    let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                    if score <= 0.0 { continue; }
                                    let payload = r.get("payload").or(r.get("result")).unwrap_or(&r);
                                    let id = r.get("id").map(|v| v.to_string()).unwrap_or_default();
                                    // For _semantic collections, use text as title/desc
                                    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                    let name = if !text.is_empty() {
                                        text.chars().take(120).collect()
                                    } else {
                                        payload.get("name").and_then(|v| v.as_str())
                                            .filter(|s| !s.is_empty())
                                            .or_else(|| payload.get("key").and_then(|v| v.as_str()))
                                            .unwrap_or("?").to_string()
                                    };
                                    let label = payload.get("doc_type").and_then(|v| v.as_str())
                                        .or_else(|| payload.get("label").and_then(|v| v.as_str()))
                                        .or_else(|| payload.get("labels").and_then(|v| v.as_array())
                                            .and_then(|arr| arr.first()).and_then(|v| v.as_str()))
                                        .unwrap_or("?").to_string();
                                    let desc = if !text.is_empty() { text.to_string() }
                                        else { payload.get("description").or(payload.get("summary"))
                                            .or(payload.get("value")).or(payload.get("text"))
                                            .and_then(|v| v.as_str()).unwrap_or("").to_string() };
                                    rank_list.push(RankedItem { id, title: name, snippet: desc, source_world: format!("vector/{}", col), entity_type: label, score });
                                }
                                if !rank_list.is_empty() { all_rank_lists.push(rank_list); }
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
            // Deduplicate and truncate snippets for clean display
            let mut seen_titles = std::collections::HashSet::new();
            for item in &fused {
                if !seen_titles.insert(item.title.clone()) { continue; }
                let short_snippet = if item.snippet.chars().count() > 80 {
                    let truncated: String = item.snippet.chars().take(80).collect();
                    format!("{}…", truncated)
                } else {
                    item.snippet.clone()
                };
                println!("  [{:.4}] {} → {}",
                    item.score, item.entity_type, item.title);
                if !short_snippet.is_empty() {
                    println!("         {}", short_snippet);
                }
            }
        }
    }

    Ok(())
}
