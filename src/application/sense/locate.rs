//! 项目定位：git root + config.yaml 项目最长前缀匹配。

use std::path::{Path, PathBuf};

/// config.yaml 项目列表中最长前缀匹配；path 必须先 canonicalize（调用方负责）。
pub fn match_project<'a>(
    path: &Path,
    projects: &'a [(String, PathBuf)],
) -> Option<(&'a str, &'a Path)> {
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

/// 反查：input 是哪些已注册项目的**直接父目录**（容器/base 场景）。
///
/// 只算直接子级（root.parent() == input），避免在 /data/aflmProjects 这类顶层
/// 祖先目录列出全部 60+ 注册项目造成简报爆炸；按 canonicalize 后的真实路径去重
/// （同一目录在不同 base 下重复注册时只保留一条）。
pub fn collect_base_children<'a>(
    input: &Path,
    projects: &'a [(String, PathBuf)],
) -> Vec<(&'a str, &'a Path)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(&str, &Path)> = Vec::new();
    for (name, root) in projects {
        let Some(parent) = root.parent() else {
            continue;
        };
        let parent_canon = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        if parent_canon.as_path() == input {
            let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
            if seen.insert(canon) {
                out.push((name.as_str(), root.as_path()));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(b.1));
    out
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
            (
                "dt".into(),
                PathBuf::from("/data/myProject/digital-twin-v2"),
            ),
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

    #[test]
    fn container_lists_only_direct_children() {
        let binding = vec![
            (
                "offen-pay".into(),
                PathBuf::from("/data/aflmProjects/others/pay/uvp-offen-pay"),
            ),
            (
                "offenpay-ui".into(),
                PathBuf::from("/data/aflmProjects/others/pay/offenpay-ui"),
            ),
            (
                "third-center".into(),
                PathBuf::from("/data/aflmProjects/others/third-center"),
            ),
        ];
        let p = Path::new("/data/aflmProjects/others/pay");
        let children = collect_base_children(p, &binding);
        assert_eq!(children.len(), 2);
        let names: Vec<&str> = children.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"offen-pay") && names.contains(&"offenpay-ui"));
        assert!(!names.contains(&"third-center"));
    }

    #[test]
    fn container_dedups_same_canonical_path() {
        // 同一目录在 others 与 others/pay 两个 base 下重复注册 → 只保留一条
        let binding = vec![
            (
                "offen-pay".into(),
                PathBuf::from("/data/aflmProjects/others/pay/uvp-offen-pay"),
            ),
            (
                "offen-pay".into(),
                PathBuf::from("/data/aflmProjects/others/pay/uvp-offen-pay"),
            ),
        ];
        let p = Path::new("/data/aflmProjects/others/pay");
        let children = collect_base_children(p, &binding);
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn non_container_returns_empty() {
        let binding = projects();
        let p = Path::new("/home/luis/other");
        assert!(collect_base_children(p, &binding).is_empty());
        // 祖先目录(非直接父)不列, 避免顶层 60+ 项爆炸
        let top = Path::new("/data/aflmProjects/others");
        let top_binding = vec![(
            "offenpay-ui".into(),
            PathBuf::from("/data/aflmProjects/others/pay/offenpay-ui"),
        )];
        assert!(collect_base_children(top, &top_binding).is_empty());
    }
}
