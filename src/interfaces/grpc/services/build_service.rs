//! DtCore 服务的 Build + Search gRPC 处理器。
//!
//! 这些处理器分别委托给 [`BuildServiceImpl`] 与向量/图谱仓库，
//! 并负责 gRPC 消息与领域类型之间的转换。

use crate::application::context::search_mcp::CrossWorldSearchTrait;
use crate::domain::traits::{
    BuildService, EmbedService, GraphRepository, RerankService, VectorRepository,
};
use crate::domain::types::BatchConfig;
use crate::proto::dt::core::*;
use std::sync::Arc;
use std::time::Instant;
use tonic::Status;

// ---------------------------------------------------------------------------
// handle_build
// ---------------------------------------------------------------------------

/// `Build` RPC 的处理器——将项目或文件索引到向量库。
pub async fn handle_build(
    req: BuildRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> Result<BuildResponse, Status> {
    let start = Instant::now();

    let project_name = if req.name.is_empty() {
        // 从路径派生项目名
        let path = std::path::Path::new(&req.path);
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        req.name.clone()
    };

    let project_path = std::path::PathBuf::from(&req.path);

    if !project_path.exists() {
        return Err(Status::not_found(format!("路径不存在: {}", req.path)));
    }

    // 构建应用层服务
    let parser_registry = Arc::new(crate::infrastructure::parser::ParserRegistry::new());
    let service = crate::application::build::service::BuildServiceImpl::new(
        parser_registry,
        graph,
        vector,
        None,  // snapshot——gRPC 构建不需要
        None,  // embed——使用 noop
        None,  // siliconflow——尚未通过 gRPC 接入
        false, // gRPC 构建默认增量
        BatchConfig::default(),
        false, // skip_embed
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
        Err(e) => Err(Status::internal(format!("构建失败: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// handle_search
// ---------------------------------------------------------------------------

/// `Search` RPC 的处理器——语义代码搜索。
///
/// 委托给 [`CrossWorldSearch`]，它通过向量库（Qdrant）搜索
/// 代码世界，并以知识图谱作为兜底。
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

    // 使用 provider 路由，从环境变量构建 embed 服务
    let embed_svc = crate::infrastructure::embedder::create_embed_router(
        crate::infrastructure::embedder::ProviderConfig {
            siliconflow_url: crate::infrastructure::siliconflow::base_url_from_env(),
            siliconflow_api_key: crate::infrastructure::siliconflow::api_key_from_env(),
            siliconflow_model_embed: crate::infrastructure::siliconflow::embed_model_from_env(),
            siliconflow_model_reranker: crate::infrastructure::siliconflow::reranker_model_from_env(
            ),
            siliconflow_model_llm: crate::infrastructure::siliconflow::llm_model_from_env(),
            xinference_url: String::new(),
            xinference_api_key: String::new(),
            xinference_model_embed: String::new(),
            xinference_model_reranker: String::new(),
            xinference_model_llm: String::new(),
            embed_provider: "siliconflow".into(),
            rerank_provider: "siliconflow".into(),
            llm_provider: "siliconflow".into(),
        },
    );
    let embed: Option<Arc<dyn EmbedService>> = Some(embed_svc);

    // Build CrossWorldSearch and delegate（rerank 经 provider 路由，S5 首个业务调用点）
    let rerank: Option<Arc<dyn RerankService>> =
        Some(crate::infrastructure::embedder::create_rerank_router(
            crate::infrastructure::embedder::ProviderConfig {
                siliconflow_url: crate::infrastructure::siliconflow::base_url_from_env(),
                siliconflow_api_key: crate::infrastructure::siliconflow::api_key_from_env(),
                siliconflow_model_embed: crate::infrastructure::siliconflow::embed_model_from_env(),
                siliconflow_model_reranker:
                    crate::infrastructure::siliconflow::reranker_model_from_env(),
                siliconflow_model_llm: crate::infrastructure::siliconflow::llm_model_from_env(),
                xinference_url: String::new(),
                xinference_api_key: String::new(),
                xinference_model_embed: String::new(),
                xinference_model_reranker: String::new(),
                xinference_model_llm: String::new(),
                embed_provider: "siliconflow".into(),
                rerank_provider: "siliconflow".into(),
                llm_provider: "siliconflow".into(),
            },
        ));
    let cws = crate::application::context::search_mcp::CrossWorldSearch::new(
        graph, vector, embed, rerank,
    );
    let cws_req = crate::application::context::search_mcp::SearchRequest {
        query: req.query,
        world: if req.world.is_empty() {
            None
        } else {
            Some(req.world)
        },
        limit: Some(limit),
        project: if req.project.is_empty() {
            None
        } else {
            Some(req.project)
        },
        max_hops: if req.max_hops == 0 {
            None
        } else {
            Some(req.max_hops)
        },
        with_evidence: Some(req.with_evidence),
        origin: if req.origin.is_empty() {
            None
        } else {
            Some(req.origin)
        },
        doc_id: if req.doc_id.is_empty() {
            None
        } else {
            Some(req.doc_id)
        },
        file_type: if req.file_type.is_empty() {
            None
        } else {
            Some(req.file_type)
        },
        entity_type_filter: if req.entity_type_filter.is_empty() {
            None
        } else {
            Some(req.entity_type_filter)
        },
    };

    let cws_result = cws
        .search(&cws_req)
        .await
        .map_err(|e| Status::internal(format!("搜索失败: {e}")))?;

    let results: Vec<SearchResult> = cws_result.hits.into_iter().map(hit_to_proto).collect();

    let total = results.len() as i32;
    let elapsed = start.elapsed().as_secs_f64();

    Ok(SearchResponse {
        results,
        total,
        elapsed_secs: elapsed,
    })
}

/// 将统一契约的命中结果映射为 proto 消息（全量字段，spec §7.3）。
fn hit_to_proto(hit: crate::application::context::search_mcp::SearchHit) -> SearchResult {
    SearchResult {
        score: hit.score as f32,
        name: hit.title,
        file_path: hit.file_path.unwrap_or_default(),
        start_line: hit.start_line.map(|l| l as i32).unwrap_or(0),
        signature: hit.signature.unwrap_or_default(),
        entity_type: hit.entity_type,
        snippet: hit.snippet,
        llm_analysis: hit.llm_analysis.unwrap_or_default(),
        end_line: hit.end_line.map(|l| l as i32).unwrap_or(0),
        hop: hit.hop.unwrap_or(0),
        rerank_degraded: hit.rerank_degraded.unwrap_or(false),
        evidence: hit.evidence.unwrap_or_default(),
        score_breakdown: hit.score_breakdown.map(|sb| ScoreBreakdown {
            semantic: sb.semantic,
            rerank: sb.rerank,
            graph_boost: sb.graph_boost,
            final_score: sb.final_score,
        }),
        relations: hit
            .relations
            .unwrap_or_default()
            .into_iter()
            .map(|r| RelationSnippet {
                rel_type: r.rel_type,
                other_end_id: r.other_end_id,
                other_end_name: r.other_end_name,
                direction: r.direction,
                confidence: r.confidence,
                evidence: r.evidence.unwrap_or_default(),
                supplementary_count: r.supplementary_count as i32,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;

    /// 返回空结果的最小化图谱仓库。
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
            world: String::new(),
            max_hops: 0,
            with_evidence: false,
            origin: String::new(),
            doc_id: String::new(),
            file_type: String::new(),
            entity_type_filter: String::new(),
        };
        let resp = handle_search(req, None, None).await.expect("应成功");
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
            world: String::new(),
            max_hops: 0,
            with_evidence: false,
            origin: String::new(),
            doc_id: String::new(),
            file_type: String::new(),
            entity_type_filter: String::new(),
        };
        let resp = handle_search(req, Some(graph), None).await.expect("应成功");
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

    #[test]
    fn hit_to_proto_maps_all_new_fields() {
        use crate::application::context::search_mcp::SearchHit;
        use crate::application::knowledge::extract::retrieve::{
            RelationSnippet as RsRelationSnippet, ScoreBreakdown as RsScoreBreakdown,
        };

        let hit = SearchHit {
            id: "1".into(),
            title: "ifCode".into(),
            snippet: "支付渠道编码".into(),
            content: None,
            source_world: "knowledge".into(),
            entity_type: "Entity".into(),
            file_type: None,
            file_type_label: None,
            score: 0.94,
            source_ref: Some("dt://doc/pay.md".into()),
            metadata: None,
            file_path: None,
            start_line: None,
            end_line: None,
            signature: None,
            calls: vec![],
            element_id: Some("4:0:1".into()),
            llm_analysis: None,
            score_breakdown: Some(RsScoreBreakdown {
                semantic: 0.71,
                rerank: 0.92,
                graph_boost: 1.0,
                final_score: 0.83,
            }),
            hop: Some(1),
            via_same_as: None,
            relations: Some(vec![RsRelationSnippet {
                rel_type: "relates".into(),
                other_end_id: "dt://entity/p/Config/waycode".into(),
                other_end_name: "wayCode".into(),
                direction: "out".into(),
                confidence: 0.9,
                evidence: None,
                supplementary_count: 0,
            }]),
            evidence: Some(vec!["证据段A".into()]),
            rerank_degraded: Some(false),
        };
        let p = hit_to_proto(hit);
        assert_eq!(p.entity_type, "Entity");
        assert_eq!(p.snippet, "支付渠道编码");
        assert_eq!(p.hop, 1);
        assert_eq!(p.evidence, vec!["证据段A".to_string()]);
        let sb = p.score_breakdown.expect("score_breakdown");
        assert!((sb.final_score - 0.83).abs() < 1e-6);
        assert_eq!(p.relations.len(), 1);
        assert_eq!(p.relations[0].other_end_name, "wayCode");
    }
}
