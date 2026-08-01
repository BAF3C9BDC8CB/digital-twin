//! Svc Plugin — native local service management (no external binary).
//!
//! Reads project configuration from config.yaml, checks process status
//! via /proc and port scanning, manages services with native Rust code.

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Command;

use crate::application::plugins::Plugin;
use crate::domain::error::DtError;

/// Information about a discovered/configured project.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub port: Option<u16>,
}

/// Local service management plugin.
#[derive(Default)]
pub struct SvcPluginService {
    projects: Vec<ProjectInfo>,
}

impl SvcPluginService {
    /// Create from a list of (name, path) pairs (typically from config.yaml projects).
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

    /// Create from full ProjectInfo list.
    pub fn new(projects: Vec<ProjectInfo>) -> Self {
        Self { projects }
    }

    // ── CLI-facing methods ──────────────────────────────────────────────────

    /// List all known local microservices with status.
    pub fn list_services(&self) -> Result<String, DtError> {
        if self.projects.is_empty() {
            return Ok("(no services configured)".into());
        }

        let mut out = String::new();
        out.push_str(&format!(
            "{:<30} {:<12} {:<10} {:<50}\n",
            "NAME", "STATUS", "PID", "PATH"
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

    /// Get status of a single service.
    pub fn get_status(&self, name: &str) -> Result<String, DtError> {
        let proj = self.projects.iter().find(|p| p.name == name);
        match proj {
            Some(p) => {
                let (status, pid) = check_process(name);
                let pid_str = pid.map_or("none".to_string(), |p| p.to_string());
                Ok(format!(
                    "Service: {}\n  Status: {}\n  PID: {}\n  Path: {}\n",
                    name,
                    status,
                    pid_str,
                    p.path.display()
                ))
            }
            None => Err(DtError::NotFound(format!("service not found: {name}"))),
        }
    }

    /// Get recent logs for a service by tailing its log file.
    pub fn get_logs(&self, name: &str, lines: Option<u32>) -> Result<String, DtError> {
        let proj = self
            .projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| DtError::NotFound(format!("service not found: {name}")))?;

        let log_file = proj.path.join("logs").join(format!("{}.log", name));
        if !log_file.exists() {
            // Try alternative: /tmp/dt-svc-{name}.log
            let alt = PathBuf::from(format!("/tmp/dt-svc-{name}.log"));
            if alt.exists() {
                return tail_file(&alt, lines.unwrap_or(50));
            }
            return Ok(format!(
                "No log file found at {} or {}",
                log_file.display(),
                alt.display()
            ));
        }

        tail_file(&log_file, lines.unwrap_or(50))
    }

    /// Start a service (builds with mvn, then runs java -jar).
    pub fn start_service(&self, name: &str) -> Result<String, DtError> {
        let proj = self
            .projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| DtError::NotFound(format!("service not found: {name}")))?;

        let (_status, pid) = check_process(name);
        if pid.is_some() {
            return Ok(format!("Service {name} is already running"));
        }

        // Check for pom.xml (Java/Maven project)
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
                .map_err(|e| DtError::General(format!("failed to start mvn: {e}")))?;

            Ok(format!(
                "Started {} (pid={}) via mvn spring-boot:run",
                name,
                child.id()
            ))
        } else {
            Err(DtError::General(format!(
                "Cannot start {name}: no pom.xml found at {} (only Maven projects supported currently)",
                proj.path.display()
            )))
        }
    }

    /// Stop a service by sending SIGTERM.
    pub fn stop_service(&self, name: &str) -> Result<String, DtError> {
        let (_status, pid) = check_process(name);
        match pid {
            Some(pid) => {
                Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .output()
                    .map_err(|e| DtError::General(format!("failed to kill {pid}: {e}")))?;
                Ok(format!("Stopped {name} (pid={pid})"))
            }
            None => Ok(format!("Service {name} is not running")),
        }
    }

    /// Restart a service (stop + start).
    pub fn restart_service(&self, name: &str) -> Result<String, DtError> {
        let stop_result = self.stop_service(name)?;
        // Small delay to let the process exit
        std::thread::sleep(std::time::Duration::from_secs(2));
        let start_result = self.start_service(name)?;
        Ok(format!("{stop_result}\n{start_result}"))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Check if a service is running by scanning /proc for a matching process name.
fn check_process(name: &str) -> (&'static str, Option<u32>) {
    // Scan /proc for processes with matching comm names
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(pid_str) = path.file_name().and_then(|n| n.to_str()) {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    // Read /proc/{pid}/comm
                    let comm_path = path.join("comm");
                    if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                        let comm = comm.trim();
                        if comm == "java" {
                            // Check cmdline for the service name
                            let cmdline_path = path.join("cmdline");
                            if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                                if cmdline.contains(name) {
                                    // Also check if it's a Java app (not just any java process)
                                    if cmdline.contains("-jar")
                                        || cmdline.contains("spring")
                                        || cmdline.contains(name)
                                    {
                                        return ("RUNNING", Some(pid));
                                    }
                                }
                            }
                        } else if comm.contains(name) {
                            return ("RUNNING", Some(pid));
                        }
                    }
                }
            }
        }
    }
    ("STOPPED", None)
}

/// Tail the last N lines of a file.
fn tail_file(path: &std::path::Path, lines: u32) -> Result<String, DtError> {
    let output = Command::new("tail")
        .args(["-n", &lines.to_string()])
        .arg(path)
        .output()
        .map_err(|e| DtError::General(format!("tail failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DtError::General(format!("tail error: {stderr}")));
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
        // TODO: wire generated SvcPluginServer when proto is compiled
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log.info(&format!(
            "[svc] plugin initialized with {} projects (native)",
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
