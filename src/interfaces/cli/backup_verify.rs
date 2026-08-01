//! Backup verification and checksum management.
//!
//! Generates and validates SHA256 checksums for backup files.

use crate::interfaces::cli::backup::{VerifyFileResult, VerifyReport};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;

/// Regular files in a backup directory that should be checksummed.
const BACKUP_FILES: &[&str] = &["memgraph.dump", "qdrant.snapshot", "sqlite.copy"];

/// Generate SHA256 checksums for all backup files in a directory.
///
/// Returns a map of `filename → sha256_hex`.
pub async fn generate_checksums(
    backup_dir: &Path,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut checksums = std::collections::HashMap::new();

    for file_name in BACKUP_FILES {
        let file_path = backup_dir.join(file_name);
        if !file_path.exists() {
            continue;
        }
        let content = tokio::fs::read(&file_path).await?;
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let hash = hex::encode(hasher.finalize());
        checksums.insert(file_name.to_string(), hash);
    }

    // Write checksums file
    let checksum_path = backup_dir.join("checksums.sha256");
    let mut lines = Vec::new();
    for (name, hash) in &checksums {
        lines.push(format!("{}  {}", hash, name));
    }
    tokio::fs::write(&checksum_path, lines.join("\n") + "\n").await?;

    Ok(checksums)
}

/// Compute SHA256 hash of a file.
async fn compute_sha256(path: &Path) -> anyhow::Result<String> {
    let content = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    Ok(hex::encode(hasher.finalize()))
}

/// Verify all files in a backup directory against their stored checksums.
///
/// Reads `checksums.sha256` from the backup directory and validates
/// each listed file.
pub async fn verify_backup(backup_dir: &Path) -> anyhow::Result<VerifyReport> {
    let start = Instant::now();
    let checksum_path = backup_dir.join("checksums.sha256");

    // Parse stored checksums
    let content = tokio::fs::read_to_string(&checksum_path).await?;
    let expected: std::collections::HashMap<String, String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(2, "  ").collect();
            if parts.len() == 2 {
                Some((parts[1].to_string(), parts[0].to_string()))
            } else {
                None
            }
        })
        .collect();

    let mut file_results = Vec::new();
    let mut all_valid = true;

    for (file_name, expected_hash) in &expected {
        let file_path = backup_dir.join(file_name);
        let (valid, actual) = if file_path.exists() {
            match compute_sha256(&file_path).await {
                Ok(hash) => (hash == *expected_hash, hash),
                Err(e) => {
                    tracing::error!("计算 {} 的哈希值失败: {}", file_name, e);
                    (false, format!("error: {e}"))
                }
            }
        } else {
            (false, "file not found".to_string())
        };

        if !valid {
            all_valid = false;
        }

        file_results.push(VerifyFileResult {
            file_name: file_name.clone(),
            valid,
            expected: expected_hash.clone(),
            actual,
        });
    }

    // Also verify files that exist but aren't in checksums (warn only)
    for file_name in BACKUP_FILES {
        if !expected.contains_key(*file_name) && backup_dir.join(file_name).exists() {
            tracing::warn!("文件 {} 存在但不在 checksums.sha256 中", file_name);
        }
    }

    Ok(VerifyReport {
        backup_dir: backup_dir.to_path_buf(),
        date: String::new(),
        all_valid,
        files: file_results,
        duration_seconds: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn generate_checksums_for_existing_files() {
        let dir = TempDir::new().unwrap();

        // Create a test file
        tokio::fs::write(dir.path().join("memgraph.dump"), b"test memgraph data")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("qdrant.snapshot"), b"test qdrant data")
            .await
            .unwrap();

        let checksums = generate_checksums(dir.path()).await.unwrap();

        assert!(checksums.contains_key("memgraph.dump"));
        assert!(checksums.contains_key("qdrant.snapshot"));
        assert!(!checksums.contains_key("sqlite.copy")); // doesn't exist

        // Verify checksums file was written
        let checksum_file = dir.path().join("checksums.sha256");
        assert!(checksum_file.exists());
    }

    #[tokio::test]
    async fn verify_backup_all_valid() {
        let dir = TempDir::new().unwrap();

        // Create test files + checksums
        tokio::fs::write(dir.path().join("memgraph.dump"), b"test memgraph data")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("sqlite.copy"), b"test sqlite data")
            .await
            .unwrap();

        let memgraph_hash = compute_sha256(&dir.path().join("memgraph.dump"))
            .await
            .unwrap();
        let sqlite_hash = compute_sha256(&dir.path().join("sqlite.copy"))
            .await
            .unwrap();

        let checksum_content = format!(
            "{}  memgraph.dump\n{}  sqlite.copy\n",
            memgraph_hash, sqlite_hash
        );
        tokio::fs::write(dir.path().join("checksums.sha256"), checksum_content)
            .await
            .unwrap();

        let report = verify_backup(dir.path()).await.unwrap();
        assert!(report.all_valid);
        assert_eq!(report.files.len(), 2);
        for f in &report.files {
            assert!(f.valid, "file {} should be valid", f.file_name);
        }
    }

    #[tokio::test]
    async fn verify_backup_detects_tampering() {
        let dir = TempDir::new().unwrap();

        tokio::fs::write(dir.path().join("memgraph.dump"), b"original data")
            .await
            .unwrap();

        let original_hash = compute_sha256(&dir.path().join("memgraph.dump"))
            .await
            .unwrap();

        // Write checksum for original
        let checksum_content = format!("{}  memgraph.dump\n", original_hash);
        tokio::fs::write(dir.path().join("checksums.sha256"), checksum_content)
            .await
            .unwrap();

        // Tamper with the file
        tokio::fs::write(dir.path().join("memgraph.dump"), b"tampered data")
            .await
            .unwrap();

        let report = verify_backup(dir.path()).await.unwrap();
        assert!(!report.all_valid);
        assert_eq!(report.files.len(), 1);
        assert!(!report.files[0].valid);
    }

    #[tokio::test]
    async fn compute_sha256_is_deterministic() {
        let hash1 = compute_sha256(Path::new("Cargo.toml")).await;
        let hash2 = compute_sha256(Path::new("Cargo.toml")).await;
        assert_eq!(hash1.unwrap(), hash2.unwrap());
    }

    #[tokio::test]
    async fn compute_sha256_length() {
        let hash = compute_sha256(Path::new("Cargo.toml")).await.unwrap();
        assert_eq!(hash.len(), 64); // SHA256 hex is 64 chars
    }
}
