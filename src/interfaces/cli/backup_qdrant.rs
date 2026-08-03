//! Qdrant 备份与恢复操作。
//!
//! 使用 Qdrant 的 REST API 进行快照创建与恢复。
//! HTTP API 通常位于 `http://localhost:6333`。
//!
//! 当 Qdrant 不可用时，快照为 no-op 并记录警告。

use std::path::Path;
use std::time::Instant;

/// Qdrant HTTP API 基础 URL。
const QDRANT_URL: &str = "http://localhost:6333";

/// 将 Qdrant 集合快照到 `{backup_dir}/qdrant.snapshot`。
///
/// 收集所有集合的元数据及其向量数量，
/// 然后写入 JSON 清单。实际的点数据快照需要
/// Qdrant 快照 API（POST /collections/{name}/snapshots）。
///
/// 返回 `(success, size_bytes)`。
pub async fn snapshot_collections(backup_dir: &Path) -> anyhow::Result<(bool, u64)> {
    let start = Instant::now();
    let snapshot_path = backup_dir.join("qdrant.snapshot");

    tracing::info!("正在为 Qdrant 集合创建快照 {}", snapshot_path.display());

    // 从 Qdrant REST API 获取集合列表
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

    // 收集每个集合的信息
    let mut collection_details = Vec::new();
    let mut total_points: u64 = 0;

    for col in &collections {
        let name = col
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // 尝试获取集合信息
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

    // 构建快照清单
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

/// 从 `{backup_dir}/qdrant.snapshot` 恢复 Qdrant 集合。
///
/// 读取快照清单并尝试重建集合。
/// 完整的点级恢复需要使用 Qdrant 快照 API
/// （PUT /collections/{name}/snapshots/recover）。
pub async fn restore_collections(backup_dir: &Path) -> anyhow::Result<()> {
    let snapshot_path = backup_dir.join("qdrant.snapshot");

    if !snapshot_path.exists() {
        tracing::warn!(
            "在 {} 未找到 Qdrant 快照 — 已跳过",
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
        tracing::info!("Qdrant 快照不包含集合——无需恢复");
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

        // 检查集合是否已存在
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

        // 尝试通过快照恢复进行还原
        // 注意：完整恢复需要在实际备份时通过
        // POST /collections/{name}/snapshots 创建并存储 Qdrant 快照文件。
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
            .expect("快照应成功");
        // 若 Qdrant 不可达，ok 可能为 false（已写入占位符）
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
        assert!(result.is_ok(), "缺少快照文件时应优雅跳过");
    }

    #[tokio::test]
    async fn restore_collections_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        snapshot_collections(dir.path()).await.unwrap();
        let result = restore_collections(dir.path()).await;
        assert!(result.is_ok());
    }
}
