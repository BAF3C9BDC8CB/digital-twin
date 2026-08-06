//! 流水线模板 — 构建流程的 Template Method 模式。
//!
//! `PipelineTemplate::execute()` 定义固定的构建流程：
//! 1. 扫描文件
//! 2. 通过策略选择文件
//! 3. 准备存储
//! 4. 解析文件
//! 5. 嵌入并写入向量
//! 6. 写入图谱
//! 7. 重建调用图
//! 8. 更新快照
//!
//! 子步骤（策略选择、准备）委托给 [`BuildStrategy`] trait。

use crate::domain::error::DtError;
use crate::domain::id::make_document_id;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, BuildReport, FileSnapshot, ScanConfig};
use futures::stream::{self, StreamExt};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

use super::strategy::BuildStrategy;
use crate::application::knowledge::extract::purge_document;
use crate::infrastructure::parser::ParserRegistry;
use crate::infrastructure::scanner;
use crate::infrastructure::siliconflow::SiliconFlowClient;

/// 发往 SiliconFlow 的最大并发 LLM 分析请求数。
const PHASE2_CONCURRENCY: usize = 5;

/// 默认提示词路径（当 config/prompts/code_analysis.yaml 缺失时使用）。
const PHASE2_DEFAULT_PROMPT: &str = "\
你收到的每条消息都是一段代码，而不是提问。直接分析这段代码。\n\
\n\
始终输出两行：\n\
用途：<该方法的功能，15字以内>\n\
逻辑：<实现原理，15字以内>\n\
\n\
规则：\n\
- 仅用中文，不要markdown\n\
- 不要反问、不要提示用户提供代码\n\
- 代码为空或无法识别时输出：用途：未知  逻辑：无法解析\n\
- 严格控制在两行\n\
\n\
示例：\n\
代码：def add(a, b): return a + b\n\
用途：将两个数相加并返回结果。\n\
逻辑：接收两个参数并返回它们的和。\n\
\n\
代码：public String getName() { return this.name; }\n\
用途：返回名称字段的值。\n\
逻辑：从对象属性中读取并返回name字段。\
";

/// 从所有变更文件中提取实体的结果。
pub struct ExtractionResult {
    pub methods: Vec<crate::domain::types::MethodBlock>,
    pub classes: Vec<crate::domain::types::ClassBlock>,
    pub modules: Vec<crate::domain::types::ModuleBlock>,
    pub snapshots: Vec<FileSnapshot>,
}

/// 编排构建流程的流水线模板。
pub struct PipelineTemplate {
    parser_registry: Arc<ParserRegistry>,
    batch_config: BatchConfig,
    /// 可选的 SiliconFlow 客户端，用于 Phase 2（代码语义分析）。
    siliconflow: Option<Arc<SiliconFlowClient>>,
    /// 跳过向量嵌入（设置 processors.embed=false 以保留已有向量）。
    skip_embed: bool,
}

impl PipelineTemplate {
    /// 使用给定的解析器注册表创建新的流水线模板。
    pub fn new(
        parser_registry: Arc<ParserRegistry>,
        batch_config: BatchConfig,
        siliconflow: Option<Arc<SiliconFlowClient>>,
    ) -> Self {
        Self {
            parser_registry,
            batch_config,
            siliconflow,
            skip_embed: false,
        }
    }

    /// 设置 skip_embed 标志（配置中的 processors.embed=false）。
    pub fn with_skip_embed(mut self, skip: bool) -> Self {
        self.skip_embed = skip;
        self
    }

    /// 执行完整的构建流水线。
    #[allow(clippy::too_many_arguments)]
    pub async fn execute(
        &self,
        project: &str,
        root: &Path,
        strategy: &dyn BuildStrategy,
        scan_config: &ScanConfig,
        snapshot_repo: Option<Arc<dyn SnapshotRepository>>,
        graph: Option<&dyn GraphRepository>,
        embed: Option<Arc<dyn EmbedService>>,
        vector: Option<Arc<dyn VectorRepository>>,
    ) -> Result<BuildReport, DtError> {
        let start = std::time::Instant::now();

        // 步骤 1：扫描文件
        let all_files = scanner::collect_files(root, scan_config);
        let files_scanned = all_files.len();

        // 步骤 1b：扫描文档文件
        let doc_files = scanner::collect_document_files(root, scan_config);

        // 步骤 2：通过策略选择文件
        let (files_to_process, deleted) = strategy
            .select_files(root, &all_files, snapshot_repo.as_deref(), project)
            .await?;
        let files_changed = files_to_process.len();

        // 步骤 3：删除已删除文件的数据
        if let Some(graph) = graph {
            if !deleted.is_empty() {
                delete_files_from_graph(graph, project, &deleted).await;
            }
        }

        // 步骤 3b（§6.5，任务 3）：文档生命周期 — 清理已删除的
        // 文档，保留快照基线用于变更/删除检测。
        // 文档提取/合并本身在流水线引擎中执行
        // （tree_sitter → chunk → llm → store），不在此处。
        //
        // 快照表按项目混合存放代码与文档行，因此只对文档文件集
        // 做差异比对会把每条代码路径都报为"已删除"——仅按扩展名
        // 保留真正的文档删除。
        let (changed_docs, deleted_docs) = strategy
            .select_files(root, &doc_files, snapshot_repo.as_deref(), project)
            .await?;
        let deleted_docs: Vec<String> = deleted_docs
            .into_iter()
            .filter(|p| is_document_path(p, &scan_config.document_extensions))
            .collect();

        // §6.5.2：清理已删除文档 — RELATES/MENTIONED_IN 边、
        // Document 节点与 doc_chunks 向量点。仅对成功清理的文档
        // 清除逐文件状态：清理失败（或后端缺失）时保留基线，
        // 这样删除会在下次构建时再次报告，而不是泄漏残留产物。
        if !deleted_docs.is_empty() {
            if let (Some(graph), Some(vector)) = (graph, vector.as_deref()) {
                let mut purged: Vec<String> = Vec::with_capacity(deleted_docs.len());
                for rel in &deleted_docs {
                    let doc_id = make_document_id(project, rel);
                    match purge_document(graph, vector, &doc_id).await {
                        Ok(()) => purged.push(rel.clone()),
                        Err(e) => tracing::warn!("清理已删除文档 {doc_id} 失败: {e}"),
                    }
                }
                if !purged.is_empty() {
                    if let Some(repo) = snapshot_repo.as_deref() {
                        let _ = repo.delete_file_progress(project, &purged).await;
                    }
                }
            }
        }

        // 步骤 4：准备存储（策略相关）
        strategy.prepare(graph, vector.as_deref(), project).await?;

        // 全量重建：清空所有流水线步骤进度与 LLM 进度，
        // 使增量跟踪从头开始。
        if strategy.force_rebuild() {
            if let Some(ref snap) = snapshot_repo {
                let _ = snap.clear_step_progress(project).await;
                let _ = snap.clear_llm_progress(project).await;
            }
        }

        // 步骤 5：解析文件并提取实体
        let extraction = self.extract_entities(project, root, &files_to_process)?;

        let methods_total = extraction.methods.len();
        let methods_new = methods_total;
        let classes_total = extraction.classes.len();

        // （任务 3）文档文件不再在此分块+嵌入 — 它们流经流水线
        // 引擎的提取链。本模板只处理它们的生命周期
        // （清理 + 快照基线，见步骤 3b / 步骤 9b）。

        // 步骤 6：写入图谱（方法、类、模块、关系）
        if let Some(graph) = graph {
            self.write_graph(graph, project, &extraction).await?;
        }

        // 步骤 7b：嵌入方法并写入 Qdrant（processors.embed=false 时跳过）
        if let (Some(embed_svc), Some(vector_repo)) = (&embed, &vector) {
            if self.skip_embed {
                tracing::info!("跳过嵌入步骤（processors.embed=false）— 保留已有的 Qdrant 向量");
            } else if !extraction.methods.is_empty() {
                let texts: Vec<String> = extraction
                    .methods
                    .iter()
                    .map(|m| format!("{} {}", m.signature, m.comment))
                    .collect();
                let embed_batch = self.batch_config.embed;
                let concurrent = self.batch_config.embed_concurrency;

                let chunk_pairs: Vec<(Vec<String>, Vec<crate::domain::types::MethodBlock>)> = texts
                    .chunks(embed_batch)
                    .zip(extraction.methods.chunks(embed_batch))
                    .map(|(t, m)| (t.to_vec(), m.to_vec()))
                    .collect();

                let embed_svc = embed_svc.clone();
                let embed_results: Vec<Result<Vec<serde_json::Value>, DtError>> =
                    stream::iter(chunk_pairs)
                        .map(|(text_chunk, method_chunk)| {
                            let svc = embed_svc.clone();
                            async move {
                                let embeddings = svc
                                    .embed_batch(&text_chunk)
                                    .await
                                    .map_err(|e| DtError::Repository(format!("embed: {}", e)))?;
                                let points: Vec<serde_json::Value> = method_chunk
                                    .iter()
                                    .zip(embeddings.iter())
                                    .map(|(m, vec)| {
                                        serde_json::json!({
                                            "id": m.method_id,
                                            "vector": vec,
                                            "payload": {
                                                // ---- 标识 ----
                                                "name": m.name,
                                                "signature": m.signature,
                                                "class_name": m.class_name,
                                                // ---- 位置 ----
                                                "file_path": m.file_path,
                                                "package_or_module": m.package_or_module,
                                                // ---- 技术栈 ----
                                                "language": m.language,
                                                "project": m.project,
                                                // ---- 代码范围 ----
                                                "start_line": m.start_line,
                                                "end_line": m.end_line,
                                                // ---- 签名 ----
                                                "params": m.params,
                                                "return_type": m.return_type,
                                                "calls": m.calls,
                                                "comment": m.comment,
                                                // ---- 元数据 ----
                                                "entity_id": m.method_id,
                                            }
                                        })
                                    })
                                    .collect();
                                Ok(points)
                            }
                        })
                        .buffer_unordered(concurrent)
                        .collect()
                        .await;

                let mut all_points = Vec::new();
                for result in embed_results {
                    all_points.extend(result?);
                }

                vector_repo
                    .ensure_collection(
                        crate::shared::collections::CODE_METHODS,
                        crate::shared::collections::VECTOR_DIM,
                    )
                    .await?;
                // 分批 upsert 以避免大 payload 导致 Qdrant 超时
                let upsert_batch = self.batch_config.upsert;
                for chunk in all_points.chunks(upsert_batch) {
                    vector_repo
                        .upsert(crate::shared::collections::CODE_METHODS, chunk.to_vec())
                        .await?;
                }
                tracing::info!(
                    "已向 Qdrant upsert {} 个向量（{} 个并发批次）",
                    extraction.methods.len(),
                    (extraction.methods.len() + embed_batch - 1) / embed_batch
                );
            }
        }

        // 步骤 8：重建调用图
        if let Some(graph) = graph {
            tracing::info!("正在为 {} 个方法重建调用图...", extraction.methods.len());
            self.rebuild_call_graph(graph, project, &extraction.methods)
                .await?;
            tracing::info!("调用图重建完成");
        }

        // 步骤 9：更新 SQLite 快照
        if let Some(repo) = snapshot_repo.as_deref() {
            tracing::info!("正在更新 {} 个快照...", extraction.snapshots.len());
            strategy
                .update_snapshots(repo, project, &extraction.snapshots)
                .await?;
            tracing::info!("快照更新完成");
        }

        // 步骤 9b（任务 3）：在策略的快照更新之后再保存文档快照
        // （FullRebuildStrategy 会先清空项目所有行）。
        // 这些基线让下次增量构建能够检测文档变更与删除（§6.5）。
        // 只有变更的文档需要新行 — 未变更的文档沿用上次构建的准确基线。
        if let Some(repo) = snapshot_repo.as_deref() {
            let doc_snapshots: Vec<FileSnapshot> = changed_docs
                .iter()
                .filter_map(|path| {
                    let (hash, mtime) = scanner::compute_file_hash(path).ok()?;
                    Some(FileSnapshot {
                        file_path: scanner::rel_path(root, path),
                        project: project.to_string(),
                        file_sha1: hash,
                        file_mtime: mtime,
                        method_count: 0,
                        updated_at: chrono::Utc::now().to_rfc3339(),
                    })
                })
                .collect();
            if !doc_snapshots.is_empty() {
                let _ = repo.save_snapshots(project, &doc_snapshots).await;
            }
        }

        // ── Phase 2：逐方法 LLM 分析（后台、非阻塞）──
        // LLM 分析作为后台 tokio 任务提交，使构建立即返回。
        // 任务并发处理方法，并用 llm_analysis 字段更新 Qdrant 点。
        //
        // 注意：仅当 processors.store 为 true 时才需要 embed 与 vector。
        // 当 store 为 false 时，我们仍运行 LLM 分析，但跳过 embed+upsert 步骤。
        let phase2_client_available = self.siliconflow.is_some();
        let phase2_snapshot_available = snapshot_repo.is_some();

        if let (true, true) = (phase2_client_available, phase2_snapshot_available) {
            let client = self.siliconflow.as_ref().unwrap();
            let repo = snapshot_repo.as_ref().unwrap();
            let methods = &extraction.methods;
            if !methods.is_empty() {
                let system_prompt = load_code_analysis_prompt();
                let collection = crate::shared::collections::CODE_METHODS.to_string();

                // 构建任务列表：跳过已用相同源码哈希分析过的方法
                let mut jobs: Vec<(crate::domain::types::MethodBlock, String)> = Vec::new();
                for m in methods {
                    let mut source_text = m.source_text.clone();
                    if source_text.len() < 10 {
                        let fp = std::path::Path::new(&m.file_path);
                        if let Ok(content) = std::fs::read_to_string(fp) {
                            source_text = content;
                        }
                    }
                    let mut hasher = Sha256::new();
                    hasher.update(source_text.as_bytes());
                    let hash = format!("{:x}", hasher.finalize());
                    // Nacos 虚拟文件使用 nacos:{data_id} 作为去重 key；
                    // 普通文件使用 method:{method_id} 保持兼容。
                    let prog_key = if m.file_path.starts_with("dt://nacos/") {
                        // 从 virtual_path 提取 data_id：dt://nacos/{ns}/{data_id}.yaml
                        let data_id = m
                            .file_path
                            .strip_prefix("dt://nacos/")
                            .and_then(|s| s.split('/').nth(1))
                            .and_then(|s| s.strip_suffix(".yaml"))
                            .unwrap_or(&m.method_id);
                        format!("nacos:{}", data_id)
                    } else {
                        format!("method:{}", m.method_id)
                    };
                    if repo
                        .is_llm_analyzed(project, &prog_key, &hash)
                        .await
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let mut m2 = m.clone();
                    m2.source_text = source_text;
                    jobs.push((m2, hash));
                }

                let total = jobs.len();
                let skipped = methods.len() - total;
                tracing::info!(
                    "Phase 2: {} 个待分析, {} 个已是最新（后台、非阻塞）",
                    total,
                    skipped,
                );

                if total > 0 {
                    // 派生后台任务 — 构建立即返回
                    // 当 processors.store 为 false 时，embed_svc 与 vector_repo 可能为 None
                    let client_cloned = client.clone();
                    let repo_cloned = repo.clone();
                    let proj = project.to_string();
                    let embed_svc_opt = embed.clone();
                    let vector_repo_opt = vector.clone();

                    tokio::spawn(async move {
                        tracing::info!("Phase 2 后台工作线程已启动: {} 个方法", total);

                        let results: Vec<(String, bool)> = stream::iter(
                            jobs.into_iter().map(|(method, hash)| {
                                let cli = client_cloned.clone();
                                let repo_snap = repo_cloned.clone();
                                let embed_svc = embed_svc_opt.clone();
                                let vector_repo = vector_repo_opt.clone();
                                let sp = system_prompt.clone();
                                let coll = collection.clone();
                                let proj = proj.clone();
                                async move {
                                    let method_name = method.name.clone();
                                    let method_id = method.method_id.clone();

                                    match cli.chat(&sp, &method.source_text, 0.1, 100).await {
                                        Ok(llm_response) => {
                                            let _ = repo_snap
                                                .mark_llm_analyzed(&proj, &format!("method:{}", method_id), &hash)
                                                .await;

                                            // 仅当启用 store 时（embed_svc 与 vector_repo 可用）才嵌入并 upsert
                                            if let (Some(svc), Some(repo_vec)) = (embed_svc.as_ref(), vector_repo.as_ref()) {
                                                match svc.embed_batch(&[llm_response.clone()]).await {
                                                    Ok(embeddings) => {
                                                        if let Some(vec) = embeddings.first() {
                                                            let point = serde_json::json!({
                                                                "id": method_id,
                                                                "vector": vec,
                                                                "payload": {
                                                                    "name": method.name,
                                                                    "signature": method.signature,
                                                                    "class_name": method.class_name,
                                                                    "file_path": method.file_path,
                                                                    "package_or_module": method.package_or_module,
                                                                    "language": method.language,
                                                                    "project": method.project,
                                                                    "start_line": method.start_line,
                                                                    "end_line": method.end_line,
                                                                    "params": method.params,
                                                                    "return_type": method.return_type,
                                                                    "calls": method.calls,
                                                                    "comment": method.comment,
                                                                    "entity_id": method.method_id,
                                                                    "llm_analysis": llm_response,
                                                                }
                                                            });
                                                            if let Err(e) = repo_vec.upsert(&coll, vec![point]).await {
                                                                tracing::warn!("Phase 2 upsert 失败 {}: {}", method_name, e);
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("Phase 2 embed 失败 {}: {}", method_name, e);
                                                    }
                                                }
                                            } else {
                                                // store=false：跳过 embed+upsert，仅记录 LLM 响应
                                                tracing::debug!("Phase 2 LLM 完成（未存储）{}: {}", method_name, llm_response.chars().take(50).collect::<String>());
                                            }

                                            tracing::info!("Phase 2 完成 {}", method_name);
                                            (method_name, true)
                                        }
                                        Err(e) => {
                                            tracing::warn!("Phase 2 失败 {}: {}", method_name, e);
                                            (method_name, false)
                                        }
                                    }
                                }
                            }),
                        )
                        .buffer_unordered(PHASE2_CONCURRENCY)
                        .collect::<Vec<_>>()
                        .await;

                        let analyzed = results.iter().filter(|(_, ok)| *ok).count();
                        tracing::info!(
                            "Phase 2 后台任务完成: {} 个已分析, {} 个已是最新, {} 个错误",
                            analyzed,
                            skipped,
                            total - analyzed,
                        );
                    });

                    tracing::info!("Phase 2: 已提交 {} 个方法进行后台 LLM 分析", total);
                }
            }
        }

        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(BuildReport {
            project: project.to_string(),
            files_scanned,
            files_changed,
            methods_total,
            methods_new,
            classes_total,
            elapsed_ms,
        })
    }

    /// 从一批文件中提取实体（方法、类、模块）。
    /// 使用多线程并行执行文件 I/O 与解析。
    fn extract_entities(
        &self,
        project: &str,
        root: &Path,
        files: &[std::path::PathBuf],
    ) -> Result<ExtractionResult, DtError> {
        // 预读所有文件内容，将 I/O 提到函数入口
        let file_entries: Vec<(String, String, String, f64)> = files
            .iter()
            .filter_map(|fp| {
                let rel_path = scanner::rel_path(root, fp);
                let (file_hash, file_mtime) =
                    scanner::compute_file_hash(fp).unwrap_or_default();
                let source = std::fs::read_to_string(fp).ok()?;
                Some((rel_path, source, file_hash, file_mtime))
            })
            .collect();
        self.extract_entities_content(project, &file_entries)
    }

    /// 从预读内容中提取实体（方法、类、模块）—— F3 要求。
    ///
    /// 参数为 `(相对路径, 文件内容, SHA1 哈希, mtime)` 元组切片，
    /// 不调用 `fs::read_to_string`。对于 `source == Fs`，调用方
    /// 预读磁盘；对于远程源（Nacos/Jenkins），内容由 API 直接提供。
    fn extract_entities_content(
        &self,
        project: &str,
        file_entries: &[(String, String, String, f64)],
    ) -> Result<ExtractionResult, DtError> {
        let all_methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_classes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let all_snapshots = Arc::new(std::sync::Mutex::new(Vec::new()));
        let module_set = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let chunk_size = (file_entries.len() + num_threads - 1) / num_threads;
        let registry = self.parser_registry.clone();

        std::thread::scope(|s| {
            for entry_chunk in file_entries.chunks(chunk_size.max(1)) {
                let registry = registry.clone();
                let project = project.to_string();
                let chunk = entry_chunk.to_vec();
                let methods = all_methods.clone();
                let classes = all_classes.clone();
                let snapshots = all_snapshots.clone();
                let modules = module_set.clone();
                s.spawn(move || {
                    for (rel_path, source, file_hash, file_mtime) in &chunk {
                        let result = match registry.parse_file(source, std::path::Path::new(rel_path), &project) {
                            Ok(r) => r,
                            Err(_) => {
                                snapshots.lock().unwrap().push(FileSnapshot {
                                    file_path: rel_path.clone(),
                                    project: project.clone(),
                                    file_sha1: file_hash.clone(),
                                    file_mtime: *file_mtime,
                                    method_count: 0,
                                    updated_at: chrono::Utc::now().to_rfc3339(),
                                });
                                continue;
                            }
                        };
                        let method_count = result.methods.len() as u32;
                        for m in &result.methods {
                            if !m.package_or_module.is_empty() {
                                modules.lock().unwrap().insert(m.package_or_module.clone());
                            }
                        }
                        for c in &result.classes {
                            if !c.package_or_module.is_empty() {
                                modules.lock().unwrap().insert(c.package_or_module.clone());
                            }
                        }
                        methods.lock().unwrap().extend(result.methods);
                        classes.lock().unwrap().extend(result.classes);
                        snapshots.lock().unwrap().push(FileSnapshot {
                            file_path: rel_path.clone(),
                            project: project.clone(),
                            file_sha1: file_hash.clone(),
                            file_mtime: *file_mtime,
                            method_count,
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                });
            }
        });

        let all_methods = Arc::try_unwrap(all_methods).unwrap().into_inner().unwrap();
        let all_classes = Arc::try_unwrap(all_classes).unwrap().into_inner().unwrap();
        let all_snapshots = Arc::try_unwrap(all_snapshots)
            .unwrap()
            .into_inner()
            .unwrap();
        let module_set = Arc::try_unwrap(module_set).unwrap().into_inner().unwrap();

        let modules: Vec<crate::domain::types::ModuleBlock> = module_set
            .into_iter()
            .map(|name| crate::domain::types::ModuleBlock {
                module_id: crate::domain::id::make_module_id(project, &name),
                name,
                project: project.to_string(),
            })
            .collect();

        Ok(ExtractionResult {
            methods: all_methods,
            classes: all_classes,
            modules,
            snapshots: all_snapshots,
        })
    }

    /// 将方法、类、模块及 CONTAINS 关系写入图数据库。
    async fn write_graph(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        extraction: &ExtractionResult,
    ) -> Result<(), DtError> {
        use std::collections::HashMap;

        let batch = self.batch_config.clone();
        let unwind = batch.unwind;

        // ---- 步骤 0：确保 Project 节点存在 ----
        {
            let lang = extraction
                .methods
                .first()
                .map(|m| m.language.as_str())
                .or_else(|| extraction.classes.first().map(|_| "unknown"))
                .unwrap_or("unknown");
            let project_type = infer_project_type(project);

            let mut params = HashMap::new();
            params.insert(
                "name".to_string(),
                serde_json::Value::String(project.to_string()),
            );
            params.insert(
                "language".to_string(),
                serde_json::Value::String(lang.to_string()),
            );
            params.insert(
                "project_type".to_string(),
                serde_json::Value::String(project_type.to_string()),
            );

            graph
                .write_query(
                    r#"MERGE (p:Project {name: $name})
                    SET p.language = $language,
                        p.project_type = $project_type"#,
                    params,
                )
                .await?;
        }

        // 并行写入方法、类与模块
        {
            let methods = &extraction.methods;
            let classes = &extraction.classes;
            let modules = &extraction.modules;

            let write_methods = async {
                for chunk in methods.chunks(unwind) {
                    let methods_json: Vec<serde_json::Value> = chunk.iter().map(|m| serde_json::json!({
                        "method_id": m.method_id, "project": m.project, "file_path": m.file_path,
                        "language": m.language, "package_or_module": m.package_or_module,
                        "class_name": m.class_name, "name": m.name, "signature": m.signature,
                        "params": m.params, "return_type": m.return_type,
                        "start_line": m.start_line, "end_line": m.end_line,
                        "calls": m.calls, "comment": m.comment, "source_text": m.source_text,
                    })).collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "methods".to_string(),
                        serde_json::Value::Array(methods_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $methods AS m
                            MERGE (n:Method {method_id: m.method_id})
                            SET n.project = m.project, n.file_path = m.file_path,
                                n.language = m.language, n.package_or_module = m.package_or_module,
                                n.class_name = m.class_name, n.name = m.name,
                                n.signature = m.signature, n.params = m.params,
                                n.return_type = m.return_type, n.start_line = m.start_line,
                                n.end_line = m.end_line, n.calls = m.calls, n.comment = m.comment
                            WITH n, m
                            MERGE (p:Project {name: m.project})
                            MERGE (n)-[:BELONGS_TO]->(p)"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let write_classes = async {
                for chunk in classes.chunks(unwind) {
                    let classes_json: Vec<serde_json::Value> = chunk.iter().map(|c| serde_json::json!({
                        "class_id": c.class_id, "name": c.name, "kind": c.kind.as_str(),
                        "file_path": c.file_path, "package_or_module": c.package_or_module,
                        "project": c.project, "start_line": c.start_line, "end_line": c.end_line,
                    })).collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "classes".to_string(),
                        serde_json::Value::Array(classes_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $classes AS c
                            MERGE (n:Class {class_id: c.class_id})
                            SET n.name = c.name, n.kind = c.kind, n.file_path = c.file_path,
                                n.package_or_module = c.package_or_module, n.project = c.project,
                                n.start_line = c.start_line, n.end_line = c.end_line
                            WITH n, c
                            MERGE (p:Project {name: c.project})
                            MERGE (n)-[:BELONGS_TO]->(p)"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let write_modules = async {
                for chunk in modules.chunks(unwind) {
                    let modules_json: Vec<serde_json::Value> = chunk
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "module_id": m.module_id, "name": m.name, "project": m.project,
                            })
                        })
                        .collect();
                    let mut params = HashMap::new();
                    params.insert(
                        "modules".to_string(),
                        serde_json::Value::Array(modules_json),
                    );
                    graph
                        .write_query(
                            r#"UNWIND $modules AS m
                            MERGE (n:Module {module_id: m.module_id})
                            SET n.name = m.name, n.project = m.project"#,
                            params,
                        )
                        .await?;
                }
                Ok::<_, DtError>(())
            };

            let (r1, r2, r3) = tokio::join!(write_methods, write_classes, write_modules);
            r1?;
            r2?;
            r3?;
        }

        // 写入 CONTAINS 关系（依赖已写入的方法与类）
        for c in &extraction.classes {
            for mid in &c.method_ids {
                let mut params = HashMap::new();
                params.insert(
                    "class_id".to_string(),
                    serde_json::Value::String(c.class_id.clone()),
                );
                params.insert(
                    "method_id".to_string(),
                    serde_json::Value::String(mid.clone()),
                );
                let _ = graph
                    .write_query(
                        "MATCH (c:Class {class_id: $class_id}) \
                         MATCH (m:Method {method_id: $method_id}) \
                         MERGE (c)-[:CONTAINS]->(m)",
                        params,
                    )
                    .await;
            }
        }

        Ok(())
    }

    /// 重建项目中所有方法的 CALLS 关系。
    async fn rebuild_call_graph(
        &self,
        graph: &dyn GraphRepository,
        project: &str,
        methods: &[crate::domain::types::MethodBlock],
    ) -> Result<(), DtError> {
        use std::collections::HashMap;

        let file_paths: Vec<String> = {
            let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
            for m in methods {
                set.insert(m.file_path.clone());
            }
            set.into_iter().collect()
        };

        let file_paths_json: Vec<serde_json::Value> = file_paths
            .iter()
            .map(|f| serde_json::Value::String(f.clone()))
            .collect();

        let mut params = HashMap::new();
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
        params.insert(
            "files".to_string(),
            serde_json::Value::Array(file_paths_json),
        );

        let _ = graph
            .write_query(
                r#"MATCH (caller:Method {project: $project})
                WHERE caller.file_path IN $files
                WITH caller
                UNWIND caller.calls AS called_name
                MATCH (callee:Method {project: $project, name: called_name})
                WHERE callee.method_id <> caller.method_id
                MERGE (caller)-[:CALLS]->(callee)"#,
                params,
            )
            .await;

        Ok(())
    }
}

/// 为一组已删除文件删除方法节点及其关系。
async fn delete_files_from_graph(graph: &dyn GraphRepository, project: &str, files: &[String]) {
    let files_json: Vec<serde_json::Value> = files
        .iter()
        .map(|f| serde_json::Value::String(f.clone()))
        .collect();

    let mut params = std::collections::HashMap::new();
    params.insert(
        "project".to_string(),
        serde_json::Value::String(project.to_string()),
    );
    params.insert("files".to_string(), serde_json::Value::Array(files_json));

    let _ = graph
        .write_query(
            "MATCH (m:Method {project: $project}) \
             WHERE m.file_path IN $files \
             DETACH DELETE m",
            params,
        )
        .await;
}

// ---------------------------------------------------------------------------
// 文档生命周期辅助函数
// ---------------------------------------------------------------------------

/// 判断项目相对路径是否为 `ScanConfig::document_extensions` 定义的文档。
///
/// 防止策略的 `deleted` 输出被代码路径污染：快照表按项目混合存放
/// 代码与文档行，因此仅对文档文件集做差异比对时，每条代码路径
/// 都会显示为"已删除"。
fn is_document_path(
    rel_path: &str,
    document_extensions: &std::collections::HashSet<String>,
) -> bool {
    Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| document_extensions.contains(e))
        .unwrap_or(false)
}

/// 根据项目名称推断人类可读的项目类型标签。
///
/// 基于常见命名约定的简单启发式规则
/// （如 `api-gateway` → `"微服务 — API 网关"`、`yimeng-website` → `"前端 — Web 应用"`）。
fn infer_project_type(project: &str) -> &str {
    let lower = project.to_lowercase();
    if lower.contains("gateway") {
        return "微服务 — API 网关";
    }
    if lower.contains("website") || lower.contains("-h5") {
        return "前端 — Web 应用";
    }
    if lower.contains("hospital")
        || lower.contains("doctor")
        || lower.contains("nurse")
        || lower.contains("med")
    {
        return "微服务 — 医疗业务";
    }
    if lower.contains("pay")
        || lower.contains("charge")
        || lower.contains("cashier")
        || lower.contains("settlement")
        || lower.contains("order")
    {
        return "微服务 — 支付/交易";
    }
    if lower.contains("content") {
        return "微服务 — 内容中台";
    }
    if lower.contains("data") || lower.contains("report") || lower.contains("statistics") {
        return "微服务 — 数据/报表";
    }
    if lower.contains("log") {
        return "微服务 — 日志/监控";
    }
    if lower.contains("warehouse") || lower.contains("goods") || lower.contains("inventory") {
        return "微服务 — 仓储/物流";
    }
    if lower.contains("user") || lower.contains("auth") || lower.contains("oauth") {
        return "微服务 — 用户/认证";
    }
    if lower.contains("message") || lower.contains("sms") || lower.contains("im-") {
        return "微服务 — 消息/通知";
    }
    if lower.contains("admin") || lower.contains("boss") {
        return "前端 — 管理后台";
    }
    if lower.contains("saas") {
        return "微服务 — SaaS";
    }
    if lower.contains("search") {
        return "微服务 — 搜索";
    }
    if lower.contains("config") || lower.contains("cache") {
        return "微服务 — 基础设施";
    }
    if lower.contains("label") || lower.contains("comment") || lower.contains("app-") {
        return "微服务 — 业务支撑";
    }
    if lower.contains("-api") || lower.contains("api-") {
        return "微服务 — API 层";
    }
    if lower.contains("center") {
        return "微服务 — 业务中台";
    }
    "微服务"
}

/// 从 `config/prompts/code_analysis.yaml` 加载 Phase 2 代码分析的系统提示词。
/// 文件缺失时回退到硬编码的默认提示词。
fn load_code_analysis_prompt() -> String {
    let paths = [
        std::env::var("HOME").ok().map(|h| {
            std::path::PathBuf::from(h)
                .join(".config")
                .join("digital-twin")
                .join("prompts")
                .join("code_analysis.yaml")
        }),
        Some(std::path::PathBuf::from(
            "config/prompts/code_analysis.yaml",
        )),
    ];
    for path in paths.iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(path) {
            use serde::Deserialize;
            #[derive(Deserialize)]
            struct Prompt {
                system: Option<String>,
            }
            if let Ok(p) = serde_yaml::from_str::<Prompt>(&content) {
                if let Some(s) = p.system {
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    PHASE2_DEFAULT_PROMPT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_can_be_created() {
        let registry = Arc::new(ParserRegistry::new());
        let _pipeline = PipelineTemplate::new(registry, BatchConfig::default(), None);
    }

    #[test]
    fn is_document_path_matches_scan_config_extensions() {
        let exts = ScanConfig::default().document_extensions;
        assert!(is_document_path("docs/guide.md", &exts));
        assert!(is_document_path("config.yaml", &exts));
        assert!(is_document_path("a/b/c.properties", &exts));
        // 混合快照表报为"已删除"的代码路径会被过滤掉 —
        // 它们从未有需要清理的 Document 节点。
        assert!(!is_document_path("src/main.rs", &exts));
        assert!(!is_document_path("Service.java", &exts));
        assert!(!is_document_path("script.py", &exts));
        assert!(!is_document_path("no_extension", &exts));
    }
}
