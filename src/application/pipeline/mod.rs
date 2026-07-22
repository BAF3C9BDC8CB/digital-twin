//! Pipeline engine — multi-stage file processing framework.
//!
//! The pipeline module provides the core abstractions for building a
//! configurable chain of file processors:
//!
//! - [`Processor`](processor::Processor) — trait for each stage
//! - [`ProcessorOutput`](output::ProcessorOutput) — flexible JSON output container
//! - [`PipelineContext`](context::PipelineContext) — shared state passed between stages
//!
//! # Architecture
//!
//! ```text
//!                             PipelineContext
//!     ┌───────────────────────────────────────────────────┐
//!     │  file_path │ file_text │ project_name │ outputs   │
//!     └──────┬────────────────────────────────────────────┘
//!            │
//!     ┌──────▼──────┐   ┌──────────▼───────┐   ┌─────────▼───────┐
//!     │ Processor A  │──▶│  Processor B     │──▶│  Processor C    │
//!     │ (lang detect)│   │ (tree-sitter)    │   │  (embedding)    │
//!     └──────┬───────┘   └──────────┬───────┘   └─────────────────┘
//!            │                      │
//!     ┌──────▼──────────────────────▼──────┐
//!     │        ProcessorOutput map         │
//!     │  { "lang_detector": {..},          │
//!     │    "tree_sitter":   {..} }        │
//!     └────────────────────────────────────┘
//! ```
//!
//! Processors declare a `priority` (lower runs first) and a `matches` guard
//! so that the pipeline runner can cheaply skip irrelevant stages.  Each
//! processor writes its results into a [`ProcessorOutput`] which is then
//! inserted into the shared context under the processor's `name`.

pub mod config;
pub mod context;
pub mod engine;
pub mod infer_client;
pub mod output;
pub mod processor;
pub mod processors;
pub mod prompt;
pub mod registry;

pub use config::PipelineConfig;
pub use context::PipelineContext;
pub use engine::ProcessorEngine;
pub use infer_client::InferClient;
pub use output::ProcessorOutput;
pub use processor::Processor;
pub use prompt::PromptRegistry;
pub use registry::ProcessorRegistry;
