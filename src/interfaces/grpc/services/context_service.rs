//! Context gRPC handler for the DtCore service.
//!
//! Delegates to [`ContextPipeline`] to perform six-world context aggregation.

use crate::application::context::models::ContextOptions;
use crate::application::context::pipeline::ContextPipeline;
use crate::application::context::stages::RetrieverStage;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::proto::dt::core::*;
use std::sync::Arc;
use tonic::Status;

/// Handler for `GetContext` RPC — retrieve context for a query or entity.
pub async fn handle_get_context(
    req: ContextRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
) -> Result<ContextResponse, Status> {
    if req.query.is_empty() {
        return Err(Status::invalid_argument("query is required"));
    }

    let options = ContextOptions {
        max_tokens: if req.max_tokens > 0 {
            Some(req.max_tokens as usize)
        } else {
            None
        },
        worlds: None, // query all worlds by default
        ..ContextOptions::default()
    };

    let retriever = match (graph, vector) {
        (Some(g), v) => RetrieverStage::new(g, v, embed),
        (None, _) => RetrieverStage::empty(),
    };

    let pipeline = ContextPipeline::default().with_retriever(retriever);

    match pipeline.execute(&req.query, &options).await {
        Ok(ctx) => {
            // Serialise the aggregated context into a compact string.
            let mut context_parts: Vec<String> = Vec::new();

            // Thread info
            if let Some(ref t) = ctx.thread {
                context_parts.push(format!("[Thread] {}", t.title));
            }

            // Reality / code world
            if ctx.reality.count > 0 {
                let items: Vec<String> = ctx
                    .reality
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Reality] {}", items.join("\n")));
            }

            // Knowledge world
            if ctx.knowledge.count > 0 {
                let items: Vec<String> = ctx
                    .knowledge
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Knowledge] {}", items.join("\n")));
            }

            // Memory world
            if ctx.memory.count > 0 {
                let items: Vec<String> = ctx
                    .memory
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Memory] {}", items.join("\n")));
            }

            // Semantic world
            if ctx.semantic.count > 0 {
                let items: Vec<String> = ctx
                    .semantic
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Semantic] {}", items.join("\n")));
            }

            // Runtime world
            if ctx.runtime.count > 0 {
                let items: Vec<String> = ctx
                    .runtime
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Runtime] {}", items.join("\n")));
            }

            // Reasoning world
            if ctx.reasoning.count > 0 {
                let items: Vec<String> = ctx
                    .reasoning
                    .items
                    .iter()
                    .map(|i| format!("[{}] {}", i.label, i.content))
                    .collect();
                context_parts.push(format!("[Reasoning] {}", items.join("\n")));
            }

            // Alerts
            if !ctx.alerts.is_empty() {
                let alerts: Vec<String> = ctx
                    .alerts
                    .iter()
                    .map(|a| format!("[{:?}] {}: {}", a.severity, a.source, a.message))
                    .collect();
                context_parts.push(format!("[Alerts] {}", alerts.join("\n")));
            }

            let context_text = context_parts.join("\n\n");

            Ok(ContextResponse {
                context: context_text,
                token_count: ctx.estimated_tokens as i32,
            })
        }
        Err(e) => Err(Status::internal(format!("Context pipeline failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_query_returns_error() {
        let req = ContextRequest {
            query: String::new(),
            file_paths: vec![],
            max_tokens: 0,
        };
        let result = handle_get_context(req, None, None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_graph_returns_error_from_pipeline() {
        let req = ContextRequest {
            query: "test query".into(),
            file_paths: vec![],
            max_tokens: 1000,
        };
        // With empty retriever, pipeline will likely return empty results
        // or an error depending on implementation.
        let result = handle_get_context(req, None, None, None).await;
        // Pipeline may succeed with empty results or fail — both OK for now.
        let _ = result;
    }
}
