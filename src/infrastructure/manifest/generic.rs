//! 通用回退解析器——项目没有任何已知 manifest 时，用根目录名
//! 生成一个 `Other` 类型制品（无依赖），保证每个索引项目都有
//! 至少一个 Artifact 可挂 PART_OF。

use crate::domain::types::{ArtifactType, ManifestArtifact};

/// 用项目根目录名生成回退制品。无依赖、无版本。
pub fn generic_from_dir_name(dir_name: &str) -> ManifestArtifact {
    ManifestArtifact {
        name: dir_name.to_string(),
        group_id: String::new(),
        version: String::new(),
        artifact_type: ArtifactType::Other,
        dependencies: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_artifact_from_dir() {
        let m = generic_from_dir_name("my-project");
        assert_eq!(m.name, "my-project");
        assert!(m.dependencies.is_empty());
        assert_eq!(m.artifact_type, ArtifactType::Other);
    }
}
