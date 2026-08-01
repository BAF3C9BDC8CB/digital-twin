//! Pipeline template — Template Method pattern for the build pipeline.
//!
//! `PipelineTemplate::execute()` defines the fixed build flow:
//! 1. Scan files
//! 2. Select files via strategy
//! 3. Prepare storage
//! 4. Parse files
//! 5. Embed + write vectors
//! 6. Write graph
//! 7. Rebuild call graph
//! 8. Update snapshots
//!
//! Sub-steps (strategy selection, prepare) are delegated to the
//! [`BuildStrategy`] trait.

use crate::domain::error::DtError;
use crate::domain::id::make_document_id;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, BuildReport, FileSnapshot, ScanConfig};
use futures::stream::{self, StreamExt};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

use super::strategy::BuildStrategy;
use crate::application::knowledge::extract::purge_document;
use crate::infrastructure::parser::ParserRegistry;
use crate::infrastructure::scanner;
use crate::infrastructure::siliconflow::SiliconFlowClient;

/// Maximum concurrent LLM analysis requests to SiliconFlow.
const PHASE2_CONCURRENCY: usize = 5;

/// Default prompt path (used when config/prompts/code_analysis.yaml is missing).
const PHASE2_DEFAULT_PROMPT: &str = "\
你收到的每条消息都是一段代码，而不是提问。直接分析这段代码。\n\
\n\
始终输出两行：\n\
用途：<该方法的功能，15字以内>\n\
逻辑：<实现原理，15字以内>\n\
\n\
规则：\n\
- 仅用中文，不要markdown\n\
- 不要反问、不要提示用户提供代码\n\
- 代码为空或无法识别时输出：用途：未知  逻辑：无法解析\n\
- 严格控制在两行\n\
\n\
示例：\n\
代码：def add(a, b): return a + b\n\
用途：将两个数相加并返回结果。\n\
逻辑：接收两个参数并返回它们的和。\n\
\n\
代码：public String getName() { return this.name; }\n\
用途：返回名称字段的值。\n\
逻辑：从对象属性中读取并返回name字段。\
";

/// Result of extracting entities from all changed files.
pub struct ExtractionResult {
    pub methods: Vec<crate::domain::types::MethodBlock>,
    pub classes: Vec<crate::domain::types::ClassBlock>,
    pub modules: Vec<crate::domain::types::ModuleBlock>,
    pub snapshots: Vec<FileSnapshot>,
    /// @knowledge annotations extracted from code comments.
    pub knowledge_annotations:
        Vec<crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation>,
}

/// The pipeline template that orchestrates the build flow.
pub struct PipelineTemplate {
    parser_registry: Arc<ParserRegistry>,
    batch_config: BatchConfig,
    /// Optional SiliconFlow client for Phase 2 (code semantic analysis).
    siliconflow: Option<Arc<SiliconFlowClient>>,
    /// Skip vector embedding (set processors.embed=false to preserve existing vectors).
    skip_embed: bool,
}

impl PipelineTemplate {
    /// Create a new pipeline template with the given parser registry.
    pub fn new(
        parser_registry: Arc<ParserRegistry>,
        batch_config: BatchConfig,
        siliconflow: Option<Arc<SiliconFlowClient>>,
    ) -> Self {
        Self {
            parser_registry,
            batch_config,
            siliconflow,
            skip_embed: false,
        }
    }

    /// Set skip_embed flag (processors.embed=false in config).
    pub fn with_skip_embed(mut self, skip: bool) -> Self {
        self.skip_embed = skip;
        self
    }

    /// Execute the full build pipeline.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        project: &str,
        root: &Path,
        strategy: &dyn BuildStrategy,
        scan_config: &ScanConfig,
        snapshot_repo: Option<Arc<dyn SnapshotRepository>>,
        graph: Option<&dyn GraphRepository>,
        embed: Option<Arc<dyn EmbedService>>,
        vector: Option<Arc<dyn VectorRepository>>,
    ) -> Result<BuildReport, DtError> {
        let start = std::time::Instant::now();

        // Step 1: Scan files
        let all_files = scanner::collect_files(root, scan_config);
        let files_scanned = all_files.len();

        // Step 1b: Scan document files
        let doc_files = scanner::collect_document_files(root, scan_config);

        // Step 2: Select files via strategy
        let (files_to_process, deleted) = strategy
            .select_files(root, &all_files, snapshot_repo.as_deref(), project)
            .await?;
        let files_changed = files_to_process.len();

        // Step 3: Delete data for deleted files
        if let Some(graph) = graph {
            if !deleted.is_empty() {
                delete_files_from_graph(graph, project, &deleted).await;
            }
        }

        // Step 3b (§6.5, Task 3): document lifecycle — purge deleted
        // documents, keep snapshot baselines for change/deletion detection.
        // Document extraction/consolidation itself runs in the pipeline
        // engine (tree_sitter → chunk → hanlp → llm → store), not here.
        //
        // The snapshots table mixes code and document rows per project, so
        // diffing only the document file set reports every code path as
        // "deleted" — keep only real document deletions by extension.
        let (changed_docs, deleted_docs) = strategy
            .select_files(root, &doc_files, snapshot_repo.as_deref(), project)
            .await?;
        let deleted_docs: Vec<String> = deleted_docs
            .into_iter()
            .filter(|p| is_document_path(p, &scan_config.document_extensions))
            .collect();

        // §6.5.2: purge deleted documents — RELATES/MENTIONED_IN edges,
        // Document node, and doc_chunks vector points. Per-file state is
        // cleared only for successfully purged documents: a failed purge (or
        // missing backends) keeps the baseline, so the deletion is reported
        // again on the next build instead of leaking artifacts.
        if !deleted_docs.is_empty() {
            if let (Some(graph), Some(vector)) = (graph, vector.as_deref()) {
                let mut purged: Vec<String> = Vec::with_capacity(deleted_docs.len());
                for rel in &deleted_docs {
                    let doc_id = make_document_id(project, rel);
                    match purge_document(graph, vector, &doc_id).await {
                        Ok(()) => purged.push(rel.clone()),
                        Err(e) => tracing::warn!("purge deleted document {doc_id} failed: {e}"),
                    }
                }
                if !purged.is_empty() {
                    if let Some(repo) = snapshot_repo.as_deref() {
                        let _ = repo.delete_file_progress(project, &purged).await;
                    }
                }
            }
        }

        // Step 4: Prepare storage (strategy-specific)
        strategy.prepare(graph, vector.as_deref(), project).await?;

        // Full rebuild: clear all pipeline step progress and LLM progress
        // so that incremental tracking starts from scratch.
        if strategy.force_rebuild() {
            if let Some(ref snap) = snapshot_repo {
                let _ = snap.clear_step_progress(project).await;
                let _ = snap.clear_llm_progress(project).await;
            }
        }

        // Step 5: Parse files and extract entities
        let extraction = self.extract_entities(project, root, &files_to_process)?;

        let methods_total = extraction.methods.len();
        let methods_new = methods_total;
        let classes_total = extraction.classes.len();

        // (Task 3) Document files are no longer chunk+embedded here — they
        // flow through the pipeline engine's extract chain. Only their
        // lifecycle (purge + snapshot baseline) is handled in this template
        // (Step 3b / Step 9b).

        // Step 6: Write graph (methods, classes, modules, relationships)
        // NOTE: Must be done BEFORE write_knowledge_annotations so that
        // IMPLEMENTED_BY relationships can match Method nodes in the graph.
        if let Some(graph) = graph {
            self.write_graph(graph, project, &extraction).await?;
        }

        // Step 6b: Write knowledge annotations (@knowledge comments)
        // Now that Method nodes exist in the graph, IMPLEMENTED_BY can match.
        let knowledge_count = extraction.knowledge_annotations.len();
        if let Some(graph) = graph {
            if knowledge_count > 0 {
                // Skip embedding in write_knowledge_annotations if processors.embed=false
                let embed_for_annotations = if self.skip_embed {
                    None
                } else {
                    embed.as_deref()
                };
                let vector_for_annotations = if self.skip_embed {
                    None
                } else {
                    vector.as_deref()
                };
                self.write_knowledge_annotations(
                    graph,
                    project,
                    &extraction.knowledge_annotations,
                    embed_for_annotations,
                    vector_for_annotations,
                )
                .await;
            }
        }

        // Step 7b: Embed methods and write to Qdrant (skip if processors.embed=false)
        if let (Some(embed_svc), Some(vector_repo)) = (&embed, &vector) {
            if self.skip_embed {
                tracing::info!(
                    "Skipping embed step (processors.embed=false) — preserving existing Qdrant vectors"
                );
            } else if !extraction.methods.is_empty() {
                let texts: Vec<String> = extraction
                    .methods
                    .iter()
                    .map(|m| format!("{} {}", m.signature, m.comment))
                    .collect();
                let embed_batch = self.batch_config.embed;
                let concurrent = self.batch_config.embed_concurrency;

                let chunk_pairs: Vec<(Vec<String>, Vec<crate::domain::types::MethodBlock>)> = texts
                    .chunks(embed_batch)
                    .zip(extraction.methods.chunks(embed_batch))
                    .map(|(t, m)| (t.to_vec(), m.to_vec()))
                    .collect();

                let embed_svc = embed_svc.clone();
                let embed_results: Vec<Result<Vec<serde_json::Value>, DtError>> =
                    stream::iter(chunk_pairs)
                        .map(|(text_chunk, method_chunk)| {
                            let svc = embed_svc.clone();
                            async move {
                                let embeddings = svc
                                    .embed_batch(&text_chunk)
                                    .await
                                    .map_err(|e| DtError::Repository(format!("embed: {}", e)))?;
                                let points: Vec<serde_json::Value> = method_chunk
                                    .iter()
                                    .zip(embeddings.iter())
                                    .map(|(m, vec)| {
                                        serde_json::json!({
                                            "id": m.method_id,
                                            "vector": vec,
                                            "payload": {
                                                // ---- identity ----
                                                "name": m.name,
                                                "signature": m.signature,
                                                "class_name": m.class_name,
                                                // ---- location ----
                                                "file_path": m.file_path,
                                                "package_or_module": m.package_or_module,
                                                // ---- tech stack ----
                                                "language": m.language,
                                                "project": m.project,
                                                // ---- code range ----
                                                "start_line": m.start_line,
                                                "end_line": m.end_line,
                                                // ---- signature ----
                                                "params": m.params,
                                                "return_type": m.return_type,
                                                "calls": m.calls,
                                                "comment": m.comment,
                                                // ---- metadata ----
                                                "entity_id": m.method_id,
                                            }
                                        })
                                    })
                                    .collect();
                                Ok(points)
                            }
                        })
                        .buffer_unordered(concurrent)
                        .collect()
                        .await;

                let mut all_points = Vec::new();
                for result in embed_results {
                    all_points.extend(result?);
                }

                vector_repo
                    .ensure_collection(
                        crate::shared::collections::CODE_METHODS,
                        crate::shared::collections::VECTOR_DIM,
                    )
                    .await?;
                // Upsert in batches to avoid Qdrant timeouts with large payloads
                let upsert_batch = self.batch_config.upsert;
                for chunk in all_points.chunks(upsert_batch) {
                    vector_repo
                        .upsert(crate::shared::collections::CODE_METHODS, chunk.to_vec())
                        .await?;
                }
                tracing::info!(
                    "upserted {} vectors to Qdrant ({} concurrent batches)",
                    extraction.methods.len(),
                    (extraction.methods.len() + embed_batch - 1) / embed_batch
                );
            }
        }

        // Step 8: Rebuild call graph
        if let Some(graph) = graph {
            tracing::info!(
                "rebuilding call graph for {} methods...",
                extraction.methods.len()
            );
            self.rebuild_call_graph(graph, project, &extraction.methods)
                .await?;
            tracing::info!("call graph rebuild complete");
        }

        // Step 9: Update SQLite snapshots
        if let Some(repo) = snapshot_repo.as_deref() {
            tracing::info!("updating {} snapshots...", extraction.snapshots.len());
            strategy
                .update_snapshots(repo, project, &extraction.snapshots)
                .await?;
            tracing::info!("snapshot update complete");
        }

        // Step 9b (Task 3): save document snapshots AFTER the strategy's
        // snapshot update (FullRebuildStrategy wipes all project rows first).
        // These baselines let the next incremental build detect document
        // changes and deletions (§6.5). Only changed docs need fresh rows —
        // unchanged docs keep accurate baselines from previous builds.
        if let Some(repo) = snapshot_repo.as_deref() {
            let doc_snapshots: Vec<FileSnapshot> = changed_docs
                .iter()
                .filter_map(|path| {
                    let (hash, mtime) = scanner::compute_file_hash(path).ok()?;
                    Some(FileSnapshot {
                        file_path: scanner::rel_path(root, path),
                        project: project.to_string(),
                        file_sha1: hash,
                        file_mtime: mtime,
                        method_count: 0,
                        updated_at: chrono::Utc::now().to_rfc3339(),
                    })
                })
                .collect();
            if !doc_snapshots.is_empty() {
                let _ = repo.save_snapshots(project, &doc_snapshots).await;
            }
        }

        // ── Phase 2: Per-method LLM analysis (background, non-blocking) ──
        // LLM analysis is submitted as a background tokio task so the build
        // returns immediately. The task processes methods concurrently and
        // updates Qdrant points with llm_analysis field.
        //
        // NOTE: embed and vector are required only when processors.store is true.
        // When store is false, we still run LLM analysis but skip the embed+upsert step.
        let phase2_client_available = self.siliconflow.is_some();
        let phase2_snapshot_available = snapshot_repo.is_some();

        if let (true, true) = (phase2_client_available, phase2_snapshot_available) {
            let client = self.siliconflow.as_ref().unwrap();
            let repo = snapshot_repo.as_ref().unwrap();
            let methods = &extraction.methods;
            if !methods.is_empty() {
                let system_prompt = load_code_analysis_prompt();
                let collection = crate::shared::collections::CODE_METHODS.to_string();

                // Build job list: skip methods already analyzed with same source hash
                let mut jobs: Vec<(crate::domain::types::MethodBlock, String)> = Vec::new();
                for m in methods {
                    let mut source_text = m.source_text.clone();
                    if source_text.len() < 10 {
                        let fp = std::path::Path::new(&m.file_path);
                        if let Ok(content) = std::fs::read_to_string(fp) {
                            source_text = content;
                        }
                    }
                    let mut hasher = Sha256::new();
                    hasher.update(source_text.as_bytes());
                    let hash = format!("{:x}", hasher.finalize());
                    let prog_key = format!("method:{}", m.method_id);
                    if repo
                        .is_llm_analyzed(project, &prog_key, &hash)
                        .await
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let mut m2 = m.clone();
                    m2.source_text = source_text;
                    jobs.push((m2, hash));
                }

                let total = jobs.len();
                let skipped = methods.len() - total;
                tracing::info!(
                    "Phase 2: {} to analyze, {} up-to-date (background, non-blocking)",
                    total,
                    skipped,
                );

                if total > 0 {
                    // Spawn background task — build returns immediately
                    // embed_svc and vector_repo may be None when processors.store is false
                    let client_cloned = client.clone();
                    let repo_cloned = repo.clone();
                    let proj = project.to_string();
                    let embed_svc_opt = embed.clone();
                    let vector_repo_opt = vector.clone();

                    tokio::spawn(async move {
                        tracing::info!("Phase 2 background worker started: {} methods", total);

                        let results: Vec<(String, bool)> = stream::iter(
                            jobs.into_iter().map(|(method, hash)| {
                                let cli = client_cloned.clone();
                                let repo_snap = repo_cloned.clone();
                                let embed_svc = embed_svc_opt.clone();
                                let vector_repo = vector_repo_opt.clone();
                                let sp = system_prompt.clone();
                                let coll = collection.clone();
                                let proj = proj.clone();
                                async move {
                                    let method_name = method.name.clone();
                                    let method_id = method.method_id.clone();

                                    match cli.chat(&sp, &method.source_text, 0.1, 100).await {
                                        Ok(llm_response) => {
                                            let _ = repo_snap
                                                .mark_llm_analyzed(&proj, &format!("method:{}", method_id), &hash)
                                                .await;

                                            // Only embed and upsert if store is enabled (embed_svc and vector_repo available)
                                            if let (Some(svc), Some(repo_vec)) = (embed_svc.as_ref(), vector_repo.as_ref()) {
                                                match svc.embed_batch(&[llm_response.clone()]).await {
                                                    Ok(embeddings) => {
                                                        if let Some(vec) = embeddings.first() {
                                                            let point = serde_json::json!({
                                                                "id": method_id,
                                                                "vector": vec,
                                                                "payload": {
                                                                    "name": method.name,
                                                                    "signature": method.signature,
                                                                    "class_name": method.class_name,
                                                                    "file_path": method.file_path,
                                                                    "package_or_module": method.package_or_module,
                                                                    "language": method.language,
                                                                    "project": method.project,
                                                                    "start_line": method.start_line,
                                                                    "end_line": method.end_line,
                                                                    "params": method.params,
                                                                    "return_type": method.return_type,
                                                                    "calls": method.calls,
                                                                    "comment": method.comment,
                                                                    "entity_id": method.method_id,
                                                                    "llm_analysis": llm_response,
                                                                }
                                                            });
                                                            if let Err(e) = repo_vec.upsert(&coll, vec![point]).await {
                                                                tracing::warn!("Phase 2 upsert fail {}: {}", method_name, e);
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("Phase 2 embed fail {}: {}", method_name, e);
                                                    }
                                                }
                                            } else {
                                                // store=false: skip embed+upsert, just log the LLM response
                                                tracing::debug!("Phase 2 LLM done (no store) {}: {}", method_name, llm_response.chars().take(50).collect::<String>());
                                            }

                                            tracing::info!("Phase 2 done {}", method_name);
                                            (method_name, true)
                                        }
                                        Err(e) => {
                                            tracing::warn!("Phase 2 failed {}: {}", method_name, e);
                                            (method_name, false)
                                        }
                                    }
                                }
                            }),
                        )
                        .buffer_unordered(PHASE2_CONCURRENCY)
                        .collect::<Vec<_>>()
                        .await;

                        let analyzed = results.iter().filter(|(_, ok)| *ok).count();
                        tracing::info!(
                            "Phase 2 background complete: {} analyzed, {} up-to-date, {} errors",
                            analyzed,
                            skipped,
                            total - analyzed,
                        );
                    });

                    tracing::info!(
                        "Phase 2: {} methods submitted for background LLM analysis",
                        total
                    );
                }
            }
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(BuildReport {
            project: project.to_string(),
            files_scanned,
            files_changed,
            methods_total,
            methods_new,
            classes_total,
            elapsed_ms,
        })
    }

    /// Extract entities (methods, classes, modules, knowledge annotations) from a batch of files.
    /// Uses multiple threads for parallel file I/O and parsing.
    fn extract_entities(
        &self,
        project: &str,
        root: &Path,
        files: &[std::path::PathBuf],
    ) -> Result<ExtractionResult, DtError> {
        let all_methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_classes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_annotations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let module_set = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let chunk_size = (files.len() + num_threads - 1) / num_threads;
        let registry = self.parser_registry.clone();

        std::thread::scope(|s| {
            for file_chunk in files.chunks(chunk_size.max(1)) {
                let registry = registry.clone();
                let project = project.to_string();
                let root = root.to_path_buf();
                let chunk = file_chunk.to_vec();
                let methods = all_methods.clone();
                let classes = all_classes.clone();
                let snapshots = all_snapshots.clone();
                let annotations = all_annotations.clone();
                let modules = module_set.clone();
                s.spawn(move || {
                    for file_path in &chunk {
                        // Compute hash FIRST (byte-level, works on all files) so
                        // we always save a snapshot — even for files that fail
                        // UTF-8 reading or parsing. Without this, unparseable
                        // files would be detected as "changed" on every run.
                        let rel_path = scanner::rel_path(&root, file_path);
                        let (file_hash, file_mtime) =
                            scanner::compute_file_hash(file_path).unwrap_or_default();

                        let source = match std::fs::read_to_string(file_path) {
                            Ok(s) => s,
                            Err(_) => {
                                snapshots.lock().unwrap().push(FileSnapshot {
                                    file_path: rel_path,
                                    project: project.clone(),
                                    file_sha1: file_hash,
                                    file_mtime,
                                    method_count: 0,
                                    updated_at: chrono::Utc::now().to_rfc3339(),
                                });
                                continue;
                            }
                        };
                        let result = match registry.parse_file(&source, file_path, &project) {
                            Ok(r) => r,
                            Err(_) => {
                                snapshots.lock().unwrap().push(FileSnapshot {
                                    file_path: rel_path,
                                    project: project.clone(),
                                    file_sha1: file_hash,
                                    file_mtime,
                                    method_count: 0,
                                    updated_at: chrono::Utc::now().to_rfc3339(),
                                });
                                continue;
                            }
                        };
                        let knowledge_anns =
                            crate::infrastructure::parser::extract_knowledge_annotations(
                                &source, &rel_path, &project,
                            );
                        annotations.lock().unwrap().extend(knowledge_anns);
                        let method_count = result.methods.len() as u32;
                        for m in &result.methods {
                            if !m.package_or_module.is_empty() {
                                modules.lock().unwrap().insert(m.package_or_module.clone());
                            }
                        }
                        for c in &result.classes {
                            if !c.package_or_module.is_empty() {
                                modules.lock().unwrap().insert(c.package_or_module.clone());
                            }
                        }
                        methods.lock().unwrap().extend(result.methods);
                        classes.lock().unwrap().extend(result.classes);
                        snapshots.lock().unwrap().push(FileSnapshot {
                            file_path: rel_path,
                            project: project.clone(),
                            file_sha1: file_hash,
                            file_mtime,
                            method_count,
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                });
            }
        });

        let all_methods = Arc::try_unwrap(all_methods).unwrap().into_inner().unwrap();
        let all_classes = Arc::try_unwrap(all_classes).unwrap().into_inner().unwrap();
        let all_snapshots = Arc::try_unwrap(all_snapshots)
            .unwrap()
            .into_inner()
            .unwrap();
        let all_annotations = Arc::try_unwrap(all_annotations)
            .unwrap()
            .into_inner()
            .unwrap();
        let module_set = Arc::try_unwrap(module_set).unwrap().into_inner().unwrap();

        let modules: Vec<crate::domain::types::ModuleBlock> = module_set
            .into_iter()
            .map(|name| crate::domain::types::ModuleBlock {
                module_id: crate::domain::id::make_module_id(project, &name),
                name,
                project: project.to_string(),
            })
            .collect();

        Ok(ExtractionResult {
            methods: all_methods,
            classes: all_classes,
            modules,
            snapshots: all_snapshots,
            knowledge_annotations: all_annotations,
        })
    }

    /// Write methods, classes, modules, and CONTAINS relationships to the graph database.
    async fn write_graph(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        extraction: &ExtractionResult,
    ) -> Result<(), DtError> {
        use std::collections::HashMap;

        let batch = self.batch_config.clone();
        let unwind = batch.unwind;

        // ---- Step 0: Ensure Project node exists ----
        {
            let lang = extraction
                .methods
                .first()
                .map(|m| m.language.as_str())
                .or_else(|| extraction.classes.first().map(|_| "unknown"))
                .unwrap_or("unknown");
            let project_type = infer_project_type(project);

            let mut params = HashMap::new();
            params.insert(
                "name".to_string(),
                serde_json::Value::String(project.to_string()),
            );
            params.insert(
                "language".to_string(),
                serde_json::Value::String(lang.to_string()),
            );
            params.insert(
                "project_type".to_string(),
                serde_json::Value::String(project_type.to_string()),
            );

            graph
                .write_query(
                    r#"MERGE (p:Project {name: $name})
                    SET p.language = $language,
                        p.project_type = $project_type"#,
                    params,
                )
                .await?;
        }

        // Write methods, classes, and modules in parallel
        {
            let methods = &extraction.methods;
            let classes = &extraction.classes;
            let modules = &extraction.modules;

            let write_methods = async {
                for chunk in methods.chunks(unwind) {
                    let methods_json: Vec<serde_json::Value> = chunk.iter().map(|m| serde_json::json!({
                        "method_id": m.method_id, "project": m.project, "file_path": m.file_path,
                        "language": m.language, "package_or_module": m.package_or_module,
                        "class_name": m.class_name, "name": m.name, "signature": m.signature,
                        "params": m.params, "return_type": m.return_type,
                        "start_line": m.start_line, "end_line": m.end_line,
                        "calls": m.calls, "comment": m.comment, "source_text": m.source_text,
                    })).collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "methods".to_string(),
                        serde_json::Value::Array(methods_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $methods AS m
                            MERGE (n:Method {method_id: m.method_id})
                            SET n.project = m.project, n.file_path = m.file_path,
                                n.language = m.language, n.package_or_module = m.package_or_module,
                                n.class_name = m.class_name, n.name = m.name,
                                n.signature = m.signature, n.params = m.params,
                                n.return_type = m.return_type, n.start_line = m.start_line,
                                n.end_line = m.end_line, n.calls = m.calls, n.comment = m.comment
                            WITH n, m
                            MERGE (p:Project {name: m.project})
                            MERGE (n)-[:BELONGS_TO]->(p)"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let write_classes = async {
                for chunk in classes.chunks(unwind) {
                    let classes_json: Vec<serde_json::Value> = chunk.iter().map(|c| serde_json::json!({
                        "class_id": c.class_id, "name": c.name, "kind": c.kind.as_str(),
                        "file_path": c.file_path, "package_or_module": c.package_or_module,
                        "project": c.project, "start_line": c.start_line, "end_line": c.end_line,
                    })).collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "classes".to_string(),
                        serde_json::Value::Array(classes_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $classes AS c
                            MERGE (n:Class {class_id: c.class_id})
                            SET n.name = c.name, n.kind = c.kind, n.file_path = c.file_path,
                                n.package_or_module = c.package_or_module, n.project = c.project,
                                n.start_line = c.start_line, n.end_line = c.end_line
                            WITH n, c
                            MERGE (p:Project {name: c.project})
                            MERGE (n)-[:BELONGS_TO]->(p)"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let write_modules = async {
                for chunk in modules.chunks(unwind) {
                    let modules_json: Vec<serde_json::Value> = chunk
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "module_id": m.module_id, "name": m.name, "project": m.project,
                            })
                        })
                        .collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "modules".to_string(),
                        serde_json::Value::Array(modules_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $modules AS m
                            MERGE (n:Module {module_id: m.module_id})
                            SET n.name = m.name, n.project = m.project"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let (r1, r2, r3) = tokio::join!(write_methods, write_classes, write_modules);
            r1?;
            r2?;
            r3?;
        }

        // Write CONTAINS relationships (depends on methods + classes being written)
        for c in &extraction.classes {
            for mid in &c.method_ids {
                let mut params = HashMap::new();
                params.insert(
                    "class_id".to_string(),
                    serde_json::Value::String(c.class_id.clone()),
                );
                params.insert(
                    "method_id".to_string(),
                    serde_json::Value::String(mid.clone()),
                );
                let _ = graph
                    .write_query(
                        "MATCH (c:Class {class_id: $class_id}) \
                         MATCH (m:Method {method_id: $method_id}) \
                         MERGE (c)-[:CONTAINS]->(m)",
                        params,
                    )
                    .await;
            }
        }

        Ok(())
    }

    /// Write @knowledge annotations as Concept and Knowledge nodes to the graph database.
    async fn write_knowledge_annotations(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        annotations: &[crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation],
        embed: Option<&dyn EmbedService>,
        vector: Option<&dyn VectorRepository>,
    ) {
        let now = chrono::Utc::now().to_rfc3339();

        for ann in annotations {
            // Write Domain node if domain is set
            if let Some(ref domain) = ann.domain {
                let domain_id = format!("dt://domain/{}", domain);
                let mut params = std::collections::HashMap::new();
                params.insert("domain_id".into(), serde_json::json!(domain_id));
                params.insert("name".into(), serde_json::json!(domain));
                params.insert(
                    "description".into(),
                    serde_json::json!(format!("{} domain knowledge", domain)),
                );
                let _ = graph
                    .write_query(
                        r#"MERGE (d:Domain {domain_id: $domain_id})
                        ON CREATE SET d.name = $name, d.description = $description
                        ON MATCH SET d.description = $description"#,
                        params,
                    )
                    .await;
            }

            // Write Concept node if concept + definition are present
            if let (Some(ref concept_name), Some(ref definition)) = (&ann.concept, &ann.definition)
            {
                let domain = ann.domain.as_deref().unwrap_or("通用");
                let concept_id = format!("dt://concept/{}/{}", domain, concept_name);
                let summary = if ann.description.is_empty() {
                    definition.clone()
                } else {
                    ann.description.clone()
                };

                let mut params = std::collections::HashMap::new();
                params.insert("concept_id".into(), serde_json::json!(concept_id));
                params.insert("name".into(), serde_json::json!(concept_name));
                params.insert("definition".into(), serde_json::json!(definition));
                params.insert("domain".into(), serde_json::json!(domain));
                params.insert("summary".into(), serde_json::json!(summary));

                let _ = graph
                    .write_query(
                        r#"MERGE (c:Concept {concept_id: $concept_id})
                        ON CREATE SET
                            c.name = $name,
                            c.definition = $definition,
                            c.domain = $domain,
                            c.summary = $summary
                        ON MATCH SET
                            c.definition = $definition,
                            c.summary = $summary"#,
                        params,
                    )
                    .await;

                // 即时嵌入 Concept 节点到 kg_nodes
                // Property map mirrors service.rs::auto_vectorize_concept so the
                // search text + payload fields stay consistent across the two
                // write paths. We reuse the `summary` computed above (defaults
                // to `definition` when `ann.description` is empty) for the
                // `description` slot — exactly what auto_vectorize_concept
                // writes (it uses `concept.summary`, which is the same value).
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let concept_props = serde_json::json!({
                        "name": concept_name,
                        "definition": definition,
                        "domain": domain,
                        "summary": summary,
                        "description": if summary.is_empty() || summary == "/" { definition.clone() } else { summary.clone() },
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Concept",
                        "concept_id",
                        &concept_id,
                        &concept_props,
                    )
                    .await
                    {
                        tracing::warn!("embed Concept {} failed: {}", concept_id, e);
                    }
                }

                // Link Concept to Domain
                if let Some(ref domain) = ann.domain {
                    let domain_id = format!("dt://domain/{}", domain);
                    let mut link_params = std::collections::HashMap::new();
                    link_params.insert("concept_id".into(), serde_json::json!(&concept_id));
                    link_params.insert("domain_id".into(), serde_json::json!(&domain_id));
                    let _ = graph
                        .write_query(
                            r#"MATCH (c:Concept {concept_id: $concept_id})
                            MATCH (d:Domain {domain_id: $domain_id})
                            MERGE (d)-[:CONTAINS]->(c)"#,
                            link_params,
                        )
                        .await;
                }

                // Link Concept to source file (Method, if exists)
                let mut file_params = std::collections::HashMap::new();
                file_params.insert("concept_id".into(), serde_json::json!(&concept_id));
                file_params.insert("file_path".into(), serde_json::json!(&ann.file_path));
                file_params.insert("project".into(), serde_json::json!(project));
                let _ = graph
                    .write_query(
                        r#"MATCH (c:Concept {concept_id: $concept_id})
                        MATCH (m:Method {project: $project})
                        WHERE m.file_path = $file_path OR m.file_path ENDS WITH $file_path
                        MERGE (c)-[:IMPLEMENTED_BY]->(m)"#,
                        file_params,
                    )
                    .await;
                // Also link Concept to source Document (if the concept came from a document/knowledge file)
                self.link_to_document(graph, project, &concept_id, &ann.file_path)
                    .await;
            }

            // Write Knowledge node if pitfall is present
            if let Some(ref pitfall) = ann.pitfall {
                let concept_key = ann.concept.as_deref().unwrap_or("unknown");
                let domain = ann.domain.as_deref().unwrap_or("通用");
                let knowledge_id = format!("dt://knowledge/{}/{}/{}", project, domain, concept_key);
                let name = format!("{}-pitfall", concept_key);
                let title = format!("{} 注意事项", concept_key);

                let mut params = std::collections::HashMap::new();
                params.insert("knowledge_id".into(), serde_json::json!(&knowledge_id));
                params.insert("name".into(), serde_json::json!(name));
                params.insert("title".into(), serde_json::json!(title));
                params.insert("domain".into(), serde_json::json!(domain));
                params.insert("summary".into(), serde_json::json!(pitfall));
                params.insert("content".into(), serde_json::json!(pitfall));
                params.insert(
                    "definition".into(),
                    serde_json::json!(ann.definition.as_deref().unwrap_or("")),
                );
                params.insert("source".into(), serde_json::json!("code_comment"));
                params.insert("project".into(), serde_json::json!(project));
                params.insert("confidence".into(), serde_json::json!(0.7));
                params.insert("verified_by".into(), serde_json::Value::Null);
                params.insert("created_at".into(), serde_json::json!(&now));
                params.insert("updated_at".into(), serde_json::json!(&now));
                params.insert("version".into(), serde_json::json!(1));

                let _ = graph
                    .write_query(
                        r#"MERGE (k:Knowledge {knowledge_id: $knowledge_id})
                        ON CREATE SET
                            k.name = $name,
                            k.title = $title,
                            k.domain = $domain,
                            k.summary = $summary,
                            k.content = $content,
                            k.definition = $definition,
                            k.source = $source,
                            k.project = $project,
                            k.confidence = $confidence,
                            k.verified_by = $verified_by,
                            k.created_at = $created_at,
                            k.updated_at = $updated_at,
                            k.version = $version
                        ON MATCH SET
                            k.summary = $summary,
                            k.content = $content,
                            k.updated_at = $updated_at"#,
                        params,
                    )
                    .await;

                // 即时嵌入 Knowledge 节点到 kg_nodes
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let knowledge_props = serde_json::json!({
                        "name": name,
                        "title": title,
                        "domain": domain,
                        "summary": pitfall,
                        "content": pitfall,
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Knowledge",
                        "knowledge_id",
                        &knowledge_id,
                        &knowledge_props,
                    )
                    .await
                    {
                        tracing::warn!("embed Knowledge {} failed: {}", knowledge_id, e);
                    }
                }

                // Link Knowledge pitfall to source Document
                self.link_to_document(graph, project, &knowledge_id, &ann.file_path)
                    .await;
            }

            // Write Experience node if experience field is present
            if let Some(ref exp_title) = ann.experience {
                let domain = ann.domain.as_deref().unwrap_or("通用");
                let experience_id = format!(
                    "dt://experience/{}/{}/{}",
                    project,
                    domain,
                    ann.concept.as_deref().unwrap_or("unknown")
                );
                let content = if ann.description.is_empty() {
                    exp_title.clone()
                } else {
                    format!("{}: {}", exp_title, ann.description)
                };

                let mut params = std::collections::HashMap::new();
                params.insert("experience_id".into(), serde_json::json!(&experience_id));
                params.insert("title".into(), serde_json::json!(exp_title));
                params.insert("summary".into(), serde_json::json!(exp_title));
                params.insert("content".into(), serde_json::json!(content));
                params.insert("domain".into(), serde_json::json!(domain));
                params.insert("severity".into(), serde_json::json!("warning"));
                params.insert("project".into(), serde_json::json!(project));
                params.insert("created_at".into(), serde_json::json!(&now));

                let _ = graph
                    .write_query(
                        r#"MERGE (e:Experience {experience_id: $experience_id})
                        ON CREATE SET
                            e.title = $title,
                            e.summary = $summary,
                            e.content = $content,
                            e.domain = $domain,
                            e.severity = $severity,
                            e.project = $project,
                            e.created_at = $created_at
                        ON MATCH SET
                            e.title = $title,
                            e.summary = $summary,
                            e.content = $content,
                            e.severity = $severity"#,
                        params,
                    )
                    .await;

                // 即时嵌入 Experience 节点到 kg_nodes
                // Property map mirrors service.rs::auto_vectorize_experience
                // (which uses `experience.title` as `name`). Here `exp_title`
                // is the closest equivalent.
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let experience_props = serde_json::json!({
                        "name": exp_title,
                        "title": exp_title,
                        "description": ann.description,
                        "domain": domain,
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Experience",
                        "experience_id",
                        &experience_id,
                        &experience_props,
                    )
                    .await
                    {
                        tracing::warn!("embed Experience {} failed: {}", experience_id, e);
                    }
                }

                // Link Experience to source Document
                self.link_to_document(graph, project, &experience_id, &ann.file_path)
                    .await;
            }
        }
    }

    /// Rebuild CALLS relationships for all methods in a project.
    async fn rebuild_call_graph(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        methods: &[crate::domain::types::MethodBlock],
    ) -> Result<(), DtError> {
        use std::collections::HashMap;

        let file_paths: Vec<String> = {
            let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
            for m in methods {
                set.insert(m.file_path.clone());
            }
            set.into_iter().collect()
        };

        let file_paths_json: Vec<serde_json::Value> = file_paths
            .iter()
            .map(|f| serde_json::Value::String(f.clone()))
            .collect();

        let mut params = HashMap::new();
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
        params.insert(
            "files".to_string(),
            serde_json::Value::Array(file_paths_json),
        );

        let _ = graph
            .write_query(
                r#"MATCH (caller:Method {project: $project})
                WHERE caller.file_path IN $files
                WITH caller
                UNWIND caller.calls AS called_name
                MATCH (callee:Method {project: $project, name: called_name})
                WHERE callee.method_id <> caller.method_id
                MERGE (caller)-[:CALLS]->(callee)"#,
                params,
            )
            .await;

        Ok(())
    }
}

/// Delete method nodes and relationships for a list of deleted files.
async fn delete_files_from_graph(graph: &dyn GraphRepository, project: &str, files: &[String]) {
    let files_json: Vec<serde_json::Value> = files
        .iter()
        .map(|f| serde_json::Value::String(f.clone()))
        .collect();

    let mut params = std::collections::HashMap::new();
    params.insert(
        "project".to_string(),
        serde_json::Value::String(project.to_string()),
    );
    params.insert("files".to_string(), serde_json::Value::Array(files_json));

    let _ = graph
        .write_query(
            "MATCH (m:Method {project: $project}) \
             WHERE m.file_path IN $files \
             DETACH DELETE m",
            params,
        )
        .await;
}

// ---------------------------------------------------------------------------
// Document lifecycle helpers
// ---------------------------------------------------------------------------

/// Whether a project-relative path is a document per `ScanConfig::document_extensions`.
///
/// Guards the strategy's `deleted` output against code-path contamination:
/// the snapshots table mixes code and document rows per project, so every
/// code path shows up as "deleted" when diffing only the document file set.
fn is_document_path(
    rel_path: &str,
    document_extensions: &std::collections::HashSet<String>,
) -> bool {
    Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| document_extensions.contains(e))
        .unwrap_or(false)
}

impl PipelineTemplate {
    /// Link a knowledge entity (Knowledge/Concept/Experience) to its source Document.
    /// Uses `file_path` to find the Document — safe no-op if no matching Document exists.
    async fn link_to_document(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        entity_id: &str,
        file_path: &str,
    ) {
        let mut params = std::collections::HashMap::new();
        params.insert("entity_id".into(), serde_json::json!(entity_id));
        params.insert("file_path".into(), serde_json::json!(file_path));
        params.insert("project".into(), serde_json::json!(project));
        let _ = graph
            .write_query(
                r#"MATCH (e)
                WHERE e.knowledge_id = $entity_id
                   OR e.experience_id = $entity_id
                   OR e.concept_id = $entity_id
                MATCH (d:Document {file_path: $file_path, project: $project})
                MERGE (e)-[:FROM_DOC]->(d)"#,
                params,
            )
            .await;
    }
}

/// Infer a human-readable project type label from the project name.
///
/// Uses simple heuristics based on common naming conventions
/// (e.g. `api-gateway` → `"微服务 — API 网关"`, `yimeng-website` → `"前端 — Web 应用"`).
fn infer_project_type(project: &str) -> &str {
    let lower = project.to_lowercase();
    if lower.contains("gateway") {
        return "微服务 — API 网关";
    }
    if lower.contains("website") || lower.contains("-h5") {
        return "前端 — Web 应用";
    }
    if lower.contains("hospital")
        || lower.contains("doctor")
        || lower.contains("nurse")
        || lower.contains("med")
    {
        return "微服务 — 医疗业务";
    }
    if lower.contains("pay")
        || lower.contains("charge")
        || lower.contains("cashier")
        || lower.contains("settlement")
        || lower.contains("order")
    {
        return "微服务 — 支付/交易";
    }
    if lower.contains("content") {
        return "微服务 — 内容中台";
    }
    if lower.contains("data") || lower.contains("report") || lower.contains("statistics") {
        return "微服务 — 数据/报表";
    }
    if lower.contains("log") {
        return "微服务 — 日志/监控";
    }
    if lower.contains("warehouse") || lower.contains("goods") || lower.contains("inventory") {
        return "微服务 — 仓储/物流";
    }
    if lower.contains("user") || lower.contains("auth") || lower.contains("oauth") {
        return "微服务 — 用户/认证";
    }
    if lower.contains("message") || lower.contains("sms") || lower.contains("im-") {
        return "微服务 — 消息/通知";
    }
    if lower.contains("admin") || lower.contains("boss") {
        return "前端 — 管理后台";
    }
    if lower.contains("saas") {
        return "微服务 — SaaS";
    }
    if lower.contains("search") {
        return "微服务 — 搜索";
    }
    if lower.contains("config") || lower.contains("cache") {
        return "微服务 — 基础设施";
    }
    if lower.contains("label") || lower.contains("comment") || lower.contains("app-") {
        return "微服务 — 业务支撑";
    }
    if lower.contains("-api") || lower.contains("api-") {
        return "微服务 — API 层";
    }
    if lower.contains("center") {
        return "微服务 — 业务中台";
    }
    "微服务"
}

/// Load the system prompt for Phase 2 code analysis from `config/prompts/code_analysis.yaml`.
/// Falls back to a hardcoded default if the file is missing.
fn load_code_analysis_prompt() -> String {
    let paths = [
        std::env::var("HOME").ok().map(|h| {
            std::path::PathBuf::from(h)
                .join(".config")
                .join("digital-twin")
                .join("prompts")
                .join("code_analysis.yaml")
        }),
        Some(std::path::PathBuf::from(
            "config/prompts/code_analysis.yaml",
        )),
    ];
    for path in paths.iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(path) {
            use serde::Deserialize;
            #[derive(Deserialize)]
            struct Prompt {
                system: Option<String>,
            }
            if let Ok(p) = serde_yaml::from_str::<Prompt>(&content) {
                if let Some(s) = p.system {
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    PHASE2_DEFAULT_PROMPT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_can_be_created() {
        let registry = Arc::new(ParserRegistry::new());
        let _pipeline = PipelineTemplate::new(registry, BatchConfig::default(), None);
    }

    #[test]
    fn is_document_path_matches_scan_config_extensions() {
        let exts = ScanConfig::default().document_extensions;
        assert!(is_document_path("docs/guide.md", &exts));
        assert!(is_document_path("config.yaml", &exts));
        assert!(is_document_path("a/b/c.properties", &exts));
        // Code paths reported as "deleted" by the mixed snapshots table are
        // filtered out — they never had Document nodes to purge.
        assert!(!is_document_path("src/main.rs", &exts));
        assert!(!is_document_path("Service.java", &exts));
        assert!(!is_document_path("script.py", &exts));
        assert!(!is_document_path("no_extension", &exts));
    }
}
