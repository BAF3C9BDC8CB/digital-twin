//! Manifest 解析器 —— 语言无关的制品/依赖发现。
//!
//! 输入一个项目根目录，扫描其构建清单（pom.xml / Cargo.toml /
//! pyproject.toml / package.json / go.mod），输出该项目的制品集合
//! （含模块层级与依赖坐标）。
//!
//! 设计要点：
//! - **语言无关分发**：注册表按文件形态匹配解析器（与 parser 注册表同构）。
//! - **归属与身份解耦**：`ArtifactBlock.artifact_id` 只含 type+name，
//!   不含 project —— 同一制品跨项目收敛到同一节点（跨项目 DEPENDS_ON 的前提）。
//! - **坐标主键**：name（Maven 即 artifactId）为全局主键，MERGE 幂等。

pub mod assemble;
pub mod generic;
pub mod go_mod;
pub mod java_maven;
pub mod node;
pub mod python_pyproject;
pub mod rust_cargo;

use crate::domain::types::ManifestArtifact;
use std::path::Path;

/// 解析一个 Manifest 文件为制品。
///
/// 按文件名（pom.xml / Cargo.toml / pyproject.toml / package.json / go.mod）
/// 分派到具体解析器。返回 `None` 表示该文件不是已知 manifest。
pub fn parse_manifest_file(path: &Path, content: &str) -> Option<ManifestArtifact> {
    let fname = path.file_name()?.to_string_lossy().to_string();
    match fname.as_str() {
        "pom.xml" => java_maven::parse_pom(content),
        "Cargo.toml" => rust_cargo::parse_cargo(content),
        "pyproject.toml" => python_pyproject::parse_pyproject(content),
        "package.json" => node::parse_package_json(content),
        "go.mod" => go_mod::parse_go_mod(content),
        _ => None,
    }
}

/// 从项目根目录发现该项目的 manifest 文件（递归、按优先级）。
///
/// 返回 `(相对路径, 内容)`。Maven 多模块项目会有多个 pom.xml，
/// 每个 module 一个制品——因此返回 Vec。
pub fn discover_manifests(root: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut visited: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();

    // 顶层优先：根 pom.xml / Cargo.toml / package.json / go.mod / pyproject.toml
    let root_files = [
        "pom.xml",
        "Cargo.toml",
        "pyproject.toml",
        "package.json",
        "go.mod",
    ];
    for f in root_files {
        let p = root.join(f);
        if p.is_file() {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let rel = f.to_string();
                if !content.trim().is_empty() {
                    found.push((rel, content));
                }
            }
        }
    }

    // 递归找嵌套 pom.xml（Maven 多模块）与嵌套 Cargo.toml（workspace）
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                !matches!(
                    name.as_str(),
                    ".git" | "target" | "node_modules" | "build" | "dist" | ".idea"
                )
            } else {
                true
            }
        });
    while let Some(Ok(entry)) = walk.next() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let fname = path.file_name().map(|f| f.to_string_lossy().to_string());
        let is_manifest = matches!(
            fname.as_deref(),
            Some("pom.xml") | Some("Cargo.toml") | Some("pyproject.toml")
        );
        if !is_manifest {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| fname.unwrap_or_default());
        // 顶层已处理过
        if !rel.contains('/') {
            continue;
        }
        if visited.contains(path) {
            continue;
        }
        visited.insert(path.to_path_buf());
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                found.push((rel, content));
            }
        }
    }

    found
}
