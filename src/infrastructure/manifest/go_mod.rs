//! Go `go.mod` 解析器 → `ManifestArtifact`。
//!
//! 抽取：module 路径（当作制品名/组）+ require 依赖。
//! Go 无 groupId/artifactId 之分，module 全路径即身份。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 从 go.mod 内容解析制品。非 module 文件返回 None。
pub fn parse_go_mod(content: &str) -> Option<ManifestArtifact> {
    let mut module = String::new();
    let mut dependencies: Vec<(String, String)> = Vec::new();
    let mut in_require_block = false;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("module ") {
            module = rest.split_whitespace().next().unwrap_or("").to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("require (") {
            in_require_block = true;
            let _ = rest;
            continue;
        }
        if line == ")" {
            in_require_block = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("require ") {
            // 单行 require
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if let Some(modpath) = parts.first() {
                dependencies.push((String::new(), modpath.to_string()));
            }
            continue;
        }
        if in_require_block {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(modpath) = parts.first() {
                dependencies.push((String::new(), modpath.to_string()));
            }
        }
    }

    if module.is_empty() {
        return None;
    }

    Some(ManifestArtifact {
        name: module.clone(),
        group_id: String::new(),
        version: String::new(),
        artifact_type: ArtifactType::Go,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_mod() {
        let modfile = r#"module github.com/myorg/myservice

go 1.21

require (
	github.com/gin-gonic/gin v1.9.0
	github.com/stretchr/testify v1.8.0
)

require github.com/pkg/errors v0.9.1
"#;
        let m = parse_go_mod(modfile).expect("parse");
        assert_eq!(m.name, "github.com/myorg/myservice");
        let names: Vec<String> = m.dependencies.iter().map(|(_, n)| n.clone()).collect();
        assert!(names.contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(names.contains(&"github.com/pkg/errors".to_string()));
    }
}
