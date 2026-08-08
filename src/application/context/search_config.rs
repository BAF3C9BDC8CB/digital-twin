//! config 世界 — 自 cli/build.rs:819-1100 迁入。
//! 多查询变体 embed → config_chunks+doc_chunks 向量 → RRF → ASCII 关键词过滤；
//! 向量不可用/无结果 → Cypher 关键词回退（ConfigKey/Server/Database/NacosConfig/NacosService）。

use std::collections::HashMap;

use crate::application::context::fusion::{reciprocal_rank_fusion, RankedItem};
use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit};
use crate::shared::collections::{CONFIG_CHUNKS, DOC_CHUNKS};

fn nacos_source_ref(payload: &serde_json::Value, section: &str) -> String {
    let namespace = payload
        .get("namespace")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("public");
    let group = payload
        .get("group")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("DEFAULT_GROUP");
    let data_id = payload
        .get("data_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("config");
    format!("dt://nacos/{namespace}/{group}/{data_id}#{section}")
}

/// 提取查询中的 ASCII 词（逐字搬移自 cli/build.rs:750-756）。
///
/// 中英混合（如 "Redis集群配置信息" → ["redis"]、
/// "我所有的MySQL数据库地址" → ["mysql"]）。
pub(crate) fn extract_ascii_words(s: &str) -> Vec<String> {
    let re = regex::Regex::new(r"[a-zA-Z0-9_.-]+").unwrap();
    re.find_iter(s)
        .map(|m| m.as_str().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// 中英查询扩写（逐字搬移自 application/search.rs:253-325；仅 config 世界使用）。
pub(crate) mod rewrite {
    use std::collections::HashMap;

    /// 中英查询扩写的查询重写器。
    pub struct QueryRewriter {
        mapping: HashMap<String, Vec<String>>,
    }

    impl QueryRewriter {
        pub fn with_defaults() -> Self {
            let mut mapping = HashMap::new();
            mapping.insert(
                "数据库".to_string(),
                vec!["database".to_string(), "db".to_string()],
            );
            mapping.insert(
                "密码".to_string(),
                vec![
                    "password".to_string(),
                    "pass".to_string(),
                    "secret".to_string(),
                ],
            );
            mapping.insert(
                "配置".to_string(),
                vec!["config".to_string(), "configuration".to_string()],
            );
            mapping.insert(
                "服务".to_string(),
                vec!["service".to_string(), "server".to_string()],
            );
            mapping.insert(
                "部署".to_string(),
                vec!["deploy".to_string(), "deployment".to_string()],
            );
            mapping.insert(
                "地址".to_string(),
                vec!["address".to_string(), "host".to_string(), "url".to_string()],
            );
            mapping.insert("端口".to_string(), vec!["port".to_string()]);
            mapping.insert(
                "日志".to_string(),
                vec!["log".to_string(), "logging".to_string()],
            );
            mapping.insert(
                "缓存".to_string(),
                vec!["cache".to_string(), "redis".to_string()],
            );
            mapping.insert(
                "消息".to_string(),
                vec!["message".to_string(), "queue".to_string(), "mq".to_string()],
            );
            QueryRewriter { mapping }
        }

        /// 将中文查询重写为可能的英文扩展。
        /// 返回 [原查询, 扩展1, 扩展2, ...]。
        pub fn rewrite(&self, query: &str) -> Vec<String> {
            let mut results = vec![query.to_string()];
            for (cn, en_list) in &self.mapping {
                if query.contains(cn) {
                    for en in en_list {
                        let expanded = query.replace(cn, en);
                        if expanded != query && !results.contains(&expanded) {
                            results.push(expanded);
                        }
                    }
                }
            }
            results
        }
    }
}

/// 关键词集：原始 ASCII 词 + QueryRewriter 展开（≥3 字符、去重、上限 5）。
/// （行为同 cli/build.rs:790-813 的 get_keywords 闭包。）
fn get_keywords(query: &str) -> Vec<String> {
    let rewriter = rewrite::QueryRewriter::with_defaults();
    let candidates = rewriter.rewrite(query);
    let mut terms: Vec<String> = Vec::new();
    for w in extract_ascii_words(query) {
        if !terms.contains(&w) {
            terms.push(w);
        }
    }
    for c in candidates.iter().skip(1) {
        if c.is_ascii() {
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
}

/// 从 Qdrant payload 读取统一 `llm_analysis` 字段（T3：配置 chunk 与代码方法
/// 同一 LLM 分析契约）。字段缺失或为空 → `None`，渲染层回退"暂无摘要"。
fn payload_llm_analysis(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("llm_analysis")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn blank_hit(
    id: String,
    title: String,
    snippet: String,
    entity_type: String,
    score: f64,
) -> SearchHit {
    SearchHit {
        id,
        title,
        snippet: snippet.clone(),
        content: Some(snippet),
        source_world: "config".into(),
        entity_type,
        file_type: None,
        file_type_label: None,
        score,
        source_ref: None,
        metadata: None,
        file_path: None,
        start_line: None,
        end_line: None,
        signature: None,
        calls: vec![],
        element_id: None,
        llm_analysis: None,
        score_breakdown: None,
        hop: None,
        via_same_as: None,
        relations: None,
        evidence: None,
        rerank_degraded: None,
    }
}

impl CrossWorldSearch {
    /// config 世界检索（自 cli/build.rs:819-1100 迁入；println 移除，改返回数据）。
    pub(crate) async fn search_config(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> (Vec<SearchHit>, Vec<String>) {
        let mut degraded: Vec<String> = Vec::new();

        // ── 1. 向量路径：多查询变体 × (config_chunks + doc_chunks) ──
        let mut fused: Vec<RankedItem> = Vec::new();
        let backends = self.vector_ref().is_some() && self.embed_ref().is_some();
        if backends {
            let vector = self.vector_ref().as_ref().unwrap();
            let embed = self.embed_ref().as_ref().unwrap();

            // 多查询变体（build.rs:824-837）
            let mut qs: Vec<String> = vec![query.to_string()];
            for t in extract_ascii_words(query) {
                if t != query && !qs.contains(&t) {
                    qs.push(t);
                }
            }
            if !query.to_lowercase().contains("config") {
                qs.push(format!("{query} config"));
            }
            qs.truncate(3);

            match embed.embed_batch(&qs).await {
                Ok(all_vectors) if !all_vectors.is_empty() => {
                    let mut rank_lists: Vec<Vec<RankedItem>> = Vec::new();
                    let collections = [CONFIG_CHUNKS, DOC_CHUNKS];
                    for col in &collections {
                        for qvec in &all_vectors {
                            if let Ok(results) =
                                vector.search(col, qvec.clone(), (limit * 2) as u64).await
                            {
                                let list: Vec<RankedItem> = results
                                    .iter()
                                    .filter_map(|r| {
                                        let score =
                                            r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                        if score <= 0.0 {
                                            return None;
                                        }
                                        let payload = r.get("payload").unwrap_or(r);
                                        if *col == CONFIG_CHUNKS {
                                            // 逐字沿用 build.rs:863-894 映射
                                            let section = payload
                                                .get("section_name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let text = payload
                                                .get("text")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let data_id = payload
                                                .get("data_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            Some(RankedItem {
                                                content: Some(text.to_string()),
                                                source_ref: Some(nacos_source_ref(payload, section)),
                                                metadata: Some(serde_json::json!({"namespace": payload.get("namespace").and_then(|v| v.as_str()).unwrap_or(""), "data_id": data_id, "group": payload.get("group").and_then(|v| v.as_str()).unwrap_or(""), "section": section})),
                                                llm_analysis: payload_llm_analysis(payload),
                                                id: r
                                                    .get("id")
                                                    .map(|v| v.to_string())
                                                    .unwrap_or_default(),
                                                title: format!(
                                                    "[{}:{}] ({} keys)",
                                                    data_id,
                                                    section,
                                                    payload
                                                        .get("key_count")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0)
                                                ),
                                                snippet: text.to_string(),
                                                source_world: "vector/config_chunks".into(),
                                                entity_type: "ConfigChunk".into(),
                                                score,
                                            })
                                        } else {
                                            // doc_chunks：仅收 #section- 配置段（build.rs:896-940）
                                            let doc_id = payload
                                                .get("doc_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            if !doc_id.contains("#section-") {
                                                return None;
                                            }
                                            let text = payload
                                                .get("text")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let section_name = doc_id
                                                .rsplit_once("#section-")
                                                .map(|(_, name)| name.to_string())
                                                .unwrap_or_default();
                                            let file_path = doc_id
                                                .strip_prefix("dt://doc/")
                                                .and_then(|s| s.split_once("#section-"))
                                                .map(|(path, _)| path.to_string())
                                                .unwrap_or_default();
                                            let display_line = format!(
                                                "{}  {}",
                                                file_path,
                                                text.lines()
                                                    .next()
                                                    .unwrap_or("")
                                                    .chars()
                                                    .take(80)
                                                    .collect::<String>()
                                            );
                                            let snippet = format!("{}\n{}", text, display_line);
                                            Some(RankedItem {
                                                content: Some(text.to_string()),
                                                source_ref: Some(nacos_source_ref(
                                                    &serde_json::json!({
                                                        "namespace": "public",
                                                        "group": "DEFAULT_GROUP",
                                                        "data_id": "config",
                                                    }),
                                                    &section_name,
                                                )),
                                                metadata: Some(serde_json::json!({"namespace": "public", "group": "DEFAULT_GROUP", "section": section_name})),
                                                llm_analysis: payload_llm_analysis(payload),
                                                id: r
                                                    .get("id")
                                                    .map(|v| v.to_string())
                                                    .unwrap_or_default(),
                                                title: section_name,
                                                snippet,
                                                source_world: format!("vector/{}", col),
                                                entity_type: "Config".into(),
                                                score,
                                            })
                                        }
                                    })
                                    .collect();
                                if !list.is_empty() {
                                    rank_lists.push(list);
                                }
                            }
                        }
                    }
                    if !rank_lists.is_empty() {
                        let mut f = reciprocal_rank_fusion(rank_lists, 60.0, limit);
                        // ASCII 关键词过滤（build.rs:951-964）
                        let keywords: Vec<String> = extract_ascii_words(query)
                            .into_iter()
                            .filter(|w| w.len() >= 3)
                            .collect();
                        if !keywords.is_empty() {
                            let require_all = keywords.iter().any(|kw| kw == "nacos")
                                && keywords.iter().any(|kw| kw == "discovery");
                            f.retain(|item| {
                                let combined =
                                    format!("{} {}", item.title, item.snippet).to_lowercase();
                                if require_all {
                                    keywords.iter().all(|kw| combined.contains(kw))
                                } else {
                                    keywords.iter().any(|kw| combined.contains(kw))
                                }
                            });
                        }
                        fused = f;
                    }
                }
                _ => {
                    degraded.push("embed_unavailable".into());
                }
            }
        } else {
            degraded.push("embed_unavailable".into());
        }

        // ── 2. 向量结果 → SearchHit ──
        if !fused.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let hits: Vec<SearchHit> = fused
                .into_iter()
                .filter(|item| seen.insert(item.title.clone()))
                .map(|item| {
                    let mut h = blank_hit(
                        item.id,
                        item.title,
                        item.snippet,
                        item.entity_type,
                        item.score,
                    );
                    h.content = item.content;
                    h.source_ref = item.source_ref;
                    h.metadata = item.metadata;
                    h.llm_analysis = item.llm_analysis;
                    h
                })
                .collect();
            return (hits, degraded);
        }

        // ── 3. Cypher 关键词回退（build.rs:1010-1099）──
        let Some(graph) = self.graph_ref().as_ref() else {
            degraded.push("graph_unavailable".into());
            return (Vec::new(), degraded);
        };
        let keywords = get_keywords(query);
        if keywords.is_empty() {
            return (Vec::new(), degraded);
        }
        let orig_ascii: Vec<String> = query
            .split_whitespace()
            .filter(|w| w.is_ascii())
            .map(|w| w.to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        // 策略（build.rs:1024-1050）：有 ASCII 词只用原词；纯中文用全部展开词
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
        let display_limit = if limit > 200 { limit } else { limit.max(50) };
        let project_filter = project
            .map(|p| format!(" AND n.project = '{}' ", p.replace('\'', "\\'")))
            .unwrap_or_default();
        let cypher = format!(
            "MATCH (n) WHERE (n:ConfigKey OR n:Server \
             OR n:Database OR n:NacosConfig OR n:NacosService) \
             AND {}{project_filter}\
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
        match graph.read_query(&cypher, params).await {
            Ok(result) => {
                let rows = result.as_array();
                let mut seen_names = std::collections::HashSet::new();
                let hits: Vec<SearchHit> = rows
                    .map(|rs| {
                        rs.iter()
                            .filter_map(|row| {
                                let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                if !seen_names.insert(name.to_string()) {
                                    return None;
                                }
                                let mut h = blank_hit(
                                    name.to_string(),
                                    name.to_string(),
                                    row.get("snippet")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    row.get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?")
                                        .to_string(),
                                    0.0,
                                );
                                h.source_ref = row
                                    .get("source")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .map(|s| s.to_string());
                                Some(h)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (hits, degraded)
            }
            Err(e) => {
                tracing::warn!("config 世界 Cypher 回退失败: {e}");
                (Vec::new(), degraded)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::{
        CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
    };
    use crate::domain::error::DtError;
    use crate::domain::traits::{EmbedService, VectorRepository};
    use std::sync::Arc;

    struct StubVector {
        hits: Vec<serde_json::Value>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for StubVector {
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
            _l: u64,
            _f: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
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
                name: name.into(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    struct StubEmbed;

    #[async_trait::async_trait]
    impl EmbedService for StubEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn config_world_maps_config_chunks_payload() {
        let chunk = serde_json::json!({
            "id": "pt-c1", "score": 0.9,
            "payload": { "section_name": "spring", "data_id": "app.yaml",
                         "text": "redis:\n  host: 127.0.0.1", "key_count": 3 }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "redis 配置".into(),
            world: Some("config".into()),
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
        assert_eq!(result.per_world_counts.get("config"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.entity_type, "ConfigChunk");
        assert!(hit.title.contains("app.yaml"));
        assert!(hit.snippet.contains("redis"));
        // T2: 无 namespace/group 的 payload → 兜底 public/DEFAULT_GROUP, 锚点裸 section
        assert_eq!(
            hit.source_ref.as_deref(),
            Some("dt://nacos/public/DEFAULT_GROUP/app.yaml#spring")
        );
    }

    #[tokio::test]
    async fn config_world_source_ref_uses_payload_namespace_and_group() {
        let chunk = serde_json::json!({
            "id": "pt-c2", "score": 0.9,
            "payload": { "section_name": "spring.cloud", "data_id": "uvp-common.yaml",
                         "namespace": "test", "group": "CUSTOM_GROUP",
                         "text": "discovery:\n  server-addr: nacos:8848", "key_count": 2 }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "server-addr".into(),
            world: Some("config".into()),
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
        let hit = &result.hits[0];
        // T2: 含 namespace/group → 原样拼接, 无 environment 段
        assert_eq!(
            hit.source_ref.as_deref(),
            Some("dt://nacos/test/CUSTOM_GROUP/uvp-common.yaml#spring.cloud")
        );
        assert!(!hit.source_ref.as_deref().unwrap().contains("environment"));
        assert_eq!(hit.file_type.as_deref(), Some("nacos_config"));
    }

    #[tokio::test]
    async fn config_world_server_addr_keyword_is_recalled_exactly() {
        let chunk = serde_json::json!({
            "id": "pt-c-server-addr", "score": 0.9,
            "payload": { "section_name": "spring.cloud.nacos.discovery", "data_id": "app.yaml",
                         "namespace": "test", "group": "DEFAULT_GROUP",
                         "text": "server-addr: nacos-headless:8848", "key_count": 1 }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "server-addr".into(),
            world: Some("config".into()),
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
        assert_eq!(result.hits.len(), 1);
        assert!(result.hits[0].snippet.contains("server-addr"));
        assert_eq!(result.hits[0].entity_type, "ConfigChunk");
        assert_eq!(result.hits[0].file_type.as_deref(), Some("nacos_config"));
    }

    #[tokio::test]
    async fn config_world_doc_chunk_source_ref_excludes_environment() {
        let chunk = serde_json::json!({
            "id": "pt-d1", "score": 0.8,
            "payload": { "doc_id": "dt://doc/proj/common.yaml#section-spring.cloud",
                         "text": "spring:\n  cloud:\n    nacos:\n      discovery:\n        server-addr: nacos:8848" }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "spring.cloud".into(),
            world: Some("config".into()),
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
        // doc_chunks 分支的噪声项(keyword 过滤后无匹配)被剔除, 仅剩 Config 命中
        let hit = result
            .hits
            .iter()
            .find(|h| h.entity_type == "Config")
            .expect("doc-chunk hit");
        // T2: 硬编码路径去掉 environment 段, 锚点 #section= → 裸 section
        assert_eq!(
            hit.source_ref.as_deref(),
            Some("dt://nacos/public/DEFAULT_GROUP/config#spring.cloud")
        );
    }

    #[tokio::test]
    async fn config_world_config_chunk_llm_analysis_reads_payload() {
        // T3: config_chunks payload 携带 llm_analysis → 透传到 SearchHit
        //（与代码方法同一契约字段，不再硬编码摘要）。
        let chunk = serde_json::json!({
            "id": "pt-c3", "score": 0.9,
            "payload": { "section_name": "spring.datasource", "data_id": "app.yaml",
                         "text": "url: jdbc:mysql://127.0.0.1:3306/app", "key_count": 1,
                         "llm_analysis": "用途：配置应用的数据源连接。\n逻辑：数据源参数。" }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "datasource 配置".into(),
            world: Some("config".into()),
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
        let hit = &result.hits[0];
        assert_eq!(
            hit.llm_analysis.as_deref(),
            Some("用途：配置应用的数据源连接。\n逻辑：数据源参数。")
        );
    }

    #[tokio::test]
    async fn config_world_missing_or_empty_llm_analysis_is_none() {
        // T3: payload 缺 llm_analysis 或为空 → None（渲染层回退"暂无摘要"）。
        // 两条命中：config_chunks（无字段）与 doc_chunks（空串）。
        let chunk = serde_json::json!({
            "id": "pt-c4", "score": 0.9,
            "payload": { "section_name": "spring.redis", "data_id": "app.yaml",
                         "text": "host: 127.0.0.1", "key_count": 1 }
        });
        let doc_chunk = serde_json::json!({
            "id": "pt-d2", "score": 0.7,
            "payload": { "doc_id": "dt://doc/proj/common.yaml#section-server",
                         "text": "port: 8080", "llm_analysis": "" }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector {
                hits: vec![chunk, doc_chunk],
            })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "redis 配置".into(),
            world: Some("config".into()),
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
        assert!(!result.hits.is_empty());
        for hit in &result.hits {
            assert!(
                hit.llm_analysis.is_none(),
                "llm_analysis 缺失或为空时应为 None，实际: {:?}",
                hit.llm_analysis
            );
        }
    }

    #[tokio::test]
    async fn config_world_empty_backends_degrades_gracefully() {
        let cws = CrossWorldSearch::empty();
        let req = SearchRequest {
            query: "q".into(),
            world: Some("config".into()),
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
        assert_eq!(result.hits.len(), 0);
        assert!(
            result.degraded.contains(&"embed_unavailable".to_string())
                || result.degraded.contains(&"graph_unavailable".to_string())
        );
    }

    #[test]
    fn query_rewriter_expands_chinese_terms() {
        let rw = rewrite::QueryRewriter::with_defaults();
        let out = rw.rewrite("数据库配置");
        assert!(out.iter().any(|s| s.contains("database")));
        assert_eq!(out[0], "数据库配置");
    }
}
