//! Artifact 组装器 —— 将 ManifestArtifact 转为可落图的 ArtifactBlock，
//! 并推导模块路径前缀（用于把代码文件按路径归属到制品）。

use crate::domain::id::make_artifact_id;
use crate::domain::types::{ArtifactBlock, ArtifactType, ManifestArtifact};

/// 语言标签（写入 Artifact.language）。
pub fn artifact_language(artifact_type: &ArtifactType) -> &'static str {
    match artifact_type {
        ArtifactType::Jar => "java",
        ArtifactType::Crate => "rust",
        ArtifactType::Python => "python",
        ArtifactType::Npm => "javascript",
        ArtifactType::Go => "go",
        ArtifactType::Other => "unknown",
    }
}

/// 由 ManifestArtifact + 归属项目 + manifest 相对路径组装 ArtifactBlock。
///
/// `manifest_rel_path`：manifest 文件相对项目根的路径（如 `pay-offen-sdk-java/pom.xml`）。
/// 其所在目录即模块根（path_prefix），用于把代码文件按路径前缀归属到该制品。
pub fn build_artifact_block(
    m: &ManifestArtifact,
    project: &str,
    manifest_rel_path: &str,
) -> ArtifactBlock {
    let artifact_type = m.artifact_type.clone();
    let type_str = artifact_type.as_str();
    let language = artifact_language(&artifact_type).to_string();
    let dir = manifest_dir(manifest_rel_path);
    ArtifactBlock {
        artifact_id: make_artifact_id(type_str, &m.name),
        name: m.name.clone(),
        group_id: m.group_id.clone(),
        version: m.version.clone(),
        artifact_type,
        language,
        project: project.to_string(),
        path_prefix: dir,
    }
}

/// manifest 相对路径 → 模块根目录（前缀匹配用）。
///
/// - 根级 manifest（`pom.xml`）→ 空（整个项目一个制品）
/// - 嵌套 manifest（`pay-offen-sdk-java/pom.xml`）→ `pay-offen-sdk-java/`
pub fn manifest_dir(manifest_rel_path: &str) -> String {
    let normalized = manifest_rel_path.replace('\\', "/");
    match normalized.rfind('/') {
        Some(idx) => format!("{}/", &normalized[..idx]),
        None => String::new(),
    }
}

/// 判断某个文件相对路径是否属于某制品（按 path_prefix 前缀归属）。
///
/// - path_prefix 为空 → 项目根制品，兜底所有未匹配更具体模块的文件
/// - 非空 → 文件路径必须以 `prefix` 开头才归属
pub fn file_belongs_to_artifact(file_rel_path: &str, path_prefix: &str) -> bool {
    let file = file_rel_path.replace('\\', "/");
    if path_prefix.is_empty() {
        // 根制品不拦截——由调用方决定（先匹配具体模块，剩余归根制品）
        true
    } else {
        file.starts_with(path_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::ArtifactType;

    #[test]
    fn manifest_dir_root_and_nested() {
        assert_eq!(manifest_dir("pom.xml"), "");
        assert_eq!(
            manifest_dir("pay-offen-sdk-java/pom.xml"),
            "pay-offen-sdk-java/"
        );
        assert_eq!(manifest_dir("a/b/Cargo.toml"), "a/b/");
    }

    #[test]
    fn artifact_id_is_project_agnostic() {
        let m = ManifestArtifact {
            name: "pay-offen-sdk-java".into(),
            group_id: "com.offen".into(),
            version: "1.2.3".into(),
            artifact_type: ArtifactType::Jar,
            dependencies: vec![],
        };
        let b1 = build_artifact_block(&m, "proj-a", "pay-offen-sdk-java/pom.xml");
        let b2 = build_artifact_block(&m, "proj-b", "pay-offen-sdk-java/pom.xml");
        // 同一制品跨项目收敛到同一 artifact_id
        assert_eq!(b1.artifact_id, b2.artifact_id);
        assert_eq!(b1.artifact_id, "dt://artifact/jar/pay-offen-sdk-java");
        assert_eq!(b1.path_prefix, "pay-offen-sdk-java/");
    }

    #[test]
    fn file_belongs_prefix_matching() {
        assert!(file_belongs_to_artifact(
            "pay-offen-sdk-java/src/main/java/com/offen/pay/OffenPayRequest.java",
            "pay-offen-sdk-java/"
        ));
        assert!(!file_belongs_to_artifact(
            "pay-offen-service/src/main/java/Other.java",
            "pay-offen-sdk-java/"
        ));
        assert!(file_belongs_to_artifact("src/main.rs", ""));
    }
}
