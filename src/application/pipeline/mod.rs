//! 流水线引擎——多阶段文件处理框架。
//!
//! 流水线模块为构建可配置的文件处理器链提供核心抽象：
//!
//! - [`Processor`](processor::Processor) —— 每个阶段的 trait
//! - [`ProcessorOutput`](output::ProcessorOutput) —— 灵活的 JSON 输出容器
//! - [`PipelineContext`](context::PipelineContext) —— 阶段间传递的共享状态
//!
//! # 架构
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
//! 处理器声明 `priority`（越低越先执行）与 `matches` 守卫，使流水线运行器
//! 可以廉价地跳过无关阶段。每个处理器将其结果写入 [`ProcessorOutput`]，
//! 随后按处理器 `name` 插入到共享上下文中。

pub mod config;
pub mod context;
pub mod engine;
pub mod infer_client;
pub mod output;
pub mod processor;
pub mod processors;
pub mod prompt;
pub mod registry;
pub mod test;
pub mod virtual_file;

pub use config::PipelineConfig;
pub use context::PipelineContext;
pub use engine::{EcosystemAnalysis, ProcessorEngine, ProjectAnalysis, ServiceDependency};
pub use infer_client::SiliconFlowChatClient;
pub use output::ProcessorOutput;
pub use processor::Processor;
pub use prompt::PromptRegistry;
pub use registry::ProcessorRegistry;
pub use virtual_file::{FileSourceKind, VirtualFile};
