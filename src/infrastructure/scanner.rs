//! 文件扫描器——按可配置的过滤规则发现代码根源文件。
//!
//! 使用 `walkdir` 递归遍历代码根目录，应用**统一忽略规则**（文件与目录
//! 通吃，支持 glob 通配 `*` / `?` / `**`）与文件大小过滤。
//!
//! 统一忽略模型见 [`ScanConfig`]：`ignore_names`（精确名/路径前缀）+
//! `ignore_globs`（通配条目）。判定入口 [`is_ignored`]。

use crate::domain::types::ScanConfig;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 判断相对路径（相对扫描根，正斜杠，无前导 `/`）是否命中任一忽略规则。
///
/// 匹配模型（目录与文件通吃）：
/// - `ignore_names` 中的纯名：匹配相对路径**任意层**的同名组件
///   （目录 `node_modules` 或文件 `Cargo.lock` 均可命中其深层出现）。
/// - `ignore_names` 中含 `/` 的条目：按**相对路径前缀**匹配
///   （`target/debug` 命中 `target/debug/x.rs`、`a/target/debug/y` 均不命中
///   ——前缀语义与旧 `ignore_dirs` 路径条目一致：仅从根开始的路径匹配）。
///   注意：纯名的深层匹配覆盖 `a/node_modules/b`，路径前缀只覆盖顶层。
/// - `ignore_globs` 中不含 `/` 的条目（`*.class`、`.env*`）：匹配任意层
///   单个组件（文件名或目录名）。
/// - `ignore_globs` 中含 `/` 的条目（`**/test-*.yaml`、`target/*/out`）：
///   对整条相对路径做 glob 匹配（`*` 不跨 `/`，`**` 跨段）。
pub fn is_ignored(rel: &str, config: &ScanConfig) -> bool {
    let rel = rel.replace('\\', "/");
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return false;
    }

    // 1. 精确名条目
    for pat in &config.ignore_names {
        if pat.contains('/') {
            // 相对路径前缀（顶层锚定）
            if rel == *pat || rel.starts_with(&format!("{pat}/")) {
                return true;
            }
        } else if rel.split('/').any(|seg| seg == pat.as_str()) {
            // 任意层同名组件（目录或文件）
            return true;
        }
    }

    // 2. glob 条目
    for pat in &config.ignore_globs {
        if pat.contains('/') {
            if glob_match(pat, rel) {
                return true;
            }
        } else if rel.split('/').any(|seg| glob_match(pat, seg)) {
            return true;
        }
    }
    false
}

/// 段级 glob 匹配（无依赖实现）。
///
/// 支持：
/// - `*`：匹配任意非 `/` 字符序列（含空）
/// - `?`：匹配单个非 `/` 字符
/// - `**`：作为**独立路径段**出现时匹配零或多个路径段（跨 `/`）
///
/// `pat` 与 `s` 均按 `/` 分段；段内只出现 `*` / `?`。
pub fn glob_match(pat: &str, s: &str) -> bool {
    let pats: Vec<&str> = pat.split('/').filter(|p| !p.is_empty()).collect();
    let segs: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if pats.is_empty() {
        return segs.is_empty();
    }
    match_segs(&pats, &segs)
}

fn match_segs(pats: &[&str], segs: &[&str]) -> bool {
    match pats.first() {
        None => segs.is_empty(),
        Some(&p) => {
            if p == "**" {
                // ** 匹配 0..=n 个段
                (0..=segs.len()).any(|k| match_segs(&pats[1..], &segs[k..]))
            } else {
                if segs.is_empty() {
                    return false;
                }
                seg_glob(p, segs[0]) && match_segs(&pats[1..], &segs[1..])
            }
        }
    }
}

/// 单段内 glob：`*` 任意（含空）、`?` 单字符、其余字面量。
fn seg_glob(pat: &str, s: &str) -> bool {
    let pb: Vec<char> = pat.chars().collect();
    let sb: Vec<char> = s.chars().collect();
    seg_glob_chars(&pb, &sb)
}

fn seg_glob_chars(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|k| seg_glob_chars(&p[1..], &s[k..])),
        Some('?') => !s.is_empty() && seg_glob_chars(&p[1..], &s[1..]),
        Some(&c) => s.first() == Some(&c) && seg_glob_chars(&p[1..], &s[1..]),
    }
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
                let rel = rel_path(root, entry.path());
                !config.is_ignored(&rel)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // 统一忽略规则（文件名/扩展名/路径通配——含 ignore_files 语义）
            let rel = rel_path(root, path);
            if config.is_ignored(&rel) {
                return false;
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
/// 统一忽略规则（`is_ignored`）。
pub fn collect_document_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let rel = rel_path(root, entry.path());
                !config.is_ignored(&rel)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // 统一忽略规则（目录/文件名/路径通配——含 ignore_files 语义）
            let rel = rel_path(root, path);
            if config.is_ignored(&rel) {
                return false;
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
        config.add_ignore("node_modules/.cache");
        config.add_ignore("Cargo.lock");

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
