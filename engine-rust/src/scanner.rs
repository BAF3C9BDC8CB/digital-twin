use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config;

pub fn collect_files(root: &str) -> Vec<PathBuf> {
    let ignore_dirs = config::ignore_dirs();
    let ignore_ext = config::ignore_ext();
    let ignore_files = config::ignore_files();
    let max_size = config::max_file_size();

    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().is_dir() { !ignore_dirs.contains(name.as_ref()) } else { true }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|path| {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ignore_ext.contains(format!(".{}", ext).as_str()) { return false; }
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if ignore_files.contains(name) { return false; }
                if name.ends_with(".min.js") || name.ends_with(".bundle.js") { return false; }
                if name.contains(".generated.") { return false; }
            }
            path.metadata().map(|m| m.len() <= max_size).unwrap_or(false)
        })
        .collect()
}

pub fn rel_path(root: &str, path: &Path) -> String {
    path.strip_prefix(Path::new(root)).unwrap_or(path).to_string_lossy().to_string()
}
