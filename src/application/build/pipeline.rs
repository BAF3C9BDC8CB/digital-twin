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
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BuildReport, FileSnapshot, ScanConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::infrastructure::parser::ParserRegistry;
use crate::infrastructure::scanner;
use super::strategy::BuildStrategy;

/// Result of extracting entities from all changed files.
pub struct ExtractionResult {
    pub methods: Vec<crate::domain::types::MethodBlock>,
    pub classes: Vec<crate::domain::types::ClassBlock>,
    pub modules: Vec<crate::domain::types::ModuleBlock>,
    pub snapshots: Vec<FileSnapshot>,
    /// @knowledge annotations extracted from code comments.
    pub knowledge_annotations: Vec<crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation>,
}

/// A document item ready for storage.
#[derive(Debug, Clone)]
pub struct DocumentItem {
    pub doc_id: String,
    pub name: String,
    pub title: String,
    pub file_path: String,
    pub content: String,
    pub summary: String,
    pub project: String,
    pub doc_type: String,
    pub tags: Vec<String>,
    pub size: u64,
    pub modified: String,
}

/// The pipeline template that orchestrates the build flow.
pub struct PipelineTemplate {
    parser_registry: Arc<ParserRegistry>,
}

impl PipelineTemplate {
    /// Create a new pipeline template with the given parser registry.
    pub fn new(parser_registry: Arc<ParserRegistry>) -> Self {
        Self { parser_registry }
    }

    /// Execute the full build pipeline.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        project: &str,
        root: &Path,
        strategy: &dyn BuildStrategy,
        scan_config: &ScanConfig,
        snapshot_repo: Option<&dyn SnapshotRepository>,
        graph: Option<&dyn GraphRepository>,
        embed: Option<&dyn EmbedService>,
        vector: Option<&dyn VectorRepository>,
    ) -> Result<BuildReport, DtError> {
        let start = std::time::Instant::now();

        // Step 1: Scan files
        let all_files = scanner::collect_files(root, scan_config);
        let files_scanned = all_files.len();

        // Step 1b: Scan document files
        let doc_files = scanner::collect_document_files(root, scan_config);

        // Step 2: Select files via strategy
        let (files_to_process, deleted) = strategy
            .select_files(root, &all_files, snapshot_repo, project)
            .await?;
        let files_changed = files_to_process.len();

        // Step 3: Delete data for deleted files
        if let Some(graph) = graph {
            if !deleted.is_empty() {
                delete_files_from_graph(graph, project, &deleted).await;
            }
        }

        // Step 4: Prepare storage (strategy-specific)
        strategy.prepare(graph, None, project).await?;

        // Step 5: Parse files and extract entities
        let extraction = self.extract_entities(project, root, &files_to_process)?;

        let methods_total = extraction.methods.len();
        let methods_new = methods_total;
        let classes_total = extraction.classes.len();

        // Step 5b: Process document files
        if !doc_files.is_empty() {
            if let Some(graph) = graph {
                let _documents_written = self
                    .process_documents(project, root, &doc_files, Some(graph), embed, vector)
                    .await?;
            }
        }

        // Step 6: Write knowledge annotations (@knowledge comments)
        let knowledge_count = extraction.knowledge_annotations.len();
        if let Some(graph) = graph {
            if knowledge_count > 0 {
                self.write_knowledge_annotations(graph, project, &extraction.knowledge_annotations)
                    .await;
            }
        }

        // Step 7: Write graph (methods, classes, modules, relationships)
        if let Some(graph) = graph {
            self.write_graph(graph, project, &extraction).await?;
        }

        // Step 7b: Embed methods and write to Qdrant
        if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
            let texts: Vec<String> = extraction.methods.iter()
                .map(|m| format!("{} {}", m.signature, m.comment))
                .collect();
            if !texts.is_empty() {
                // Chunked embedding: batch of 64 to stay under gRPC 4MB limit
                const EMBED_BATCH: usize = 64;
                let mut all_points = Vec::new();
                for (chunk_start, (text_chunk, method_chunk)) in 
                    texts.chunks(EMBED_BATCH).zip(extraction.methods.chunks(EMBED_BATCH)).enumerate()
                {
                    let embeddings = embed_svc.embed_batch(text_chunk).await?;
                    tracing::info!("embedded batch {}: {} methods", chunk_start, embeddings.len());
                    for (m, vec) in method_chunk.iter().zip(embeddings.iter()) {
                        all_points.push(serde_json::json!({
                            "id": m.method_id,
                            "vector": vec,
                            "payload": {
                                "name": m.name,
                                "signature": m.signature,
                                "class_name": m.class_name,
                                "file_path": m.file_path,
                                "package_or_module": m.package_or_module,
                                "language": m.language,
                                "project": m.project,
                                "start_line": m.start_line,
                                "end_line": m.end_line,
                                "calls": m.calls,
                                "comment": m.comment,
                                "params": m.params,
                                "return_type": m.return_type,
                                "entity_id": m.method_id,
                            }
                        }));
                    }
                }
                // Ensure collection exists before upsert
                vector_repo.ensure_collection(&format!("{}_methods", project), 1024).await?;
                vector_repo.upsert(&format!("{}_methods", project), all_points).await?;
                tracing::info!("upserted {} vectors to Qdrant", extraction.methods.len());
            }
        }

        // Step 8: Rebuild call graph
        if let Some(graph) = graph {
            self.rebuild_call_graph(graph, project, &extraction.methods).await?;
        }

        // Step 9: Update SQLite snapshots
        if let Some(repo) = snapshot_repo {
            strategy.update_snapshots(repo, project, &extraction.snapshots).await?;
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
    fn extract_entities(
        &self,
        project: &str,
        root: &Path,
        files: &[std::path::PathBuf],
    ) -> Result<ExtractionResult, DtError> {
        let mut all_methods = Vec::new();
        let mut all_classes = Vec::new();
        let mut all_snapshots = Vec::new();
        let mut all_annotations = Vec::new();
        let mut module_set: std::collections::HashSet<String> = std::collections::HashSet::new();

        for file_path in files {
            let source = match std::fs::read_to_string(file_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let rel_path = scanner::rel_path(root, file_path);
            let (file_hash, file_mtime) = scanner::compute_file_hash(file_path).unwrap_or_default();

            // Parse
            let result = match self.parser_registry.parse_file(&source, file_path, project) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Extract @knowledge annotations from code comments
            let knowledge_anns = crate::infrastructure::parser::extract_knowledge_annotations(
                &source,
                &rel_path,
                project,
            );
            all_annotations.extend(knowledge_anns);

            let method_count = result.methods.len() as u32;

            // Collect modules from package paths
            for m in &result.methods {
                if !m.package_or_module.is_empty() {
                    module_set.insert(m.package_or_module.clone());
                }
            }
            for c in &result.classes {
                if !c.package_or_module.is_empty() {
                    module_set.insert(c.package_or_module.clone());
                }
            }

            all_methods.extend(result.methods);
            all_classes.extend(result.classes);

            let updated_at = chrono::Utc::now().to_rfc3339();
            all_snapshots.push(FileSnapshot {
                file_path: rel_path,
                project: project.to_string(),
                file_sha1: file_hash,
                file_mtime,
                method_count,
                updated_at,
            });
        }

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

    /// Write methods, classes, modules, and CONTAINS relationships to Neo4j.
    async fn write_graph(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        extraction: &ExtractionResult,
    ) -> Result<(), DtError> {
        use std::collections::HashMap;

        // ---- Step 0: Ensure Project node exists ----
        {
            let lang = extraction.methods.first()
                .map(|m| m.language.as_str())
                .or_else(|| extraction.classes.first().map(|_| "unknown"))
                .unwrap_or("unknown");
            let project_type = infer_project_type(project);

            let mut params = HashMap::new();
            params.insert("name".to_string(), serde_json::Value::String(project.to_string()));
            params.insert("language".to_string(), serde_json::Value::String(lang.to_string()));
            params.insert("project_type".to_string(), serde_json::Value::String(project_type.to_string()));

            graph
                .write_query(
                    r#"MERGE (p:Project {name: $name})
                    SET p.language = $language,
                        p.project_type = $project_type"#,
                    params,
                )
                .await?;
        }

        // Write methods in batches
        for chunk in extraction.methods.chunks(200) {
            let methods_json: Vec<serde_json::Value> = chunk
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "method_id": m.method_id,
                        "project": m.project,
                        "file_path": m.file_path,
                        "language": m.language,
                        "package_or_module": m.package_or_module,
                        "class_name": m.class_name,
                        "name": m.name,
                        "signature": m.signature,
                        "params": m.params,
                        "return_type": m.return_type,
                        "start_line": m.start_line,
                        "end_line": m.end_line,
                        "calls": m.calls,
                        "comment": m.comment,
                        "source_text": m.source_text,
                    })
                })
                .collect();

            let mut params = HashMap::new();
            params.insert("methods".to_string(), serde_json::Value::Array(methods_json));

            graph
                .write_query(
                    r#"UNWIND $methods AS m
                    MERGE (n:Method {method_id: m.method_id})
                    SET n.project = m.project,
                        n.file_path = m.file_path,
                        n.language = m.language,
                        n.package_or_module = m.package_or_module,
                        n.class_name = m.class_name,
                        n.name = m.name,
                        n.signature = m.signature,
                        n.params = m.params,
                        n.return_type = m.return_type,
                        n.start_line = m.start_line,
                        n.end_line = m.end_line,
                        n.calls = m.calls,
                        n.comment = m.comment
                    WITH n, m
                    MERGE (p:Project {name: m.project})
                    MERGE (n)-[:BELONGS_TO]->(p)"#,
                    params,
                )
                .await?;
        }

        // Write classes in batches
        for chunk in extraction.classes.chunks(100) {
            let classes_json: Vec<serde_json::Value> = chunk
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "class_id": c.class_id,
                        "name": c.name,
                        "kind": c.kind.as_str(),
                        "file_path": c.file_path,
                        "package_or_module": c.package_or_module,
                        "project": c.project,
                        "start_line": c.start_line,
                        "end_line": c.end_line,
                    })
                })
                .collect();

            let mut params = HashMap::new();
            params.insert(
                "classes".to_string(),
                serde_json::Value::Array(classes_json),
            );

            graph
                .write_query(
                    r#"UNWIND $classes AS c
                    MERGE (n:Class {class_id: c.class_id})
                    SET n.name = c.name,
                        n.kind = c.kind,
                        n.file_path = c.file_path,
                        n.package_or_module = c.package_or_module,
                        n.project = c.project,
                        n.start_line = c.start_line,
                        n.end_line = c.end_line
                    WITH n, c
                    MERGE (p:Project {name: c.project})
                    MERGE (n)-[:BELONGS_TO]->(p)"#,
                    params,
                )
                .await?;
        }

        // Write CONTAINS relationships
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

        // Write modules
        for chunk in extraction.modules.chunks(100) {
            let modules_json: Vec<serde_json::Value> = chunk
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "module_id": m.module_id,
                        "name": m.name,
                        "project": m.project,
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

        let _ = &project;
        Ok(())
    }

    /// Write @knowledge annotations as Concept and Knowledge nodes to Neo4j.
    async fn write_knowledge_annotations(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        annotations: &[crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation],
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
            if let (Some(ref concept_name), Some(ref definition)) =
                (&ann.concept, &ann.definition)
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

                // Link Concept to source file
                let mut file_params = std::collections::HashMap::new();
                file_params.insert("concept_id".into(), serde_json::json!(&concept_id));
                file_params.insert(
                    "file_path".into(),
                    serde_json::json!(&ann.file_path),
                );
                file_params.insert("project".into(), serde_json::json!(project));
                let _ = graph
                    .write_query(
                        r#"MATCH (c:Concept {concept_id: $concept_id})
                        MATCH (m:Method {file_path: $file_path, project: $project})
                        MERGE (c)-[:IMPLEMENTED_BY]->(m)"#,
                        file_params,
                    )
                    .await;
            }

            // Write Knowledge node if pitfall is present
            if let Some(ref pitfall) = ann.pitfall {
                let concept_key = ann.concept.as_deref().unwrap_or("unknown");
                let domain = ann.domain.as_deref().unwrap_or("通用");
                let knowledge_id = format!(
                    "dt://knowledge/{}/{}/{}",
                    project, domain, concept_key
                );
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
async fn delete_files_from_graph(
    graph: &dyn GraphRepository,
    project: &str,
    files: &[String],
) {
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
// Document processing
// ---------------------------------------------------------------------------

impl PipelineTemplate {
    /// Process document files: parse, chunk, embed, and write to Neo4j.
    ///
    /// Returns the number of documents successfully written.
    async fn process_documents(
        &self,
        project: &str,
        root: &Path,
        doc_files: &[PathBuf],
        graph: Option<&dyn GraphRepository>,
        embed: Option<&dyn EmbedService>,
        vector: Option<&dyn VectorRepository>,
    ) -> Result<usize, DtError> {
        let mut written = 0usize;
        let config = crate::shared::chunker::ChunkConfig::default();

        for file_path in doc_files {
            // Parse the document
            let parsed = match crate::infrastructure::parser::document::parse_document(file_path, project, root) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to parse document {}: {}", file_path.display(), e);
                    continue;
                }
            };

            // Skip empty content for text/markdown
            if parsed.content.is_empty() && parsed.doc_type != "pdf" {
                continue;
            }

            let doc_id = parsed.doc_id.clone();

            // Chunk the text
            let chunks = if parsed.content.is_empty() {
                // PDF stub: just the metadata, no content chunks
                vec![]
            } else {
                crate::shared::chunker::chunk_text(&parsed.content, &doc_id, &config)
            };

            // Embed chunks if embed service is available
            if let Some(embed_svc) = embed {
                if !chunks.is_empty() {
                    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
                    let embeddings = embed_svc.embed_batch(&texts).await?;
                    if let Some(vector_repo) = vector {
                        vector_repo.ensure_collection(&format!("{project}_semantic"), 1024).await?;
                        let points: Vec<serde_json::Value> = chunks.iter()
                            .zip(embeddings.iter())
                            .map(|(chunk, vec)| serde_json::json!({
                                "id": chunk.chunk_id,
                                "vector": vec,
                                "entity_id": chunk.chunk_id,
                                "text": &chunk.text,
                            }))
                            .collect();
                        vector_repo.upsert(&format!("{project}_semantic"), points).await?;
                    }
                }
            }

            // Build DocumentItem
            let doc_item = DocumentItem {
                doc_id: parsed.doc_id,
                name: parsed.name,
                title: parsed.title,
                file_path: parsed.rel_path,
                content: parsed.content,
                summary: parsed.summary,
                project: parsed.project,
                doc_type: parsed.doc_type,
                tags: vec![],
                size: parsed.size,
                modified: parsed.modified,
            };

            // Write to Neo4j
            if let Some(graph) = graph {
                self.write_document_to_graph(graph, &doc_item).await;
                for chunk in &chunks {
                    self.write_chunk_to_graph(graph, &doc_id, chunk).await;
                }
            }

            written += 1;
        }

        Ok(written)
    }

    /// Write a single Document node to Neo4j.
    async fn write_document_to_graph(
        &self,
        graph: &dyn GraphRepository,
        doc: &DocumentItem,
    ) {
        let mut params = std::collections::HashMap::new();
        params.insert("doc_id".into(), serde_json::json!(&doc.doc_id));
        params.insert("name".into(), serde_json::json!(&doc.name));
        params.insert("title".into(), serde_json::json!(&doc.title));
        params.insert("file_path".into(), serde_json::json!(&doc.file_path));
        params.insert("summary".into(), serde_json::json!(&doc.summary));
        params.insert("project".into(), serde_json::json!(&doc.project));
        params.insert("doc_type".into(), serde_json::json!(&doc.doc_type));
        params.insert("tags".into(), serde_json::json!(&doc.tags));
        params.insert("size".into(), serde_json::json!(doc.size));
        params.insert("modified".into(), serde_json::json!(&doc.modified));

        let _ = graph
            .write_query(
                r#"MERGE (d:Document {doc_id: $doc_id})
                ON CREATE SET
                    d.name = $name,
                    d.title = $title,
                    d.file_path = $file_path,
                    d.summary = $summary,
                    d.project = $project,
                    d.doc_type = $doc_type,
                    d.tags = $tags,
                    d.size = $size,
                    d.modified = $modified
                ON MATCH SET
                    d.name = $name,
                    d.title = $title,
                    d.summary = $summary,
                    d.doc_type = $doc_type,
                    d.tags = $tags,
                    d.size = $size,
                    d.modified = $modified"#,
                params,
            )
            .await;
    }

    /// Write a single chunk node to Neo4j, linked to its parent Document.
    async fn write_chunk_to_graph(
        &self,
        graph: &dyn GraphRepository,
        doc_id: &str,
        chunk: &crate::shared::chunker::DocumentChunk,
    ) {
        let mut params = std::collections::HashMap::new();
        params.insert("chunk_id".into(), serde_json::json!(&chunk.chunk_id));
        params.insert("doc_id".into(), serde_json::json!(doc_id));
        params.insert("chunk_index".into(), serde_json::json!(chunk.chunk_index));
        params.insert("text".into(), serde_json::json!(&chunk.text));
        params.insert("prev_chunk_id".into(), serde_json::json!(&chunk.prev_chunk_id));
        params.insert("next_chunk_id".into(), serde_json::json!(&chunk.next_chunk_id));
        params.insert("start_char".into(), serde_json::json!(chunk.start_char));
        params.insert("end_char".into(), serde_json::json!(chunk.end_char));

        let _ = graph
            .write_query(
                r#"MERGE (c:DocumentChunk {chunk_id: $chunk_id})
                ON CREATE SET
                    c.chunk_index = $chunk_index,
                    c.text = $text,
                    c.prev_chunk_id = $prev_chunk_id,
                    c.next_chunk_id = $next_chunk_id,
                    c.start_char = $start_char,
                    c.end_char = $end_char
                ON MATCH SET
                    c.text = $text,
                    c.prev_chunk_id = $prev_chunk_id,
                    c.next_chunk_id = $next_chunk_id,
                    c.start_char = $start_char,
                    c.end_char = $end_char
                WITH c
                MATCH (d:Document {doc_id: $doc_id})
                MERGE (d)-[:CONTAINS]->(c)"#,
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
    if lower.contains("gateway") { return "微服务 — API 网关"; }
    if lower.contains("website") || lower.contains("-h5") { return "前端 — Web 应用"; }
    if lower.contains("hospital") || lower.contains("doctor") || lower.contains("nurse") || lower.contains("med") { return "微服务 — 医疗业务"; }
    if lower.contains("pay") || lower.contains("charge") || lower.contains("cashier") || lower.contains("settlement") || lower.contains("order") { return "微服务 — 支付/交易"; }
    if lower.contains("content") { return "微服务 — 内容中台"; }
    if lower.contains("data") || lower.contains("report") || lower.contains("statistics") { return "微服务 — 数据/报表"; }
    if lower.contains("log") { return "微服务 — 日志/监控"; }
    if lower.contains("warehouse") || lower.contains("goods") || lower.contains("inventory") { return "微服务 — 仓储/物流"; }
    if lower.contains("user") || lower.contains("auth") || lower.contains("oauth") { return "微服务 — 用户/认证"; }
    if lower.contains("message") || lower.contains("sms") || lower.contains("im-") { return "微服务 — 消息/通知"; }
    if lower.contains("admin") || lower.contains("boss") { return "前端 — 管理后台"; }
    if lower.contains("saas") { return "微服务 — SaaS"; }
    if lower.contains("search") { return "微服务 — 搜索"; }
    if lower.contains("config") || lower.contains("cache") { return "微服务 — 基础设施"; }
    if lower.contains("label") || lower.contains("comment") || lower.contains("app-") { return "微服务 — 业务支撑"; }
    if lower.contains("-api") || lower.contains("api-") { return "微服务 — API 层"; }
    if lower.contains("center") { return "微服务 — 业务中台"; }
    "微服务"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_can_be_created() {
        let registry = Arc::new(ParserRegistry::new());
        let _pipeline = PipelineTemplate::new(registry);
    }
}
