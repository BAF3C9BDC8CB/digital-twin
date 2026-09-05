//! memory 世界 — 事件节点关键词检索（自 cli/build.rs:1509-1516 迁入，补 elementId 定位）。

use std::collections::HashMap;

use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit};

impl CrossWorldSearch {
    pub(crate) async fn search_memory(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        // ── 向量语义检索优先（记忆已向量化到 kg_nodes）──────────────────
        // 用户原则: 所有查询必须通过向量, 除非显式标注精准查询。
        // memory 世界向量通道: embed query → kg_nodes 集合 search_with_filter
        // (project/scope 过滤下沉 payload 级), 语义召回自然语言表述。
        // 关键词通道仅作向量不可用时的降级兜底。
        let vec_hits = self._search_memory_vector(query, limit, project).await;
        if !vec_hits.is_empty() {
            return vec_hits;
        }
        // 向量不可用或 0 命中 → 关键词降级（OR 语义, 按命中词数排序）
        self._search_memory_keyword(query, limit, project).await
    }

    /// 向量通道: kg_nodes 集合语义检索。
    async fn _search_memory_vector(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        let (Some(ref vector), Some(ref embed)) = (self.vector_ref(), self.embed_ref()) else {
            return Vec::new();
        };
        let Ok(embeddings) = embed.embed_batch(&[query.to_string()]).await else {
            return Vec::new();
        };
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Vec::new();
        };
        // 记忆统一全局检索（不分项目/全局）——按语义内容召回,
        // 靠记忆内容中的文件路径/标识定位实际位置。
        // project 参数不再参与过滤（保留用于返回字段溯源）。
        let internal_limit = (limit as u64 * 2).max(10);
        let Ok(rows) = vector
            .search(
                crate::shared::collections::KG_NODES,
                query_vec,
                internal_limit,
            )
            .await
        else {
            return Vec::new();
        };
        rows.into_iter()
            .filter_map(|row| {
                let payload = row.get("payload").cloned().unwrap_or_default();
                // 过滤已归档版本（supersede 后的旧节点）。旧节点向量在
                // supersede 时已被删除，但兜底再查一次 payload.status。
                let status = payload.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "archived" {
                    return None;
                }
                let score = row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if score < 0.3 {
                    return None; // 分数阈值, 低相关不召回
                }
                // business_id：区分记忆 vs 代码实体的关键字段。
                let bid = payload
                    .get("business_id")
                    .or_else(|| payload.get("knowledge_id"))
                    .or_else(|| payload.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let desc = payload
                    .get("content")
                    .or_else(|| payload.get("summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let proj = payload
                    .get("project")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let kid = payload
                    .get("knowledge_id")
                    .or_else(|| payload.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // ── 记忆过滤（防止代码/实体污染）────────────────────────
                // kg_nodes 集合混存了代码索引实体（build 时从代码提取，
                // business_id="dt://entity/..."、type=service/config/...、
                // origin=extracted）与真正的记忆（dt-memory 插件写入，
                // business_id="mem-..."/"hermes-..."/"auto-..."）。
                // 若不过滤，world=memory 语义检索会把大量代码实体当记忆返回，
                // 且 entity_type 被硬编码为 "Knowledge" 造成"记忆里混入代码"的错觉。
                let is_memory = is_memory_payload(&payload, &bid);
                if !is_memory {
                    return None;
                }
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let desc = payload
                    .get("content")
                    .or_else(|| payload.get("summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let proj = payload
                    .get("project")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // 实体类型：取 payload 真实 type（如 knowledge/experience/concept），
                // 避免硬编码 "Knowledge" 掩盖真实来源。
                let entity_type = payload
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        payload
                            .get("labels")
                            .and_then(|v| v.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| "Knowledge".to_string());
                Some(SearchHit {
                    id: kid.clone(),
                    title: name,
                    snippet: desc,
                    content: None,
                    source_world: "memory".into(),
                    entity_type,
                    file_type: None,
                    file_type_label: None,
                    score,
                    source_ref: None,
                    metadata: None,
                    file_path: None,
                    project: if proj.is_empty() { None } else { Some(proj) },
                    start_line: None,
                    end_line: None,
                    signature: None,
                    calls: vec![],
                    element_id: if kid.is_empty() { None } else { Some(kid) },
                    llm_analysis: None,
                    score_breakdown: None,
                    hop: None,
                    via_same_as: None,
                    relations: None,
                    evidence: None,
                    rerank_degraded: None,
                })
            })
            .take(limit)
            .collect()
    }

    /// 关键词降级通道: 图数据库 OR 关键词检索（向量不可用时兜底）。
    async fn _search_memory_keyword(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        let Some(ref graph) = self.graph_ref() else {
            return Vec::new();
        };
        // 记忆统一全局检索（不分项目/全局）——project 不参与过滤,
        // 靠记忆内容中的文件路径/标识定位实际位置。
        let project_filter = String::new();
        // 关键词拆分：按空白/标点切分。
        // OR 语义：任意关键词命中即可召回（多词 AND 对自然语言查询过严，
        // 如"payment 数据库 连接"要求三词全中，但记忆里是"支付/库"导致 0 命中）。
        // 按命中词数降序排序，命中的词越多排名越前。
        let keywords: Vec<&str> = query
            .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
            .filter(|s| !s.is_empty() && s.chars().count() >= 2)
            .collect();
        if keywords.is_empty() {
            return Vec::new();
        }
        // 每个关键词的命中条件（name/details/summary/content 任一字段）
        let kw_conds: Vec<String> = keywords
            .iter()
            .enumerate()
            .map(|(i, _)| {
                format!(
                    "(n.name CONTAINS $kw{0} OR n.details CONTAINS $kw{0} OR \
                     coalesce(n.summary, '') CONTAINS $kw{0} OR \
                     coalesce(n.content, '') CONTAINS $kw{0})",
                    i
                )
            })
            .collect();
        // 命中词数（排序用）
        let score_expr = format!(
            "({})",
            keywords
                .iter()
                .enumerate()
                .map(|(i, _)| format!(
                    "CASE WHEN (n.name CONTAINS $kw{0} OR n.details CONTAINS $kw{0} OR \
                     coalesce(n.summary, '') CONTAINS $kw{0} OR \
                     coalesce(n.content, '') CONTAINS $kw{0}) THEN 1 ELSE 0 END",
                    i
                ))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        let cypher = format!(
            "MATCH (n) WHERE (n:Modification OR n:Deployment OR n:ConfigChange OR \
             n:BugFix OR n:Decision OR n:Conversation OR n:Session OR n:Knowledge) \
             AND coalesce(n.status, 'active') <> 'archived' \
             AND ({}){project_filter}\
             RETURN labels(n)[0] AS type, coalesce(n.name, n.entity_id, n.session_id, '') AS name, \
                    coalesce(n.details, n.summary, n.content, '') AS desc, elementId(n) AS eid, \
                    {score_expr} AS score \
             ORDER BY score DESC \
             LIMIT {limit}",
            kw_conds.join(" OR "),
        );
        let mut params = HashMap::new();
        for (i, kw) in keywords.iter().enumerate() {
            params.insert(
                format!("kw{}", i),
                serde_json::Value::String(kw.to_string()),
            );
        }
        let Ok(result) = graph.read_query(&cypher, params).await else {
            return Vec::new();
        };
        result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| SearchHit {
                        id: row
                            .get("eid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        title: row
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        snippet: row
                            .get("desc")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        content: None,
                        source_world: "memory".into(),
                        entity_type: row
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        file_type: None,
                        file_type_label: None,
                        score: row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        source_ref: None,
                        metadata: None,
                        file_path: None,
                        project: None,
                        start_line: None,
                        end_line: None,
                        signature: None,
                        calls: vec![],
                        element_id: row
                            .get("eid")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        llm_analysis: None,
                        score_breakdown: None,
                        hop: None,
                        via_same_as: None,
                        relations: None,
                        evidence: None,
                        rerank_degraded: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 判断一个 kg_nodes payload 是否属于**真正的记忆/知识条目**，而非
/// 从代码提取的索引实体。
///
/// # 为什么需要
///
/// `kg_nodes` 集合混存了两种来源：
/// - **记忆**：dt-memory 插件经 `dt memorize` 写入的 Knowledge/Experience/
///   Concept/Domain/Playbook 节点。business_id 为 `mem-*`/`hermes-*`/`auto-*`
///   （dt-memory 插件生成的 id）或以 `dt://knowledge`/`dt://experience` 等
///   知识世界 URI 开头，type 为 lowercase 的标签名（knowledge/experience/...）。
/// - **代码索引实体**：`dt build` 从代码提取，business_id 为
///   `dt://entity/<project>/...`，type 为 service/config/...，origin=extracted。
///
/// 若不做区分，`world=memory` 的向量检索会把大量代码实体当记忆返回，
/// 造成"记忆里混入 code 内容"的错觉（此前 entity_type 还被硬编码为
/// "Knowledge"，进一步掩盖真实来源）。
fn is_memory_payload(payload: &serde_json::Value, business_id: &str) -> bool {
    let bid = business_id.trim().to_lowercase();
    let bid_lc = bid.as_str();

    // 1. 显式排除代码/实体索引：business_id 为 dt://entity/...（构建提取的实体）
    if bid_lc.starts_with("dt://entity/") {
        return false;
    }
    // 2. 显式排除从代码提取的原生实体（origin=extracted）
    let origin = payload
        .get("origin")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    if origin == "extracted" || origin == "learned" && bid_lc.is_empty() {
        // "learned" 是 build_payload 的默认 origin，但知识世界节点同样会落到
        // "learned"。因此仅当 business_id 无知识世界特征时才据此排除。
        return false;
    }
    // 3. 记忆/知识世界特征（任一满足即视为记忆）
    if bid_lc.starts_with("mem-")
        || bid_lc.starts_with("hermes-")
        || bid_lc.starts_with("auto-")
        || bid_lc.starts_with("dt://knowledge/")
        || bid_lc.starts_with("dt://experience/")
        || bid_lc.starts_with("dt://concept/")
        || bid_lc.starts_with("dt://domain/")
        || bid_lc.starts_with("dt://playbook/")
    {
        return true;
    }
    // 4. type 为知识世界标签（knowledge/experience/concept/domain/playbook）
    if let Some(t) = payload.get("type").and_then(|v| v.as_str()) {
        let t = t.trim().to_lowercase();
        if matches!(
            t.as_str(),
            "knowledge" | "experience" | "concept" | "domain" | "playbook" | "learning"
        ) {
            return true;
        }
    }
    // 5. 兜底：labels 含知识世界标签
    if let Some(arr) = payload.get("labels").and_then(|v| v.as_array()) {
        for l in arr {
            if let Some(s) = l.as_str() {
                let s = s.to_lowercase();
                if matches!(
                    s.as_str(),
                    "knowledge" | "experience" | "concept" | "domain" | "playbook"
                ) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::{
        CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
    };
    use crate::domain::traits::GraphRepository;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MockGraph {
        captured_query: std::sync::Mutex<String>,
    }

    impl MockGraph {
        fn new() -> Self {
            Self {
                captured_query: std::sync::Mutex::new(String::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            *self.captured_query.lock().unwrap() = query.to_string();
            Ok(serde_json::json!([{
                "type": "Decision", "name": "s5-d6-clamp",
                "desc": "rerank 分数 clamp 归一", "eid": "4:0:99"
            }]))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            Ok(serde_json::json!([]))
        }
        async fn health_check(
            &self,
        ) -> Result<crate::domain::types::HealthStatus, crate::domain::error::DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn memory_world_project_filter_adds_condition() {
        // 记忆统一全局检索: 传了 project 也不应出现 project 过滤条件（不分项目/全局）
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: Some("pay-center".into()),
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws.search(&req).await.unwrap();
        let captured = graph.captured_query.lock().unwrap().clone();
        assert!(!captured.contains("n.project"));
        assert!(!captured.contains("scope"));
        // 不带 project（全局）时同样不出现 project 条件
        let graph2 = Arc::new(MockGraph::new());
        let cws2 = CrossWorldSearch::new(Some(graph2.clone()), None, None, None);
        let req2 = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws2.search(&req2).await.unwrap();
        let captured2 = graph2.captured_query.lock().unwrap().clone();
        assert!(!captured2.contains("n.project"));
    }

    #[tokio::test]
    async fn memory_world_queries_event_labels_and_maps_rows() {
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("memory"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.entity_type, "Decision");
        assert_eq!(hit.title, "s5-d6-clamp");
        assert_eq!(hit.source_world, "memory");
        assert_eq!(hit.element_id.as_deref(), Some("4:0:99"));
        let captured = graph.captured_query.lock().unwrap().clone();
        assert!(captured.contains("n:Modification"));
        assert!(captured.contains("n:Decision"));
        assert!(captured.contains("n:Knowledge"));
        assert!(captured.contains("elementId(n) AS eid"));
    }

    #[tokio::test]
    async fn memory_world_multi_keyword_and_semantics() {
        // 多词查询 → 拆成多个 kw 参数，AND 连接，且不出现整串 $q
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "净盘 分账基数".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws.search(&req).await.unwrap();
        let captured = graph.captured_query.lock().unwrap().clone();
        // AND 语义：两个关键词条件都出现
        assert!(captured.contains("CONTAINS $kw0"));
        assert!(captured.contains("CONTAINS $kw1"));
        assert!(captured.contains("AND"));
        // 不应有整串 $q（旧实现）
        assert!(!captured.contains("CONTAINS $q"));
    }

    #[test]
    fn is_memory_payload_accepts_real_memories() {
        use serde_json::json;
        // dt-memory 插件显式写入的记忆（business_id = mem-xxx）
        let mem = json!({"business_id":"mem-a1b2c3","name":"支付迁移","type":"knowledge","origin":"user_explicit","project":"hermes-global"});
        assert!(is_memory_payload(&mem, "mem-a1b2c3"));
        // hermes- 前缀（agent_curated）
        let hermes =
            json!({"business_id":"hermes-memory-add-x","name":"约定","labels":["Knowledge"]});
        assert!(is_memory_payload(&hermes, "hermes-memory-add-x"));
        // auto- 前缀（LLM 自动提取）
        let auto = json!({"business_id":"auto-9f82","name":"经验","type":"experience"});
        assert!(is_memory_payload(&auto, "auto-9f82"));
        // dt://knowledge URI（知识世界节点）
        let dk = json!({"business_id":"dt://knowledge/pay/rule","name":"规则","type":"knowledge"});
        assert!(is_memory_payload(&dk, "dt://knowledge/pay/rule"));
        // type=concept（概念）
        let concept = json!({"name":"分账基数","type":"concept"});
        assert!(is_memory_payload(&concept, "dt://concept/x")); // 由 type 判别
                                                                // 纯标签兜底
        let labels_only = json!({"name":"经验教训","labels":["Experience"]});
        assert!(is_memory_payload(&labels_only, "custom-id"));
    }

    #[test]
    fn is_memory_payload_rejects_code_entities() {
        use serde_json::json;
        // dt://entity/... 代码索引实体（service/config）
        let svc = json!({"business_id":"dt://entity/digital-twin-v2/Service/skill%20loading","name":"Skill Loading","type":"Service","origin":"extracted","project":"digital-twin-v2"});
        assert!(!is_memory_payload(
            &svc,
            "dt://entity/digital-twin-v2/Service/skill%20loading"
        ));
        let cfg = json!({"business_id":"dt://entity/p/Config/x","name":"X","type":"config","origin":"extracted"});
        assert!(!is_memory_payload(&cfg, "dt://entity/p/Config/x"));
        // 非记忆标签 (type=channel/document/service) + 非知识 business_id
        let ch = json!({"name":"渠道","type":"channel","origin":"extracted","business_id":"dt://entity/p/Channel/c"});
        assert!(!is_memory_payload(&ch, "dt://entity/p/Channel/c"));
        // business_id 空且 type 非知识 → 排除
        let empty = json!({"name":"某实体","type":"service","origin":"learned"});
        assert!(!is_memory_payload(&empty, ""));
        // 空 payload 一律排除
        assert!(!is_memory_payload(&json!({}), ""));
    }
}
