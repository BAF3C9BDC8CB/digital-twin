//! Maven `pom.xml` 解析器 → `ManifestArtifact`。
//!
//! 抽取：groupId/artifactId/version + dependencies 坐标。
//! 多模块聚合 pom 由上层递归发现（每个 module 的 pom.xml 单独解析为一个制品）。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 轻量 XML 文本抽取：取某标签第一个非嵌套出现的内容（无 XML 依赖）。
/// 仅处理无嵌套同名标签的坐标字段（Maven 坐标字段正是如此）。
fn xml_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)?;
    let rest = &content[start + open.len()..];
    let end = rest.find(&close)?;
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

/// 解析依赖块中的坐标列表：按 `<dependency>` 块切分，每块抽 groupId/artifactId。
fn extract_dependencies(content: &str) -> Vec<(String, String)> {
    let mut deps = Vec::new();
    let mut search_from = 0;
    let dep_open = "<dependency>";
    let dep_close = "</dependency>";
    while let Some(start_rel) = content[search_from..].find(dep_open) {
        let start = search_from + start_rel;
        let after_open = start + dep_open.len();
        let Some(end_rel) = content[after_open..].find(dep_close) else {
            break;
        };
        let end = after_open + end_rel;
        let block = &content[after_open..end];
        // 跳过 dependencyManagement 里的依赖（管理不=直接依赖）：
        // 最近一个未闭合的 <dependencyManagement> 若在 </dependencyManagement> 之后则不在管理段
        let before = &content[..start];
        let last_open = before.rfind("<dependencyManagement>");
        let last_close = before.rfind("</dependencyManagement>");
        let in_dep_mgmt = match (last_open, last_close) {
            (Some(o), Some(c)) => o > c,
            (Some(_), None) => true,
            _ => false,
        };
        if !in_dep_mgmt {
            let gid = xml_tag(block, "groupId").unwrap_or_default();
            let aid = xml_tag(block, "artifactId").unwrap_or_default();
            if !aid.is_empty() {
                deps.push((gid, aid));
            }
        }
        search_from = end + dep_close.len();
    }
    deps
}

/// 从 pom.xml 内容解析制品。
pub fn parse_pom(content: &str) -> Option<ManifestArtifact> {
    let artifact_id = xml_tag(content, "artifactId")?;
    let group_id = xml_tag(content, "groupId").unwrap_or_default();
    let version = xml_tag(content, "version").unwrap_or_default();

    Some(ManifestArtifact {
        name: artifact_id,
        group_id,
        version,
        artifact_type: ArtifactType::Jar,
        dependencies: extract_dependencies(content),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_pom() {
        let pom = r#"<?xml version="1.0"?>
<project>
  <groupId>com.offen</groupId>
  <artifactId>pay-offen-sdk-java</artifactId>
  <version>1.2.3</version>
  <dependencies>
    <dependency>
      <groupId>com.offen</groupId>
      <artifactId>pay-offen-common</artifactId>
      <version>1.0.0</version>
    </dependency>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
    </dependency>
  </dependencies>
</project>"#;
        let m = parse_pom(pom).expect("parse");
        assert_eq!(m.name, "pay-offen-sdk-java");
        assert_eq!(m.group_id, "com.offen");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(
            m.dependencies,
            vec![
                ("com.offen".to_string(), "pay-offen-common".to_string()),
                ("org.slf4j".to_string(), "slf4j-api".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_dependency_management() {
        let pom = r#"<project>
  <groupId>g</groupId>
  <artifactId>a</artifactId>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>g</groupId>
        <artifactId>managed-only</artifactId>
        <version>9</version>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>g</groupId>
      <artifactId>real-dep</artifactId>
    </dependency>
  </dependencies>
</project>"#;
        let m = parse_pom(pom).expect("parse");
        assert_eq!(
            m.dependencies,
            vec![("g".to_string(), "real-dep".to_string())]
        );
    }
}
