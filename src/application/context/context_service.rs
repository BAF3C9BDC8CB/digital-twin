//! **ContextServiceImpl** — MCP-tool wrapper around ContextPipeline (4.2).
//!
//! Receives a task from the MCP layer, runs the full context-building pipeline,
//! and returns the [`AggregatedContext`] as JSON.
//!
//! # MCP tool: `dt_context`
//!
//! ```text
//! dt_context(task: str, worlds?: list[str], max_tokens?: int, thread_id?: str)
//!   → AggregatedContext JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

use super::models::{AggregatedContext, ContextOptions};
use super::service::ContextService;

/// MCP tool request for dt_context.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextRequest {
    /// Natural-language task or question.
    pub task: String,
    /// Which worlds to include.  `None` → all six worlds.
    pub worlds: Option<Vec<String>>,
    /// Token budget override.
    pub max_tokens: Option<usize>,
    /// Optional Digital Thread ID.
    pub thread_id: Option<String>,
    /// Minimum relevance score [0.0, 1.0].
    pub min_score: Option<f64>,
    /// Maximum items per world.
    pub max_items_per_world: Option<usize>,
}

/// MCP tool response for dt_context.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextResponse {
    /// The fully aggregated context.
    pub context: AggregatedContext,
    /// Number of items retrieved (before ranking).
    pub raw_count: usize,
    /// Number of items retained (after pipeline).
    pub retained_count: usize,
    /// Pipeline timing in milliseconds.
    pub elapsed_ms: u64,
}

/// MCP-facing wrapper around the context pipeline.
///
/// Converts MCP tool parameters into pipeline options, runs the pipeline,
/// and returns a structured response.
pub struct ContextServiceImpl {
    inner: ContextService,
}

impl ContextServiceImpl {
    /// Create from existing backends.
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self {
            inner: ContextService::new(graph, vector, embed),
        }
    }

    /// Create with no backends (for testing).
    pub fn empty() -> Self {
        Self {
            inner: ContextService::empty(),
        }
    }

    /// Build context for the given MCP request.
    ///
    /// Times the pipeline execution and returns both the aggregated context
    /// and some metadata (raw count, retained count, elapsed time).
    pub async fn build_context(&self, request: &ContextRequest) -> Result<ContextResponse, DtError> {
        let start = std::time::Instant::now();

        let options = ContextOptions {
            worlds: request.worlds.clone(),
            max_tokens: request.max_tokens.or(Some(4096)),
            thread_id: request.thread_id.clone(),
            min_score: request.min_score.or(Some(0.5)),
            max_items_per_world: request.max_items_per_world.or(Some(20)),
        };

        let context = self
            .inner
            .build_with_options(&request.task, &options)
            .await?;

        let elapsed = start.elapsed();

        // Count total retained items
        let retained = [
            &context.reality,
            &context.knowledge,
            &context.memory,
            &context.semantic,
            &context.runtime,
            &context.reasoning,
        ]
        .iter()
        .map(|s| s.count)
        .sum();

        Ok(ContextResponse {
            context,
            raw_count: retained,      // raw ≈ retained after empty pipeline
            retained_count: retained,
            elapsed_ms: elapsed.as_millis() as u64,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn context_service_impl_empty() {
        let svc = ContextServiceImpl::empty();
        let req = ContextRequest {
            task: "test task".into(),
            worlds: None,
            max_tokens: Some(1024),
            thread_id: None,
            min_score: None,
            max_items_per_world: None,
        };
        let resp = svc
            .build_context(&req)
            .await
            .expect("build_context should succeed");
        assert_eq!(resp.retained_count, 0);
        assert!(resp.elapsed_ms < 1000);
        assert_eq!(resp.context.alerts.len(), 1); // low-coverage alert
    }

    #[tokio::test]
    async fn context_request_round_trip() {
        let req = ContextRequest {
            task: "how to fix payment timeout".into(),
            worlds: Some(vec!["reality".into(), "knowledge".into()]),
            max_tokens: Some(2048),
            thread_id: Some("thread-42".into()),
            min_score: Some(0.6),
            max_items_per_world: Some(10),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ContextRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task, req.task);
        assert_eq!(back.worlds, req.worlds);
        assert_eq!(back.thread_id, req.thread_id);
    }
}
