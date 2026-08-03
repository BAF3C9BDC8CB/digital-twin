//! 项目定位：git root + config.yaml 项目最长前缀匹配。

use std::path::{Path, PathBuf};

/// config.yaml 项目列表中最长前缀匹配；path 必须先 canonicalize（调用方负责）。
pub fn match_project<'a>(path: &Path, projects: &'a [(String, PathBuf)]) -> Option<(&'a str, &'a Path)> {
    projects
        .iter()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.as_os_str().len())
        .map(|(n, r)| (n.as_str(), r.as_path()))
}

/// 从 path 向上找含 .git 的目录（用于提示，不参与归属判定）。
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        cur = p.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projects() -> Vec<(String, PathBuf)> {
        vec![
            ("warehouse".into(), PathBuf::from("/data/aflm/warehouse")),
            (
                "warehouse-api".into(),
                PathBuf::from("/data/aflm/warehouse/warehouse-api"),
            ),
            ("dt".into(), PathBuf::from("/data/myProject/digital-twin-v2")),
        ]
    }

    #[test]
    fn longest_prefix_wins_for_nested_project() {
        let p = Path::new("/data/aflm/warehouse/warehouse-api/src/main");
        let binding = projects();
        let (name, root) = match_project(p, &binding).unwrap();
        assert_eq!(name, "warehouse-api");
        assert_eq!(root, Path::new("/data/aflm/warehouse/warehouse-api"));
    }

    #[test]
    fn falls_back_to_parent_project() {
        let p = Path::new("/data/aflm/warehouse/shared-lib");
        let binding = projects();
        let (name, _) = match_project(p, &binding).unwrap();
        assert_eq!(name, "warehouse");
    }

    #[test]
    fn unregistered_returns_none() {
        let p = Path::new("/home/luis/other");
        let binding = projects();
        assert!(match_project(p, &binding).is_none());
    }

    #[test]
    fn exact_root_matches() {
        let p = Path::new("/data/myProject/digital-twin-v2");
        let binding = projects();
        assert_eq!(match_project(p, &binding).unwrap().0, "dt");
    }
}
