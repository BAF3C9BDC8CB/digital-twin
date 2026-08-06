//! 流水线处理器实现。
//!
//! 每个模块将一个既有基础设施组件包装为
//! [`Processor`](super::processor::Processor)，使流水线引擎可以将其
//! 组合成可配置的处理链。
//!
//! # 处理顺序（按优先级）
//!
//! | 优先级 | 处理器           | 职责                       |
//! |--------|------------------|----------------------------|
//! | 100    | `TreeSitter`     | AST 解析（代码文件）       |
//! | 90     | `Chunk`          | 文本分块（文档文件）       |
//! | 60     | `LlmClient`      | LLM 分析                   |
//! | 10     | `Store`          | 持久化到图 + 向量数据库    |

pub mod chunk;
pub mod llm_client;
pub mod store;
pub mod tree_sitter;

pub use chunk::ChunkProcessor;
pub use llm_client::LlmClientProcessor;
pub use store::StoreProcessor;
pub use tree_sitter::TreeSitterProcessor;
