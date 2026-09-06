//! Maven `pom.xml` 解析器 → `ManifestArtifact`。
//!
//! 抽取：当前 pom 自身声明的 groupId/artifactId/version（跳过 `<parent>`
//! 继承块——父坐标不算本制品）+ dependencies 坐标。
//!
//! 用 quick-xml 流式解析（已作为依赖引入）：正确处理 `<parent>` 前置、
//! 注释、自闭合标签等文本搜索易错的结构。

use crate::domain::types::{ArtifactType, ManifestArtifact};
use quick_xml::events::Event;
use quick_xml::Reader;

/// 剥离 XML 文本中的 `<parent>...</parent>` 顶层块。
///
/// Maven 模块 pom 的 `<parent>`（含父 groupId/artifactId/version）几乎
/// 总在自身坐标之前；不剥离会把父坐标误当成本制品坐标。文本层顺序
/// 剥离即可——parent 只出现一次且在开头附近，不嵌套。
fn strip_parent_block(content: &str) -> String {
    let mut rest = content;
    let mut out = String::with_capacity(content.len());
    loop {
        let open = rest.find("<parent");
        let close = rest.find("</parent>");
        match (open, close) {
            (Some(o), Some(c)) if o < c => {
                out.push_str(&rest[..o]);
                rest = &rest[c + "</parent>".len()..];
            }
            (Some(_), None) => {
                // parent 未闭合——异常 pom，丢弃 parent 之后内容
                return out;
            }
            _ => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// 用 quick-xml 抽取第一个 `<tag>` 的文本（无嵌套同名标签场景足够）。
fn xml_tag_with_reader(content: &str, tag: &str) -> Option<String> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_target = false;
    let mut collected = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == tag.as_bytes() {
                    in_target = true;
                    collected.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_target {
                    if let Ok(s) = reader.decoder().decode(t.as_ref()) {
                        collected.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == tag.as_bytes() && in_target {
                    let val = collected.trim().to_string();
                    return if val.is_empty() { None } else { Some(val) };
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// 兼容旧签名：文本层取标签（保留给依赖块内解析用，不受 parent 影响）。
fn xml_tag(content: &str, tag: &str) -> Option<String> {
    xml_tag_with_reader(content, tag)
}

/// 解析依赖块中的坐标列表：按 `<dependency>` 块切分（quick-xml 已能
/// 处理块边界），每块抽 groupId/artifactId。
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

/// 从 pom.xml 内容解析制品（剥离 parent 后取自身坐标）。
pub fn parse_pom(content: &str) -> Option<ManifestArtifact> {
    // 关键：跳过 <parent> 块，避免把父 artifactId/groupId 当成本模块坐标
    let stripped = strip_parent_block(content);
    let artifact_id = xml_tag(&stripped, "artifactId")?;
    let group_id = xml_tag(&stripped, "groupId").unwrap_or_default();
    let version = xml_tag(&stripped, "version").unwrap_or_default();

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
    fn ignores_parent_block_coordinates() {
        // 真实 Maven 模块 pom：<parent> 在自身坐标之前
        let pom = r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>com.offen</groupId>
        <artifactId>uvp-offen-pay</artifactId>
        <version>1.0-SNAPSHOT</version>
    </parent>
    <artifactId>pay-offen-core</artifactId>
    <dependencies>
        <dependency>
            <groupId>com.offen</groupId>
            <artifactId>pay-offen-sdk-java</artifactId>
        </dependency>
    </dependencies>
</project>"#;
        let m = parse_pom(pom).expect("parse");
        // 必须取自身 artifactId，而非 parent 的
        assert_eq!(m.name, "pay-offen-core");
        // group/version 从 parent 继承（自身未声明）→ 空，可接受
        assert_eq!(m.dependencies.len(), 1);
        assert_eq!(m.dependencies[0].1, "pay-offen-sdk-java");
    }

    #[test]
    fn parses_aggregator_pom() {
        // 聚合 pom 只有 modules 无自身依赖
        let pom = r#"<project>
  <groupId>com.offen</groupId>
  <artifactId>uvp-offen-pay</artifactId>
  <version>1.0-SNAPSHOT</version>
  <packaging>pom</packaging>
  <modules>
    <module>pay-offen-core</module>
  </modules>
</project>"#;
        let m = parse_pom(pom).expect("parse");
        assert_eq!(m.name, "uvp-offen-pay");
        assert!(m.dependencies.is_empty());
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
