//! 文件扫描器——按可配置的过滤规则发现项目源文件。
//!
//! 使用 `walkdir` 递归遍历项目目录，应用目录、扩展名与文件大小的
//! 忽略规则。

use crate::domain::types::ScanConfig;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 判断目录是否应被忽略。
///
/// `ignore_dirs` 支持两种匹配：
/// - 单段目录名（如 "node_modules"、"target"）—— 匹配任何同名目录；
/// - 带路径前缀的条目（如 "target/debug"、"node_modules/.cache"）
///   —— 匹配 `entry_path` 相对扫描根的子路径，或以该前缀开头的深层路径。
/// 传入 `rel`（entry_path 相对 root 的正斜杠路径）用于前缀匹配。
pub fn dir_is_ignored(
    rel: &str,
    name: &str,
    ignore_dirs: &std::collections::HashSet<String>,
) -> bool {
    if ignore_dirs.contains(name) {
        return true;
    }
    // 深层路径前缀匹配：例如 "src/main/java/.../target" 命中 "target"，
    // "node_modules/.cache/foo" 命中 "node_modules/.cache"。
    let rel_std = rel.replace('\\', "/");
    ignore_dirs
        .iter()
        .filter(|pat| pat.contains('/')) // 仅对带路径的条目做前缀匹配
        .any(|pat| {
            let pat = pat.trim_end_matches('/');
            !pat.is_empty() && (rel_std == pat || rel_std.starts_with(&format!("{pat}/")))
        })
}

/// 从项目根目录收集所有源文件，遵循扫描配置。
///
/// 返回通过所有过滤器的绝对文件路径。
pub fn collect_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                let rel = rel_path(root, entry.path());
                !dir_is_ignored(&rel, name.as_ref(), &config.ignore_dirs)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // 按文件名过滤（精确匹配 ignore_files）
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if config.ignore_files.contains(name) {
                    return false;
                }
            }
            // 按扩展名过滤
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let dot_ext = format!(".{}", ext);
                if config.ignore_ext.contains(&dot_ext) {
                    return false;
                }
            }
            // 按文件名过滤（压缩/生成文件）
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".min.js") || name.ends_with(".bundle.js") {
                    return false;
                }
                if name.contains(".generated.") {
                    return false;
                }
            }
            // 按大小过滤
            match path.metadata() {
                Ok(m) => m.len() <= config.max_file_size,
                Err(_) => false,
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// 从项目根目录收集文档文件（.md、.txt、.pdf）。
///
/// 使用 `ScanConfig::document_extensions` 匹配文件，
/// 用 `ScanConfig::max_doc_file_size` 做大小过滤。仍然遵循
/// `ignore_dirs`。
pub fn collect_document_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                let rel = rel_path(root, entry.path());
                !dir_is_ignored(&rel, name.as_ref(), &config.ignore_dirs)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // 按文件名过滤（精确匹配 ignore_files，与 collect_files 一致）。
            // 否则 .gitlab-ci.yml/banner.txt 等无价值文件会绕过代码收集、
            // 以文档身份进入 doc_chunks。
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if config.ignore_files.contains(name) {
                    return false;
                }
            }
            // 只匹配文档扩展名
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if config.document_extensions.contains(ext) {
                    // 使用文档特定的限制按大小过滤
                    match path.metadata() {
                        Ok(m) => m.len() <= config.max_doc_file_size,
                        Err(_) => false,
                    }
                } else {
                    false
                }
            } else {
                // 无扩展名的文件——不是文档
                false
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// 计算文件相对项目根目录的路径。
pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// 计算文件内容的 SHA1 哈希并返回 (hash_hex, mtime_secs)。
pub fn compute_file_hash(path: &Path) -> Result<(String, f64), std::io::Error> {
    let content = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let hash = format!("{:x}", hasher.finalize());

    let mtime = path
        .metadata()?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    Ok((hash, mtime))
}

/// 使用 rayon 风格的简单多线程并行计算一批文件的 SHA1 哈希。
/// 返回 `(rel_path, sha1_hex, mtime)` 元组。
pub fn compute_hashes(root: &Path, files: &[PathBuf]) -> Vec<(String, String, f64)> {
    files
        .iter()
        .filter_map(|path| {
            let (hash, mtime) = compute_file_hash(path).ok()?;
            let rel = rel_path(root, path);
            Some((rel, hash, mtime))
        })
        .collect()
}

/// 检测与存储的快照相比哪些文件发生了变化。
///
/// 返回 `(changed, deleted)`：
/// - `changed`：新文件或哈希不同的文件。
/// - `deleted`：之前已索引但当前不再存在的文件。
pub fn detect_changes(
    current_hashes: &HashMap<String, (String, f64)>,
    stored_snapshots: &HashMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut changed = Vec::new();
    let mut deleted = Vec::new();

    for (path, (hash, _mtime)) in current_hashes {
        match stored_snapshots.get(path) {
            Some(stored_hash) if stored_hash == hash => {
                // 未变化
            }
            _ => {
                // 新增或已修改
                changed.push(path.clone());
            }
        }
    }

    // 已存储但磁盘上不存在的文件是被删除的
    for stored_path in stored_snapshots.keys() {
        if !current_hashes.contains_key(stored_path) {
            deleted.push(stored_path.clone());
        }
    }

    (changed, deleted)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_files_respects_ignore_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // 创建普通文件
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        // 在被忽略的目录中创建文件
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/foo.rs"), "fn foo() {}").unwrap();

        let config = ScanConfig::default();
        let files = collect_files(root, &config);

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        // target/ 被忽略，因此 foo.rs 不应出现
        assert!(!names.contains(&"foo.rs".to_string()));
    }

    #[test]
    fn collect_files_respects_path_prefix_and_ignore_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // 深层路径前缀忽略：node_modules/.cache 下的文件不应出现
        fs::create_dir_all(root.join("node_modules/.cache")).unwrap();
        fs::write(root.join("node_modules/.cache/cache.rs"), "fn c() {}").unwrap();
        // ignore_files 精确匹配：Cargo.lock 不应出现
        fs::write(root.join("Cargo.lock"), "# lock").unwrap();
        // 正常文件应出现
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let mut config = ScanConfig::default();
        config.ignore_dirs.insert("node_modules/.cache".to_string());
        config.ignore_files.insert("Cargo.lock".to_string());

        let files = collect_files(root, &config);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"cache.rs".to_string()));
        assert!(!names.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn compute_file_hash_returns_sha1_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();

        let (hash, mtime) = compute_file_hash(&path).unwrap();
        assert_eq!(hash.len(), 64); // SHA256 十六进制输出为 64 个字符
        assert!(mtime > 0.0);
    }

    #[test]
    fn detect_changes_finds_new_and_modified() {
        let mut current: HashMap<String, (String, f64)> = HashMap::new();
        current.insert("src/a.rs".into(), ("hash_a".into(), 1.0));
        current.insert("src/b.rs".into(), ("hash_b_new".into(), 2.0));

        let mut stored: HashMap<String, String> = HashMap::new();
        stored.insert("src/a.rs".into(), "hash_a".into());
        stored.insert("src/b.rs".into(), "hash_b_old".into());
        stored.insert("src/deleted.rs".into(), "hash_d".into());

        let (changed, deleted) = detect_changes(&current, &stored);

        // src/a.rs 哈希相同→未变化
        assert!(!changed.contains(&"src/a.rs".to_string()));
        // src/b.rs 哈希不同→已变化
        assert!(changed.contains(&"src/b.rs".to_string()));
        // src/deleted.rs 在存储中但不在当前→已删除
        assert!(deleted.contains(&"src/deleted.rs".to_string()));
        assert_eq!(changed.len(), 1);
        assert_eq!(deleted.len(), 1);
    }

    #[test]
    fn rel_path_strips_root() {
        let root = Path::new("/project");
        let file = Path::new("/project/src/main.rs");
        assert_eq!(rel_path(root, file), "src/main.rs");
    }

    #[test]
    fn rel_path_fallback_on_non_prefix() {
        let root = Path::new("/project");
        let file = Path::new("/other/baz.rs");
        assert_eq!(rel_path(root, file), "/other/baz.rs");
    }

    #[test]
    fn collect_document_files_finds_md_txt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/readme.md"), "# Hello").unwrap();
        fs::write(root.join("docs/notes.txt"), "some notes").unwrap();
        // PDF 被跳过，因为我们不容易创建真正的 PDF；txt 与 md 已足够
        fs::write(root.join("docs/image.png"), "fake").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let config = ScanConfig::default();
        let docs = collect_document_files(root, &config);

        let names: Vec<String> = docs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"readme.md".to_string()));
        assert!(names.contains(&"notes.txt".to_string()));
        assert!(!names.contains(&"image.png".to_string()));
        assert!(!names.contains(&"main.rs".to_string()));
    }

    #[test]
    fn collect_document_files_respects_ignore_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("node_modules/docs")).unwrap();
        fs::write(root.join("node_modules/docs/readme.md"), "# ignored").unwrap();
        fs::write(root.join("README.md"), "# top-level").unwrap();

        let config = ScanConfig::default();
        let docs = collect_document_files(root, &config);

        let names: Vec<String> = docs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"README.md".to_string()));
        assert!(!names.contains(&"readme.md".to_string())); // 位于 node_modules 中
    }

    #[test]
    fn collect_document_files_respects_max_doc_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("small.md"), "# small").unwrap();
        // 写入一个超过默认 5MB 上限的大文件
        let large_content = "x".repeat(6_000_000);
        fs::write(root.join("large.md"), &large_content).unwrap();

        let config = ScanConfig::default();
        let docs = collect_document_files(root, &config);

        let names: Vec<String> = docs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"small.md".to_string()));
        assert!(!names.contains(&"large.md".to_string()));
    }
}
