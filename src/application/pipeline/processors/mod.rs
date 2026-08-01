//! Pipeline processor implementations.
//!
//! Each module wraps an existing infrastructure component into a
//! [`Processor`](super::processor::Processor) so that the pipeline engine
//! can compose them into a configurable chain.
//!
//! # Processing order (by priority)
//!
//! | Priority | Processor        | Responsibility                |
//! |----------|------------------|------------------------------|
//! | 100      | `TreeSitter`     | AST parsing (code files)     |
//! | 90       | `Chunk`          | Text chunking (doc files)    |
//! | 80       | `HanlpClient`    | NLP analysis (placeholder)   |
//! | 60       | `LlmClient`      | LLM analysis                  |
//! | 10       | `Store`          | Persist to graph + vector DB  |

pub mod chunk;
pub mod hanlp_client;
pub mod llm_client;
pub mod store;
pub mod tree_sitter;

pub use chunk::ChunkProcessor;
pub use hanlp_client::HanlpClientProcessor;
pub use llm_client::LlmClientProcessor;
pub use store::StoreProcessor;
pub use tree_sitter::TreeSitterProcessor;
