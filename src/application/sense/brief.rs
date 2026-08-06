//! 简报聚合：目录画像、语言分布、关键实体。

use crate::application::sense::{DirStat, KeyEntity, LangStat};
use crate::domain::traits::GraphRepository;

/// 由 payloads 聚合目录画像与语言分布。
/// dir 口径：file_path 去掉 project_root 前缀后的第一级目录；根级文件归 "."。
pub fn aggregate_dirs(
    payloads: &[serde_json::Value],
    project_root: &str,
) -> (Vec<DirStat>, Vec<LangStat>) {
    use std::collections::HashMap;
    let root = project_root.trim_end_matches('/');
    let mut dir_counts: HashMap<String, u64> = HashMap::new();
    let mut ext_counts: HashMap<String, u64> = HashMap::new();
    let mut total = 0u64;

    for p in payloads {
        let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) else {
            continue;
        };
        total += 1;
        let rel = fp.strip_prefix(root).unwrap_or(fp).trim_start_matches('/');
        let dir = if rel.contains('/') {
            rel.split('/').next().unwrap_or(".").to_string()
        } else {
            ".".to_string()
        };
        *dir_counts.entry(dir).or_default() += 1;
        if rel.contains('.') {
            if let Some(ext) = rel.rsplit('.').next() {
                if !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_alphanumeric()) {
                    *ext_counts.entry(ext.to_string()).or_default() += 1;
                }
            }
        }
    }

    let mut dirs: Vec<DirStat> = dir_counts
        .into_iter()
        .map(|(dir, methods)| DirStat { dir, methods })
        .collect();
    dirs.sort_by(|a, b| b.methods.cmp(&a.methods).then(a.dir.cmp(&b.dir)));
    dirs.truncate(10);

    let mut languages: Vec<LangStat> = ext_counts
        .into_iter()
        .map(|(ext, n)| LangStat {
            ext,
            pct: ((n * 100 + total / 2) / total.max(1)) as u8,
        })
        .collect();
    languages.sort_by(|a, b| b.pct.cmp(&a.pct).then(a.ext.cmp(&b.ext)));

    (dirs, languages)
}

/// 关键实体：CALLS 入度 top10；0 行 → Class 名称启发式；仍空 → vec![]。
pub async fn key_entities(graph: &dyn GraphRepository, project: &str) -> Vec<KeyEntity> {
    let mut params = std::collections::HashMap::new();
    params.insert(
        "p".to_string(),
        serde_json::Value::String(project.to_string()),
    );

    // 首选：CALLS 入度
    let in_degree = graph
        .read_query(
            "MATCH (m:Method {project: $p})<-[:CALLS]-() \
             RETURN m.name AS name, count(*) AS d ORDER BY d DESC LIMIT 10",
            params.clone(),
        )
        .await
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    if !in_degree.is_empty() {
        return in_degree
            .iter()
            .map(|r| KeyEntity {
                name: r
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("?")
                    .to_string(),
                kind: "method".into(),
                source: "in_degree".into(),
                in_degree: r.get("d").and_then(|d| d.as_u64()).unwrap_or(0),
            })
            .collect();
    }

    // 降级：Class 名称启发式
    let heuristic = graph
        .read_query(
            "MATCH (c:Class {project: $p}) \
             WHERE c.name ENDS WITH 'Controller' OR c.name ENDS WITH 'Service' \
                OR c.name ENDS WITH 'Mapper' OR c.name ENDS WITH 'Application' \
             RETURN c.name AS name LIMIT 10",
            params,
        )
        .await
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    heuristic
        .iter()
        .map(|r| KeyEntity {
            name: r
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string(),
            kind: "class".into(),
            source: "heuristic".into(),
            in_degree: 0,
        })
        .collect()
}

#[cfg(test)]
mod key_entity_tests {
    use super::key_entities;
    use crate::application::sense::stubs::StubGraph;

    #[tokio::test]
    async fn in_degree_top10() {
        let g = StubGraph {
            method_count: 0,
            class_count: 0,
            calls_rows: vec![serde_json::json!({"name": "handle_build", "d": 12})],
            heuristic_rows: vec![],
        };
        let out = key_entities(&g, "p").await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "handle_build");
        assert_eq!(out[0].in_degree, 12);
        assert_eq!(out[0].source, "in_degree");
        assert_eq!(out[0].kind, "method");
    }

    #[tokio::test]
    async fn heuristic_fallback() {
        let g = StubGraph {
            method_count: 0,
            class_count: 0,
            calls_rows: vec![],
            heuristic_rows: vec![serde_json::json!({"name": "PayController"})],
        };
        let out = key_entities(&g, "p").await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "heuristic");
        assert_eq!(out[0].kind, "class");
    }

    #[tokio::test]
    async fn both_empty_returns_empty() {
        let g = StubGraph {
            method_count: 0,
            class_count: 0,
            calls_rows: vec![],
            heuristic_rows: vec![],
        };
        assert!(key_entities(&g, "p").await.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(paths: &[&str]) -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| serde_json::json!({"file_path": p}))
            .collect()
    }

    #[test]
    fn aggregates_first_level_dirs() {
        let payloads = pl(&[
            "/data/p/src/a.java",
            "/data/p/src/b.java",
            "/data/p/web/c.ts",
            "/data/p/README.md",
        ]);
        let (dirs, _) = aggregate_dirs(&payloads, "/data/p");
        assert_eq!(dirs[0].dir, "src");
        assert_eq!(dirs[0].methods, 2);
        assert!(dirs.iter().any(|d| d.dir == "web" && d.methods == 1));
        assert!(dirs.iter().any(|d| d.dir == "." && d.methods == 1));
    }

    #[test]
    fn languages_pct_sums_roughly_100() {
        let payloads = pl(&["/p/a.java", "/p/b.java", "/p/c.java", "/p/d.ts"]);
        let (_, langs) = aggregate_dirs(&payloads, "/p");
        assert_eq!(langs[0].ext, "java");
        assert_eq!(langs[0].pct, 75);
        assert_eq!(langs[1].pct, 25);
    }

    #[test]
    fn empty_payloads_empty_output() {
        let (dirs, langs) = aggregate_dirs(&[], "/p");
        assert!(dirs.is_empty() && langs.is_empty());
    }
}
