//! **HistoryService** — historical task retrieval along the Memory World timeline (4.5).
//!
//! Searches for similar tasks, bug fixes, deployments, and decisions
//! in the Memory World, ordered by recency or relevance.
//!
//! # MCP tool: `dt_history`
//!
//! ```text
//! dt_history(task: str, limit?: int, since?: str)
//!   → HistoryResult JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Input for history search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryRequest {
    /// Task description or keywords to search for.
    pub task: String,
    /// Maximum number of results.
    pub limit: Option<usize>,
    /// ISO 8601 timestamp to filter results since.
    pub since: Option<String>,
    /// Filter by entity type (e.g. "BugFix", "Deployment", "Decision").
    pub entity_types: Option<Vec<String>>,
    /// Filter by project.
    pub project: Option<String>,
}

/// A single similar task found in history.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SimilarTask {
    /// Entity ID from the graph.
    pub id: String,
    /// Task title or name.
    pub title: String,
    /// Description or summary.
    pub description: String,
    /// Entity type (BugFix, Deployment, Decision, Experience, etc.).
    pub entity_type: String,
    /// Relevance score [0.0, 1.0].
    pub score: f64,
    /// Timestamp as ISO 8601 string.
    pub timestamp: Option<String>,
    /// Project this event belongs to.
    pub project: Option<String>,
    /// Whether this was successful (if known).
    pub success: Option<bool>,
}

/// Output of the history query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryResult {
    /// The original query.
    pub query: String,
    /// Matched similar tasks.
    pub similar_tasks: Vec<SimilarTask>,
    /// Total matches found (before limit/cap).
    pub total_found: usize,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Retrieves similar tasks from the Memory World.
#[async_trait::async_trait]
pub trait HistoryTrait: Send + Sync {
    /// Search for similar historical tasks.
    async fn search(&self, request: &HistoryRequest) -> Result<HistoryResult, DtError>;
}

/// Canonical implementation of [`HistoryTrait`].
pub struct HistoryService {
    graph: Arc<dyn GraphRepository>,
}

impl HistoryService {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    /// Tokenize a task string into keywords.
    fn tokenize(text: &str) -> Vec<String> {
        text.split_whitespace()
            .filter(|w| w.len() > 2)
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .take(10)
            .collect()
    }
}

#[async_trait::async_trait]
impl HistoryTrait for HistoryService {
    async fn search(&self, request: &HistoryRequest) -> Result<HistoryResult, DtError> {
        let limit = request.limit.unwrap_or(20);
        let keywords = Self::tokenize(&request.task);

        // Build entity-type filter clause
        let type_clause = match &request.entity_types {
            Some(types) if !types.is_empty() => {
                let type_list: Vec<String> = types.iter().map(|t| format!("'{t}'")).collect();
                format!("AND labels(n)[0] IN [{}]", type_list.join(", "))
            }
            _ => {
                "AND labels(n)[0] IN ['BugFix','Deployment','Decision','Experience','KnowledgeAdded']"
                    .to_string()
            }
        };

        let since_clause = match &request.since {
            Some(s) => format!("AND n.created_at >= '{s}'"),
            None => String::new(),
        };

        let project_clause = match &request.project {
            Some(p) => format!("AND (n.project = '{p}' OR n.project CONTAINS '{p}')"),
            None => String::new(),
        };

        let cypher = format!(
            r#"
            MATCH (n)
            WHERE (n.title CONTAINS $fragment
                OR n.description CONTAINS $fragment
                OR n.summary CONTAINS $fragment
                OR n.name CONTAINS $fragment
                OR any(kw IN $keywords WHERE n.title CONTAINS kw OR n.description CONTAINS kw))
            {type_clause}
            {since_clause}
            {project_clause}
            RETURN elementId(n) AS id,
                   coalesce(n.title, n.name, '') AS title,
                   coalesce(n.description, n.summary, '') AS description,
                   labels(n)[0] AS type,
                   coalesce(n.created_at, '') AS timestamp,
                   coalesce(n.project, '') AS project,
                   coalesce(n.success, NULL) AS success
            ORDER BY n.created_at DESC
            LIMIT {limit}
            "#,
        );

        let mut params = std::collections::HashMap::new();
        params.insert(
            "fragment".to_string(),
            serde_json::Value::String(request.task.clone()),
        );
        params.insert(
            "keywords".to_string(),
            serde_json::Value::Array(
                keywords
                    .iter()
                    .map(|k| serde_json::Value::String(k.clone()))
                    .collect(),
            ),
        );

        let result = self.graph.read_query(&cypher, params).await?;

        let mut similar_tasks = Vec::new();

        let rows = result
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|results| results.first())
            .and_then(|first| first.get("data"))
            .and_then(|data| data.as_array());

        if let Some(rows) = rows {
            for row_val in rows {
                let row = row_val.get("row").and_then(|r| r.as_array());
                if let Some(row) = row {
                    let id = row
                        .first()
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let title = row
                        .get(1)
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let description = row
                        .get(2)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let etype = row
                        .get(3)
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let timestamp = row.get(4).and_then(|v| v.as_str()).map(|s| s.to_string());
                    let project = row.get(5).and_then(|v| v.as_str()).map(|s| s.to_string());
                    let success = row.get(6).and_then(|v| v.as_bool());

                    similar_tasks.push(SimilarTask {
                        id,
                        title,
                        description,
                        entity_type: etype,
                        score: 0.7, // placeholder; real impl would use similarity scoring
                        timestamp,
                        project,
                        success,
                    });
                }
            }
        }

        let total = similar_tasks.len();
        Ok(HistoryResult {
            query: request.task.clone(),
            similar_tasks,
            total_found: total,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_task_construction() {
        let task = SimilarTask {
            id: "4:abc".into(),
            title: "Fixed payment timeout".into(),
            description: "Increased connection timeout".into(),
            entity_type: "BugFix".into(),
            score: 0.85,
            timestamp: Some("2026-07-01T10:00:00Z".into()),
            project: Some("digital-twin-v2".into()),
            success: Some(true),
        };
        assert_eq!(task.entity_type, "BugFix");
        assert!(task.score > 0.8);
    }

    #[test]
    fn history_result_empty() {
        let result = HistoryResult {
            query: "test".into(),
            similar_tasks: vec![],
            total_found: 0,
        };
        assert_eq!(result.total_found, 0);
    }

    #[test]
    fn history_request_defaults() {
        let req = HistoryRequest {
            task: "fix timeout".into(),
            limit: None,
            since: None,
            entity_types: None,
            project: None,
        };
        assert_eq!(req.task, "fix timeout");
        assert_eq!(req.limit, None);
    }

    #[test]
    fn tokenize_simple() {
        let tokens = HistoryService::tokenize("fix payment timeout bug");
        assert!(!tokens.is_empty());
        for t in &tokens {
            assert!(t.len() > 2);
        }
    }

    #[test]
    fn history_result_serialization() {
        let result = HistoryResult {
            query: "fix payment".into(),
            similar_tasks: vec![SimilarTask {
                id: "4:x".into(),
                title: "Fixed payment".into(),
                description: "desc".into(),
                entity_type: "BugFix".into(),
                score: 0.9,
                timestamp: Some("2026-07-01".into()),
                project: Some("dt".into()),
                success: Some(true),
            }],
            total_found: 1,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("fix payment"));
        assert!(json.contains("Fixed payment"));
    }
}
