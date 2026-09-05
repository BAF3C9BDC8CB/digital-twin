//! dt-mcp — digital-twin 的 MCP server(Rust 实现, 9 个 dt_* 工具)
//!
//! 直接调用 `digital_twin::interfaces::cli` 的 handler 与 `runtime::DtRuntime`,
//! 不再走 CLI 子进程。工具名与参数 schema 对齐旧 mcp-server.py:
//!   dt_search / dt_sense / dt_memorize / dt_event / dt_learn / dt_build
//!   / dt_kg_sync / dt_health / dt_backup
//!
//! dt_search 是统一检索入口(对应 CLI `dt search`), 融合原 dt_router 的
//! L0 拦截 + 意图路由 + LLM 过滤 与原 dt_search_kg 的 KG 优先语义。
//! 旧工具名 dt_router / dt_search_kg 在 call 层保留为兼容别名(list_tools
//! 不再暴露), 存量调用自动转发到同一实现。
//!
//! 输出适配: CLI handler 直接 `println!` 到 stdout, 而 MCP server 的 stdout
//! 是 JSON-RPC 协议通道 —— 因此 call_tool 内把 stdout 重定向到临时文件,
//! handler 执行完恢复 stdout 再读回内容作为工具结果(dup2 方案,
//! 与 svc 的 redirect_stderr 同思路)。

use std::future::Future;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::pin::Pin;
use std::sync::Arc;

use digital_twin::application::hooks::HookContext;
use digital_twin::runtime::DtRuntime;
use mcp_server::router::{CapabilitiesBuilder, RouterService};
use mcp_server::{ByteTransport, Router, Server};
use mcp_spec::content::Content;
use mcp_spec::handler::{PromptError, ResourceError, ToolError};
use mcp_spec::prompt::Prompt;
use mcp_spec::protocol::ServerCapabilities;
use mcp_spec::resource::Resource;
use mcp_spec::tool::Tool;
use serde_json::Value;

// ---- stdout 捕获 ---------------------------------------------------

/// 把 stdout 重定向到临时文件执行 `f`, 恢复后返回捕获的输出。
/// handler 的 `println!` 输出全部落入文件, 不污染 MCP 协议通道。
#[cfg(unix)]
fn capture_stdout<F>(f: F) -> String
where
    F: FnOnce() -> anyhow::Result<()>,
{
    let path = std::env::temp_dir().join(format!("dt-mcp-{}.out", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .expect("open capture file");
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    unsafe {
        libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO);
    }
    let result = f();
    unsafe {
        libc::dup2(saved, libc::STDOUT_FILENO);
        libc::close(saved);
    }
    drop(file);
    let mut output = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if let Err(e) = result {
        output.push_str(&format!("\n[dt-mcp 错误: {e}]\n"));
    }
    output
}

/// Windows 版 stdout 捕获：把文件 HANDLE 转为 CRT 文件描述符（`_open_osfhandle`），
/// 再用与 Unix 相同的 `dup2` 重定向 stdout。
#[cfg(windows)]
fn capture_stdout<F>(f: F) -> String
where
    F: FnOnce() -> anyhow::Result<()>,
{
    let path = std::env::temp_dir().join(format!("dt-mcp-{}.out", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .expect("open capture file");
    let fd = unsafe { libc::open_osfhandle(file.as_raw_handle() as libc::intptr_t, 0) };
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    unsafe {
        libc::dup2(fd, libc::STDOUT_FILENO);
    }
    let result = f();
    unsafe {
        libc::dup2(saved, libc::STDOUT_FILENO);
        libc::close(saved);
    }
    unsafe { libc::close(fd) };
    // fd 已接管文件句柄，避免 File drop 时二次 CloseHandle
    std::mem::forget(file);
    let mut output = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if let Err(e) = result {
        output.push_str(&format!("\n[dt-mcp 错误: {e}]\n"));
    }
    output
}

// ---- Router --------------------------------------------------------

#[derive(Clone)]
struct DtRouter {
    rt: Arc<DtRuntime>,
}

impl DtRouter {
    async fn call(&self, tool_name: &str, arguments: Value) -> Result<String, ToolError> {
        let rt = &self.rt;
        match tool_name {
            // 统一检索入口：dt_search（融合 dt_router 智能路由 + dt_search 裸检索 + dt_search_kg KG 优先）。
            // 旧名 dt_router / dt_search_kg 保留为兼容别名（list_tools 不再暴露，但存量调用不报错）：
            //   dt_router    → 同 dt_search（world 默认 all，智能路由 + 可选 LLM 过滤）
            //   dt_search_kg → world 缺省时默认 knowledge（KG 优先语义）
            "dt_search" | "dt_router" | "dt_search_kg" => {
                let query = str_arg(&arguments, "query", true)?.unwrap_or_default();
                let legacy_kg = tool_name == "dt_search_kg";
                let default_world = if legacy_kg { "knowledge" } else { "all" };
                let world = str_arg(&arguments, "world", false)?
                    .unwrap_or_else(|| default_world.to_string());
                let project = str_arg(&arguments, "project", false)?;
                let limit = int_arg(&arguments, "limit")?.unwrap_or(10) as usize;
                let enable_filter = match str_arg(&arguments, "filter", false)? {
                    Some(v) if v == "true" => Some(true),
                    Some(v) if v == "false" => Some(false),
                    _ => None, // 跟随配置 kg_router.result_filter.enabled
                };
                let threshold = str_arg(&arguments, "threshold", false)?
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0); // <=0 表示沿用配置
                let file_type = str_arg(&arguments, "file_type", false)?;
                let content_type = str_arg(&arguments, "content_type", false)?;
                let show_content = bool_arg(&arguments, "show_content")?.unwrap_or(false);
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(async {
                            digital_twin::interfaces::cli::router::handle_router_search(
                                &query,
                                &world,
                                limit,
                                true,
                                &project,
                                enable_filter,
                                threshold,
                                false, // explain: MCP 内不需要路由决策打印
                                file_type,
                                content_type,
                                show_content,
                            )
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                            Ok(())
                        })
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_sense" => {
                let path = str_arg(&arguments, "path", false)?.map(std::path::PathBuf::from);
                let roots = rt.roots.clone();
                let g = rt.graph.clone();
                let v = rt.vector.clone();
                let snap = rt.snapshot.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::sense::handle_sense(
                                path, true, roots, g, v, snap, None,
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_memorize" => {
                let knowledge_type = str_arg(&arguments, "type", true)?.unwrap_or_default();
                let entity_id = str_arg(&arguments, "entity_id", true)?.unwrap_or_default();
                let entity_type = str_arg(&arguments, "entity_type", false)?;
                let project = str_arg(&arguments, "project", false)?;
                let details = str_arg(&arguments, "details", true)?.unwrap_or_default();
                let action =
                    str_arg(&arguments, "action", false)?.unwrap_or_else(|| "write".into());
                let supersede = str_arg(&arguments, "supersede", false)?;
                let g = rt.graph.clone();
                let acc = rt.sync_acc.clone();
                let v = rt.vector.clone();
                let e = rt.embed.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::memorize::handle_memorize(
                                knowledge_type,
                                entity_id,
                                entity_type,
                                project,
                                details,
                                g,
                                acc,
                                Some(action),
                                supersede,
                                v,
                                e,
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_event" => {
                // Python 版 dt_event 传 --type/--entity-id 给 `dt event`(该命令只接受
                // hook_name+context), 实际是坏的。这里修正为: 把事件字段组装成
                // HookContext JSON, 触发 mcp_event hook。
                let event_type = str_arg(&arguments, "type", true)?.unwrap_or_default();
                let entity_id = str_arg(&arguments, "entity_id", true)?.unwrap_or_default();
                let entity_type = str_arg(&arguments, "entity_type", false)?
                    .unwrap_or_else(|| "Event".to_string());
                let project = str_arg(&arguments, "project", false)?.unwrap_or_default();
                let details = str_arg(&arguments, "details", true)?.unwrap_or_default();
                let ctx = serde_json::json!({
                    "hook_name": "mcp_event",
                    "project": project,
                    "session_id": "mcp",
                    "entity_id": entity_id,
                    "entity_type": entity_type,
                    "details": details,
                    "event_type": event_type,
                });
                let engine = rt.hook_engine.clone();
                let bridge = rt.kg_bridge.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::event::handle_event(
                                "mcp_event".to_string(),
                                ctx.to_string(),
                                engine,
                                bridge,
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_learn" => {
                let task = str_arg(&arguments, "task", true)?.unwrap_or_default();
                let entities = split_list(str_arg(&arguments, "entities", false)?);
                let pattern = str_arg(&arguments, "pattern", false)?;
                let pitfalls = split_list(str_arg(&arguments, "pitfalls", false)?);
                let decisions = split_list(str_arg(&arguments, "decisions", false)?);
                let thread_id = str_arg(&arguments, "thread_id", false)?;
                let success = bool_arg(&arguments, "success")?;
                let project = str_arg(&arguments, "project", false)?;
                let g = rt.graph.clone();
                let acc = rt.sync_acc.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::learn::handle_learn(
                                task, entities, pattern, pitfalls, decisions, thread_id, success,
                                project, g, acc,
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_build" => {
                let all = bool_arg(&arguments, "all")?.unwrap_or(false);
                let full = bool_arg(&arguments, "full")?.unwrap_or(false);
                let name = str_arg(&arguments, "name", false)?;
                let g = rt.graph.clone();
                let v = rt.vector.clone();
                let e = rt.embed.clone();
                let snap = rt.snapshot.clone();
                let batch = rt.batch_config.clone();
                let scan = rt.scan_config.clone();
                if all {
                    let roots = rt.roots.clone();
                    let out = tokio::task::spawn_blocking(move || {
                        capture_stdout(move || {
                            tokio::runtime::Runtime::new().unwrap().block_on(
                                digital_twin::interfaces::cli::build::handle_build_all(
                                    roots,
                                    full,
                                    true,
                                    true,
                                    g,
                                    v,
                                    e,
                                    snap,
                                    batch.expect("config.yaml batch 缺失"),
                                    scan.expect("scan 配置缺失"),
                                ),
                            )
                        })
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                    Ok(out)
                } else {
                    let path = str_arg(&arguments, "path", true)?.unwrap_or_default();
                    let path = std::path::PathBuf::from(path);
                    let out = tokio::task::spawn_blocking(move || {
                        capture_stdout(move || {
                            tokio::runtime::Runtime::new().unwrap().block_on(
                                digital_twin::interfaces::cli::build::handle_build(
                                    path,
                                    name,
                                    None,
                                    full,
                                    true,
                                    true,
                                    g,
                                    v,
                                    e,
                                    snap,
                                    batch.expect("config.yaml batch 缺失"),
                                    scan.expect("scan 配置缺失"),
                                ),
                            )
                        })
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                    Ok(out)
                }
            }
            "dt_kg_sync" => {
                let config_chunks = bool_arg(&arguments, "config_chunks")?.unwrap_or(false);
                let g = rt.graph.clone();
                let q = rt.queue.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::sync::handle_kg_sync(
                                false,
                                None,
                                config_chunks,
                                g,
                                q,
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_health" => {
                let g = rt.graph.clone();
                let v = rt.vector.clone();
                let snap = rt.snapshot.clone();
                let e = rt.embed.clone();
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        tokio::runtime::Runtime::new().unwrap().block_on(
                            digital_twin::interfaces::cli::cleanup::run_health(
                                g.as_deref(),
                                v.as_deref(),
                                snap.as_deref(),
                                e.as_deref(),
                            ),
                        )
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            "dt_backup" => {
                let action =
                    str_arg(&arguments, "action", false)?.unwrap_or_else(|| "backup".to_string());
                let date = str_arg(&arguments, "date", false)?;
                let out = tokio::task::spawn_blocking(move || {
                    capture_stdout(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        match action.as_str() {
                            "list" => {
                                let entries = rt.block_on(
                                    digital_twin::interfaces::cli::backup::list_backups(),
                                )?;
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&entries).unwrap_or_default()
                                );
                            }
                            "restore" => {
                                let d =
                                    date.ok_or_else(|| anyhow::anyhow!("restore 需要 date 参数"))?;
                                rt.block_on(
                                    digital_twin::interfaces::cli::backup::restore_backup(&d),
                                )?;
                                println!("恢复完成: {d}");
                            }
                            "verify" => {
                                let d =
                                    date.ok_or_else(|| anyhow::anyhow!("verify 需要 date 参数"))?;
                                let report = rt.block_on(
                                    digital_twin::interfaces::cli::backup::verify_backup_files(&d),
                                )?;
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&report).unwrap_or_default()
                                );
                            }
                            _ => {
                                let report = rt.block_on(
                                    digital_twin::interfaces::cli::backup::create_backup(),
                                )?;
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&report).unwrap_or_default()
                                );
                            }
                        }
                        Ok(())
                    })
                })
                .await
                .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

                Ok(out)
            }
            _ => Err(ToolError::NotFound(format!("Tool {tool_name} not found"))),
        }
    }
}

// ---- 参数解析助手 ----------------------------------------------------

fn str_arg(arguments: &Value, key: &str, required: bool) -> Result<Option<String>, ToolError> {
    match arguments.get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => {
            if required {
                Err(ToolError::InvalidParameters(format!("缺少必填参数 {key}")))
            } else {
                Ok(None)
            }
        }
        Some(v) => Err(ToolError::InvalidParameters(format!(
            "参数 {key} 应为字符串, 实际: {v}"
        ))),
    }
}

fn int_arg(arguments: &Value, key: &str) -> Result<Option<u64>, ToolError> {
    match arguments.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidParameters(format!("参数 {key} 应为整数"))),
        Some(Value::Null) | None => Ok(None),
        Some(v) => Err(ToolError::InvalidParameters(format!(
            "参数 {key} 应为整数, 实际: {v}"
        ))),
    }
}

fn bool_arg(arguments: &Value, key: &str) -> Result<Option<bool>, ToolError> {
    match arguments.get(key) {
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::Null) | None => Ok(None),
        Some(v) => Err(ToolError::InvalidParameters(format!(
            "参数 {key} 应为布尔, 实际: {v}"
        ))),
    }
}

fn split_list(v: Option<String>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

// ---- Router trait 实现 ----------------------------------------------

impl Router for DtRouter {
    fn name(&self) -> String {
        "digital-twin".to_string()
    }

    fn instructions(&self) -> String {
        "Digital Twin 知识图谱工具: 搜索(KG/代码/文档)、感知、知识写入(memorize/learn/event)、构建、健康检查、备份。".to_string()
    }

    fn capabilities(&self) -> ServerCapabilities {
        CapabilitiesBuilder::new().with_tools(true).build()
    }

    fn list_tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "dt_search".to_string(),
                "统一检索入口(融合 dt_router 智能路由 + dt_search 裸检索 + dt_search_kg KG 优先): L0 闲聊/无锚点拦截 + 意图识别路由 + 可选 LLM 过滤, 跨 code/doc/knowledge/config/memory。查代码/文件/文档/记忆/配置统一用它; 命中先读取确认, 0 命中才读源码。参数: query/world(默认all)/project/limit(默认10)/filter(可选 true|false)/threshold/file_type/content_type/show_content。已知精确方法/类名直接作为 query 触发精确匹配; 查知识图谱记忆用 world=knowledge".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "自然语言搜索关键词/定位目标"},
                        "world": {"type": "string", "description": "检索世界: all|code|knowledge|doc|config|memory, 默认 all(跨世界)"},
                        "project": {"type": "string", "description": "限定项目名(查代码/文档必带)"},
                        "limit": {"type": "integer", "description": "返回数量, 默认 10"},
                        "filter": {"type": "string", "description": "LLM 相关性过滤: true 强制开 / false 强制关 / 省略跟随配置"},
                        "threshold": {"type": "string", "description": "过滤阈值 0-1, 默认跟随配置 0.6"},
                        "file_type": {"type": "string", "description": "按文件类型过滤: document/code/config 或后缀 md/yaml/rs…"},
                        "content_type": {"type": "string", "description": "按实体类型过滤: Method/Class/Config/Service…"},
                        "show_content": {"type": "boolean", "description": "展开命中正文原文块"}
                    },
                    "required": ["query"]
                }),
            ),
            Tool::new(
                "dt_sense".to_string(),
                "环境感知: 定位目录所属代码根(root), 返回项目简报(统计/目录画像/语言/关键实体)".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "目标目录, 缺省为当前工作目录"}},
                    "required": []
                }),
            ),
            Tool::new(
                "dt_memorize".to_string(),
                "写入知识节点到KG(架构决策/用户说记住)。type: Decision/KnowledgeAdded/Environment/Dependencies。action: write(默认)/delete/update——delete 删除记忆(图+向量), update 版本化更新(配 supersede 指定旧ID)".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "description": "知识类型"},
                        "entity_id": {"type": "string", "description": "唯一标识"},
                        "entity_type": {"type": "string", "description": "实体类型"},
                        "project": {"type": "string", "description": "所属项目"},
                        "details": {"type": "string", "description": "详细内容"},
                        "action": {"type": "string", "description": "write(默认)|delete|update/supersede。AI 验证记忆失效后: 完全无用→delete, 部分过时→update+supersede"},
                        "supersede": {"type": "string", "description": "版本化更新时被取代的旧 entity_id"}
                    },
                    "required": ["type", "entity_id", "details"]
                }),
            ),
            Tool::new(
                "dt_event".to_string(),
                "写入事件节点到KG(部署/安装/配置变更/会话记录)。type: Deploy/SoftwareInstalled/ConfigChange/Conversation".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "description": "事件类型"},
                        "entity_id": {"type": "string", "description": "唯一标识"},
                        "entity_type": {"type": "string", "description": "实体类型"},
                        "project": {"type": "string", "description": "所属项目"},
                        "details": {"type": "string", "description": "详细内容"}
                    },
                    "required": ["type", "entity_id", "entity_type", "details"]
                }),
            ),
            Tool::new(
                "dt_learn".to_string(),
                "从AI任务执行结果批量写入知识(模式/踩坑/决策)到 Knowledge World".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {"type": "string", "description": "任务标题"},
                        "entities": {"type": "string", "description": "涉及实体, 逗号分隔"},
                        "pattern": {"type": "string", "description": "解决方案模式"},
                        "pitfalls": {"type": "string", "description": "踩坑经验, 逗号分隔"},
                        "decisions": {"type": "string", "description": "架构决策, 逗号分隔"},
                        "thread_id": {"type": "string", "description": "Digital Thread ID"},
                        "success": {"type": "boolean", "description": "任务是否成功"},
                        "project": {"type": "string", "description": "所属项目"}
                    },
                    "required": ["task"]
                }),
            ),
            Tool::new(
                "dt_build".to_string(),
                "构建/索引项目到知识图谱。all=全部项目; path=项目根路径或文件路径; full=全量重建".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "all": {"type": "boolean", "description": "构建 config.yaml 中所有项目"},
                        "full": {"type": "boolean", "description": "全量重建, 绕过增量快照"},
                        "path": {"type": "string", "description": "项目根路径或文件绝对路径"},
                        "name": {"type": "string", "description": "项目名称(传目录时必填)"}
                    },
                    "required": []
                }),
            ),
            Tool::new(
                "dt_kg_sync".to_string(),
                "同步知识图谱节点变更到向量库(已弃用, 建议 dt build --source knowledge)".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {"config_chunks": {"type": "boolean", "description": "同时同步配置分块"}},
                    "required": []
                }),
            ),
            Tool::new(
                "dt_health".to_string(),
                "检查所有后端服务健康状态(Memgraph/Qdrant/SQLite/Embed)".to_string(),
                serde_json::json!({"type": "object", "properties": {}, "required": []}),
            ),
            Tool::new(
                "dt_backup".to_string(),
                "系统备份: backup/restore/list/verify 四种操作".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "description": "操作: backup/restore/list/verify, 默认 backup"},
                        "date": {"type": "string", "description": "恢复/校验的日期 YYYY-MM-DD"}
                    },
                    "required": []
                }),
            ),
        ]
    }

    fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Content>, ToolError>> + Send + 'static>> {
        let this = self.clone();
        let tool_name = tool_name.to_string();
        Box::pin(async move {
            let text = this.call(&tool_name, arguments).await?;
            Ok(vec![Content::text(text)])
        })
    }

    fn list_resources(&self) -> Vec<Resource> {
        vec![]
    }

    fn read_resource(
        &self,
        _uri: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ResourceError>> + Send + 'static>> {
        Box::pin(async move { Err(ResourceError::NotFound("no resources".into())) })
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        vec![]
    }

    fn get_prompt(
        &self,
        _prompt_name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, PromptError>> + Send + 'static>> {
        Box::pin(async move { Err(PromptError::NotFound("no prompts".into())) })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 复用统一异步日志管线:JSON → dt.log + stderr 人类可读(warn+);
    // guard 存活到 server.run 结束(drop 时冲刷队列)。
    // MCP 协议用 stdin/stdout 通信,stderr 日志不污染协议流。
    let _log_guard = digital_twin::shared::logging::init::init_logging()?;

    let rt = DtRuntime::connect().await;
    tracing::info!(
        "dt-mcp: 运行时连接完成 (graph={} vector={} embed={})",
        rt.graph.is_some(),
        rt.vector.is_some(),
        rt.embed.is_some()
    );
    let router = RouterService(DtRouter { rt: Arc::new(rt) });
    let server = Server::new(router);
    let transport = ByteTransport::new(tokio::io::stdin(), tokio::io::stdout());
    server.run(transport).await?;
    Ok(())
}
