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

    /// A graph node returned by expand_nodes.
    #[derive(Debug, Clone)]
    pub struct ExpandedNode {
        pub element_id: String,
        pub name: String,
        pub label: String,
        pub relation_type: String,
    }

    /// Expand graph nodes from vector search element IDs.
    pub async fn expand_nodes(
        _graph: &(dyn GraphRepository + 'static),
        _element_ids: &Vec<String>,
        _depth: usize,
        _limit: usize,
    ) -> anyhow::Result<Vec<ExpandedNode>> {
        Ok(vec![])
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
