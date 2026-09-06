//! Node `package.json` 解析器 → `ManifestArtifact`。
//!
//! 抽取：name/version + dependencies/devDependencies/peerDependencies。
//! scoped 包名（`@org/pkg`）保留原名（含 @ 与 /）。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 从 package.json 内容解析制品。非项目文件（无 name）返回 None。
pub fn parse_package_json(content: &str) -> Option<ManifestArtifact> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    let name = value.get("name")?.as_str()?.to_string();
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut dependencies: Vec<(String, String)> = Vec::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(deps) = value.get(key).and_then(|d| d.as_object()) {
            for (dep_name, _) in deps {
                dependencies.push((String::new(), dep_name.clone()));
            }
        }
    }

    Some(ManifestArtifact {
        name,
        group_id: String::new(),
        version,
        artifact_type: ArtifactType::Npm,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_json() {
        let pkg = r#"{
  "name": "my-service",
  "version": "2.1.0",
  "dependencies": { "express": "^4", "lodash": "~4.17" },
  "devDependencies": { "jest": "^29" },
  "peerDependencies": { "react": "^18" }
}"#;
        let m = parse_package_json(pkg).expect("parse");
        assert_eq!(m.name, "my-service");
        assert_eq!(m.version, "2.1.0");
        let names: Vec<String> = m.dependencies.iter().map(|(_, n)| n.clone()).collect();
        assert!(names.contains(&"express".to_string()));
        assert!(names.contains(&"lodash".to_string()));
        assert!(names.contains(&"jest".to_string()));
        assert!(names.contains(&"react".to_string()));
    }

    #[test]
    fn scoped_package_preserved() {
        let pkg = r#"{"name": "@myorg/core", "dependencies": {"@myorg/util": "^1"}}"#;
        let m = parse_package_json(pkg).expect("parse");
        assert_eq!(m.name, "@myorg/core");
        assert_eq!(
            m.dependencies,
            vec![(String::new(), "@myorg/util".to_string())]
        );
    }
}
