//! Pipeline stages for the Context Builder Chain of Responsibility.
//!
//! Each stage implements [`ContextStage`] and transforms the
//! [`ContextState`](super::models::ContextState) in sequence.

mod retriever;
mod ranker;
mod dedup;
mod resolver;
mod summarizer;

pub use retriever::RetrieverStage;
pub use ranker::RankerStage;
pub use dedup::DedupStage;
pub use resolver::ResolverStage;
pub use summarizer::SummarizerStage;

use async_trait::async_trait;
use crate::application::context::models::ContextState;
use crate::domain::error::DtError;

/// A single stage in the Context Pipeline.
///
/// Each stage receives the current [`ContextState`], performs its
/// transformation, and returns the next state (or an error).
#[async_trait]
pub trait ContextStage: Send + Sync {
    /// Human-readable name for this stage (logging / debugging).
    fn name(&self) -> &str;

    /// Process the state and return the transformed state.
    async fn process(&self, state: ContextState) -> Result<ContextState, DtError>;
}
