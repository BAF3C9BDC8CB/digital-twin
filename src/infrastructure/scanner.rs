//! File scanner — discovers project source files with configurable filters.
//!
//! Uses `walkdir` to recursively traverse project directories, applying
//! ignore rules for directories, extensions, and file size.

use crate::domain::types::ScanConfig;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Collect all source files from a project root, respecting the scan config.
///
/// Returns absolute file paths that pass all filters.
pub fn collect_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                !config.ignore_dirs.contains(name.as_ref())
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // Filter by extension
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let dot_ext = format!(".{}", ext);
                if config.ignore_ext.contains(&dot_ext) {
                    return false;
                }
            }
            // Filter by file name (minified/generated)
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".min.js") || name.ends_with(".bundle.js") {
                    return false;
                }
                if name.contains(".generated.") {
                    return false;
                }
            }
            // Filter by size
            match path.metadata() {
                Ok(m) => m.len() <= config.max_file_size,
                Err(_) => false,
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Collect document files (.md, .txt, .pdf) from a project root.
///
/// Uses `ScanConfig::document_extensions` to match files and
/// `ScanConfig::max_doc_file_size` for size filtering. Still respects
/// `ignore_dirs`.
pub fn collect_document_files(root: &Path, config: &ScanConfig) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                !config.ignore_dirs.contains(name.as_ref())
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let path = e.path();
            // Only match document extensions
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if config.document_extensions.contains(ext) {
                    // Filter by size using document-specific limit
                    match path.metadata() {
                        Ok(m) => m.len() <= config.max_doc_file_size,
                        Err(_) => false,
                    }
                } else {
                    false
                }
            } else {
                // Files without extension — not a doc
                false
            }
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Compute the relative path of a file from the project root.
pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// Compute a SHA1 hash of file contents and return (hash_hex, mtime_secs).
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

/// Compute SHA1 hashes for a batch of files in parallel using rayon-style
/// simple multithreading. Returns `(rel_path, sha1_hex, mtime)` tuples.
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

/// Detect which files have changed compared to stored snapshots.
///
/// Returns `(changed, deleted)`:
/// - `changed`: files that are new or have a different hash.
/// - `deleted`: files that were previously indexed but no longer exist.
pub fn detect_changes(
    current_hashes: &HashMap<String, (String, f64)>,
    stored_snapshots: &HashMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut changed = Vec::new();
    let mut deleted = Vec::new();

    for (path, (hash, _mtime)) in current_hashes {
        match stored_snapshots.get(path) {
            Some(stored_hash) if stored_hash == hash => {
                // unchanged
            }
            _ => {
                // new or modified
                changed.push(path.clone());
            }
        }
    }

    // Files in stored but not on disk are deleted
    for stored_path in stored_snapshots.keys() {
        if !current_hashes.contains_key(stored_path) {
            deleted.push(stored_path.clone());
        }
    }

    (changed, deleted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_files_respects_ignore_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a normal file
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        // Create a file in an ignored dir
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/foo.rs"), "fn foo() {}").unwrap();

        let config = ScanConfig::default();
        let files = collect_files(root, &config);

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(names.contains(&"main.rs".to_string()));
        // target/ is ignored, so foo.rs should not appear
        assert!(!names.contains(&"foo.rs".to_string()));
    }

    #[test]
    fn compute_file_hash_returns_sha1_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();

        let (hash, mtime) = compute_file_hash(&path).unwrap();
        assert_eq!(hash.len(), 64); // SHA256 hex output is 64 chars
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

        // src/a.rs has same hash → not changed
        assert!(!changed.contains(&"src/a.rs".to_string()));
        // src/b.rs has different hash → changed
        assert!(changed.contains(&"src/b.rs".to_string()));
        // src/deleted.rs is in stored but not current → deleted
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
        // PDF is skipped because we can't easily create a real one; txt and md suffice
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
        assert!(!names.contains(&"readme.md".to_string())); // in node_modules
    }

    #[test]
    fn collect_document_files_respects_max_doc_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("small.md"), "# small").unwrap();
        // Write a file larger than default 5MB max
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
