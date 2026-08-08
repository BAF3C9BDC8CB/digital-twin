//! 流水线编排引擎。
//!
//! [`ProcessorEngine`] 将 [`ProcessorRegistry`] 与 [`Processor`] trait
//! 结合起来，通过可配置的处理流水线分析源文件。
//!
//! # 执行模型
//!
//! 处理器分为两类：
//!
//! * **CPU 密集型阶段**（优先级 ≥ [`CPU_PRIORITY_THRESHOLD`]）——廉价
//!   操作，如语言检测、tree-sitter 解析与文本分块。这些在文件间完全并行运行。
//!
//! * **GPU 密集型阶段**（优先级 < [`CPU_PRIORITY_THRESHOLD`]）——命中
//!   推理服务器的操作，如 LLM chat completions、embedding 与 rerank。
//!   并发由 [`Semaphore`] 限制，以避免压垮 GPU 服务器。

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::infer_client::SiliconFlowChatClient;
use crate::application::pipeline::processor::Processor;
use crate::application::pipeline::registry::ProcessorRegistry;
use crate::application::pipeline::virtual_file::FileSourceKind;
use futures::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 区分 CPU 密集型阶段与 GPU 密集型阶段的优先级阈值。
///
/// 优先级 **≥** 该值的处理器视为 CPU 密集型，可不受限制地并行运行。
/// 优先级 **<** 该值的处理器为 GPU 密集型，使用信号量限制并发。
///
/// 现有代码库中的约定：
/// - tree_sitter = 100, chunk = 90 —— CPU 密集型
/// - llm = 60 —— GPU 密集型
pub const CPU_PRIORITY_THRESHOLD: i32 = 85;

/// 当 `std::thread::available_parallelism` 无法确定时，
/// CPU 阶段的默认并行度。
const DEFAULT_CPU_CONCURRENCY: usize = 8;

// ---------------------------------------------------------------------------
// ProcessorEngine
// ---------------------------------------------------------------------------

/// 针对一个或多个文件运行所有匹配处理器的编排器。
pub struct ProcessorEngine {
    /// 共享处理器注册表。
    registry: Arc<ProcessorRegistry>,
    /// 最大在途 GPU 密集型操作数。
    max_concurrent: usize,
}

/// 单个文件经流水线处理的聚合结果。
#[derive(Debug)]
pub struct FileAnalysis {
    /// 被分析文件的路径。
    pub file_path: PathBuf,
    /// 该文件是否*所有*处理器都无错误完成。
    pub success: bool,
    /// 每个处理器的错误消息（若有）。
    pub errors: Vec<String>,
    /// 携带每个处理器输出的累积流水线上下文。
    pub context: PipelineContext,
}

// ---------------------------------------------------------------------------
// 项目级分析类型
// ---------------------------------------------------------------------------

/// 单个项目的聚合摘要。
#[derive(Debug)]
pub struct ProjectAnalysis {
    /// 项目名。
    pub project_name: String,
    /// 分析的文件总数。
    pub file_count: usize,
    /// 无错误完成的文件数。
    pub success_count: usize,
    /// 从实体名与 LLM 输出中发现的 service 名。
    pub services: Vec<String>,
    /// 从 Feign/REST 注解与方法调用中发现的 service 间依赖。
    pub dependencies: Vec<ServiceDependency>,
    /// 人类可读的架构摘要（可用时由 LLM 生成）。
    pub summary: String,
    /// 所有文件累积的错误。
    pub errors: Vec<String>,
}

/// 从一个 service 到另一个 service 的有向依赖。
#[derive(Debug, Clone)]
pub struct ServiceDependency {
    /// 源 / 调用方 service。
    pub from: String,
    /// 目标 / 被调用方 service。
    pub to: String,
    /// 通信协议——`"HTTP"`、`"RPC"` 或 `"MQ"`。
    pub protocol: String,
}

// ---------------------------------------------------------------------------
// 生态系统级分析类型
// ---------------------------------------------------------------------------

/// 跨项目生态系统拓扑分析。
#[derive(Debug)]
pub struct EcosystemAnalysis {
    /// 该生态系统的名称（例如 `"ecommerce"`、`"data-platform"`）。
    pub name: String,
    /// 参与生态系统的项目。
    pub projects: Vec<String>,
    /// 所有项目中的跨 service 依赖。
    pub service_mesh: Vec<ServiceDependency>,
    /// 识别出的共享基础设施（数据库、缓存、消息队列）。
    pub shared_infrastructure: Vec<String>,
    /// 检测到的风险（单点故障、依赖环）。
    pub risks: Vec<String>,
    /// 人类可读的拓扑摘要（可用时由 LLM 生成）。
    pub topology_summary: String,
}

impl ProcessorEngine {
    /// 从共享注册表创建新引擎。
    ///
    /// `max_concurrent` 控制可同时运行的 GPU 密集型处理器调用数上限
    ///（由 [`run_gpu_stages`] 使用）。
    pub fn new(registry: Arc<ProcessorRegistry>, max_concurrent: usize) -> Self {
        Self {
            registry,
            max_concurrent,
        }
    }

    // ------------------------------------------------------------------
    // 单文件分析
    // ------------------------------------------------------------------

    /// 通过所有匹配的处理器处理单个文件。
    ///
    /// CPU 密集型处理器（优先级 ≥ [`CPU_PRIORITY_THRESHOLD`]）先运行，
    /// 使其输出可供下游 GPU 密集型阶段使用。在每个组内，处理器按优先级
    /// 顺序执行（最低优先、最重要的最后）。
    ///
    /// 若某个处理器失败，记录警告并继续处理下一个匹配的处理器。
    /// 返回的 [`FileAnalysis`] 包含所有累积输出与任何错误消息。
    pub async fn analyze_file(
        &self,
        file_path: impl Into<PathBuf>,
        file_text: String,
        project_name: String,
    ) -> FileAnalysis {
        let file_path_buf: PathBuf = file_path.into();
        let mut ctx = PipelineContext::new(
            file_path_buf.clone(),
            file_text,
            project_name,
            FileSourceKind::Fs,
            None,
            None,
        );

        let matching = self.registry.matching(&ctx);
        let mut errors: Vec<String> = Vec::new();

        // 阶段 1：CPU 密集型阶段（优先级 >= 阈值）。
        for &processor in matching
            .iter()
            .filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD)
        {
            Self::run_processor(processor, &mut ctx, &mut errors, &file_path_buf).await;
        }

        // 阶段 2：GPU 密集型阶段（优先级 < 阈值）。
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
    // 批量分析
    // ------------------------------------------------------------------

    /// 并行处理多个文件。
    ///
    /// 1. **CPU 阶段**先以完全并行度在所有文件上运行
    ///    （受 `available_parallelism` 限制）。
    /// 2. **GPU 阶段**在结果上下文上运行，通过内部信号量将并发限制为
    ///    `max_concurrent`。
    ///
    /// `skip_steps` 是可选的要跳过的处理器名集合（按文件）。当文件的某个
    /// 步骤在跳过集合中时，即使其他处理器仍可运行，该处理器也不会为该文件
    /// 执行。这实现了仅执行缺失步骤的增量构建。
    pub async fn analyze_batch(
        &self,
        files: Vec<(PathBuf, String)>,
        project_name: String,
        skip_steps: Option<Arc<HashMap<PathBuf, HashSet<String>>>>,
    ) -> Vec<FileAnalysis> {
        // 阶段 1：在所有文件上并行运行 CPU 密集型处理器。
        let cpu_results = self
            .run_cpu_stages(files, project_name, skip_steps.clone())
            .await;

        // 阶段 2：在结果分析上运行 GPU 密集型处理器。
        self.run_gpu_stages(cpu_results, skip_steps).await
    }

    /// G1: 通过所有匹配的处理器处理一组虚拟文件（Nacos/Jenkins 远程源）。
    ///
    /// 与 [`Self::analyze_batch`] 相同执行模型（CPU 并行 → GPU 信号量节流），
    /// 但 [`PipelineContext`] 携带 `VirtualFile` 的来源/哈希元数据
    /// （`source_kind`/`mtime`/`content_hash`），使处理器能按来源路由
    /// （如 `select_prompt` 将 Nacos 源路由到 nacos_config 词表）。
    pub async fn analyze_virtual_batch(
        &self,
        files: Vec<crate::application::pipeline::virtual_file::VirtualFile>,
        project_name: String,
    ) -> Vec<FileAnalysis> {
        // 阶段 1：在所有虚拟文件上并行运行 CPU 密集型处理器。
        let cpu_results = self.run_cpu_virtual_stages(files, project_name).await;

        // 阶段 2：在结果分析上运行 GPU 密集型处理器。
        self.run_gpu_stages(cpu_results, None).await
    }

    // ------------------------------------------------------------------
    // CPU 阶段
    // ------------------------------------------------------------------

    /// 在所有文件上并行运行 CPU 密集型处理器（优先级 ≥
    /// [`CPU_PRIORITY_THRESHOLD`]）。
    ///
    /// 并发度设为可用 CPU 核心数，使处理器密集型工作在不超额订阅的情况下
    /// 占满所有核心。
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
                let mut ctx = PipelineContext::new(
                    path_clone,
                    text,
                    (*proj).clone(),
                    FileSourceKind::Fs,
                    None,
                    None,
                );
                let mut errors: Vec<String> = Vec::new();

                // 收集匹配该文件的 CPU 密集型处理器。
                let cpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD && p.matches(&ctx))
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &cpu_processors {
                    // 若该处理器在此文件的跳过集合中则跳过
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
                                "CPU 处理器执行失败"
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

    /// G1: 在所有虚拟文件上并行运行 CPU 密集型处理器（优先级 ≥
    /// [`CPU_PRIORITY_THRESHOLD`]），构造携带 `VirtualFile` 元数据的
    /// [`PipelineContext`]（`source_kind`/`mtime`/`content_hash`）。
    pub async fn run_cpu_virtual_stages(
        &self,
        files: Vec<crate::application::pipeline::virtual_file::VirtualFile>,
        project_name: String,
    ) -> Vec<FileAnalysis> {
        let concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(DEFAULT_CPU_CONCURRENCY);

        let registry = Arc::clone(&self.registry);
        let project_name = Arc::new(project_name);

        stream::iter(files.into_iter().map(move |vf| {
            let registry = Arc::clone(&registry);
            let proj = Arc::clone(&project_name);
            let path = PathBuf::from(&vf.virtual_path);
            let path_clone = path.clone();

            async move {
                let mut ctx = PipelineContext::new(
                    path_clone,
                    vf.content,
                    (*proj).clone(),
                    vf.source,
                    vf.mtime,
                    Some(vf.content_hash),
                );
                let mut errors: Vec<String> = Vec::new();

                // 收集匹配该虚拟文件的 CPU 密集型处理器。
                let cpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| p.priority() >= CPU_PRIORITY_THRESHOLD && p.matches(&ctx))
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
                                "CPU 处理器执行失败"
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
    // GPU 阶段
    // ------------------------------------------------------------------

    /// 在 [`run_cpu_stages`] 产生的分析上运行 GPU 密集型处理器
    ///（优先级 < [`CPU_PRIORITY_THRESHOLD`]）。
    ///
    /// 并发由带 `max_concurrent` 个许可的 [`Semaphore`] 节流，
    /// 使推理服务器永远不会被压垮。
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
                // 在触碰 GPU 服务器之前获取信号量许可。
                let _permit = match sem.acquire().await {
                    Ok(p) => p,
                    Err(e) => {
                        analysis.success = false;
                        analysis.errors.push(format!("信号量错误: {e}"));
                        return analysis;
                    }
                };

                let path = analysis.file_path.clone();

                // 收集匹配该文件的 GPU 密集型处理器。
                let gpu_processors: Vec<&dyn Processor> = (*registry)
                    .all()
                    .iter()
                    .filter(|p| {
                        p.priority() < CPU_PRIORITY_THRESHOLD && p.matches(&analysis.context)
                    })
                    .map(|p| p.as_ref())
                    .collect();

                for processor in &gpu_processors {
                    // 若该处理器在此文件的跳过集合中则跳过
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
                                "GPU 处理器执行失败"
                            );
                            analysis.success = false;
                            analysis.errors.push(format!("{}: {}", processor.name(), e));
                        }
                    }
                }

                tracing::info!(
                    file = %path.display(),
                    success = analysis.success,
                    errors = analysis.errors.len(),
                    "流水线文件处理完成"
                );
                analysis
            }
        }))
        .buffer_unordered(self.max_concurrent)
        .collect::<Vec<_>>()
        .await
    }

    // ------------------------------------------------------------------
    // 项目级分析
    // ------------------------------------------------------------------

    /// 分析所有累积的文件分析，并产生项目级摘要。
    ///
    /// 该方法：
    /// 1. 聚合 tree-sitter 输出中的实体（classes、methods）
    /// 2. 依据类名约定发现 service 名
    /// 3. 从方法调用与导入中提取 service 间依赖
    /// 4. 当提供 `infer_client` 时通过 LLM 生成架构摘要
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

        // ---- 从 tree-sitter 输出提取实体 ----
        let mut all_classes: Vec<String> = Vec::new();
        let mut all_methods: Vec<(String, Vec<String>)> = Vec::new(); // (class_name, calls)
        let mut all_imports: Vec<String> = Vec::new();
        let mut llm_responses: Vec<String> = Vec::new();

        for analysis in file_analyses {
            if let Some(ts_out) = analysis.context.get_output("tree_sitter") {
                // 类
                if let Some(entities) = ts_out.get("entities") {
                    if let Some(classes) = entities.get("classes").and_then(|c| c.as_array()) {
                        for class in classes {
                            if let Some(name) = class.get("name").and_then(|n| n.as_str()) {
                                all_classes.push(name.to_string());
                            }
                        }
                    }
                    // 方法及其调用
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

                // 导入
                if let Some(imports) = ts_out.get("imports").and_then(|i| i.as_array()) {
                    for imp in imports {
                        if let Some(path) = imp.as_str() {
                            all_imports.push(path.to_string());
                        }
                    }
                }
            }

            // 收集 LLM 响应作为额外上下文
            if let Some(llm_out) = analysis.context.get_output("llm") {
                if let Some(response) = llm_out.get("response").and_then(|r| r.as_str()) {
                    llm_responses.push(response.to_string());
                }
            }
        }

        // ---- 发现 service 名 ----
        let mut services: Vec<String> = Vec::new();
        services.push(project_name.clone());

        // 类名匹配典型 service 类后缀的类
        for class_name in &all_classes {
            for suffix in &["Application", "Service", "Controller", "Client", "Feign"] {
                if let Some(stripped) = class_name.strip_suffix(suffix) {
                    if !stripped.is_empty() && !services.contains(&stripped.to_string()) {
                        services.push(stripped.to_string());
                    }
                }
            }
        }

        // 启发式：包含 "feign" 或 "rest" 的导入暗示 service 客户端
        let has_feign_imports = all_imports
            .iter()
            .any(|i| i.to_lowercase().contains("feign"));
        let has_rest_imports = all_imports
            .iter()
            .any(|i| i.to_lowercase().contains("rest"));

        // ---- 提取 service 依赖 ----
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

        // 同时扫描导入中的 Feign / REST 客户端模式
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

        // ---- 生成 LLM 摘要 ----
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
                    tracing::warn!(project = %project_name, error = %e, "LLM 项目摘要生成失败");
                    format!(
                        "项目 {}：{} 个文件，{} 个服务",
                        project_name,
                        file_count,
                        services.len()
                    )
                }
            }
        } else {
            format!(
                "项目 {}：{} 个文件，{} 个服务",
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
            "项目分析完成"
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
    // 生态系统级分析
    // ------------------------------------------------------------------

    /// 分析相关项目（微服务）的整个生态系统。
    ///
    /// 合并单个 [`ProjectAnalysis`] 结果以：
    /// 1. 构建跨项目 service mesh
    /// 2. 检测共享基础设施（相同的 DB / MQ / 缓存标识）
    /// 3. 识别风险（环、单点故障）
    /// 4. 当提供 `infer_client` 时通过 LLM 生成拓扑摘要
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

        // ---- 合并所有 service 依赖 ----
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

        // ---- 检测共享基础设施 ----
        // 将出现在多个项目导入或依赖 `to` 字段中的依赖目标分组。
        let mut infra_mentions: HashMap<String, Vec<String>> = HashMap::new();

        for pa in project_analyses {
            // 检查每个依赖的 `to` 字段是否含基础设施关键字
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

        // ---- 检测风险 ----
        let mut risks: Vec<String> = Vec::new();

        // 通过 service mesh 上的简单 DFS 检测环
        let mut service_set: HashSet<String> = HashSet::new();
        for dep in &service_mesh {
            service_set.insert(dep.from.clone());
            service_set.insert(dep.to.clone());
        }
        let services_list: Vec<&String> = service_set.iter().collect();

        // 为出边构建邻接表
        let mut adj: HashMap<&String, Vec<&String>> = HashMap::new();
        for dep in &service_mesh {
            adj.entry(&dep.from).or_default().push(&dep.to);
        }

        // DFS 环检测（简单的逐节点 visited 集合检查）
        for start in &services_list {
            let mut visited: HashSet<&String> = HashSet::new();
            let mut stack: Vec<&String> = vec![start];

            while let Some(current) = stack.pop() {
                visited.insert(current);
                if let Some(neighbors) = adj.get(current) {
                    for &next in neighbors {
                        if next == *start {
                            // 发现回到起始节点的环
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

        // 单点故障：入向依赖很多的 service
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

        // 对风险去重
        risks.sort();
        risks.dedup();

        // ---- 生成 LLM 拓扑摘要 ----
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
                    tracing::warn!(ecosystem = %ecosystem_name, error = %e, "LLM 生态系统摘要生成失败");
                    format!(
                        "生态系统 {}：{} 个项目，{} 条网格边",
                        ecosystem_name,
                        projects.len(),
                        service_mesh.len()
                    )
                }
            }
        } else {
            format!(
                "生态系统 {}：{} 个项目，{} 条网格边",
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
            "生态系统分析完成"
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

    /// 执行单个处理器并记录其输出或错误。
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
                    "处理器执行失败"
                );
                errors.push(format!("{}: {}", processor.name(), e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 将方法调用分类为 `(target, protocol)` 对。
///
/// 启发式：
/// - 调用包含 `http`、`RestTemplate`、`Feign`、`HttpClient` → HTTP
/// - 调用包含 `Kafka`、`Rabbit`、`MQ`、`Jms` → MQ
/// - 其他情况 → RPC（项目内的简单方法调用）
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
        // 从调用中提取有意义的 target 名
        let target = if call.contains("::") {
            // Rust 风格：module::function 或 service::method
            call.split("::").next().unwrap_or(call).to_string()
        } else if call.contains('.') {
            // Java 风格：object.method 或 Class.method
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

/// 调用 LLM 总结结构化数据。
///
/// 以低温度（0.1）向推理服务器发送 `chat` 请求，并限制为 2048 token。
/// 返回模型的文本响应或错误字符串。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::output::ProcessorOutput;
    use crate::domain::error::DtError;
    use async_trait::async_trait;

    /// 处理 `.rs` 文件的 CPU 密集型处理器（优先级 = 100）。
    struct RsCpuProcessor;

    #[async_trait]
    impl Processor for RsCpuProcessor {
        fn name(&self) -> &str {
            "rs_cpu"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn matches(&self, ctx: &PipelineContext) -> bool {
            ctx.file_path.extension().and_then(|e| e.to_str()) == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            out.set("language", "Rust");
            out.set("lines", ctx.file_text.lines().count());
            Ok(out)
        }
    }

    /// 处理 `.rs` 文件的 GPU 密集型处理器（优先级 = 60）。
    struct RsGpuProcessor;

    #[async_trait]
    impl Processor for RsGpuProcessor {
        fn name(&self) -> &str {
            "rs_gpu"
        }

        fn priority(&self) -> i32 {
            60
        }

        fn matches(&self, ctx: &PipelineContext) -> bool {
            ctx.file_path.extension().and_then(|e| e.to_str()) == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            // GPU 处理器读取 CPU 输出来生成摘要。
            let lines = ctx
                .get_output("rs_cpu")
                .and_then(|o| o.get("lines"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            out.set("summary", format!("{} lines of Rust", lines));
            Ok(out)
        }
    }

    /// 一个总是失败的处理器（用于错误路径测试）。
    struct FailingProcessor;

    #[async_trait]
    impl Processor for FailingProcessor {
        fn name(&self) -> &str {
            "failing"
        }

        fn priority(&self) -> i32 {
            50
        }

        fn matches(&self, _ctx: &PipelineContext) -> bool {
            true
        }

        async fn execute(&self, _ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            Err(DtError::General("故意失败".into()))
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

        assert!(result.success, "应成功，但得到错误: {:?}", result.errors);
        assert_eq!(result.file_path.to_string_lossy(), "src/main.rs");

        // 两个处理器都应产生输出。
        assert!(result.context.get_output("rs_cpu").is_some());
        assert!(result.context.get_output("rs_gpu").is_some());

        // rs_gpu 应已读取 rs_cpu 的输出。
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
        registry.register(Box::new(RsCpuProcessor)); // 只匹配 .rs

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
        registry.register(Box::new(FailingProcessor)); // 匹配所有文件

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let result = engine
            .analyze_file("test.rs", "fn main() {}".to_string(), "p".to_string())
            .await;

        // 尽管失败，处理仍应继续。
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
        registry.register(Box::new(RsCpuProcessor)); // 优先级 100 >= 85 → CPU
        registry.register(Box::new(RsGpuProcessor)); // 优先级 60 < 85   → GPU

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let files = vec![(PathBuf::from("a.rs"), "fn a() {}".to_string())];

        let results = engine.run_cpu_stages(files, "p".to_string(), None).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        // rs_cpu 应已运行（它是 CPU 密集型）。
        assert!(r.context.get_output("rs_cpu").is_some());
        // rs_gpu 不应运行（它是 GPU 密集型）。
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
    // analyze_virtual_batch（G1）
    // ------------------------------------------------------------------

    /// 处理任意来源的处理器（用于验证 VirtualFile 元数据透传）。
    struct SourceAwareProcessor;

    #[async_trait]
    impl Processor for SourceAwareProcessor {
        fn name(&self) -> &str {
            "source_aware"
        }

        fn priority(&self) -> i32 {
            90 // CPU 密集型
        }

        fn matches(&self, _ctx: &PipelineContext) -> bool {
            true
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            out.set("source", ctx.source_kind.as_str());
            out.set("hash", ctx.content_hash.clone().unwrap_or_default());
            Ok(out)
        }
    }

    /// G1: analyze_virtual_batch 构造携带 source_kind/content_hash 的上下文，
    /// 处理器能感知来源（Nacos → nacos_config 路由的前提）。
    #[tokio::test]
    async fn analyze_virtual_batch_preserves_virtual_file_metadata() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(SourceAwareProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let vf = crate::application::pipeline::virtual_file::VirtualFile::new(
            "dt://nacos/prod/app.yaml",
            "server.port: 8080",
            "proj",
            FileSourceKind::Nacos,
            None,
            "abc123hash",
        );

        let results = engine
            .analyze_virtual_batch(vec![vf], "proj".to_string())
            .await;
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(r.success, "错误: {:?}", r.errors);
        let out = r.context.get_output("source_aware").unwrap();
        assert_eq!(out.get("source").and_then(|v| v.as_str()), Some("Nacos"));
        assert_eq!(out.get("hash").and_then(|v| v.as_str()), Some("abc123hash"));
        // 上下文路径保持 dt:// 虚拟路径
        assert_eq!(
            r.context.file_path.to_string_lossy(),
            "dt://nacos/prod/app.yaml"
        );
    }

    /// G1: analyze_virtual_batch 与 analyze_batch 相同两阶段执行——
    /// CPU 处理器在虚拟文件上运行，GPU 处理器随后运行并读到 CPU 输出。
    #[tokio::test]
    async fn analyze_virtual_batch_runs_cpu_then_gpu() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor)); // 只匹配 .rs
        registry.register(Box::new(RsGpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        let vf = crate::application::pipeline::virtual_file::VirtualFile::new(
            "dt://nacos/prod/script.rs",
            "fn a() {}\nfn b() {}\nfn c() {}",
            "proj",
            FileSourceKind::Nacos,
            None,
            "hash1",
        );

        let results = engine
            .analyze_virtual_batch(vec![vf], "proj".to_string())
            .await;
        let r = &results[0];
        assert!(r.success, "错误: {:?}", r.errors);
        // GPU 处理器读取 CPU 输出 → 两阶段都执行
        assert_eq!(
            r.context
                .get_output("rs_gpu")
                .and_then(|o| o.get("summary"))
                .and_then(|v| v.as_str()),
            Some("3 lines of Rust")
        );
    }

    // ------------------------------------------------------------------
    // run_gpu_stages
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn run_gpu_stages_only_runs_gpu_processors() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor)); // CPU 密集型
        registry.register(Box::new(RsGpuProcessor)); // GPU 密集型

        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        // 创建一个已含 rs_cpu 输出的"CPU 阶段结果"。
        let mut ctx = PipelineContext::new(
            PathBuf::from("test.rs"),
            "fn x() {}".to_string(),
            "p".to_string(),
            FileSourceKind::Fs,
            None,
            None,
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
        // rs_gpu 现在应已运行。
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
        // CPU 与 GPU 处理器都应产生输出。
        assert!(r.context.get_output("rs_cpu").is_some());
        assert!(r.context.get_output("rs_gpu").is_some());
    }

    #[tokio::test]
    async fn analyze_batch_handles_non_matching_files_gracefully() {
        let mut registry = ProcessorRegistry::new();
        registry.register(Box::new(RsCpuProcessor));
        registry.register(Box::new(RsGpuProcessor));

        let engine = ProcessorEngine::new(Arc::new(registry), 4);
        // 发送 .py 文件——不应有处理器匹配。
        let files = vec![(PathBuf::from("script.py"), "import sys".to_string())];

        let results = engine.analyze_batch(files, "p".to_string(), None).await;
        assert_eq!(results.len(), 1);

        let r = &results[0];
        assert!(r.success); // 无错误即成功
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

    /// 构建一个带 tree-sitter 输出的假 FileAnalysis。
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
            FileSourceKind::Fs,
            None,
            None,
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
            errors.push("处理错误".to_string());
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
        // 应已发现 service 名
        assert!(result.services.contains(&"user-service".to_string()));
        assert!(result.services.contains(&"User".to_string())); // 来自 UserService/UserController
                                                                // 应已提取依赖
        assert!(!result.dependencies.is_empty());
        // 应有默认摘要
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
        // 应已合并所有唯一依赖
        assert!(result.projects.contains(&"service-a".to_string()));
        assert!(result.projects.contains(&"service-b".to_string()));
        assert!(result.projects.contains(&"service-c".to_string()));
        // 应已检测到共享基础设施（mysql-db 被 2 个 service 使用）
        assert!(result
            .shared_infrastructure
            .iter()
            .any(|s| s.to_lowercase().contains("mysql")));
        // 应有默认摘要
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
    // 生态系统风险检测
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn analyze_ecosystem_detects_cycles() {
        let registry = ProcessorRegistry::new();
        let engine = ProcessorEngine::new(Arc::new(registry), 4);

        // A → B → C → A 构成一个环
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

        // 应至少检测到一个循环依赖
        let cycle_risks: Vec<&String> = result
            .risks
            .iter()
            .filter(|r| r.contains("circular") || r.contains("cycle"))
            .collect();
        assert!(
            !cycle_risks.is_empty(),
            "应检测到环，但风险为: {:?}",
            result.risks
        );
    }
}
