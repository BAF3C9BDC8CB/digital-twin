//! **ContextService** — high-level service for building context.
//!
//! Wraps a [`ContextPipeline`] and provides a simplified API for consuming
//! code (CLI, gRPC handlers, etc.).

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

use super::models::{AggregatedContext, ContextOptions};
use super::pipeline::ContextPipeline;

/// Entry point for building aggregated context.
///
/// Holds references to the three backend services and delegates to the
/// pipeline for execution.
pub struct ContextService {
    pipeline: ContextPipeline,
}

impl ContextService {
    /// Create a new service with the given backends.
    ///
    /// # Arguments
    /// * `graph` — graph repository for Reality/Knowledge/Memory/Reasoning worlds.
    /// * `vector` — Qdrant repository for the Semantic world.
    /// * `embed` — Embedding service for generating query vectors.
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self {
            pipeline: ContextPipeline::default_with_backends(graph, vector, embed),
        }
    }

    /// Create a service with no backends (for testing).
    pub fn empty() -> Self {
        Self {
            pipeline: ContextPipeline::default(),
        }
    }

    /// Build context for a given task or question.
    ///
    /// Equivalent to calling `pipeline.execute(task, &ContextOptions::default())`.
    pub async fn build(&self, task: &str) -> Result<AggregatedContext, DtError> {
        self.build_with_options(task, &ContextOptions::default())
            .await
    }

    /// Build context with custom options.
    ///
    /// This is the primary API.  The task is any natural-language description
    /// of what the caller is trying to do (e.g. "Fix the payment timeout bug").
    pub async fn build_with_options(
        &self,
        task: &str,
        options: &ContextOptions,
    ) -> Result<AggregatedContext, DtError> {
        self.pipeline.execute(task, options).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::ContextOptions;

    #[tokio::test]
    async fn service_build_empty() {
        let svc = ContextService::empty();
        let ctx = svc.build("test").await.expect("build should succeed");
        assert_eq!(ctx.reality.count, 0);
    }

    #[tokio::test]
    async fn service_build_with_options() {
        let svc = ContextService::empty();
        let opts = ContextOptions {
            worlds: Some(vec!["reality".into(), "knowledge".into()]),
            max_tokens: Some(1024),
            ..Default::default()
        };
        let ctx = svc
            .build_with_options("specific task", &opts)
            .await
            .expect("build should succeed");
        assert_eq!(ctx.reality.count, 0);
    }
}
