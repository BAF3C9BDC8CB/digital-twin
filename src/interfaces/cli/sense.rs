//! CLI handler for `dt sense` — environment sensing.

use crate::application::sense::{SenseService, SenseStatus};
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn handle_sense(
    path: Option<PathBuf>,
    json: bool,
    projects: Vec<(String, PathBuf)>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    ignored_dirs_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    let input = match path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let input = input.canonicalize().unwrap_or(input);
    if !input.exists() {
        anyhow::bail!("path not found: {}", input.display());
    }

    let svc = SenseService {
        graph,
        vector,
        snapshot,
        ignored_dirs_file,
    };
    let report = svc.sense(&input, &projects).await;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // ---- human readable ----
    let status_str = match report.status {
        SenseStatus::Indexed => "indexed",
        SenseStatus::RegisteredNotIndexed => "registered (not built)",
        SenseStatus::Unregistered => "unregistered",
    };
    println!("📍 {}", input.display());
    println!("  Status: {status_str}");

    if let Some(p) = &report.project {
        println!("  Project: {} ({})", p.name, p.path);
    }
    if let Some(s) = &report.stats {
        println!(
            "  Stats:  methods={} classes={} vectors={} last_build={}",
            s.methods,
            s.classes,
            s.vectors,
            s.last_build.as_deref().unwrap_or("-"),
        );
    }
    if !report.languages.is_empty() {
        let langs: Vec<String> = report
            .languages
            .iter()
            .take(5)
            .map(|l| format!(".{} {}%", l.ext, l.pct))
            .collect();
        println!("  Languages: {}", langs.join(" · "));
    }
    if !report.dirs.is_empty() {
        println!("  Top dirs:");
        for d in report.dirs.iter().take(8) {
            println!("    {:<40} {} methods", d.dir, d.methods);
        }
    }
    if !report.key_entities.is_empty() {
        println!("  Key entities:");
        for e in &report.key_entities {
            let suffix = if e.source == "in_degree" {
                format!("in-degree {}", e.in_degree)
            } else {
                e.kind.clone()
            };
            println!("    [{}] {} ({})", e.source, e.name, suffix);
        }
    }
    if report.status == SenseStatus::RegisteredNotIndexed {
        if let Some(p) = &report.project {
            println!("  💡 已注册未构建，建议: dt build --name {}", p.name);
        }
    }
    if !report.candidates.is_empty() {
        println!("  Candidates ({}):", report.candidates.len());
        for c in &report.candidates {
            println!("    {}  →  {}", c.path, c.build_cmd);
        }
    }
    if !report.degraded.is_empty() {
        println!("  ⚠ degraded: {}", report.degraded.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod live_tests {
    /// live：需 Memgraph :7688 + Qdrant :6334 在线；在已注册项目目录运行。
    /// Run: cargo build && cargo test -- --ignored sense_live
    #[tokio::test]
    #[ignore]
    async fn sense_live_returns_valid_json_shape() {
        let bin =
            std::env::var("CARGO_BIN_EXE_dt").unwrap_or_else(|_| "./target/release/dt".to_string());
        let out = std::process::Command::new(bin)
            .args(["sense", "--json"])
            .output()
            .expect("run dt sense");
        assert!(out.status.success());
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
        assert!(v.get("status").is_some());
        assert!(v.get("degraded").is_some());
        let status = v["status"].as_str().unwrap();
        assert!(
            ["indexed", "registered_not_indexed", "unregistered"].contains(&status),
            "unexpected status: {status}"
        );
    }
}
