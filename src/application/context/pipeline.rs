//! **ContextPipeline** — Chain of Responsibility for context building.
//!
//! Executes a sequence of [`ContextStage`] implementations to transform raw
//! retrieval results into a compact, ranked, deduplicated, and conflict-free
//! [`AggregatedContext`].
//!
//! # Example
//!
//! ```ignore
//! let pipeline = ContextPipeline::default();
//! let ctx = pipeline.execute("How to fix payment timeout?", &ContextOptions::default()).await?;
//! for item in &ctx.reality.items {
//!     println!("{}: {}", item.label, item.content);
//! }
//! ```

use super::models::{AggregatedContext, ContextOptions, ContextState};
use super::stages::{
    ContextStage, DedupStage, RankerStage, ResolverStage, RetrieverStage, SummarizerStage,
};
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use std::sync::Arc;

/// The Context Builder pipeline.
///
/// Holds an ordered list of stages and executes them in sequence on a
/// [`ContextState`] that flows through the pipeline.
pub struct ContextPipeline {
    stages: Vec<Box<dyn ContextStage>>,
}

impl Default for ContextPipeline {
    fn default() -> Self {
        Self::default_pipeline()
    }
}

impl ContextPipeline {
    /// Create a pipeline with custom stages.
    pub fn new(stages: Vec<Box<dyn ContextStage>>) -> Self {
        Self { stages }
    }

    /// Create the default pipeline with all five stages wired to backends.
    ///
    /// Stages execute in order: Retriever → Ranker → Dedup → Resolver → Summarizer.
    pub fn default_with_backends(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self {
            stages: vec![
                Box::new(RetrieverStage::new(Some(graph), Some(vector), Some(embed))),
                Box::new(RankerStage),
                Box::new(DedupStage),
                Box::new(ResolverStage),
                Box::new(SummarizerStage),
            ],
        }
    }

    /// Create a pipeline with no backends (for testing or when backends are
    /// unavailable).  The retriever will return empty results.
    fn default_pipeline() -> Self {
        Self {
            stages: vec![
                Box::new(RetrieverStage::empty()),
                Box::new(RankerStage),
                Box::new(DedupStage),
                Box::new(ResolverStage),
                Box::new(SummarizerStage),
            ],
        }
    }

    /// Replace the built-in RetrieverStage with a custom one (e.g. wired to
    /// real Memgraph / Qdrant backends).  The new stage is placed at index 0.
    ///
    /// This is the preferred way to inject live backends without changing the
    /// rest of the pipeline.
    pub fn with_retriever(mut self, stage: RetrieverStage) -> Self {
        self.stages[0] = Box::new(stage);
        self
    }

    /// Execute the full pipeline for a given task and options.
    ///
    /// This is the main entry point.  State flows through each stage
    /// sequentially and is finally consumed to produce an [`AggregatedContext`].
    pub async fn execute(
        &self,
        task: &str,
        options: &ContextOptions,
    ) -> Result<AggregatedContext, DtError> {
        let mut state = ContextState::new(task, options);

        for stage in &self.stages {
            let stage_name = stage.name();
            tracing::debug!("[pipeline] entering stage: {stage_name}");
            state = stage
                .process(state)
                .await
                .map_err(|e| DtError::General(format!("stage '{stage_name}' failed: {e}")))?;
            tracing::debug!("[pipeline] stage '{stage_name}' complete");
        }

        Ok(state.into_aggregated())
    }

    /// Returns the number of stages in this pipeline.
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::ContextOptions;

    #[test]
    fn default_pipeline_has_five_stages() {
        let pipeline = ContextPipeline::default();
        assert_eq!(pipeline.stage_count(), 5);
    }

    #[test]
    fn pipeline_can_be_constructed_with_custom_stages() {
        // Empty pipeline
        let pipeline = ContextPipeline::new(Vec::new());
        assert_eq!(pipeline.stage_count(), 0);
    }

    #[tokio::test]
    async fn pipeline_execute_empty_backends() {
        let pipeline = ContextPipeline::default();
        let result = pipeline
            .execute("test task", &ContextOptions::default())
            .await
            .expect("pipeline should succeed");

        // All worlds should be empty (no backends)
        assert!(result.reality.items.is_empty());
        assert!(result.knowledge.items.is_empty());
        assert!(result.memory.items.is_empty());
        assert!(result.semantic.items.is_empty());
        assert!(result.runtime.items.is_empty());
        assert!(result.reasoning.items.is_empty());
        // Low-coverage alert from resolver
        assert_eq!(result.alerts.len(), 1);
    }

    #[tokio::test]
    async fn pipeline_execute_with_world_filter() {
        let pipeline = ContextPipeline::default();
        let opts = ContextOptions {
            worlds: Some(vec!["reality".into()]),
            ..Default::default()
        };

        let result = pipeline
            .execute("test task", &opts)
            .await
            .expect("pipeline should succeed");

        // Only reality was queried; other worlds stay empty
        // (all empty since no backends, but code path is exercised)
        assert!(result.reality.items.is_empty());
    }
}
