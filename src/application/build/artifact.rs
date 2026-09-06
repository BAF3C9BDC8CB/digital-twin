//! Artifact 落图 —— 将项目 manifest 解析出的制品写入 Memgraph。
//!
//! # 切片 A（2026-09-06）
//!
//! 在构建流水线写完 Method/Class/Module 后执行：
//! 1. 发现项目根下的 manifest（pom.xml/Cargo.toml/...，递归）
//! 2. 每个 manifest 解析为一个 `Artifact` 节点（MERGE，跨项目幂等）
//! 3. 代码文件（Method/Class 按 file_path）归属到对应 Artifact：
//!    - 优先匹配具体模块前缀（如 `pay-offen-sdk-java/`）
//!    - 剩余文件归项目根制品（根 manifest / 目录名回退）
//! 4. 记录 `(Class|Method)-[:PART_OF]->(Artifact)`
//!
//! Artifact 的 `artifact_id` 不含 project —— 同一制品跨项目收敛到同一节点，
//! 这是后续切片 B（跨项目 DEPENDS_ON）的前提。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use crate::infrastructure::manifest::assemble::build_artifact_block;
use crate::infrastructure::manifest::{discover_manifests, parse_manifest_file};
use std::collections::HashMap;
use std::path::Path;

/// 项目根的代码文件相对路径列表 + 项目名 + manifest 发现的产物。
pub struct ArtifactWriteOutcome {
    /// 写入的 Artifact 节点数。
    pub artifacts_written: usize,
    /// 建立的 PART_OF 边数（代码文件 → 制品）。
    pub part_of_edges: usize,
    /// 建立的 DEPENDS_ON 边数（制品 → 依赖制品，切片 B）。
    pub depends_on_edges: usize,
}

/// 从项目根发现并解析所有制品。
///
/// 返回 `(ArtifactBlock, manifest 相对路径)` 列表。
/// 无任何 manifest 时，用目录名生成一个通用回退制品，保证每个项目
/// 至少有一个 Artifact 可挂载。
pub fn collect_artifacts(
    project: &str,
    root: &Path,
) -> Vec<(crate::domain::types::ArtifactBlock, String)> {
    let manifests = discover_manifests(root);
    let mut blocks: Vec<(crate::domain::types::ArtifactBlock, String)> = Vec::new();

    for (rel_path, content) in &manifests {
        if let Some(m) = parse_manifest_file(Path::new(rel_path), content) {
            let block = build_artifact_block(&m, project, rel_path);
            blocks.push((block, rel_path.clone()));
        }
    }

    if blocks.is_empty() {
        // 回退：目录名制品
        let dir_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| project.to_string());
        let fallback = crate::infrastructure::manifest::generic::generic_from_dir_name(&dir_name);
        let block = build_artifact_block(&fallback, project, "");
        blocks.push((block, String::new()));
    }

    blocks
}

/// 将制品写入图谱并建立 PART_OF 边。
///
/// `code_files`：该项目所有被解析代码文件的相对路径（用于按路径前缀归属制品）。
pub async fn write_artifacts_and_part_of(
    graph: &dyn GraphRepository,
    project: &str,
    root: &Path,
    code_files: &[String],
) -> Result<ArtifactWriteOutcome, DtError> {
    let blocks = collect_artifacts(project, root);
    if blocks.is_empty() {
        return Ok(ArtifactWriteOutcome {
            artifacts_written: 0,
            part_of_edges: 0,
            depends_on_edges: 0,
        });
    }

    // 1. 写入 Artifact 节点（MERGE，幂等）
    let artifacts_json: Vec<serde_json::Value> = blocks
        .iter()
        .map(|(b, _)| {
            serde_json::json!({
                "artifact_id": b.artifact_id,
                "name": b.name,
                "group_id": b.group_id,
                "version": b.version,
                "type": b.artifact_type.as_str(),
                "language": b.language,
                "project": b.project,
                "path_prefix": b.path_prefix,
            })
        })
        .collect();
    let mut params = HashMap::new();
    params.insert(
        "artifacts".to_string(),
        serde_json::Value::Array(artifacts_json),
    );
    graph
        .write_query(
            r#"UNWIND $artifacts AS a
            MERGE (n:Artifact {artifact_id: a.artifact_id})
            SET n.name = a.name,
                n.group_id = a.group_id,
                n.version = a.version,
                n.type = a.type,
                n.language = a.language,
                n.project = a.project,
                n.path_prefix = a.path_prefix"#,
            params,
        )
        .await?;
    let artifacts_written = blocks.len();

    // 2. 为每个代码文件归属制品并建 PART_OF
    // 先按前缀长度降序排制品：更具体的模块前缀优先匹配
    let mut sorted: Vec<&(crate::domain::types::ArtifactBlock, String)> = blocks.iter().collect();
    sorted.sort_by_key(|(b, _)| std::cmp::Reverse(b.path_prefix.len()));

    // 根制品（path_prefix 为空）兜底
    let root_artifact = sorted
        .iter()
        .find(|(b, _)| b.path_prefix.is_empty())
        .map(|(b, _)| b.artifact_id.clone());

    // 具体模块制品（path_prefix 非空）
    let module_artifacts: Vec<&(crate::domain::types::ArtifactBlock, String)> = sorted
        .iter()
        .filter(|(b, _)| !b.path_prefix.is_empty())
        .copied()
        .collect();

    let mut part_of_edges = 0usize;
    // 收集「文件 → artifact_id」映射（避免逐文件查询）
    // 先统计每个前缀能覆盖多少文件, 只为有文件的制品建边
    let mut files_by_artifact: HashMap<String, Vec<String>> = HashMap::new();
    for file in code_files {
        let matched = module_artifacts
            .iter()
            .find(|(b, _)| file.starts_with(&b.path_prefix))
            .map(|(b, _)| b.artifact_id.clone());
        let aid = matched.or_else(|| root_artifact.clone());
        if let Some(aid) = aid {
            files_by_artifact.entry(aid).or_default().push(file.clone());
        }
    }

    // 逐制品批量建边（每个制品一批 UNWIND，避免大量单条查询）
    for (aid, files) in &files_by_artifact {
        for chunk in files.chunks(200) {
            let files_json: Vec<serde_json::Value> = chunk
                .iter()
                .map(|f| serde_json::Value::String(f.clone()))
                .collect();
            let mut params = HashMap::new();
            params.insert(
                "artifact_id".to_string(),
                serde_json::Value::String(aid.clone()),
            );
            params.insert("files".to_string(), serde_json::Value::Array(files_json));
            params.insert(
                "project".to_string(),
                serde_json::Value::String(project.to_string()),
            );
            // 用文件相对路径匹配 Method/Class 并挂 PART_OF
            graph
                .write_query(
                    r#"MATCH (a:Artifact {artifact_id: $artifact_id})
                    UNWIND $files AS fp
                    MATCH (c {project: $project})
                    WHERE (c:Class OR c:Method) AND c.file_path = fp
                    MERGE (c)-[:PART_OF]->(a)"#,
                    params,
                )
                .await?;
            part_of_edges += files.len();
        }
    }

    // 3. 切片 B：DEPENDS_ON 依赖边 —— 本项目各制品之间的依赖，
    //    以及指向「库中已存在」的其它制品（含跨项目：同一制品跨项目
    //    收敛到同一 Artifact 节点，因此依赖边天然跨项目）。
    //    依赖目标 = 与 src 同类型的 (type, dep_name) 全局 artifact_id，
    //    Cypher 端按 artifact_id 匹配库内已存在节点（MERGE 幂等、天然跨项目）。
    let mut depends_on_edges = 0usize;
    {
        for (b, _) in &blocks {
            if b.dependencies.is_empty() {
                continue;
            }
            let src_id = b.artifact_id.clone();
            let mut edges: Vec<(String, String)> = Vec::new();
            for (_dep_group, dep_name) in &b.dependencies {
                let dst_id =
                    crate::domain::id::make_artifact_id(b.artifact_type.as_str(), dep_name);
                if dst_id != src_id {
                    edges.push((src_id.clone(), dst_id));
                }
            }
            for chunk in edges.chunks(200) {
                let edges_json: Vec<serde_json::Value> = chunk
                    .iter()
                    .map(|(s, d)| serde_json::json!({ "src": s, "dst": d }))
                    .collect();
                let mut params = HashMap::new();
                params.insert("edges".to_string(), serde_json::Value::Array(edges_json));
                let _ = graph
                    .write_query(
                        r#"UNWIND $edges AS e
                        MATCH (a:Artifact {artifact_id: e.src})
                        MATCH (b:Artifact {artifact_id: e.dst})
                        MERGE (a)-[:DEPENDS_ON]->(b)"#,
                        params,
                    )
                    .await;
            }
            depends_on_edges += edges.len();
        }
    }

    tracing::info!(
        project = %project,
        artifacts_written,
        part_of_edges,
        depends_on_edges,
        "Artifact 落图完成（切片 A+B：manifest → Artifact + PART_OF + DEPENDS_ON）"
    );

    Ok(ArtifactWriteOutcome {
        artifacts_written,
        part_of_edges,
        depends_on_edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_artifacts_discovers_maven_modules() {
        // 用临时目录造一个多模块 Maven 项目
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pay-offen-sdk-java/src")).unwrap();
        std::fs::create_dir_all(root.join("pay-offen-service/src")).unwrap();
        std::fs::write(
            root.join("pom.xml"),
            r#"<project><groupId>com.offen</groupId><artifactId>pay-offen-parent</artifactId><version>1.0</version></project>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("pay-offen-sdk-java/pom.xml"),
            r#"<project><groupId>com.offen</groupId><artifactId>pay-offen-sdk-java</artifactId><version>1.2.3</version>
            <dependencies><dependency><groupId>com.offen</groupId><artifactId>pay-offen-common</artifactId></dependency></dependencies>
            </project>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("pay-offen-service/pom.xml"),
            r#"<project><groupId>com.offen</groupId><artifactId>pay-offen-service</artifactId><version>2.0</version>
            <dependencies><dependency><groupId>com.offen</groupId><artifactId>pay-offen-sdk-java</artifactId></dependency></dependencies>
            </project>"#,
        )
        .unwrap();

        let blocks = collect_artifacts("offen-pay", root);
        assert!(blocks.len() >= 3, "应发现 3 个制品, 实际 {}", blocks.len());
        let names: Vec<&str> = blocks.iter().map(|(b, _)| b.name.as_str()).collect();
        assert!(names.contains(&"pay-offen-sdk-java"));
        assert!(names.contains(&"pay-offen-service"));
        // 验证 ArtifactBlock 的关键属性（依赖坐标已在 ManifestArtifact 解析器层测试覆盖）
        let sdk = blocks
            .iter()
            .find(|(b, _)| b.name == "pay-offen-sdk-java")
            .unwrap();
        let svc = blocks
            .iter()
            .find(|(b, _)| b.name == "pay-offen-service")
            .unwrap();
        // artifact_id 跨项目唯一（不含 project）
        assert_eq!(sdk.0.artifact_id, "dt://artifact/jar/pay-offen-sdk-java");
        assert_eq!(sdk.0.project, "offen-pay");
        // 模块路径前缀（PART_OF 归属用）
        assert_eq!(sdk.0.path_prefix, "pay-offen-sdk-java/");
        assert_eq!(svc.0.path_prefix, "pay-offen-service/");
        // 依赖坐标保留（切片 B DEPENDS_ON 数据源）
        assert_eq!(
            svc.0.dependencies,
            vec![("com.offen".to_string(), "pay-offen-sdk-java".to_string())]
        );
        // 外部依赖（pay-offen-common 未索引）不产生制品节点——留待切片 B 占位
        assert!(!names.contains(&"pay-offen-common"));
    }
}
