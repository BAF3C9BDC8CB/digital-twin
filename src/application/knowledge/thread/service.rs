//! **ThreadService** — Digital Thread node management (4.10).
//!
//! Manages Digital Thread nodes in the Knowledge Graph for tracking
//! long-running conversations, investigations, and multi-task workflows.
//! A thread connects related sessions, decisions, and events on a timeline.
//!
//! # MCP tool: `dt_thread`
//!
//! ```text
//! dt_thread(action: str, thread_id?: str, title?: str, ...)
//!   → ThreadInfo JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Action to perform on a thread.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ThreadAction {
    Create {
        title: String,
        description: Option<String>,
        project: Option<String>,
    },
    AddSession {
        thread_id: String,
        session_id: String,
        summary: Option<String>,
    },
    AddDecision {
        thread_id: String,
        decision: String,
        reason: Option<String>,
        impact: Option<String>,
    },
    Get {
        thread_id: String,
    },
    List {
        project: Option<String>,
        limit: Option<usize>,
    },
    Close {
        thread_id: String,
        outcome: Option<String>,
    },
}

/// Request parameter parsing from MCP tool arguments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadRequest {
    pub action: String,
    pub thread_id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub session_id: Option<String>,
    pub summary: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub impact: Option<String>,
    pub outcome: Option<String>,
    pub project: Option<String>,
    pub limit: Option<usize>,
}

/// A thread session entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadSession {
    pub session_id: String,
    pub summary: String,
    pub timestamp: String,
}

/// A decision recorded in a thread.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadDecision {
    pub decision: String,
    pub reason: Option<String>,
    pub impact: Option<String>,
    pub timestamp: String,
}

/// Full thread information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadInfo {
    pub thread_id: String,
    pub title: String,
    pub description: String,
    pub project: Option<String>,
    pub status: String,
    pub sessions: Vec<ThreadSession>,
    pub decisions: Vec<ThreadDecision>,
    pub event_count: usize,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

/// Thread list result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadListResult {
    pub threads: Vec<ThreadInfo>,
    pub total: usize,
}

/// Unified thread response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadResponse {
    pub action: String,
    pub thread: Option<ThreadInfo>,
    pub list: Option<ThreadListResult>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Manages Digital Thread nodes in the Knowledge Graph.
#[async_trait::async_trait]
pub trait ThreadTrait: Send + Sync {
    /// Execute a thread action.
    async fn execute(&self, request: &ThreadRequest) -> Result<ThreadResponse, DtError>;
}

/// Canonical implementation of [`ThreadTrait`].
pub struct ThreadService {
    graph: Arc<dyn GraphRepository>,
}

impl ThreadService {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    fn now_iso() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    // ── Row helpers ──────────────────────────────────────────────────────

    /// Extract the first result row as a JSON object with named keys.
    ///
    /// Handles both the Bolt driver format (Array of row objects with
    /// named keys) and the legacy HTTP API format (nested results/data/row).
    /// Returns the row object for named-key access.
    fn first_row_obj(result: &serde_json::Value) -> Option<serde_json::Value> {
        // Bolt driver format: Array of row objects like [{"id": "4:xxx", ...}]
        if let Some(rows) = result.as_array() {
            return rows.first().cloned();
        }
        // Legacy HTTP API format
        result
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|first| first.get("data"))
            .and_then(|d| d.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row_val| row_val.get("row"))
            .cloned()
    }

    /// Extract a string value from a row object by named key.
    fn row_val_str(row: &Option<serde_json::Value>, key: &str, default: &str) -> String {
        row.as_ref()
            .and_then(|r| r.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    /// Extract an optional string value from a row object by named key.
    fn row_val_str_opt(row: &Option<serde_json::Value>, key: &str) -> Option<String> {
        row.as_ref()
            .and_then(|r| r.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    // ── ThreadInfo from row ──────────────────────────────────────────────

    #[allow(dead_code)]
    fn thread_info_from_row(thread_id: &str, row: &Option<serde_json::Value>) -> ThreadInfo {
        ThreadInfo {
            thread_id: thread_id.to_string(),
            title: Self::row_val_str(row, "title", "?"),
            description: Self::row_val_str(row, "description", ""),
            status: Self::row_val_str(row, "status", "active"),
            project: Self::row_val_str_opt(row, "project"),
            sessions: vec![],
            decisions: vec![],
            event_count: 0,
            created_at: Self::row_val_str(row, "created_at", "?"),
            updated_at: Self::now_iso(),
            closed_at: Self::row_val_str_opt(row, "closed_at"),
        }
    }

    // ── Action: Create ───────────────────────────────────────────────────

    async fn create_thread(
        &self,
        title: &str,
        description: Option<&str>,
        project: Option<&str>,
    ) -> Result<ThreadInfo, DtError> {
        let now = Self::now_iso();
        let desc = description.unwrap_or("");
        let proj = project.unwrap_or("unknown");

        let cypher = r#"
            CREATE (t:Thread {
                name: $title,
                title: $title,
                description: $description,
                project: $project,
                status: 'active',
                created_at: $now,
                updated_at: $now
            })
            RETURN elementId(t) AS id
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert("title".into(), serde_json::Value::String(title.to_string()));
        params.insert(
            "description".into(),
            serde_json::Value::String(desc.to_string()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(proj.to_string()),
        );
        params.insert("now".into(), serde_json::Value::String(now.clone()));

        let result = self.graph.write_query(cypher, params).await?;

        let row = Self::first_row_obj(&result);
        let thread_id = Self::row_val_str(&row, "id", "?");

        Ok(ThreadInfo {
            thread_id,
            title: title.to_string(),
            description: desc.to_string(),
            project: Some(proj.to_string()),
            status: "active".into(),
            sessions: vec![],
            decisions: vec![],
            event_count: 0,
            created_at: now.clone(),
            updated_at: now,
            closed_at: None,
        })
    }

    // ── Action: AddSession ───────────────────────────────────────────────

    async fn add_session(
        &self,
        thread_id: &str,
        session_id: &str,
        summary: Option<&str>,
    ) -> Result<ThreadInfo, DtError> {
        let now = Self::now_iso();
        let sum = summary.unwrap_or("");

        let cypher = r#"
            MATCH (t:Thread) WHERE elementId(t) = $thread_id
            SET t.updated_at = $now
            CREATE (s:Session {
                session_id: $session_id,
                summary: $summary,
                thread_id: $thread_id,
                timestamp: $now
            })
            CREATE (t)-[:HAS_SESSION]->(s)
            RETURN t.title AS title, t.description AS description,
                   t.status AS status, t.project AS project,
                   t.created_at AS created_at, t.closed_at AS closed_at
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "thread_id".into(),
            serde_json::Value::String(thread_id.to_string()),
        );
        params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );
        params.insert("summary".into(), serde_json::Value::String(sum.to_string()));
        params.insert("now".into(), serde_json::Value::String(now.clone()));

        let result = self.graph.write_query(cypher, params).await?;
        let row = Self::first_row_obj(&result);

        Ok(ThreadInfo {
            thread_id: thread_id.to_string(),
            title: Self::row_val_str(&row, "title", "?"),
            description: Self::row_val_str(&row, "description", ""),
            status: Self::row_val_str(&row, "status", "active"),
            project: Self::row_val_str_opt(&row, "project"),
            sessions: vec![ThreadSession {
                session_id: session_id.to_string(),
                summary: sum.to_string(),
                timestamp: now.clone(),
            }],
            decisions: vec![],
            event_count: 1,
            created_at: Self::row_val_str(&row, "created_at", "?"),
            updated_at: now,
            closed_at: Self::row_val_str_opt(&row, "closed_at"),
        })
    }

    // ── Action: AddDecision ──────────────────────────────────────────────

    async fn add_decision(
        &self,
        thread_id: &str,
        decision: &str,
        reason: Option<&str>,
        impact: Option<&str>,
    ) -> Result<ThreadInfo, DtError> {
        let now = Self::now_iso();

        let cypher = r#"
            MATCH (t:Thread) WHERE elementId(t) = $thread_id
            SET t.updated_at = $now
            CREATE (d:Decision {
                decision: $decision,
                reason: $reason,
                impact: $impact,
                timestamp: $now
            })
            CREATE (t)-[:HAS_DECISION]->(d)
            RETURN t.title AS title, t.description AS description,
                   t.status AS status, t.project AS project,
                   t.created_at AS created_at, t.closed_at AS closed_at
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "thread_id".into(),
            serde_json::Value::String(thread_id.to_string()),
        );
        params.insert(
            "decision".into(),
            serde_json::Value::String(decision.to_string()),
        );
        params.insert(
            "reason".into(),
            serde_json::Value::String(reason.unwrap_or("").to_string()),
        );
        params.insert(
            "impact".into(),
            serde_json::Value::String(impact.unwrap_or("").to_string()),
        );
        params.insert("now".into(), serde_json::Value::String(now.clone()));

        let result = self.graph.write_query(cypher, params).await?;
        let row = Self::first_row_obj(&result);

        Ok(ThreadInfo {
            thread_id: thread_id.to_string(),
            title: Self::row_val_str(&row, "title", "?"),
            description: Self::row_val_str(&row, "description", ""),
            status: Self::row_val_str(&row, "status", "active"),
            project: Self::row_val_str_opt(&row, "project"),
            sessions: vec![],
            decisions: vec![ThreadDecision {
                decision: decision.to_string(),
                reason: reason.map(|s| s.to_string()),
                impact: impact.map(|s| s.to_string()),
                timestamp: now.clone(),
            }],
            event_count: 1,
            created_at: Self::row_val_str(&row, "created_at", "?"),
            updated_at: now,
            closed_at: Self::row_val_str_opt(&row, "closed_at"),
        })
    }

    // ── Action: Get ──────────────────────────────────────────────────────

    async fn get_thread(&self, thread_id: &str) -> Result<ThreadInfo, DtError> {
        let cypher = r#"
            MATCH (t:Thread) WHERE elementId(t) = $thread_id
            OPTIONAL MATCH (t)-[:HAS_SESSION]->(s:Session)
            OPTIONAL MATCH (t)-[:HAS_DECISION]->(d:Decision)
            RETURN t.title AS title, t.description AS description,
                   t.status AS status, t.project AS project,
                   t.created_at AS created_at, t.updated_at AS updated_at,
                   t.closed_at AS closed_at,
                   collect(DISTINCT {
                     session_id: s.session_id,
                     summary: s.summary,
                     timestamp: s.timestamp
                   }) AS sessions,
                   collect(DISTINCT {
                     decision: d.decision,
                     reason: d.reason,
                     impact: d.impact,
                     timestamp: d.timestamp
                   }) AS decisions
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "thread_id".into(),
            serde_json::Value::String(thread_id.to_string()),
        );

        let result = self.graph.read_query(cypher, params).await?;

        let row = Self::first_row_obj(&result);
        match row {
            Some(ref row_obj) => {
                // Parse sessions
                let sessions: Vec<ThreadSession> = row_obj
                    .get("sessions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                Some(ThreadSession {
                                    session_id: item.get("session_id")?.as_str()?.to_string(),
                                    summary: item.get("summary")?.as_str()?.to_string(),
                                    timestamp: item.get("timestamp")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                // Parse decisions
                let decisions: Vec<ThreadDecision> = row_obj
                    .get("decisions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                Some(ThreadDecision {
                                    decision: item.get("decision")?.as_str()?.to_string(),
                                    reason: item.get("reason")?.as_str().map(|s| s.to_string()),
                                    impact: item.get("impact")?.as_str().map(|s| s.to_string()),
                                    timestamp: item.get("timestamp")?.as_str()?.to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let event_count = sessions.len() + decisions.len();

                Ok(ThreadInfo {
                    thread_id: thread_id.to_string(),
                    title: Self::row_val_str(&row, "title", "?"),
                    description: Self::row_val_str(&row, "description", ""),
                    status: Self::row_val_str(&row, "status", "?"),
                    project: Self::row_val_str_opt(&row, "project"),
                    sessions,
                    decisions,
                    event_count,
                    created_at: Self::row_val_str(&row, "created_at", "?"),
                    updated_at: Self::row_val_str(&row, "updated_at", "?"),
                    closed_at: Self::row_val_str_opt(&row, "closed_at"),
                })
            }
            _ => Err(DtError::NotFound(format!("Thread not found: {thread_id}"))),
        }
    }

    // ── Action: List ─────────────────────────────────────────────────────

    async fn list_threads(
        &self,
        project: Option<&str>,
        limit: usize,
    ) -> Result<ThreadListResult, DtError> {
        let project_clause = match project {
            Some(p) => format!("AND t.project = '{p}'"),
            None => String::new(),
        };

        let cypher = format!(
            r#"
            MATCH (t:Thread)
            WHERE 1=1 {project_clause}
            OPTIONAL MATCH (t)-[:HAS_SESSION]->(s:Session)
            OPTIONAL MATCH (t)-[:HAS_DECISION]->(d:Decision)
            RETURN elementId(t) AS id,
                   t.title AS title,
                   t.description AS description,
                   t.status AS status,
                   t.project AS project,
                   t.created_at AS created_at,
                   t.updated_at AS updated_at,
                   t.closed_at AS closed_at,
                   count(DISTINCT s) AS session_count,
                   count(DISTINCT d) AS decision_count
            ORDER BY t.updated_at DESC
            LIMIT {limit}
            "#
        );

        let params = std::collections::HashMap::new();
        let result = self.graph.read_query(&cypher, params).await?;

        // Handle both Bolt driver format (Array of row objects) and legacy HTTP format
        let rows: Vec<&serde_json::Value> = if let Some(arr) = result.as_array() {
            // Bolt driver format: Array of row objects with named keys
            arr.iter().collect()
        } else {
            // Legacy HTTP API format
            result
                .get("results")
                .and_then(|r| r.as_array())
                .and_then(|results| results.first())
                .and_then(|first| first.get("data"))
                .and_then(|data| data.as_array())
                .map(|arr| arr.iter().collect::<Vec<_>>())
                .unwrap_or_default()
        };

        let mut threads = Vec::new();
        for row_val in rows {
            // Bolt driver format: row object has named keys like {"id": ..., "title": ..., ...}
            // Legacy format: row_val.get("row") returns positional array
            let is_driver_format = row_val.as_object().is_some();
            let get = |key: &str, default: &str| -> String {
                if is_driver_format {
                    row_val
                        .get(key)
                        .and_then(|v| v.as_str())
                        .unwrap_or(default)
                        .to_string()
                } else {
                    // Legacy: positional array by index
                    let idx = match key {
                        "id" => 0,
                        "title" => 1,
                        "description" => 2,
                        "status" => 3,
                        "project" => 4,
                        "created_at" => 5,
                        "updated_at" => 6,
                        "closed_at" => 7,
                        "session_count" => 8,
                        "decision_count" => 9,
                        _ => return default.to_string(),
                    };
                    row_val
                        .get("row")
                        .and_then(|r| r.as_array())
                        .and_then(|arr| arr.get(idx))
                        .and_then(|v| v.as_str())
                        .unwrap_or(default)
                        .to_string()
                }
            };
            let get_opt = |key: &str| -> Option<String> {
                if is_driver_format {
                    row_val
                        .get(key)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                } else {
                    let idx = match key {
                        "project" => 4,
                        "closed_at" => 7,
                        _ => return None,
                    };
                    row_val
                        .get("row")
                        .and_then(|r| r.as_array())
                        .and_then(|arr| arr.get(idx))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                }
            };
            let get_int = |key: &str| -> usize {
                if is_driver_format {
                    row_val.get(key).and_then(|v| v.as_i64()).unwrap_or(0) as usize
                } else {
                    let idx = match key {
                        "session_count" => 8,
                        "decision_count" => 9,
                        _ => return 0,
                    };
                    row_val
                        .get("row")
                        .and_then(|r| r.as_array())
                        .and_then(|arr| arr.get(idx))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as usize
                }
            };

            let id = get("id", "?");
            let title = get("title", "?");
            let desc = get("description", "");
            let status = get("status", "?");
            let proj = get_opt("project");
            let created = get("created_at", "?");
            let updated = get("updated_at", "?");
            let closed = get_opt("closed_at");
            let session_count = get_int("session_count");
            let decision_count = get_int("decision_count");

            threads.push(ThreadInfo {
                thread_id: id,
                title,
                description: desc,
                project: proj,
                status,
                sessions: vec![],
                decisions: vec![],
                event_count: session_count + decision_count,
                created_at: created,
                updated_at: updated,
                closed_at: closed,
            });
        }

        let total = threads.len();
        Ok(ThreadListResult { threads, total })
    }

    // ── Action: Close ────────────────────────────────────────────────────

    async fn close_thread(
        &self,
        thread_id: &str,
        outcome: Option<&str>,
    ) -> Result<ThreadInfo, DtError> {
        let now = Self::now_iso();
        let outcome_val = outcome.unwrap_or("completed");

        let cypher = r#"
            MATCH (t:Thread) WHERE elementId(t) = $thread_id
            SET t.status = 'closed',
                t.closed_at = $now,
                t.updated_at = $now,
                t.outcome = $outcome
            RETURN t.title AS title, t.description AS description,
                   t.project AS project, t.created_at AS created_at
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "thread_id".into(),
            serde_json::Value::String(thread_id.to_string()),
        );
        params.insert("now".into(), serde_json::Value::String(now.clone()));
        params.insert(
            "outcome".into(),
            serde_json::Value::String(outcome_val.to_string()),
        );

        let result = self.graph.write_query(cypher, params).await?;
        let row = Self::first_row_obj(&result);

        Ok(ThreadInfo {
            thread_id: thread_id.to_string(),
            title: Self::row_val_str(&row, "title", "?"),
            description: Self::row_val_str(&row, "description", ""),
            project: Self::row_val_str_opt(&row, "project"),
            status: "closed".into(),
            sessions: vec![],
            decisions: vec![],
            event_count: 0,
            created_at: Self::row_val_str(&row, "created_at", "?"),
            updated_at: now,
            closed_at: Some(Self::now_iso()),
        })
    }

    // ── Action parser ────────────────────────────────────────────────────

    fn parse_action(request: &ThreadRequest) -> Result<ThreadAction, DtError> {
        match request.action.as_str() {
            "create" => Ok(ThreadAction::Create {
                title: request.title.clone().unwrap_or_default(),
                description: request.description.clone(),
                project: request.project.clone(),
            }),
            "add_session" => Ok(ThreadAction::AddSession {
                thread_id: request.thread_id.clone().unwrap_or_default(),
                session_id: request.session_id.clone().unwrap_or_default(),
                summary: request.summary.clone(),
            }),
            "add_decision" => Ok(ThreadAction::AddDecision {
                thread_id: request.thread_id.clone().unwrap_or_default(),
                decision: request.decision.clone().unwrap_or_default(),
                reason: request.reason.clone(),
                impact: request.impact.clone(),
            }),
            "get" => Ok(ThreadAction::Get {
                thread_id: request.thread_id.clone().unwrap_or_default(),
            }),
            "list" => Ok(ThreadAction::List {
                project: request.project.clone(),
                limit: request.limit,
            }),
            "close" => Ok(ThreadAction::Close {
                thread_id: request.thread_id.clone().unwrap_or_default(),
                outcome: request.outcome.clone(),
            }),
            other => Err(DtError::General(format!("Unknown action: {other}"))),
        }
    }
}

#[async_trait::async_trait]
impl ThreadTrait for ThreadService {
    async fn execute(&self, request: &ThreadRequest) -> Result<ThreadResponse, DtError> {
        let action = Self::parse_action(request)?;
        let action_name = request.action.clone();

        match action {
            ThreadAction::Create {
                title,
                description,
                project,
            } => {
                let thread = self
                    .create_thread(&title, description.as_deref(), project.as_deref())
                    .await?;
                let msg = format!("Thread '{}' created", thread.title);
                Ok(ThreadResponse {
                    action: action_name,
                    thread: Some(thread),
                    list: None,
                    message: msg,
                })
            }
            ThreadAction::AddSession {
                thread_id,
                session_id,
                summary,
            } => {
                let thread = self
                    .add_session(&thread_id, &session_id, summary.as_deref())
                    .await?;
                let msg = format!("Session '{}' added to thread", session_id);
                Ok(ThreadResponse {
                    action: action_name,
                    thread: Some(thread),
                    list: None,
                    message: msg,
                })
            }
            ThreadAction::AddDecision {
                thread_id,
                decision,
                reason,
                impact,
            } => {
                let thread = self
                    .add_decision(&thread_id, &decision, reason.as_deref(), impact.as_deref())
                    .await?;
                Ok(ThreadResponse {
                    action: action_name,
                    thread: Some(thread),
                    list: None,
                    message: "Decision recorded in thread".into(),
                })
            }
            ThreadAction::Get { thread_id } => {
                let thread = self.get_thread(&thread_id).await?;
                let title = thread.title.clone();
                Ok(ThreadResponse {
                    action: action_name,
                    thread: Some(thread),
                    list: None,
                    message: format!("Thread '{}' retrieved", title),
                })
            }
            ThreadAction::List { project, limit } => {
                let list = self
                    .list_threads(project.as_deref(), limit.unwrap_or(20))
                    .await?;
                let total = list.total;
                Ok(ThreadResponse {
                    action: action_name,
                    thread: None,
                    list: Some(list),
                    message: format!("Found {} threads", total),
                })
            }
            ThreadAction::Close { thread_id, outcome } => {
                let thread = self.close_thread(&thread_id, outcome.as_deref()).await?;
                Ok(ThreadResponse {
                    action: action_name,
                    thread: Some(thread),
                    list: None,
                    message: "Thread closed".into(),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_info_construction() {
        let info = ThreadInfo {
            thread_id: "4:abc".into(),
            title: "Payment Migration".into(),
            description: "Migrating payment platform".into(),
            project: Some("digital-twin-v2".into()),
            status: "active".into(),
            sessions: vec![],
            decisions: vec![],
            event_count: 0,
            created_at: "2026-07-01T00:00:00Z".into(),
            updated_at: "2026-07-10T00:00:00Z".into(),
            closed_at: None,
        };
        assert_eq!(info.title, "Payment Migration");
        assert_eq!(info.status, "active");
    }

    #[test]
    fn thread_session_construction() {
        let session = ThreadSession {
            session_id: "2026-07-10-001".into(),
            summary: "Fixed payment timeout".into(),
            timestamp: "2026-07-10T12:00:00Z".into(),
        };
        assert_eq!(session.session_id, "2026-07-10-001");
    }

    #[test]
    fn thread_decision_construction() {
        let dec = ThreadDecision {
            decision: "Use Agento payment gateway".into(),
            reason: Some("Better rate limits".into()),
            impact: Some("Payment flow changes".into()),
            timestamp: "2026-07-10T13:00:00Z".into(),
        };
        assert_eq!(dec.decision, "Use Agento payment gateway");
        assert!(dec.reason.is_some());
    }

    #[test]
    fn thread_response_serialization() {
        let resp = ThreadResponse {
            action: "create".into(),
            thread: Some(ThreadInfo {
                thread_id: "4:x".into(),
                title: "Test".into(),
                description: "desc".into(),
                project: Some("dt".into()),
                status: "active".into(),
                sessions: vec![],
                decisions: vec![],
                event_count: 0,
                created_at: "now".into(),
                updated_at: "now".into(),
                closed_at: None,
            }),
            list: None,
            message: "Thread 'Test' created".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("create"));
        assert!(json.contains("Test"));
    }

    #[test]
    fn parse_action_create() {
        let req = ThreadRequest {
            action: "create".into(),
            title: Some("My Thread".into()),
            description: Some("desc".into()),
            project: Some("dt".into()),
            thread_id: None,
            session_id: None,
            summary: None,
            decision: None,
            reason: None,
            impact: None,
            outcome: None,
            limit: None,
        };
        let action = ThreadService::parse_action(&req).unwrap();
        match action {
            ThreadAction::Create { title, .. } => assert_eq!(title, "My Thread"),
            _ => panic!("Wrong action type"),
        }
    }

    #[test]
    fn parse_action_unknown() {
        let req = ThreadRequest {
            action: "invalid".into(),
            title: None,
            description: None,
            project: None,
            thread_id: None,
            session_id: None,
            summary: None,
            decision: None,
            reason: None,
            impact: None,
            outcome: None,
            limit: None,
        };
        let result = ThreadService::parse_action(&req);
        assert!(result.is_err());
    }

    #[test]
    fn thread_list_result() {
        let list = ThreadListResult {
            threads: vec![],
            total: 0,
        };
        assert_eq!(list.total, 0);
    }

    #[test]
    fn row_val_str_extracts() {
        let row = Some(serde_json::json!({
            "title": "MyTitle",
            "description": "Description",
            "status": "active"
        }));
        assert_eq!(
            ThreadService::row_val_str(&row, "title", "fallback"),
            "MyTitle"
        );
        assert_eq!(
            ThreadService::row_val_str(&row, "description", "fallback"),
            "Description"
        );
        assert_eq!(
            ThreadService::row_val_str(&row, "missing", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn row_val_str_opt_extracts() {
        let row = Some(serde_json::json!({
            "project": "hello"
        }));
        assert_eq!(
            ThreadService::row_val_str_opt(&row, "project"),
            Some("hello".into())
        );
        assert_eq!(ThreadService::row_val_str_opt(&row, "missing"), None);
    }

    #[test]
    fn first_row_obj_empty() {
        let raw = serde_json::json!({"results": [{"data": []}]});
        assert!(ThreadService::first_row_obj(&raw).is_none());
    }

    #[test]
    fn first_row_obj_driver_format() {
        let raw = serde_json::json!([
            {"id": "4:abc123", "title": "Test Thread"}
        ]);
        let result = ThreadService::first_row_obj(&raw);
        assert!(result.is_some());
        let obj = result.unwrap();
        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("4:abc123"));
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("Test Thread")
        );
    }
}
