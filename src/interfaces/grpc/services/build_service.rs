//! Build + Search gRPC handlers for the DtCore service.
//!
//! These handlers delegate to [`BuildServiceImpl`] and the vector/graph
//! repository respectively, converting gRPC messages to/from domain types.

use crate::application::context::search_mcp::CrossWorldSearchTrait;
use crate::domain::traits::{
    BuildService, EmbedService, GraphRepository, VectorRepository,
};
use crate::domain::types::BatchConfig;
use crate::proto::dt::core::*;
use std::sync::Arc;
use std::time::Instant;
use tonic::Status;

// ---------------------------------------------------------------------------
// handle_build
// ---------------------------------------------------------------------------

/// Handler for `Build` RPC — index a project or file into the vector store.
pub async fn handle_build(
    req: BuildRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> Result<BuildResponse, Status> {
    let start = Instant::now();

    let project_name = if req.name.is_empty() {
        // Derive project name from path
        let path = std::path::Path::new(&req.path);
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        req.name.clone()
    };

    let project_path = std::path::PathBuf::from(&req.path);

    if !project_path.exists() {
        return Err(Status::not_found(format!(
            "path does not exist: {}",
            req.path
        )));
    }

    // Build the application-layer service
    let parser_registry = Arc::new(crate::infrastructure::parser::ParserRegistry::new());
    let service = crate::application::build::service::BuildServiceImpl::new(
        parser_registry,
        graph,
        vector,
        None, // snapshot — not required for gRPC build
        None, // embed — using noop
        None, // siliconflow — not wired through gRPC yet
        false, // gRPC builds default to incremental
        BatchConfig::default(),
    );

    match service.build(&project_name, &project_path).await {
        Ok(report) => {
            let elapsed = start.elapsed().as_secs_f64();
            Ok(BuildResponse {
                files_indexed: report.files_changed as i32,
                vectors_created: report.methods_new as i32,
                files_skipped: (report.files_scanned - report.files_changed) as i32,
                elapsed_secs: elapsed,
            })
        }
        Err(e) => Err(Status::internal(format!("Build failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// handle_search
// ---------------------------------------------------------------------------

/// Handler for `Search` RPC — semantic code search.
///
/// Delegates to [`CrossWorldSearch`] which searches the code world via
/// vector store (Qdrant) with a fallback to the knowledge graph.
pub async fn handle_search(
    req: SearchRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> Result<SearchResponse, Status> {
    let start = Instant::now();

    let limit: usize = if req.limit > 0 {
        req.limit as usize
    } else {
        10
    };

    // Build embed service from environment variables
    let embed_svc = crate::infrastructure::siliconflow::SiliconFlowClient::new(
        crate::infrastructure::siliconflow::base_url_from_env(),
        crate::infrastructure::siliconflow::api_key_from_env(),
        crate::infrastructure::siliconflow::embed_model_from_env(),
        crate::infrastructure::siliconflow::reranker_model_from_env(),
        crate::infrastructure::siliconflow::llm_model_from_env(),
    );
    let embed: Option<Arc<dyn EmbedService>> = Some(Arc::new(embed_svc));

    // Build CrossWorldSearch and delegate
    let cws =
        crate::application::context::search_mcp::CrossWorldSearch::new(graph, vector, embed);
    let cws_req = crate::application::context::search_mcp::SearchRequest {
        query: req.query,
        world: Some("code".into()),
        limit: Some(limit),
        project: if req.project.is_empty() {
            None
        } else {
            Some(req.project)
        },
    };

    let cws_result = cws
        .search(&cws_req)
        .await
        .map_err(|e| Status::internal(format!("Search failed: {e}")))?;

    // Convert CrossWorldSearch hits to gRPC SearchResults,
    // reading start_line from the hit payload (no longer hardcoded 0).
    let results: Vec<SearchResult> = cws_result
        .hits
        .into_iter()
        .map(|hit| SearchResult {
            score: hit.score as f32,
            name: hit.title,
            file_path: hit.file_path.unwrap_or_default(),
            start_line: hit.start_line.map(|l| l as i32).unwrap_or(0),
            signature: hit.signature.unwrap_or_default(),
        })
        .collect();

    let total = results.len() as i32;
    let elapsed = start.elapsed().as_secs_f64();

    Ok(SearchResponse {
        results,
        total,
        elapsed_secs: elapsed,
    })
}

/// Search using the Qdrant vector repository.
///
/// Embeds the query text, discovers all `*_methods` collections,
/// searches each one, merges results by score, and returns the top matches.
#[deprecated(note = "Use CrossWorldSearch instead")]
async fn search_via_vector(
    vec_repo: &dyn VectorRepository,
    query: &str,
    limit: u64,
) -> Result<Vec<SearchResult>, Status> {
    // 1. Connect to embed service
    let embed_svc = crate::infrastructure::siliconflow::SiliconFlowClient::new(
        crate::infrastructure::siliconflow::base_url_from_env(),
        crate::infrastructure::siliconflow::api_key_from_env(),
        crate::infrastructure::siliconflow::embed_model_from_env(),
        crate::infrastructure::siliconflow::reranker_model_from_env(),
        crate::infrastructure::siliconflow::llm_model_from_env(),
    );
    let embed: Arc<dyn EmbedService> = Arc::new(embed_svc);

    // 2. Generate query vector
    let vectors = embed.embed_batch(&[query.to_string()]).await
        .map_err(|e| Status::internal(format!("embed failed: {e}")))?;
    if vectors.is_empty() {
        return Ok(vec![]);
    }
    let query_vec = vectors[0].clone();

    // 3. Discover method collections
    let collections = vec_repo.list_collections().await
        .map_err(|e| Status::internal(format!("list collections: {e}")))?;
    let method_cols: Vec<&str> = collections.iter()
        .filter(|c| c.ends_with("_methods"))
        .map(|s| s.as_str())
        .collect();

    if method_cols.is_empty() {
        return Ok(vec![]);
    }

    // 4. Search across all method collections
    let mut all_results: Vec<(f64, SearchResult)> = Vec::new();
    for col in &method_cols {
        match vec_repo.search(col, query_vec.clone(), limit * 3).await {
            Ok(results) => {
                for r in results {
                    let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    if score < 0.3 { continue; }
                    let payload = r.get("payload").or(r.get("result")).unwrap_or(&r);
                    let name = payload.get("name").and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty() && *s != "?")
                        .unwrap_or("");
                    if name.is_empty() { continue; }
                    let file_path = payload.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let signature = payload.get("signature").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    all_results.push((score, SearchResult {
                        score: score as f32,
                        name: name.to_string(),
                        file_path,
                        start_line: 0,
                        signature,
                    }));
                }
            }
            Err(e) => {
                tracing::warn!("Qdrant search on {col}: {e}");
            }
        }
    }

    // 5. Sort by score descending, limit
    all_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    all_results.truncate(limit as usize);

    let results: Vec<SearchResult> = all_results.into_iter().map(|(_, r)| r).collect();
    Ok(results)
}

/// Fallback search using the graph database (fulltext index).
#[deprecated(note = "Use CrossWorldSearch instead")]
async fn search_via_graph(
    graph: &dyn GraphRepository,
    query: &str,
    limit: u64,
) -> Result<Vec<SearchResult>, Status> {
    let cypher = r#"
        CALL db.index.fulltext.queryNodes('infra_search', $q)
        YIELD node, score
        WHERE any(lbl IN labels(node) WHERE lbl IN ['Method', 'Class', 'Interface', 'Module'])
        RETURN coalesce(node.name, node.method_name, node.class_name, '') AS name,
               coalesce(node.source_file, '') AS file_path,
               coalesce(node.start_line, 0) AS start_line,
               coalesce(node.signature, '') AS signature,
               score
        ORDER BY score DESC
        LIMIT $limit
    "#;

    let mut params = std::collections::HashMap::new();
    params.insert("q".into(), serde_json::Value::String(query.to_string()));
    params.insert("limit".into(), serde_json::json!(limit as i64));

    match graph.read_query(&cypher, params).await {
        Ok(result) => {
            let rows = result.as_array().cloned().unwrap_or_default();
            let results: Vec<SearchResult> = rows
                .iter()
                .map(|row| SearchResult {
                    score: row
                        .get("score")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0) as f32,
                    name: row
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    file_path: row
                        .get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    start_line: row
                        .get("start_line")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32,
                    signature: row
                        .get("signature")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .collect();
            Ok(results)
        }
        Err(e) => {
            tracing::warn!("Graph search failed: {e}");
            Ok(vec![])
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;

    /// A minimal graph repo that returns empty results.
    struct MockGraphRepo;
    #[async_trait::async_trait]
    impl GraphRepository for MockGraphRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            Ok(serde_json::json!([]))
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn search_returns_empty_when_no_backend() {
        let req = SearchRequest {
            query: "test".into(),
            limit: 5,
            expand: false,
            path: String::new(),
            project: String::new(),
        };
        let resp = handle_search(req, None, None).await.expect("should succeed");
        assert_eq!(resp.total, 0);
        assert!(resp.results.is_empty());
    }

    #[tokio::test]
    async fn search_graph_fallback_works() {
        let graph: Arc<dyn GraphRepository> = Arc::new(MockGraphRepo);
        let req = SearchRequest {
            query: "something".into(),
            limit: 5,
            expand: false,
            path: String::new(),
            project: String::new(),
        };
        let resp = handle_search(req, Some(graph), None)
            .await
            .expect("should succeed");
        assert!(resp.results.is_empty());
    }

    #[test]
    fn build_request_defaults() {
        let req = BuildRequest {
            path: "/tmp/test".into(),
            name: String::new(),
            is_file: false,
        };
        assert_eq!(req.path, "/tmp/test");
        assert!(req.name.is_empty());
        assert!(!req.is_file);
    }
}
