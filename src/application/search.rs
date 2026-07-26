//! Search module — query expansion, fusion, rewrite.

pub mod fusion {
    use std::collections::HashMap;

    /// A ranked search result item for multi-source fusion.
    #[derive(Debug, Clone)]
    pub struct RankedItem {
        pub id: String,
        pub title: String,
        pub snippet: String,
        pub source_world: String,
        pub entity_type: String,
        pub score: f64,
    }

    /// Reciprocal Rank Fusion (RRF) — combines ranked lists from multiple
    /// sources into a single ranked list.
    ///
    /// Each item in each ranked list receives a score of `1.0 / (k + rank)`,
    /// where `k` is a damping constant (default: 60). Scores are accumulated
    /// across all lists, then the items are sorted by descending score.
    pub fn reciprocal_rank_fusion(
        rank_lists: Vec<Vec<RankedItem>>,
        k: f64,
        limit: usize,
    ) -> Vec<RankedItem> {
        let mut score_map: HashMap<String, (f64, RankedItem)> = HashMap::new();

        for list in &rank_lists {
            for (rank, item) in list.iter().enumerate() {
                let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
                let key = format!("{}:{}", item.source_world, item.id);
                score_map
                    .entry(key)
                    .and_modify(|(score, _)| *score += rrf_score)
                    .or_insert_with(|| (rrf_score, item.clone()));
            }
        }

        let mut fused: Vec<_> = score_map.into_values().collect();
        fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        fused
            .into_iter()
            .take(limit)
            .map(|(score, mut item)| {
                item.score = score;
                item
            })
            .collect()
    }
}

pub mod expansion {
    use crate::domain::traits::GraphRepository;
    use std::collections::HashMap;

    /// A graph node returned by expand_nodes.
    #[derive(Debug, Clone)]
    pub struct ExpandedNode {
        pub element_id: String,
        pub name: String,
        pub label: String,
        pub relation_type: String,
    }

    /// Expand graph nodes from vector search element IDs.
    ///
    /// Traverses 1-2 hop relationships from the given element IDs to find
    /// related nodes (e.g. Method→CALLS→Method, Concept→IMPLEMENTED_BY→Method).
    pub async fn expand_nodes(
        graph: &(dyn GraphRepository + 'static),
        element_ids: &Vec<String>,
        depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ExpandedNode>> {
        if element_ids.is_empty() {
            return Ok(vec![]);
        }

        // Use variable-length path *1..N syntax (Memgraph supports this).
        // depth=2 → *1..2
        let max_hops = depth.max(1).min(3); // cap at 3 hops for performance
        let path_pattern = format!("*1..{}", max_hops);

        let cypher = format!(
            r#"
            MATCH (n) WHERE elementId(n) IN $ids
            OPTIONAL MATCH (n)-[r{path_pattern}]-(related)
            WITH n, related, relationships(r) AS rels
            UNWIND rels AS rel
            WITH n, related, type(rel) AS rel_type
            RETURN DISTINCT elementId(related) AS eid, labels(related) AS labels,
                   coalesce(related.name, related.title, '') AS name,
                   collect(DISTINCT rel_type)[0] AS rel_type
            LIMIT $limit
            "#
        );

        let mut params = HashMap::new();
        params.insert(
            "ids".to_string(),
            serde_json::Value::Array(
                element_ids
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        params.insert(
            "limit".to_string(),
            serde_json::Value::from(limit as i64),
        );

        let result = graph
            .read_query(&cypher, params)
            .await
            .map_err(|e| anyhow::anyhow!("expand_nodes query failed: {e}"))?;

        let rows = result.as_array().ok_or_else(|| {
            anyhow::anyhow!("expand_nodes: expected array response, got: {result}")
        })?;

        let nodes: Vec<ExpandedNode> = rows
            .iter()
            .filter_map(|row| {
                let element_id = row.get("eid").and_then(|v| v.as_str())?.to_string();
                let labels = row
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let label = labels.first().cloned().unwrap_or_else(|| "Unknown".into());
                let name = row
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let relation_type = row
                    .get("rel_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(ExpandedNode {
                    element_id,
                    name,
                    label,
                    relation_type,
                })
            })
            .collect();

        Ok(nodes)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::domain::traits::GraphRepository;
        use crate::domain::types::HealthStatus;
        use async_trait::async_trait;
        use std::collections::HashMap;

        /// Mock graph that captures the Cypher query and returns a fixed response
        /// simulating a Method node with one CALLS relationship.
        struct MockGraph {
            captured_query: std::sync::Mutex<String>,
        }

        impl MockGraph {
            fn new() -> Self {
                Self { captured_query: std::sync::Mutex::new(String::new()) }
            }
        }

        #[async_trait]
        impl GraphRepository for MockGraph {
            async fn read_query(
                &self,
                query: &str,
                _params: HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, crate::domain::error::DtError> {
                *self.captured_query.lock().unwrap() = query.to_string();
                // Simulate Memgraph Bolt response: array of row objects.
                // NOTE: `dir` column was dropped from the production Cypher
                // (it caused duplicate rows for multi-hop paths and was
                // never consumed by ExpandedNode).
                Ok(serde_json::json!([
                    {
                        "eid": "4:1:abc",
                        "labels": ["Method"],
                        "name": "createPay",
                        "rel_type": "CALLS"
                    }
                ]))
            }
            async fn write_query(
                &self,
                _query: &str,
                _params: HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, crate::domain::error::DtError> {
                Ok(serde_json::json!([]))
            }
            async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        #[tokio::test]
        async fn expand_nodes_returns_related_nodes() {
            let graph = MockGraph::new();
            let ids = vec!["4:0:source".to_string()];
            let result = expand_nodes(
                &graph as &dyn GraphRepository,
                &ids,
                2,   // depth
                50,  // limit
            )
            .await
            .expect("expand_nodes should succeed");

            assert!(!result.is_empty(), "should return at least one related node");
            assert_eq!(result[0].element_id, "4:1:abc");
            assert_eq!(result[0].name, "createPay");
            assert_eq!(result[0].label, "Method");
            assert_eq!(result[0].relation_type, "CALLS");

            // Verify the Cypher query contains key fragments
            let captured = graph.captured_query.lock().unwrap().clone();
            assert!(captured.contains("*1..2"), "depth=2 should produce *1..2, got: {captured}");
            assert!(captured.contains("elementId(n) IN $ids"), "should filter by elementId, got: {captured}");
            assert!(captured.contains("LIMIT $limit"), "should have LIMIT, got: {captured}");
        }
    }
}

pub mod rewrite {
    use std::collections::HashMap;

    /// Query rewriter for Chinese-English query expansion.
    pub struct QueryRewriter {
        mapping: HashMap<String, Vec<String>>,
    }

    impl QueryRewriter {
        pub fn with_defaults() -> Self {
            let mut mapping = HashMap::new();
            mapping.insert("数据库".to_string(), vec!["database".to_string(), "db".to_string()]);
            mapping.insert("密码".to_string(), vec!["password".to_string(), "pass".to_string(), "secret".to_string()]);
            mapping.insert("配置".to_string(), vec!["config".to_string(), "configuration".to_string()]);
            mapping.insert("服务".to_string(), vec!["service".to_string(), "server".to_string()]);
            mapping.insert("部署".to_string(), vec!["deploy".to_string(), "deployment".to_string()]);
            mapping.insert("地址".to_string(), vec!["address".to_string(), "host".to_string(), "url".to_string()]);
            mapping.insert("端口".to_string(), vec!["port".to_string()]);
            mapping.insert("日志".to_string(), vec!["log".to_string(), "logging".to_string()]);
            mapping.insert("缓存".to_string(), vec!["cache".to_string(), "redis".to_string()]);
            mapping.insert("消息".to_string(), vec!["message".to_string(), "queue".to_string(), "mq".to_string()]);
            QueryRewriter { mapping }
        }

        /// Rewrite a Chinese query into possible English expansions.
        /// Returns [original, expansion1, expansion2, ...].
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
