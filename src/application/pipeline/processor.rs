//! [`Processor`] trait — the contract every pipeline stage must implement.
//!
//! A processor is responsible for one stage of file analysis (e.g. language
//! detection, tree-sitter parsing, NLP analysis, embedding).  Processors
//! declare what files they can handle via [`Processor::matches`] and produce
//! a [`ProcessorOutput`] struct that gets merged into the shared
//! [`PipelineContext`](super::context::PipelineContext).

use async_trait::async_trait;
use std::path::Path;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::domain::error::DtError;

/// A single stage in the pipeline processing chain.
///
/// # Ordering
///
/// Processors are sorted by [`priority`](Processor::priority) (lower values
/// run first) so that cheap "gatekeeper" checks (language detection,
/// file-type filtering) happen before expensive analysis.
///
/// # Thread safety
///
/// All trait methods are `&self` and require `Send + Sync` so that a
/// pipeline runner can execute independent processors in parallel when
/// there are no data dependencies.
#[async_trait]
pub trait Processor: Send + Sync {
    /// Human-readable name of this processor (e.g. `"tree_sitter"`,
    /// `"hanlp"`, `"code_embedder"`).
    ///
    /// This name is used as the key under which the processor stores its
    /// output in the [`PipelineContext`].
    fn name(&self) -> &str;

    /// Relative execution priority.  Lower values run first.
    ///
    /// Typical conventions:
    /// - `0–99`   — file-type / language detection
    /// - `100–199` — structural parsers (tree-sitter)
    /// - `200–299` — NLP / semantic analyzers
    /// - `300+`   — embedding / vectorization (often depends on prior stages)
    fn priority(&self) -> i32;

    /// Return `true` if this processor can handle the given file.
    ///
    /// This is checked *before* [`execute`](Processor::execute) so the
    /// pipeline can skip unsuitable processors cheaply.
    fn matches(&self, file_path: &Path) -> bool;

    /// Execute the processor against the shared [`PipelineContext`].
    ///
    /// The context carries the raw file content as well as outputs from
    /// earlier stages, enabling downstream processors to build on upstream
    /// results.
    ///
    /// # Errors
    ///
    /// Returns [`DtError`] on failure.  The pipeline runner may decide to
    /// skip or abort depending on the error severity.
    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::context::PipelineContext;
    use std::path::PathBuf;

    struct DummyProcessor;

    #[async_trait]
    impl Processor for DummyProcessor {
        fn name(&self) -> &str {
            "dummy"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn matches(&self, file_path: &Path) -> bool {
            file_path.extension().and_then(|e| e.to_str()) == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            out.set("file_name", ctx.file_path.to_string_lossy().to_string());
            out.set("project", ctx.project_name.clone());
            Ok(out)
        }
    }

    #[test]
    fn processor_basics() {
        let p = DummyProcessor;
        assert_eq!(p.name(), "dummy");
        assert_eq!(p.priority(), 100);
        assert!(p.matches(Path::new("main.rs")));
        assert!(!p.matches(Path::new("main.py")));
    }

    #[tokio::test]
    async fn processor_execute() {
        let p = DummyProcessor;
        let ctx = PipelineContext::new(
            PathBuf::from("main.rs"),
            "fn main() {}".to_string(),
            "my_project".to_string(),
        );
        let out = p.execute(&ctx).await.unwrap();
        let file_name = out.get("file_name").and_then(|v| v.as_str()).unwrap();
        assert!(file_name.ends_with("main.rs"));
        assert_eq!(
            out.get("project").and_then(|v| v.as_str()),
            Some("my_project")
        );
    }
}
