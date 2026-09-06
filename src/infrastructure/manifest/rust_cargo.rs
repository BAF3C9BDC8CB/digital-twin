//! Rust `Cargo.toml` 解析器 → `ManifestArtifact`。
//!
//! 抽取：package.name/version + dependencies（含 dev/build 依赖）。
//! workspace 根 Cargo.toml（无 [package] 有 [workspace]）不产生制品，
//! 其成员 crate 由各自 Cargo.toml 单独解析。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 从 Cargo.toml 内容解析制品。非 crate manifest（纯 workspace 根）返回 None。
pub fn parse_cargo(content: &str) -> Option<ManifestArtifact> {
    let value: toml::Value = content.parse().ok()?;
    let package = value.get("package")?;
    let name = package.get("name")?.as_str()?.to_string();
    let version = package
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut dependencies: Vec<(String, String)> = Vec::new();
    // 依赖段：dependencies / dev-dependencies / build-dependencies
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(deps) = value.get(key).and_then(|d| d.as_table()) {
            for (dep_name, dep_val) in deps {
                // 值可以是字符串（版本）或表（含 package 重命名）
                let real_name = dep_val
                    .get("package")
                    .and_then(|p| p.as_str())
                    .unwrap_or(dep_name);
                let is_path_dep = dep_val.get("path").and_then(|p| p.as_str()).is_some();
                // path 依赖（同 workspace 内部 crate）也记录——它产生内部边
                let _ = is_path_dep;
                dependencies.push((String::new(), real_name.to_string()));
            }
        }
    }

    Some(ManifestArtifact {
        name,
        group_id: String::new(),
        version,
        artifact_type: ArtifactType::Crate,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_toml() {
        let cargo = r#"[package]
name = "digital-twin"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
inner-crate = { path = "../inner" }

[dev-dependencies]
tempfile = "3"
"#;
        let m = parse_cargo(cargo).expect("parse");
        assert_eq!(m.name, "digital-twin");
        assert_eq!(m.version, "0.1.0");
        let names: Vec<String> = m.dependencies.iter().map(|(_, n)| n.clone()).collect();
        assert!(names.contains(&"tokio".to_string()));
        assert!(names.contains(&"serde".to_string()));
        assert!(names.contains(&"inner-crate".to_string()));
        assert!(names.contains(&"tempfile".to_string()));
    }

    #[test]
    fn workspace_root_returns_none() {
        let cargo = r#"[workspace]
members = ["crates/a", "crates/b"]
"#;
        assert!(parse_cargo(cargo).is_none());
    }

    #[test]
    fn renamed_dep_uses_real_crate_name() {
        let cargo = r#"[package]
name = "my-app"
version = "0.1.0"

[dependencies]
my-alias = { package = "real-crate", version = "1" }
"#;
        let m = parse_cargo(cargo).expect("parse");
        assert_eq!(
            m.dependencies,
            vec![(String::new(), "real-crate".to_string())]
        );
    }
}
