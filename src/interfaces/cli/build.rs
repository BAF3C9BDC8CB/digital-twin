//! CLI handlers for `dt build` and `dt search` commands.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};

/// Handle `dt build` — index a project into the knowledge graph.
///
/// All backend connections (Neo4j, Qdrant, embed, SQLite) must be established
/// by the caller and passed as `Option<Arc<...>>`.
pub async fn handle_build(
    path: PathBuf,
    name: Option<String>,
    file: Option<PathBuf>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
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
        full: false,
        verbose: true,
    };

    let deps = crate::application::build::builder::BuildDependencies {
        graph,
        vector,
        snapshot,
        embed,
    };

    cmd.run(deps).await?;

    Ok(())
}

/// Handle `dt search` — semantic code search across worlds.
///
/// For the "code" world, uses Qdrant vector search across `*_methods`
/// collections when Qdrant + embed are available. When `project` is set,
/// only searches `{project}_methods`. Falls back to Neo4j CONTAINS text
/// search if vector search is unavailable.
/// For "knowledge" / "memory" / etc., uses Neo4j Cypher text search.
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

    // For "code" world, try vector search first (if Qdrant + embed available).
    if world == "code" {
        if let Some(vec_repo) = &vector {
            // Try to connect embed service
            let embed: Option<Arc<dyn EmbedService>> =
                match crate::infrastructure::embedder::GrpcEmbedService::connect("http://[::1]:50052").await {
                    Ok(svc) => {
                        tracing::info!("dt-embed connected for vector code search");
                        Some(Arc::new(svc) as Arc<dyn EmbedService>)
                    }
                    Err(e) => {
                        tracing::warn!("dt-embed unavailable for vector search: {e}");
                        None
                    }
                };

            if let Some(embed_svc) = embed {
                // Generate query embedding
                let vectors = embed_svc.embed_batch(&[query.clone()]).await
                    .map_err(|e| anyhow::anyhow!("embed failed: {e}"))?;

                if !vectors.is_empty() {
                    let query_vec = vectors[0].clone();

                    // Determine which collections to search
                    let method_collections: Vec<String> = if let Some(ref proj) = project {
                        // Project-scoped: only search {project}_methods
                        let col = format!("{}_methods", proj);
                        tracing::info!("project-scoped search: collection={col}");
                        vec![col]
                    } else {
                        // Global: search all *_methods collections
                        let collections = match vec_repo.list_collections().await {
                            Ok(cols) => cols,
                            Err(e) => {
                                tracing::warn!("Failed to list Qdrant collections: {e}");
                                vec![]
                            }
                        };
                        collections
                            .into_iter()
                            .filter(|c| c.ends_with("_methods"))
                            .collect()
                    };

                    if !method_collections.is_empty() {
                        // Search across collections, merge results
                        let mut all_results: Vec<(f64, serde_json::Value)> = Vec::new();

                        for col in &method_collections {
                            match vec_repo.search(col, query_vec.clone(), (limit * 3) as u64).await {
                                Ok(results) => {
                                    for r in results {
                                        let score = r.get("score")
                                            .and_then(|v| v.as_f64())
                                            .unwrap_or(0.0);
                                        if score > 0.3 {
                                            // filter low-relevance noise
                                            all_results.push((score, r));
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Qdrant search on {col}: {e}");
                                }
                            }
                        }

                        // Sort by score descending, take top N
                        all_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                        all_results.truncate(limit);

                        if !all_results.is_empty() {
                            for (score, result) in &all_results {
                                let payload = result.get("payload")
                                    .or(result.get("result"))
                                    .unwrap_or(&result);
                                let name = payload.get("name")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty() && *s != "?")
                                    .unwrap_or("");
                                if name.is_empty() { continue; }
                                let file = payload.get("file_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let proj = payload.get("project")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                println!("  [{:.2}] [{}] {} • {}", score, proj, name, file);
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }

        // Fall through to Neo4j search below if vector search unavailable
        println!("  (vector search unavailable — falling back to Neo4j text search)");
    }

    // Neo4j path — text search (also serves as fallback for code world).
    match graph {
        Some(graph) => {
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
            match graph.read_query(&cypher, params).await {
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
                tracing::warn!("Neither Neo4j nor Qdrant available — no search results");
                println!("  (No search backend available)");
            }
        }
    }

    Ok(())
}

/// Handle `dt search-kg` — semantic search of KG nodes via Qdrant.
///
/// Queries the `kg_nodes` Qdrant collection (populated by `dt kg-sync`)
/// and prints matching nodes with their labels and properties.
pub async fn handle_search_kg(
    query: String,
    limit: usize,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: search-kg \"{query}\" --limit {limit}");

    println!("Search-KG: query=\"{query}\" limit={limit}");

    let vector = match vector {
        Some(v) => v,
        None => {
            tracing::warn!("Qdrant unavailable — no KG search results");
            println!("  (Qdrant unavailable — no results)");
            return Ok(());
        }
    };

    // Use the embed service to get a query vector, then search Qdrant.
    let embed: Option<Arc<dyn EmbedService>> = match crate::infrastructure::embedder::GrpcEmbedService::connect("http://[::1]:50052").await {
        Ok(svc) => {
            tracing::info!("dt-embed connected for search-kg");
            Some(Arc::new(svc) as Arc<dyn EmbedService>)
        }
        Err(e) => {
            tracing::warn!("dt-embed unavailable for search-kg: {e}");
            None
        }
    };

    let embed = match embed {
        Some(e) => e,
        None => {
            println!("  (dt-embed unavailable — cannot generate query vector)");
            return Ok(());
        }
    };

    // Generate embedding for the query
    let vectors = embed.embed_batch(&[query.clone()]).await
        .map_err(|e| anyhow::anyhow!("embed failed: {e}"))?;

    if vectors.is_empty() {
        println!("  (empty embedding result)");
        return Ok(());
    }

    let query_vec = vectors[0].clone();

    // Search in the kg_nodes collection
    match vector.search("kg_nodes", query_vec, limit as u64).await {
        Ok(results) => {
            if results.is_empty() {
                println!("  (no results)");
            } else {
                for result in results.iter() {
                    let score = result.get("score")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let payload = result.get("payload")
                        .or(result.get("result"))
                        .unwrap_or(result);
                    let name = payload.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let label = payload.get("label")
                        .or(payload.get("labels"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let desc = payload.get("description")
                        .or(payload.get("summary"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("  [{:.2}] {} → {} {}", score, label, name, desc);
                }
            }
        }
        Err(e) => {
            eprintln!("  Search-KG error: {e}");
        }
    }

    Ok(())
}
