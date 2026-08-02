//! Retrieve 检索层 — GraphRAG 式混合检索（spec S5 / 主文档 §8）。
//!
//! 管线：召回(kg_nodes) → 图扩展(SAME_AS/RELATES/elementId) → 分桶截断
//! → rerank(sigmoid) → 融合排序。任一路故障降级，不整体失败（spec §3）。

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};
use crate::shared::collections::KG_NODES;

// ---------------------------------------------------------------------------
// 输出契约子结构（spec §5.7.3）
// ---------------------------------------------------------------------------

/// 排序分解——权重调参（主文档 §8 收敛回写）的观测数据源。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreBreakdown {
    /// 向量召回分；邻居 = 种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    /// sigmoid 归一后；rerank_degraded 时为 0.0。
    pub rerank: f64,
    /// 正常 1.0/0.5/0.25（0.5^hop）；降级模式一阶衰减 1.0/0.5（§5.4）。
    pub graph_boost: f64,
    /// = SearchHit.score，冗余字段便于消费方免对齐。
    pub final_score: f64,
}

/// 关系摘要——命中实体对外的 top-5 关系（按 confidence 降序）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationSnippet {
    /// RELATES.type，如 routes_to / depends_on。
    pub rel_type: String,
    /// 对端 business_id。
    pub other_end_id: String,
    /// 对端 name（展示用；不在候选集中时为空串）。
    pub other_end_name: String,
    /// "out" | "in"（相对命中实体）。
    pub direction: String,
    pub confidence: f64,
    /// 最高 confidence 边的证据句。
    pub evidence: Option<String>,
    /// 其余文档来源的补充证据数（边聚合产物；>0 表示多文档佐证）。
    pub supplementary_count: u32,
}

// ---------------------------------------------------------------------------
// 配置（env；spec §8 配置项汇总表）
// ---------------------------------------------------------------------------

/// 语义分阈值（复用 code 世界同款 env，S5 起 knowledge/doc 世界生效）。
pub fn min_score() -> f64 {
    std::env::var("DT_SEARCH_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.3)
}

/// rerank 候选总上限（分桶规则见 §5.2.3）。
pub fn rerank_top_n() -> usize {
    std::env::var("DT_KG_RERANK_TOP_N")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
}

/// 图扩展跳数白名单 {1,2}，其他值钳到 2（S5-D3；严禁直接拼请求整数进 Cypher）。
pub fn clamp_max_hops(h: u32) -> u32 {
    h.clamp(1, 2)
}

/// bge-reranker-v2-m3 返回 logit，sigmoid 归一到 [0,1]（S5-D6）。
pub fn sigmoid(x: f32) -> f64 {
    1.0 / (1.0 + (-(x as f64)).exp())
}

// ---------------------------------------------------------------------------
// Retriever — 混合检索执行器（管线在后续任务逐层填充）
// ---------------------------------------------------------------------------

/// GraphRAG 混合检索执行器。`graph`/`rerank` 可选（缺失即走对应降级路径）。
pub struct Retriever {
    pub(crate) graph: Option<Arc<dyn GraphRepository>>,
    pub(crate) vector: Arc<dyn VectorRepository>,
    pub(crate) embed: Arc<dyn EmbedService>,
    pub(crate) rerank: Option<Arc<dyn RerankService>>,
}

impl Retriever {
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            rerank,
        }
    }

    /// embed(query) → kg_nodes.search_with_filter(project[+origin], k) → 种子（§5.1）。
    pub(crate) async fn recall(
        &self,
        query: &str,
        project: Option<&str>,
        origin: Option<&str>,
        k: u64,
    ) -> Result<Vec<Seed>, DtError> {
        let embeddings = self.embed.embed_batch(&[query.to_string()]).await?;
        let Some(qvec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };
        let hits = self
            .vector
            .search_with_filter(KG_NODES, qvec, k, recall_filter(project, origin))
            .await?;
        let threshold = min_score();
        Ok(hits.iter().filter_map(|h| parse_seed(h, threshold)).collect())
    }
}

// ---------------------------------------------------------------------------
// ① 召回（§5.1）
// ---------------------------------------------------------------------------

/// 向量召回的种子节点。`business_id` 为稳定业务主键（旧格式点缺失即丢弃）。
#[derive(Debug, Clone)]
pub(crate) struct Seed {
    pub business_id: String,
    pub element_id: Option<String>,
    pub labels: Vec<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    pub semantic: f64,
}

/// Qdrant 原生 filter：project/origin 可选 must 条件（R7；§5.1/§5.7.1）。
fn recall_filter(project: Option<&str>, origin: Option<&str>) -> serde_json::Value {
    let mut must = Vec::new();
    if let Some(p) = project {
        must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
    }
    if let Some(o) = origin {
        must.push(serde_json::json!({"key": "origin", "match": {"value": o}}));
    }
    serde_json::json!({ "must": must })
}

/// 从 Qdrant hit 解析种子；语义分 < min_score 或无 business_id 丢弃（§5.1）。
fn parse_seed(hit: &serde_json::Value, min_score: f64) -> Option<Seed> {
    let score = hit.get("score")?.as_f64()?;
    if score < min_score {
        return None;
    }
    let p = hit.get("payload")?;
    let business_id = p.get("business_id")?.as_str()?;
    if business_id.is_empty() {
        return None;
    }
    let labels: Vec<String> = p
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let summary = p
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entity_type = p
        .get("type")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| labels.first().cloned())
        .unwrap_or_else(|| "?".to_string());
    let element_id = p
        .get("elementId")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(Seed {
        business_id: business_id.to_string(),
        element_id,
        labels,
        name,
        summary,
        entity_type,
        semantic: score,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use serde_json::json;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    pub(crate) struct MockGraph {
        pub responses: Mutex<VecDeque<serde_json::Value>>,
        pub captured: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.captured.lock().unwrap().push((query.to_string(), params));
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!([])))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(json!([]))
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) struct MockVector {
        pub hits: Vec<serde_json::Value>,
        pub captured_filter: Mutex<Option<serde_json::Value>>,
        pub captured_limit: Mutex<Option<u64>>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
        }
        async fn search_with_filter(
            &self,
            _c: &str,
            _v: Vec<f32>,
            limit: u64,
            filter: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            *self.captured_filter.lock().unwrap() = Some(filter);
            *self.captured_limit.lock().unwrap() = Some(limit);
            Ok(self.hits.clone())
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.to_string(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) struct MockEmbed;

    #[async_trait::async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) fn empty_vector() -> MockVector {
        MockVector {
            hits: vec![],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        }
    }

    pub(crate) fn seed_hit(score: f64, business_id: &str, labels: &[&str]) -> serde_json::Value {
        json!({
            "id": "pt-1",
            "score": score,
            "payload": {
                "business_id": business_id,
                "elementId": "4:91:1",
                "labels": labels,
                "name": "ifCode",
                "summary": "渠道路由字段",
                "type": "Channel",
                "project": "offen-pay",
                "origin": "extracted"
            }
        })
    }

    #[tokio::test]
    async fn recall_filters_by_score_and_business_id() {
        let vector = MockVector {
            hits: vec![
                seed_hit(0.9, "dt://entity/p/Channel/ifcode", &["Entity"]),
                seed_hit(0.1, "dt://entity/p/Channel/low", &["Entity"]), // 低于阈值
                {
                    // 旧格式点：无 business_id → 丢弃
                    let mut h = seed_hit(0.95, "x", &["Knowledge"]);
                    h["payload"].as_object_mut().unwrap().remove("business_id");
                    h
                },
            ],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let r = Retriever::new(None, Arc::new(vector), Arc::new(MockEmbed), None);
        let seeds = r
            .recall("渠道怎么路由", Some("offen-pay"), Some("extracted"), 60)
            .await
            .unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].business_id, "dt://entity/p/Channel/ifcode");
        assert_eq!(seeds[0].semantic, 0.9);
        assert!(seeds[0].labels.iter().any(|l| l == "Entity"));
    }

    #[tokio::test]
    async fn recall_builds_native_filter_with_project_and_origin() {
        let vector = Arc::new(empty_vector());
        let r = Retriever::new(None, vector.clone(), Arc::new(MockEmbed), None);
        let _ = r.recall("q", Some("p1"), Some("manual"), 60).await.unwrap();
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        let must = filter["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "project" && c["match"]["value"] == "p1"));
        assert!(must
            .iter()
            .any(|c| c["key"] == "origin" && c["match"]["value"] == "manual"));
        assert_eq!(*vector.captured_limit.lock().unwrap(), Some(60));
    }
}
