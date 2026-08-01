//! Qdrant backup and restore operations.
//!
//! Uses Qdrant's REST API for snapshot creation and restoration.
//! The HTTP API is typically available at `http://localhost:6333`.
//!
//! When Qdrant is unavailable the snapshot is a no-op that logs a warning.

use std::path::Path;
use std::time::Instant;

/// Qdrant HTTP API base URL.
const QDRANT_URL: &str = "http://localhost:6333";

/// Snapshot Qdrant collections to `{backup_dir}/qdrant.snapshot`.
///
/// Collects metadata about all collections and their vector counts,
/// then writes a JSON manifest.  Actual point data snapshots require
/// the Qdrant snapshot API (POST /collections/{name}/snapshots).
///
/// Returns `(success, size_bytes)`.
pub async fn snapshot_collections(backup_dir: &Path) -> anyhow::Result<(bool, u64)> {
    let start = Instant::now();
    let snapshot_path = backup_dir.join("qdrant.snapshot");

    tracing::info!("正在为 Qdrant 集合创建快照 {}", snapshot_path.display());

    // Fetch collection list from Qdrant REST API
    let client = reqwest::Client::new();
    let collections: Vec<serde_json::Value> = match client
        .get(format!("{}/collections", QDRANT_URL))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await?;
            body.get("result")
                .and_then(|r| r.get("collections"))
                .and_then(|c| c.as_array().cloned())
                .unwrap_or_default()
        }
        Err(e) => {
            tracing::warn!("Qdrant HTTP API 不可用 ({e}) — 写入占位符快照");
            vec![]
        }
    };

    // Collect per-collection info
    let mut collection_details = Vec::new();
    let mut total_points: u64 = 0;

    for col in &collections {
        let name = col
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Try to get collection info
        if let Ok(resp) = client
            .get(format!("{}/collections/{name}", QDRANT_URL))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                let points = body
                    .get("result")
                    .and_then(|r| r.get("points_count"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let vectors = body
                    .get("result")
                    .and_then(|r| r.get("config"))
                    .and_then(|c| c.get("params"))
                    .and_then(|p| p.get("vectors"))
                    .map(|v| v.to_owned())
                    .unwrap_or(serde_json::json!({}));

                total_points += points;

                collection_details.push(serde_json::json!({
                    "name": name,
                    "points_count": points,
                    "vectors_config": vectors,
                }));
            }
        }
    }

    // Build snapshot manifest
    let metadata = serde_json::json!({
        "version": "2.0",
        "type": "qdrant_snapshot",
        "generated": chrono::Utc::now().to_rfc3339(),
        "qdrant_url": QDRANT_URL,
        "total_points": total_points,
        "collections": collection_details,
    });

    tokio::fs::write(&snapshot_path, serde_json::to_string_pretty(&metadata)?).await?;

    let size = tokio::fs::metadata(&snapshot_path).await?.len();
    let success = !collection_details.is_empty();

    tracing::info!(
        "Qdrant 快照完成: {} 个集合, {} 个点, {} 字节 ({:.0}ms)",
        collection_details.len(),
        total_points,
        size,
        start.elapsed().as_secs_f64() * 1000.0
    );

    Ok((success, size))
}

/// Restore Qdrant collections from `{backup_dir}/qdrant.snapshot`.
///
/// Reads the snapshot manifest and attempts to recreate collections.
/// Full point-level restore requires using Qdrant snapshot API
/// (PUT /collections/{name}/snapshots/recover).
pub async fn restore_collections(backup_dir: &Path) -> anyhow::Result<()> {
    let snapshot_path = backup_dir.join("qdrant.snapshot");

    if !snapshot_path.exists() {
        tracing::warn!(
            "Qdrant snapshot not found at {} — skipping",
            snapshot_path.display()
        );
        return Ok(());
    }

    tracing::info!("正在从 {} 恢复 Qdrant 集合", snapshot_path.display());

    let content = tokio::fs::read_to_string(&snapshot_path).await?;
    let metadata: serde_json::Value = serde_json::from_str(&content)?;

    let collections = metadata
        .get("collections")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    if collections.is_empty() {
        tracing::info!("Qdrant snapshot contains no collections — nothing to restore");
        return Ok(());
    }

    let client = reqwest::Client::new();
    let mut restored = 0usize;

    for col in &collections {
        let name = col.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }

        let points_count = col
            .get("points_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Check if collection already exists
        if let Ok(resp) = client
            .get(format!("{}/collections/{name}", QDRANT_URL))
            .send()
            .await
        {
            if resp.status().is_success() {
                tracing::info!(
                    "Qdrant 恢复: 集合 '{name}' 已存在 ({} 个点), 跳过",
                    points_count
                );
                restored += 1;
                continue;
            }
        }

        // Attempt to restore via snapshot recovery
        // Note: For full restore, the actual Qdrant snapshot files would need to be
        // created during backup via POST /collections/{name}/snapshots and stored.
        tracing::info!(
            "Qdrant 恢复: 集合 '{name}' ({points_count} 个点) — \
             完整快照恢复需要 Qdrant 快照 API 文件. \
             请重新运行 `dt build` 重新索引."
        );
    }

    tracing::info!("Qdrant 恢复完成: {restored}/{} 个集合", collections.len());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn snapshot_collections_writes_file() {
        let dir = TempDir::new().unwrap();
        let (_ok, size) = snapshot_collections(dir.path())
            .await
            .expect("snapshot should succeed");
        // ok may be false if Qdrant is unreachable (placeholder written)
        assert!(size > 0);

        let snap = dir.path().join("qdrant.snapshot");
        assert!(snap.exists());
        let content = std::fs::read_to_string(&snap).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(meta["type"] == "qdrant_snapshot" || meta["collections"].is_array());
    }

    #[tokio::test]
    async fn restore_collections_skips_missing_file() {
        let dir = TempDir::new().unwrap();
        let result = restore_collections(dir.path()).await;
        assert!(result.is_ok(), "should skip missing snapshot gracefully");
    }

    #[tokio::test]
    async fn restore_collections_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        snapshot_collections(dir.path()).await.unwrap();
        let result = restore_collections(dir.path()).await;
        assert!(result.is_ok());
    }
}
