//! Python `pyproject.toml` 解析器 → `ManifestArtifact`。
//!
//! 抽取：project.name/version + project.dependencies / optional-dependencies
//! （PEP 621）。依赖行是 `name[extra]>=1.0` 形式，取包名主体。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 从 PEP 621 依赖行抽取包名：`foo[bar]>=1.0` → `foo`。
fn dep_name(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // 去掉行内注释
    let line = line.split('#').next().unwrap_or("").trim();
    // 取第一个非空白 token
    let token = line.split_whitespace().next().unwrap_or("");
    // 去掉 extra 与版本约束：包名终止于 [ = < > ! ~ ; 空格
    let name = token
        .find(['[', '=', '<', '>', '!', '~', ';'])
        .map_or(token, |idx| &token[..idx]);
    // 规范化：PEP 503 包名 `_` 与 `.` 等价，保留原名即可
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 从 pyproject.toml 内容解析制品。非项目 pyproject（纯工具配置）返回 None。
pub fn parse_pyproject(content: &str) -> Option<ManifestArtifact> {
    let value: toml::Value = content.parse().ok()?;
    // 支持 PEP 621 ([project]) 与旧 setuptools ([tool.poetry] / [tool.pdm])
    let project = value.get("project")?;
    let name = project
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_string();
    if name.is_empty() {
        // 尝试 poetry 风格
        let poetry = value.get("tool")?.get("poetry")?;
        let name = poetry.get("name")?.as_str()?.to_string();
        let version = poetry
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut dependencies: Vec<(String, String)> = Vec::new();
        if let Some(deps) = poetry.get("dependencies").and_then(|d| d.as_table()) {
            for (dep_name_, _) in deps {
                if dep_name_ != "python" {
                    dependencies.push((String::new(), dep_name_.clone()));
                }
            }
        }
        return Some(ManifestArtifact {
            name,
            group_id: String::new(),
            version,
            artifact_type: ArtifactType::Python,
            dependencies,
        });
    }

    let version = project
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut dependencies: Vec<(String, String)> = Vec::new();
    if let Some(deps) = project.get("dependencies").and_then(|d| d.as_array()) {
        for line in deps {
            if let Some(l) = line.as_str() {
                if let Some(n) = dep_name(l) {
                    dependencies.push((String::new(), n));
                }
            }
        }
    }
    if let Some(opt) = project
        .get("optional-dependencies")
        .and_then(|d| d.as_table())
    {
        for (_, group) in opt {
            if let Some(arr) = group.as_array() {
                for line in arr {
                    if let Some(l) = line.as_str() {
                        if let Some(n) = dep_name(l) {
                            dependencies.push((String::new(), n));
                        }
                    }
                }
            }
        }
    }

    Some(ManifestArtifact {
        name,
        group_id: String::new(),
        version,
        artifact_type: ArtifactType::Python,
        dependencies,
    })
}

/// 从 requirements.txt 内容追加解析（简单回退：每行一个依赖）。
pub fn parse_requirements(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|l| {
            let name = dep_name(l)?;
            // 跳过 `-r other.txt` / `--index-url ...` 等指令
            if name.starts_with('-') {
                return None;
            }
            Some((String::new(), name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pep621() {
        let py = r#"[project]
name = "my-lib"
version = "0.3.0"
dependencies = [
  "requests>=2.0",
  "pydantic[email]>=2",
  "click==8.1",
]

[project.optional-dependencies]
dev = ["pytest", "mypy"]
"#;
        let m = parse_pyproject(py).expect("parse");
        assert_eq!(m.name, "my-lib");
        assert_eq!(m.version, "0.3.0");
        let names: Vec<String> = m.dependencies.iter().map(|(_, n)| n.clone()).collect();
        assert!(names.contains(&"requests".to_string()));
        assert!(names.contains(&"pydantic".to_string()));
        assert!(names.contains(&"click".to_string()));
        assert!(names.contains(&"pytest".to_string()));
    }

    #[test]
    fn requirements_lines() {
        let reqs = "requests>=2.0\n# comment\nflask\n-r other.txt\n";
        let deps = parse_requirements(reqs);
        assert_eq!(
            deps,
            vec![
                (String::new(), "requests".to_string()),
                (String::new(), "flask".to_string()),
            ]
        );
    }

    #[test]
    fn non_project_pyproject_none() {
        let py = "[tool.black]\nline-length = 100\n";
        assert!(parse_pyproject(py).is_none());
    }
}
