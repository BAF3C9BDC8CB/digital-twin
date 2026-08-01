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
use crate::application::pipeline::infer_client::SiliconFlowChatClient;
use crate::application::pipeline::processor::Processor;
use crate::application::pipeline::registry::ProcessorRegistry;
use futures::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
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

// ---------------------------------------------------------------------------
// Project-level analysis types
// ---------------------------------------------------------------------------

/// Aggregated summary of a single project.
#[derive(Debug)]
pub struct ProjectAnalysis {
    /// The project name.
    pub project_name: String,
    /// Total number of files analysed.
    pub file_count: usize,
    /// Number of files that completed without errors.
    pub success_count: usize,
    /// Discovered service names extracted from entity names and LLM output.
    pub services: Vec<String>,
    /// Inter-service dependencies discovered from Feign/REST annotations and
    /// method calls.
    pub dependencies: Vec<ServiceDependency>,
    /// Human-readable architecture summary (LLM-generated when available).
    pub summary: String,
    /// Errors accumulated across all files.
    pub errors: Vec<String>,
}

/// A directed dependency from one service to another.
#[derive(Debug, Clone)]
pub struct ServiceDependency {
    /// The source/caller service.
    pub from: String,
    /// The target/callee service.
    pub to: String,
    /// Communication protocol — `"HTTP"`, `"RPC"`, or `"MQ"`.
    pub protocol: String,
}

// ---------------------------------------------------------------------------
// Ecosystem-level analysis types
// ---------------------------------------------------------------------------

/// Cross-project ecosystem topology analysis.
#[derive(Debug)]
pub struct EcosystemAnalysis {
    /// Name of this ecosystem (e.g. `"ecommerce"`, `"data-platform"`).
    pub name: String,
    /// Projects participating in the ecosystem.
    pub projects: Vec<String>,
    /// Cross-service dependencies across all projects.
    pub service_mesh: Vec<ServiceDependency>,
    /// Identified shared infrastructure (databases, caches, message queues).
    pub shared_infrastructure: Vec<String>,
    /// Detected risks (single points of failure, dependency cycles).
    pub risks: Vec<String>,
    /// Human-readable topology summary (LLM-generated when available).
    pub topology_summary: String,
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
        for &processor in matching
            .iter()
            .filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD)
        {
            Self::run_processor(processor, &mut ctx, &mut errors, &file_path_buf).await;
        }

        // Phase 2: GPU-bound stages (priority < threshold).
        for &processor in matching
            .iter()
            .filter(|p| p.priority() < CPU_PRIORITY_THRESHOLD)
        {
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
    ///
    /// `skip_steps` is an optional per-file set of processor names to skip.
    /// When a file's step is in the skip set, that processor is not executed
    /// for that file — even though other processors may still run. This
    /// enables incremental builds where only missing steps are executed.
    pub async fn analyze_batch(
        &self,
        files: Vec<(PathBuf, String)>,
        project_name: String,
        skip_steps: Option<Arc<HashMap<PathBuf, HashSet<String>>>>,
    ) -> Vec<FileAnalysis> {
        // Phase 1: CPU-bound processors on all files in parallel.
        let cpu_results = self
            .run_cpu_stages(files, project_name, skip_steps.clone())
            .await;

        // Phase 2: GPU-bound processors on the resulting analyses.
        self.run_gpu_stages(cpu_results, skip_steps).await
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
        skip_steps: Option<Arc<HashMap<PathBuf, HashSet<String>>>>,
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
            let skip = skip_steps.clone();

            async move {
                let mut ctx = PipelineContext::new(path_clone, text, (*proj).clone());
                let mut errors: Vec<String> = Vec::new();

                // Collect CPU-bound processors that match this file.
                let cpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD && p.matches(&path))
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &cpu_processors {
                    // Skip this processor if it's in the per-file skip set
                    if let Some(ref skip_map) = skip {
                        if let Some(steps) = skip_map.get(&path) {
                            if steps.contains(processor.name()) {
                                continue;
                            }
                        }
                    }
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
        skip_steps: Option<Arc<HashMap<PathBuf, HashSet<String>>>>,
    ) -> Vec<FileAnalysis> {
        let registry = Arc::clone(&self.registry);
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));

        stream::iter(analyses.into_iter().map(move |mut analysis| {
            let registry = Arc::clone(&registry);
            let sem = Arc::clone(&semaphore);
            let skip = skip_steps.clone();

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
                    .filter(|p| p.priority() < CPU_PRIORITY_THRESHOLD && p.matches(&path))
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &gpu_processors {
                    // Skip this processor if it's in the per-file skip set
                    if let Some(ref skip_map) = skip {
                        if let Some(steps) = skip_map.get(&path) {
                            if steps.contains(processor.name()) {
                                continue;
                            }
                        }
                    }
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

    // ------------------------------------------------------------------
    // Project-level analysis
    // ------------------------------------------------------------------

    /// Analyse all accumulated file analyses and produce a project-wide
    /// summary.
    ///
    /// This method:
    /// 1. Aggregates entities (classes, methods) from tree‑sitter outputs
    /// 2. Discovers service names from class‑name conventions
    /// 3. Extracts inter‑service dependencies from method calls and imports
    /// 4. Generates an architecture summary via LLM when `infer_client` is
    ///    provided
    pub async fn analyze_project(
        &self,
        project_name: String,
        file_analyses: &[FileAnalysis],
        infer_client: Option<Arc<SiliconFlowChatClient>>,
    ) -> ProjectAnalysis {
        let file_count = file_analyses.len();
        let success_count = file_analyses.iter().filter(|a| a.success).count();

        let errors: Vec<String> = file_analyses
            .iter()
            .flat_map(|a| a.errors.clone())
            .collect();

        // ---- extract entities from tree‑sitter outputs ----
        let mut all_classes: Vec<String> = Vec::new();
        let mut all_methods: Vec<(String, Vec<String>)> = Vec::new(); // (class_name, calls)
        let mut all_imports: Vec<String> = Vec::new();
        let mut llm_responses: Vec<String> = Vec::new();

        for analysis in file_analyses {
            if let Some(ts_out) = analysis.context.get_output("tree_sitter") {
                // Classes
                if let Some(entities) = ts_out.get("entities") {
                    if let Some(classes) = entities.get("classes").and_then(|c| c.as_array()) {
                        for class in classes {
                            if let Some(name) = class.get("name").and_then(|n| n.as_str()) {
                                all_classes.push(name.to_string());
                            }
                        }
                    }
                    // Methods + their calls
                    if let Some(methods) = entities.get("methods").and_then(|m| m.as_array()) {
                        for method in methods {
                            let class_name = method
                                .get("class_name")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let calls: Vec<String> = method
                                .get("calls")
                                .and_then(|c| c.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();
                            all_methods.push((class_name, calls));
                        }
                    }
                }

                // Imports
                if let Some(imports) = ts_out.get("imports").and_then(|i| i.as_array()) {
                    for imp in imports {
                        if let Some(path) = imp.as_str() {
                            all_imports.push(path.to_string());
                        }
                    }
                }
            }

            // Collect LLM responses for additional context
            if let Some(llm_out) = analysis.context.get_output("llm") {
                if let Some(response) = llm_out.get("response").and_then(|r| r.as_str()) {
                    llm_responses.push(response.to_string());
                }
            }
        }

        // ---- discover service names ----
        let mut services: Vec<String> = Vec::new();
        services.push(project_name.clone());

        // Classes whose names match typical service‑class suffixes
        for class_name in &all_classes {
            for suffix in &["Application", "Service", "Controller", "Client", "Feign"] {
                if let Some(stripped) = class_name.strip_suffix(suffix) {
                    if !stripped.is_empty() && !services.contains(&stripped.to_string()) {
                        services.push(stripped.to_string());
                    }
                }
            }
        }

        // Heuristic: imports containing "feign" or "rest" hint at service clients
        let has_feign_imports = all_imports
            .iter()
            .any(|i| i.to_lowercase().contains("feign"));
        let has_rest_imports = all_imports
            .iter()
            .any(|i| i.to_lowercase().contains("rest"));

        // ---- extract service dependencies ----
        let mut dependencies: Vec<ServiceDependency> = Vec::new();
        let mut seen_deps: HashSet<(String, String, String)> = HashSet::new();

        for (class_name, calls) in &all_methods {
            for call in calls {
                let from = if class_name.is_empty() {
                    project_name.clone()
                } else {
                    class_name.clone()
                };

                let (to, protocol) = classify_call(call, has_feign_imports, has_rest_imports);

                let key = (from.clone(), to.clone(), protocol.clone());
                if seen_deps.insert(key) {
                    dependencies.push(ServiceDependency { from, to, protocol });
                }
            }
        }

        // Also scan imports for Feign / REST client patterns
        for imp in &all_imports {
            let lower = imp.to_lowercase();
            let (protocol, hint) = if lower.contains("feign") || lower.contains("resttemplate") {
                ("HTTP", "RestTemplate/Feign")
            } else if lower.contains("kafkatemplate")
                || lower.contains("rabbittemplate")
                || lower.contains("jms")
            {
                ("MQ", "Messaging")
            } else if lower.contains("grpc") || lower.contains("stub") {
                ("RPC", "gRPC")
            } else {
                continue;
            };

            let from = project_name.clone();
            let to = format!("{}({})", imp, hint);
            let key = (from.clone(), to.clone(), protocol.to_string());
            if seen_deps.insert(key.clone()) {
                dependencies.push(ServiceDependency {
                    from,
                    to,
                    protocol: protocol.to_string(),
                });
            }
        }

        services.sort();
        services.dedup();

        // ---- generate LLM summary ----
        let summary = if let Some(ref client) = infer_client {
            let data_json = serde_json::json!({
                "project_name": project_name,
                "file_count": file_count,
                "success_count": success_count,
                "services": services,
                "dependencies_count": dependencies.len(),
                "llm_observations": llm_responses.iter().take(5).collect::<Vec<_>>(),
            });

            match summarize_via_llm(
                client,
                "You are a software architect analysing a project. \
                 Based on the data below, provide a concise architecture \
                 summary in 3-5 sentences. Include the project's purpose, \
                 the services discovered, and the communication patterns.",
                &data_json.to_string(),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(project = %project_name, error = %e, "LLM project summary failed");
                    format!(
                        "Project {}: {} files, {} services",
                        project_name,
                        file_count,
                        services.len()
                    )
                }
            }
        } else {
            format!(
                "Project {}: {} files, {} services",
                project_name,
                file_count,
                services.len()
            )
        };

        tracing::info!(
            project = %project_name,
            files = file_count,
            services = services.len(),
            dependencies = dependencies.len(),
            "Project analysis complete"
        );

        ProjectAnalysis {
            project_name,
            file_count,
            success_count,
            services,
            dependencies,
            summary,
            errors,
        }
    }

    // ------------------------------------------------------------------
    // Ecosystem-level analysis
    // ------------------------------------------------------------------

    /// Analyse the entire ecosystem of related projects (micro‑services).
    ///
    /// Merges individual [`ProjectAnalysis`] results to:
    /// 1. Build a cross‑project service mesh
    /// 2. Detect shared infrastructure (same DB / MQ / cache identifiers)
    /// 3. Identify risks (cycles, single points of failure)
    /// 4. Generate a topology summary via LLM when `infer_client` is
    ///    provided
    pub async fn analyze_ecosystem(
        &self,
        ecosystem_name: String,
        project_analyses: &[ProjectAnalysis],
        infer_client: Option<Arc<SiliconFlowChatClient>>,
    ) -> EcosystemAnalysis {
        let projects: Vec<String> = project_analyses
            .iter()
            .map(|pa| pa.project_name.clone())
            .collect();

        // ---- merge all service dependencies ----
        let mut service_mesh: Vec<ServiceDependency> = Vec::new();
        let mut seen_mesh: HashSet<(String, String, String)> = HashSet::new();

        for pa in project_analyses {
            for dep in &pa.dependencies {
                let key = (dep.from.clone(), dep.to.clone(), dep.protocol.clone());
                if seen_mesh.insert(key) {
                    service_mesh.push(dep.clone());
                }
            }
        }

        // ---- detect shared infrastructure ----
        // Group dependency targets that appear in multiple projects' imports
        // or dependency `to` fields.
        let mut infra_mentions: HashMap<String, Vec<String>> = HashMap::new();

        for pa in project_analyses {
            // Check each dep's `to` field for infrastructure keywords
            for dep in &pa.dependencies {
                let lower_to = dep.to.to_lowercase();
                for keyword in &["mysql", "postgres", "redis", "kafka", "rabbitmq", "mongodb"] {
                    if lower_to.contains(keyword) {
                        infra_mentions
                            .entry(keyword.to_string())
                            .or_default()
                            .push(pa.project_name.clone());
                        break;
                    }
                }
            }
        }

        let mut shared_infrastructure: Vec<String> = Vec::new();
        for (infra, projs) in &infra_mentions {
            if projs.len() > 1 {
                shared_infrastructure.push(format!("{} (used by {})", infra, projs.join(", ")));
            }
        }
        shared_infrastructure.sort();

        // ---- detect risks ----
        let mut risks: Vec<String> = Vec::new();

        // Cycle detection via simple DFS on the service mesh
        let mut service_set: HashSet<String> = HashSet::new();
        for dep in &service_mesh {
            service_set.insert(dep.from.clone());
            service_set.insert(dep.to.clone());
        }
        let services_list: Vec<&String> = service_set.iter().collect();

        // Build adjacency list for outgoing edges
        let mut adj: HashMap<&String, Vec<&String>> = HashMap::new();
        for dep in &service_mesh {
            adj.entry(&dep.from).or_default().push(&dep.to);
        }

        // DFS cycle detection (simple per‑node visited‑set check)
        for start in &services_list {
            let mut visited: HashSet<&String> = HashSet::new();
            let mut stack: Vec<&String> = vec![start];

            while let Some(current) = stack.pop() {
                visited.insert(current);
                if let Some(neighbors) = adj.get(current) {
                    for &next in neighbors {
                        if next == *start {
                            // Found a cycle back to the start node
                            risks.push(format!(
                                "Potential circular dependency: {} → … → {}",
                                start, start
                            ));
                            break;
                        }
                        if !visited.contains(next) {
                            stack.push(next);
                        }
                    }
                }
            }
        }

        // Single points of failure: services with many incoming dependencies
        let mut incoming_count: HashMap<&String, usize> = HashMap::new();
        for dep in &service_mesh {
            *incoming_count.entry(&dep.to).or_default() += 1;
        }
        for (svc, count) in &incoming_count {
            if *count >= 3 {
                risks.push(format!(
                    "Potential single point of failure: {} has {} incoming dependencies",
                    svc, count
                ));
            }
        }

        // Deduplicate risks
        risks.sort();
        risks.dedup();

        // ---- generate LLM topology summary ----
        let topology_summary = if let Some(ref client) = infer_client {
            let data_json = serde_json::json!({
                "ecosystem_name": ecosystem_name,
                "projects": projects,
                "mesh_edges": service_mesh.len(),
                "shared_infrastructure": shared_infrastructure,
                "risks": risks,
            });

            match summarize_via_llm(
                client,
                "You are a systems architect analysing a microservice \
                 ecosystem. Based on the topology data below, provide a \
                 concise summary in 3-5 sentences. Highlight cross-project \
                 coupling, shared infrastructure, and any critical risks.",
                &data_json.to_string(),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(ecosystem = %ecosystem_name, error = %e, "LLM ecosystem summary failed");
                    format!(
                        "Ecosystem {}: {} projects, {} mesh edges",
                        ecosystem_name,
                        projects.len(),
                        service_mesh.len()
                    )
                }
            }
        } else {
            format!(
                "Ecosystem {}: {} projects, {} mesh edges",
                ecosystem_name,
                projects.len(),
                service_mesh.len()
            )
        };

        tracing::info!(
            ecosystem = %ecosystem_name,
            projects = projects.len(),
            mesh_edges = service_mesh.len(),
            risks = risks.len(),
            "Ecosystem analysis complete"
        );

        EcosystemAnalysis {
            name: ecosystem_name,
            projects,
            service_mesh,
            shared_infrastructure,
            risks,
            topology_summary,
        }
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
// Helper functions
// ---------------------------------------------------------------------------

/// Classify a method call into a `(target, protocol)` pair.
///
/// Heuristics:
/// - Calls containing `http`, `RestTemplate`, `Feign`, `HttpClient` → HTTP
/// - Calls containing `Kafka`, `Rabbit`, `MQ`, `Jms` → MQ
/// - Everything else → RPC (simple method call within the project)
fn classify_call(call: &str, has_feign: bool, has_rest: bool) -> (String, String) {
    let lower = call.to_lowercase();

    if lower.contains("http")
        || lower.contains("resttemplate")
        || lower.contains("feign")
        || lower.contains("httpclient")
        || lower.contains("webclient")
        || has_feign
        || has_rest
    {
        // Extract a meaningful target name from the call
        let target = if call.contains("::") {
            // Rust-style: module::function or service::method
            call.split("::").next().unwrap_or(call).to_string()
        } else if call.contains('.') {
            // Java-style: object.method or Class.method
            call.split('.').next().unwrap_or(call).to_string()
        } else {
            call.to_string()
        };
        (target, "HTTP".to_string())
    } else if lower.contains("kafka")
        || lower.contains("rabbit")
        || lower.contains("jms")
        || lower.contains("mq_")
        || lower.contains("publish")
        || lower.contains("subscribe")
    {
        (call.to_string(), "MQ".to_string())
    } else {
        (call.to_string(), "RPC".to_string())
    }
}

/// Call the LLM to summarise structured data.
///
/// Sends a `chat` request to the inference server with a low temperature
/// (0.1) and capped at 2048 tokens.  Returns the model's text response
/// or an error string.
async fn summarize_via_llm(
    client: &SiliconFlowChatClient,
    system_prompt: &str,
    data_json: &str,
) -> Result<String, String> {
    let response = client
        .chat("default", system_prompt, data_json, 0.1, 2048)
        .await?;
    Ok(response.choices[0].message.content.clone())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::output::ProcessorOutput;
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

        assert!(
            result.success,
            "expected success, got errors: {:?}",
            result.errors
        );
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
        let files = vec![(PathBuf::from("a.rs"), "fn a() {}".to_string())];

        let results = engine.run_cpu_stages(files, "p".to_string(), None).await;
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

        let results = engine.run_cpu_stages(files, "p".to_string(), None).await;
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

        let results = engine.run_gpu_stages(analyses, None).await;
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
        let files = vec![(PathBuf::from("test.rs"), "fn main() {}".to_string())];

        let results = engine.analyze_batch(files, "p".to_string(), None).await;
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
        let files = vec![(PathBuf::from("script.py"), "import sys".to_string())];

        let results = engine.analyze_batch(files, "p".to_string(), None).await;
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
        let results = engine.analyze_batch(vec![], "p".to_string(), None).await;
        assert!(results.is_empty());
    }

    // ------------------------------------------------------------------
    // classify_call
    // ------------------------------------------------------------------

    #[test]
    fn classify_call_http_patterns() {
        let (target, proto) = classify_call("restTemplate.exchange(...)", false, true);
        assert_eq!(proto, "HTTP");
        assert_eq!(target, "restTemplate");

        let (target, proto) = classify_call("feignClient.getUser()", true, false);
        assert_eq!(proto, "HTTP");
        assert_eq!(target, "feignClient");
    }

    #[test]
    fn classify_call_rpc_default() {
        let (target, proto) = classify_call("someInternalMethod()", false, false);
        assert_eq!(proto, "RPC");
        assert_eq!(target, "someInternalMethod()");
    }

    #[test]
    fn classify_call_mq_patterns() {
        let (target, proto) = classify_call("kafkaTemplate.send(...)", false, false);
        assert_eq!(proto, "MQ");
        assert!(target.contains("kafkaTemplate"));
    }

    #[test]
    fn classify_call_rust_style_double_colon() {
        let (target, proto) = classify_call("http::Client::new()", false, false);
        assert_eq!(proto, "HTTP");
        assert_eq!(target, "http");
    }

    #[test]
    fn classify_call_java_style_dot() {
        let (target, proto) = classify_call("httpClient.send()", false, false);
        assert_eq!(proto, "HTTP");
        assert_eq!(target, "httpClient");
    }

    // ------------------------------------------------------------------
    // analyze_project
    // ------------------------------------------------------------------

    /// Build a fake FileAnalysis with tree‑sitter output.
    fn make_ts_analysis(
        file_path: &str,
        classes: Vec<(&str, &str)>, // (class_name, package)
        methods: Vec<(&str, &str, &str, Vec<&str>)>, // (class_name, method, return_type, calls)
        imports: Vec<&str>,
        success: bool,
    ) -> FileAnalysis {
        let mut ctx = PipelineContext::new(
            PathBuf::from(file_path),
            String::new(),
            "test_project".to_string(),
        );

        let mut ts_out = ProcessorOutput::new();

        let class_json: Vec<serde_json::Value> = classes
            .iter()
            .map(|(name, pkg)| {
                serde_json::json!({
                    "name": name,
                    "package": pkg,
                    "kind": "class",
                    "file_path": file_path,
                })
            })
            .collect();

        let method_json: Vec<serde_json::Value> = methods
            .iter()
            .map(|(class_name, name, _ret, calls)| {
                serde_json::json!({
                    "name": name,
                    "class_name": class_name,
                    "signature": format!("{}()", name),
                    "params": [],
                    "return_type": "",
                    "file_path": file_path,
                    "calls": calls,
                })
            })
            .collect();

        ts_out.set(
            "entities",
            serde_json::json!({
                "classes": class_json,
                "methods": method_json,
            }),
        );
        ts_out.set("imports", imports);
        ts_out.set("method_count", methods.len());
        ts_out.set("class_count", classes.len());

        ctx.add_output("tree_sitter", ts_out);

        let mut errors = vec![];
        if !success {
            errors.push("processing error".to_string());
        }

        FileAnalysis {
            file_path: PathBuf::from(file_path),
            success,
            errors,
            context: ctx,
        }
    }

    #[tokio::test]
    async fn analyze_project_extracts_services_and_deps() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        let analyses = vec![
            make_ts_analysis(
                "UserController.java",
                vec![("UserController", "com.app.controller")],
                vec![(
                    "UserController",
                    "getUser",
                    "User",
                    vec!["userService.findById()", "restTemplate.get()"],
                )],
                vec!["org.springframework.web.client.RestTemplate"],
                true,
            ),
            make_ts_analysis(
                "UserService.java",
                vec![("UserService", "com.app.service")],
                vec![(
                    "UserService",
                    "findById",
                    "User",
                    vec!["userRepository.findById()"],
                )],
                vec![],
                true,
            ),
        ];

        let result = engine
            .analyze_project("user-service".to_string(), &analyses, None)
            .await;

        assert_eq!(result.project_name, "user-service");
        assert_eq!(result.file_count, 2);
        assert_eq!(result.success_count, 2);
        // Should have discovered service names
        assert!(result.services.contains(&"user-service".to_string()));
        assert!(result.services.contains(&"User".to_string())); // from UserService/UserController
                                                                // Should have extracted dependencies
        assert!(!result.dependencies.is_empty());
        // Should have a default summary
        assert!(result.summary.contains("user-service"));
    }

    #[tokio::test]
    async fn analyze_project_handles_empty_input() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        let result = engine
            .analyze_project("empty-project".to_string(), &[], None)
            .await;

        assert_eq!(result.file_count, 0);
        assert_eq!(result.success_count, 0);
        assert!(result.services.contains(&"empty-project".to_string()));
        assert!(result.dependencies.is_empty());
    }

    #[tokio::test]
    async fn analyze_project_counts_errors_correctly() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        let analyses = vec![
            make_ts_analysis("ok.rs", vec![], vec![], vec![], true),
            make_ts_analysis("fail.rs", vec![], vec![], vec![], false),
        ];

        let result = engine
            .analyze_project("mixed".to_string(), &analyses, None)
            .await;

        assert_eq!(result.file_count, 2);
        assert_eq!(result.success_count, 1);
        assert_eq!(result.errors.len(), 1);
    }

    // ------------------------------------------------------------------
    // analyze_ecosystem
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn analyze_ecosystem_merges_multi_project_deps() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        let project_a = ProjectAnalysis {
            project_name: "service-a".to_string(),
            file_count: 5,
            success_count: 5,
            services: vec!["service-a".to_string()],
            dependencies: vec![ServiceDependency {
                from: "service-a".to_string(),
                to: "service-b".to_string(),
                protocol: "HTTP".to_string(),
            }],
            summary: "Service A summary".to_string(),
            errors: vec![],
        };

        let project_b = ProjectAnalysis {
            project_name: "service-b".to_string(),
            file_count: 3,
            success_count: 3,
            services: vec!["service-b".to_string()],
            dependencies: vec![
                ServiceDependency {
                    from: "service-b".to_string(),
                    to: "service-c".to_string(),
                    protocol: "HTTP".to_string(),
                },
                ServiceDependency {
                    from: "service-b".to_string(),
                    to: "mysql-db".to_string(),
                    protocol: "RPC".to_string(),
                },
            ],
            summary: "Service B summary".to_string(),
            errors: vec![],
        };

        let project_c = ProjectAnalysis {
            project_name: "service-c".to_string(),
            file_count: 2,
            success_count: 2,
            services: vec!["service-c".to_string()],
            dependencies: vec![ServiceDependency {
                from: "service-c".to_string(),
                to: "mysql-db".to_string(),
                protocol: "RPC".to_string(),
            }],
            summary: "Service C summary".to_string(),
            errors: vec![],
        };

        let result = engine
            .analyze_ecosystem(
                "test-ecosystem".to_string(),
                &[project_a, project_b, project_c],
                None,
            )
            .await;

        assert_eq!(result.name, "test-ecosystem");
        assert_eq!(result.projects.len(), 3);
        // Should have merged all unique dependencies
        assert!(result.projects.contains(&"service-a".to_string()));
        assert!(result.projects.contains(&"service-b".to_string()));
        assert!(result.projects.contains(&"service-c".to_string()));
        // Should have detected shared infrastructure (mysql-db used by 2 services)
        assert!(result
            .shared_infrastructure
            .iter()
            .any(|s| s.to_lowercase().contains("mysql")));
        // Should have a default summary
        assert!(result.topology_summary.contains("test-ecosystem"));
    }

    #[tokio::test]
    async fn analyze_ecosystem_handles_single_project() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        let project = ProjectAnalysis {
            project_name: "solo".to_string(),
            file_count: 1,
            success_count: 1,
            services: vec!["solo".to_string()],
            dependencies: vec![],
            summary: "".to_string(),
            errors: vec![],
        };

        let result = engine
            .analyze_ecosystem("single".to_string(), &[project], None)
            .await;

        assert_eq!(result.projects.len(), 1);
        assert!(result.service_mesh.is_empty());
        assert!(result.shared_infrastructure.is_empty());
    }

    // ------------------------------------------------------------------
    // Ecosystem risk detection
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn analyze_ecosystem_detects_cycles() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        // A → B → C → A creates a cycle
        let pa = ProjectAnalysis {
            project_name: "proj-a".to_string(),
            file_count: 1,
            success_count: 1,
            services: vec![],
            dependencies: vec![ServiceDependency {
                from: "svc-a".to_string(),
                to: "svc-b".to_string(),
                protocol: "HTTP".to_string(),
            }],
            summary: "".to_string(),
            errors: vec![],
        };

        let pb = ProjectAnalysis {
            project_name: "proj-b".to_string(),
            file_count: 1,
            success_count: 1,
            services: vec![],
            dependencies: vec![ServiceDependency {
                from: "svc-b".to_string(),
                to: "svc-c".to_string(),
                protocol: "HTTP".to_string(),
            }],
            summary: "".to_string(),
            errors: vec![],
        };

        let pc = ProjectAnalysis {
            project_name: "proj-c".to_string(),
            file_count: 1,
            success_count: 1,
            services: vec![],
            dependencies: vec![ServiceDependency {
                from: "svc-c".to_string(),
                to: "svc-a".to_string(),
                protocol: "HTTP".to_string(),
            }],
            summary: "".to_string(),
            errors: vec![],
        };

        let result = engine
            .analyze_ecosystem("cyclic".to_string(), &[pa, pb, pc], None)
            .await;

        // Should detect at least one circular dependency
        let cycle_risks: Vec<&String> = result
            .risks
            .iter()
            .filter(|r| r.contains("circular") || r.contains("cycle"))
            .collect();
        assert!(
            !cycle_risks.is_empty(),
            "Expected cycle detection, risks: {:?}",
            result.risks
        );
    }
}
