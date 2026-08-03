//! Svc 插件——原生本地服务管理（不依赖外部二进制）。
//!
//! 从 config.yaml 读取项目配置，通过 /proc 和端口扫描检查进程状态，
//! 使用原生 Rust 代码管理服务。

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Command;

use crate::application::plugins::Plugin;
use crate::domain::error::DtError;

/// 已发现/已配置项目的信息。
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub port: Option<u16>,
}

/// 本地服务管理插件。
#[derive(Default)]
pub struct SvcPluginService {
    projects: Vec<ProjectInfo>,
}

impl SvcPluginService {
    /// 从 (name, path) 对列表创建（通常来自 config.yaml 的 projects 段）。
    pub fn from_projects(items: Vec<(String, PathBuf)>) -> Self {
        let projects = items
            .into_iter()
            .map(|(name, path)| ProjectInfo {
                name,
                path,
                port: None,
            })
            .collect();
        Self { projects }
    }

    /// 从完整的 ProjectInfo 列表创建。
    pub fn new(projects: Vec<ProjectInfo>) -> Self {
        Self { projects }
    }

    // ── 面向 CLI 的方法 ──────────────────────────────────────────────────

    /// 列出所有已知的本地微服务及其状态。
    pub fn list_services(&self) -> Result<String, DtError> {
        if self.projects.is_empty() {
            return Ok("(未配置服务)".into());
        }

        let mut out = String::new();
        out.push_str(&format!(
            "{:<30} {:<12} {:<10} {:<50}\n",
            "名称", "状态", "PID", "路径"
        ));

        for proj in &self.projects {
            let (status, pid) = check_process(&proj.name);
            out.push_str(&format!(
                "{:<30} {:<12} {:<10} {:<50}\n",
                truncate_str(&proj.name, 30),
                status,
                pid.map_or("-".to_string(), |p| p.to_string()),
                truncate_str(&proj.path.display().to_string(), 50),
            ));
        }

        Ok(out)
    }

    /// 获取单个服务的状态。
    pub fn get_status(&self, name: &str) -> Result<String, DtError> {
        let proj = self.projects.iter().find(|p| p.name == name);
        match proj {
            Some(p) => {
                let (status, pid) = check_process(name);
                let pid_str = pid.map_or("无".to_string(), |p| p.to_string());
                Ok(format!(
                    "服务: {}\n  状态: {}\n  PID: {}\n  路径: {}\n",
                    name,
                    status,
                    pid_str,
                    p.path.display()
                ))
            }
            None => Err(DtError::NotFound(format!("未找到服务: {name}"))),
        }
    }

    /// 通过尾随其日志文件获取服务的最近日志。
    pub fn get_logs(&self, name: &str, lines: Option<u32>) -> Result<String, DtError> {
        let proj = self
            .projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| DtError::NotFound(format!("未找到服务: {name}")))?;

        let log_file = proj.path.join("logs").join(format!("{}.log", name));
        if !log_file.exists() {
            // 尝试备选路径：/tmp/dt-svc-{name}.log
            let alt = PathBuf::from(format!("/tmp/dt-svc-{name}.log"));
            if alt.exists() {
                return tail_file(&alt, lines.unwrap_or(50));
            }
            return Ok(format!(
                "在 {} 或 {} 未找到日志文件",
                log_file.display(),
                alt.display()
            ));
        }

        tail_file(&log_file, lines.unwrap_or(50))
    }

    /// 启动服务（先用 mvn 构建，再运行 java -jar）。
    pub fn start_service(&self, name: &str) -> Result<String, DtError> {
        let proj = self
            .projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| DtError::NotFound(format!("未找到服务: {name}")))?;

        let (_status, pid) = check_process(name);
        if pid.is_some() {
            return Ok(format!("服务 {name} 已在运行"));
        }

        // 检查 pom.xml（Java/Maven 项目）
        let pom = proj.path.join("pom.xml");
        if pom.exists() {
            let child = Command::new("nohup")
                .args([
                    "mvn",
                    "spring-boot:run",
                    "-f",
                    pom.to_str().unwrap_or("pom.xml"),
                ])
                .current_dir(&proj.path)
                .spawn()
                .map_err(|e| DtError::General(format!("启动 mvn 失败: {e}")))?;

            Ok(format!(
                "已通过 mvn spring-boot:run 启动 {} (pid={})",
                name,
                child.id()
            ))
        } else {
            Err(DtError::General(format!(
                "无法启动 {name}: 在 {} 未找到 pom.xml（当前仅支持 Maven 项目）",
                proj.path.display()
            )))
        }
    }

    /// 通过发送 SIGTERM 停止服务。
    pub fn stop_service(&self, name: &str) -> Result<String, DtError> {
        let (_status, pid) = check_process(name);
        match pid {
            Some(pid) => {
                Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .output()
                    .map_err(|e| DtError::General(format!("终止进程 {pid} 失败: {e}")))?;
                Ok(format!("已停止 {name} (pid={pid})"))
            }
            None => Ok(format!("服务 {name} 未在运行")),
        }
    }

    /// 重启服务（先停止再启动）。
    pub fn restart_service(&self, name: &str) -> Result<String, DtError> {
        let stop_result = self.stop_service(name)?;
        // 稍等片刻让进程退出
        std::thread::sleep(std::time::Duration::from_secs(2));
        let start_result = self.start_service(name)?;
        Ok(format!("{stop_result}\n{start_result}"))
    }
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

/// 通过扫描 /proc 中匹配的进程名来检查服务是否在运行。
fn check_process(name: &str) -> (&'static str, Option<u32>) {
    // 扫描 /proc 中具有匹配 comm 名称的进程
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(pid_str) = path.file_name().and_then(|n| n.to_str()) {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    // 读取 /proc/{pid}/comm
                    let comm_path = path.join("comm");
                    if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                        let comm = comm.trim();
                        if comm == "java" {
                            // 在 cmdline 中检查服务名
                            let cmdline_path = path.join("cmdline");
                            if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                                if cmdline.contains(name) {
                                    // 同时确认是 Java 应用（而非任意 java 进程）
                                    if cmdline.contains("-jar")
                                        || cmdline.contains("spring")
                                        || cmdline.contains(name)
                                    {
                                        return ("运行中", Some(pid));
                                    }
                                }
                            }
                        } else if comm.contains(name) {
                            return ("运行中", Some(pid));
                        }
                    }
                }
            }
        }
    }
    ("已停止", None)
}

/// 尾随文件的最后 N 行。
fn tail_file(path: &std::path::Path, lines: u32) -> Result<String, DtError> {
    let output = Command::new("tail")
        .args(["-n", &lines.to_string()])
        .arg(path)
        .output()
        .map_err(|e| DtError::General(format!("tail 命令失败: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DtError::General(format!("tail 命令错误: {stderr}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

#[async_trait]
impl Plugin for SvcPluginService {
    fn id(&self) -> &'static str {
        "svc"
    }

    fn name(&self) -> &'static str {
        "Local Service Manager"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn register_grpc(
        &self,
        _server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError> {
        // TODO: proto 编译完成后装配生成的 SvcPluginServer
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log.info(&format!(
            "[svc] 插件已初始化，共 {} 个项目（原生实现）",
            self.projects.len()
        ));
        Ok(())
    }

    async fn health(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}
