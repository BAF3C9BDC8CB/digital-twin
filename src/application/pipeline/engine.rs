//! Pipeline orchestration engine.
//!
//! [`ProcessorEngine`] ties together the [`ProcessorRegistry`] and the
//! [`Processor`] trait to analyse source files through a configurable
//! processing pipeline.
//!
//! # Execution model
//!
//! Processors are split into two categories:
//!
//! * **CPU-bound stages** (priority ≥ [`CPU_PRIORITY_THRESHOLD`]) — cheap
//!   operations such as language detection, tree‑sitter parsing, and text
//!   chunking.  These run in full parallel across files.
//!
//! * **GPU-bound stages** (priority < [`CPU_PRIORITY_THRESHOLD`]) —
//!   operations that hit the inference server, such as LLM chat completions,
//!   HanLP NLP analysis, and embedding.  Concurrency is capped by a
//!   [`Semaphore`] to avoid overwhelming the GPU server.

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::application::pipeline::registry::ProcessorRegistry;
use futures::stream::{self, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Priority threshold separating CPU-bound stages from GPU-bound stages.
///
/// Processors with priority **≥** this value are considered CPU-bound and can
/// run with unrestricted parallelism.  Processors with priority **<** this
/// value are GPU-bound and use a semaphore to cap concurrency.
///
/// Conventions in the existing codebase:
/// - tree_sitter = 100, chunk = 90 — CPU-bound
/// - hanlp = 80, llm = 60 — GPU-bound
pub const CPU_PRIORITY_THRESHOLD: i32 = 85;

/// Default parallelism for CPU stages when `std::thread::available_parallelism`
/// cannot be determined.
const DEFAULT_CPU_CONCURRENCY: usize = 8;

// ---------------------------------------------------------------------------
// ProcessorEngine
// ---------------------------------------------------------------------------

/// Orchestrator that runs all matching processors against one or more files.
pub struct ProcessorEngine {
    /// Shared processor registry.
    registry: Arc<ProcessorRegistry>,
    /// Maximum number of in-flight GPU-bound operations.
    max_concurrent: usize,
}

/// Aggregated result from processing a single file through the pipeline.
#[derive(Debug)]
pub struct FileAnalysis {
    /// Path of the analysed file.
    pub file_path: PathBuf,
    /// Whether *all* processors completed without error for this file.
    pub success: bool,
    /// Per-processor error messages (if any).
    pub errors: Vec<String>,
    /// Accumulated pipeline context carrying every processor's output.
    pub context: PipelineContext,
}

impl ProcessorEngine {
    /// Create a new engine from a shared registry.
    ///
    /// `max_concurrent` controls the maximum number of GPU-bound processor
    /// invocations that may run simultaneously (used by [`run_gpu_stages`]).
    pub fn new(registry: Arc<ProcessorRegistry>, max_concurrent: usize) -> Self {
        Self {
            registry,
            max_concurrent,
        }
    }

    // ------------------------------------------------------------------
    // Single-file analysis
    // ------------------------------------------------------------------

    /// Process a single file through all matching processors.
    ///
    /// CPU-bound processors (priority ≥ [`CPU_PRIORITY_THRESHOLD`]) run first
    /// so their outputs are available to downstream GPU-bound stages.  Within
    /// each group, processors execute in priority order (lowest first, most
    /// important last).
    ///
    /// If a processor fails, a warning is logged and processing continues
    /// with the next matching processor.  The returned [`FileAnalysis`]
    /// contains all accumulated outputs and any error messages.
    pub async fn analyze_file(
        &self,
        file_path: impl Into<PathBuf>,
        file_text: String,
        project_name: String,
    ) -> FileAnalysis {
        let file_path_buf: PathBuf = file_path.into();
        let mut ctx = PipelineContext::new(file_path_buf.clone(), file_text, project_name);

        let matching = self.registry.matching(&file_path_buf);
        let mut errors: Vec<String> = Vec::new();

        // Phase 1: CPU-bound stages (priority >= threshold).
        for &processor in matching.iter().filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD) {
            Self::run_processor(processor, &mut ctx, &mut errors, &file_path_buf).await;
        }

        // Phase 2: GPU-bound stages (priority < threshold).
        for &processor in matching.iter().filter(|p| p.priority() < CPU_PRIORITY_THRESHOLD) {
            Self::run_processor(processor, &mut ctx, &mut errors, &file_path_buf).await;
        }

        FileAnalysis {
            file_path: file_path_buf,
            success: errors.is_empty(),
            errors,
            context: ctx,
        }
    }

    // ------------------------------------------------------------------
    // Batch analysis
    // ------------------------------------------------------------------

    /// Process multiple files in parallel.
    ///
    /// 1. **CPU stages** run first on all files with full parallelism
    ///    (bounded by `available_parallelism`).
    /// 2. **GPU stages** run on the resulting contexts with concurrency
    ///    limited to `max_concurrent` via an internal semaphore.
    pub async fn analyze_batch(
        &self,
        files: Vec<(PathBuf, String)>,
        project_name: String,
    ) -> Vec<FileAnalysis> {
        // Phase 1: CPU-bound processors on all files in parallel.
        let cpu_results = self.run_cpu_stages(files, project_name).await;

        // Phase 2: GPU-bound processors on the resulting analyses.
        self.run_gpu_stages(cpu_results).await
    }

    // ------------------------------------------------------------------
    // CPU stages
    // ------------------------------------------------------------------

    /// Run CPU-bound processors (priority ≥ [`CPU_PRIORITY_THRESHOLD`]) on
    /// all files in parallel.
    ///
    /// Concurrency is set to the number of available CPU cores so that
    /// processor‑intensive work saturates all cores without over‑subscribing.
    pub async fn run_cpu_stages(
        &self,
        files: Vec<(PathBuf, String)>,
        project_name: String,
    ) -> Vec<FileAnalysis> {
        let concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(DEFAULT_CPU_CONCURRENCY);

        let registry = Arc::clone(&self.registry);
        let project_name = Arc::new(project_name);

        stream::iter(files.into_iter().map(move |(path, text)| {
            let registry = Arc::clone(&registry);
            let proj = Arc::clone(&project_name);
            let path_clone = path.clone();

            async move {
                let mut ctx = PipelineContext::new(path_clone, text, (*proj).clone());
                let mut errors: Vec<String> = Vec::new();

                // Collect CPU-bound processors that match this file.
                let cpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| {
                        p.priority() >= CPU_PRIORITY_THRESHOLD && p.matches(&path)
                    })
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &cpu_processors {
                    match processor.execute(&ctx).await {
                        Ok(output) => {
                            ctx.add_output(processor.name(), output);
                        }
                        Err(e) => {
                            tracing::warn!(
                                processor = processor.name(),
                                path = %path.display(),
                                error = %e,
                                "CPU processor failed"
                            );
                            errors.push(format!("{}: {}", processor.name(), e));
                        }
                    }
                }

                FileAnalysis {
                    file_path: path,
                    success: errors.is_empty(),
                    errors,
                    context: ctx,
                }
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await
    }

    // ------------------------------------------------------------------
    // GPU stages
    // ------------------------------------------------------------------

    /// Run GPU-bound processors (priority < [`CPU_PRIORITY_THRESHOLD`]) on
    /// analyses produced by [`run_cpu_stages`].
    ///
    /// Concurrency is throttled by a [`Semaphore`] with `max_concurrent`
    /// permits so the inference server is never overwhelmed.
    pub async fn run_gpu_stages(
        &self,
        analyses: Vec<FileAnalysis>,
    ) -> Vec<FileAnalysis> {
        let registry = Arc::clone(&self.registry);
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));

        stream::iter(analyses.into_iter().map(move |mut analysis| {
            let registry = Arc::clone(&registry);
            let sem = Arc::clone(&semaphore);

            async move {
                // Acquire a semaphore permit before touching the GPU server.
                let _permit = match sem.acquire().await {
                    Ok(p) => p,
                    Err(e) => {
                        analysis.success = false;
                        analysis.errors.push(format!("semaphore error: {e}"));
                        return analysis;
                    }
                };

                let path = analysis.file_path.clone();

                // Collect GPU-bound processors that match this file.
                let gpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| {
                        p.priority() < CPU_PRIORITY_THRESHOLD && p.matches(&path)
                    })
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &gpu_processors {
                    match processor.execute(&analysis.context).await {
                        Ok(output) => {
                            analysis.context.add_output(processor.name(), output);
                        }
                        Err(e) => {
                            tracing::warn!(
                                processor = processor.name(),
                                path = %path.display(),
                                error = %e,
                                "GPU processor failed"
                            );
                            analysis.success = false;
                            analysis.errors.push(format!("{}: {}", processor.name(), e));
                        }
                    }
                }

                analysis
            }
        }))
        .buffer_unordered(self.max_concurrent)
        .collect::<Vec<_>>()
        .await
    }

    /// Execute a single processor and record its output or error.
    async fn run_processor(
        processor: &dyn Processor,
        ctx: &mut PipelineContext,
        errors: &mut Vec<String>,
        file_path: &Path,
    ) {
        match processor.execute(ctx).await {
            Ok(output) => {
                ctx.add_output(processor.name(), output);
            }
            Err(e) => {
                tracing::warn!(
                    processor = processor.name(),
                    path = %file_path.display(),
                    error = %e,
                    "Processor failed"
                );
                errors.push(format!("{}: {}", processor.name(), e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DtError;
    use async_trait::async_trait;
    use std::path::Path;

    /// CPU-bound processor that handles `.rs` files (priority = 100).
    struct RsCpuProcessor;

    #[async_trait]
    impl Processor for RsCpuProcessor {
        fn name(&self) -> &str {
            "rs_cpu"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn matches(&self, file_path: &Path) -> bool {
            file_path.extension().and_then(|e| e.to_str()) == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            out.set("language", "Rust");
            out.set("lines", ctx.file_text.lines().count());
            Ok(out)
        }
    }

    /// GPU-bound processor that handles `.rs` files (priority = 60).
    struct RsGpuProcessor;

    #[async_trait]
    impl Processor for RsGpuProcessor {
        fn name(&self) -> &str {
            "rs_gpu"
        }

        fn priority(&self) -> i32 {
            60
        }

        fn matches(&self, file_path: &Path) -> bool {
            file_path.extension().and_then(|e| e.to_str()) == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            // GPU processor reads the CPU output to produce a summary.
            let lines = ctx
                .get_output("rs_cpu")
                .and_then(|o| o.get("lines"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            out.set("summary", format!("{} lines of Rust", lines));
            Ok(out)
        }
    }

    /// A processor that always fails (for error-path tests).
    struct FailingProcessor;

    #[async_trait]
    impl Processor for FailingProcessor {
        fn name(&self) -> &str {
            "failing"
        }

        fn priority(&self) -> i32 {
            50
        }

        fn matches(&self, _file_path: &Path) -> bool {
            true
        }

        async fn execute(&self, _ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            Err(DtError::General("intentional failure".into()))
        }
    }

    // ------------------------------------------------------------------
    // analyze_file
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn analyze_file_runs_matching_processors_in_priority_order() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));
        registry.register(Box::new(RsGpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let result = engine
            .analyze_file(
                "src/main.rs",
                "fn main() {\n    println!(\"hello\");\n}\n".to_string(),
                "test_project".to_string(),
            )
            .await;

        assert!(result.success, "expected success, got errors: {:?}", result.errors);
        assert_eq!(result.file_path.to_string_lossy(), "src/main.rs");

        // Both processors should have produced output.
        assert!(result.context.get_output("rs_cpu").is_some());
        assert!(result.context.get_output("rs_gpu").is_some());

        // rs_gpu should have read rs_cpu's output.
        let summary = result
            .context
            .get_output("rs_gpu")
            .and_then(|o| o.get("summary"))
            .and_then(|v| v.as_str());
        assert_eq!(summary, Some("3 lines of Rust"));
    }

    #[tokio::test]
    async fn analyze_file_skips_non_matching_processors() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor)); // only matches .rs

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let result = engine
            .analyze_file("app.py", "print('hello')".to_string(), "test".to_string())
            .await;

        assert!(result.success);
        assert!(result.context.get_output("rs_cpu").is_none());
    }

    #[tokio::test]
    async fn analyze_file_continues_on_processor_failure() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));
        registry.register(Box::new(FailingProcessor)); // matches all files

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let result = engine
            .analyze_file("test.rs", "fn main() {}".to_string(), "p".to_string())
            .await;

        // Should have continued despite the failure.
        assert!(!result.success);
        assert!(result.context.get_output("rs_cpu").is_some());
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].contains("failing"));
    }

    // ------------------------------------------------------------------
    // run_cpu_stages
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn run_cpu_stages_only_runs_cpu_processors() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor)); // priority 100 >= 85 → CPU
        registry.register(Box::new(RsGpuProcessor)); // priority 60 < 85   → GPU

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let files = vec![(
            PathBuf::from("a.rs"),
            "fn a() {}".to_string(),
        )];

        let results = engine.run_cpu_stages(files, "p".to_string()).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        // rs_cpu should have run (it is CPU-bound).
        assert!(r.context.get_output("rs_cpu").is_some());
        // rs_gpu should NOT have run (it is GPU-bound).
        assert!(r.context.get_output("rs_gpu").is_none());
    }

    #[tokio::test]
    async fn run_cpu_stages_handles_multiple_files() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let files = vec![
            (PathBuf::from("a.rs"), "fn a() {}".to_string()),
            (PathBuf::from("b.rs"), "fn b() {}".to_string()),
        ];

        let results = engine.run_cpu_stages(files, "p".to_string()).await;
        assert_eq!(results.len(), 2);
        for r in &results {
            assert!(r.context.get_output("rs_cpu").is_some());
        }
    }

    // ------------------------------------------------------------------
    // run_gpu_stages
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn run_gpu_stages_only_runs_gpu_processors() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor)); // CPU-bound
        registry.register(Box::new(RsGpuProcessor)); // GPU-bound

        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        // Create a "CPU-stage result" that already has rs_cpu output.
        let mut ctx = PipelineContext::new(
            PathBuf::from("test.rs"),
            "fn x() {}".to_string(),
            "p".to_string(),
        );
        let mut cpu_out = ProcessorOutput::new();
        cpu_out.set("language", "Rust");
        cpu_out.set("lines", 1);
        ctx.add_output("rs_cpu", cpu_out);

        let analyses = vec![FileAnalysis {
            file_path: PathBuf::from("test.rs"),
            success: true,
            errors: vec![],
            context: ctx,
        }];

        let results = engine.run_gpu_stages(analyses).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        // rs_gpu should now have run.
        assert!(r.context.get_output("rs_gpu").is_some());
        let summary = r
            .context
            .get_output("rs_gpu")
            .and_then(|o| o.get("summary"))
            .and_then(|v| v.as_str());
        assert_eq!(summary, Some("1 lines of Rust"));
    }

    // ------------------------------------------------------------------
    // analyze_batch
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn analyze_batch_runs_cpu_then_gpu() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));
        registry.register(Box::new(RsGpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let files = vec![(
            PathBuf::from("test.rs"),
            "fn main() {}".to_string(),
        )];

        let results = engine.analyze_batch(files, "p".to_string()).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        assert!(r.success);
        // Both CPU and GPU processors should have produced output.
        assert!(r.context.get_output("rs_cpu").is_some());
        assert!(r.context.get_output("rs_gpu").is_some());
    }

    #[tokio::test]
    async fn analyze_batch_handles_non_matching_files_gracefully() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));
        registry.register(Box::new(RsGpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        // Send a .py file — no processor should match.
        let files = vec![(
            PathBuf::from("script.py"),
            "import sys".to_string(),
        )];

        let results = engine.analyze_batch(files, "p".to_string()).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        assert!(r.success); // no errors = success
        assert!(r.context.get_output("rs_cpu").is_none());
        assert!(r.context.get_output("rs_gpu").is_none());
    }

    #[tokio::test]
    async fn analyze_batch_empty_input() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let results = engine.analyze_batch(vec![], "p".to_string()).await;
        assert!(results.is_empty());
    }
}
