//! 候选项目发现：未注册目录的一级子目录扫描（depth<=2 找源码文件）。

use crate::application::sense::Candidate;
use std::path::{Path, PathBuf};

const SOURCE_EXTS: &[&str] = &["java", "py", "ts", "tsx", "js", "jsx", "go", "rs", "php"];

/// 扫描 dir 的一级子目录，返回含源码的候选项目（按 path 排序）。
/// 排除：ScanConfig::default().ignore_dirs 目录名 + ignored_dirs_file 中列出的绝对路径。
pub fn scan_candidates(dir: &Path, ignored_dirs_file: &Path) -> Vec<Candidate> {
    let extra_ignored = read_ignored_file(ignored_dirs_file);
    let default_ignored = crate::domain::types::ScanConfig::default().ignore_dirs;

    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if default_ignored.contains(&name) || name.starts_with('.') {
            continue;
        }
        if extra_ignored.iter().any(|p| p == &path) {
            continue;
        }
        if has_source_within(&path, 2) {
            out.push(Candidate {
                path: path.display().to_string(),
                suggested_name: name.clone(),
                build_cmd: format!("dt build --path {} --name {}", path.display(), name),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// ignored_dirs.yaml：逐行绝对路径（# 开头为注释）。
fn read_ignored_file(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .map(|c| {
            c.lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn has_source_within(dir: &Path, depth: u8) -> bool {
    let default_ignored = crate::domain::types::ScanConfig::default().ignore_dirs;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // depth 只限制目录下钻层数；本层文件始终检查
            if depth > 0
                && !default_ignored.contains(&name)
                && !name.starts_with('.')
                && has_source_within(&path, depth - 1)
            {
                return true;
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if SOURCE_EXTS.contains(&ext) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_source_dirs_and_skips_noise() {
        let tmp = std::env::temp_dir().join(format!("dt-sense-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("proj-a/src")).unwrap();
        std::fs::write(tmp.join("proj-a/src/Main.java"), "class Main {}").unwrap();
        std::fs::create_dir_all(tmp.join("node_modules/dep")).unwrap();
        std::fs::write(tmp.join("node_modules/dep/x.js"), "x").unwrap();
        std::fs::create_dir_all(tmp.join("docs-only")).unwrap();
        std::fs::write(tmp.join("docs-only/README.md"), "doc").unwrap();
        std::fs::create_dir_all(tmp.join("proj-b")).unwrap(); // 无源码
        std::fs::write(tmp.join("proj-b/config.yaml"), "x: 1").unwrap();

        let ignored = tmp.join("ignored.yaml");
        std::fs::write(
            &ignored,
            format!("# comment\n{}\n", tmp.join("proj-c").display()),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.join("proj-c")).unwrap();
        std::fs::write(tmp.join("proj-c/app.py"), "x=1").unwrap();

        let out = scan_candidates(&tmp, &ignored);
        let names: Vec<&str> = out.iter().map(|c| c.suggested_name.as_str()).collect();
        assert_eq!(names, vec!["proj-a"]);
        assert!(out[0].build_cmd.contains("dt build --path"));
        assert!(out[0].build_cmd.contains("--name proj-a"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn depth2_source_counts() {
        let tmp = std::env::temp_dir().join(format!("dt-sense-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("deep/src/main")).unwrap();
        std::fs::write(tmp.join("deep/src/main/app.go"), "package main").unwrap();
        let out = scan_candidates(&tmp, &tmp.join("none.yaml"));
        assert_eq!(out.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
