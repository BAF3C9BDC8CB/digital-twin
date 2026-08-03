//! 测试数据清理——从 Memgraph 删除所有 `test-` 前缀节点，从 Qdrant
//! 删除所有 `test-` 前缀集合。
//!
//! 这确保测试运行器是自包含的，默认不留下任何痕迹（除非传入 `--keep`）。

use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use std::collections::HashMap;
use std::sync::Arc;

/// 删除所有测试数据：project 以 "test-" 开头的节点，以及标签包含
/// `test-` 前缀的节点。同时删除 test-* Qdrant 集合与 SQLite 中的
/// test-pipeline 快照。
pub async fn cleanup_test_data(
    graph: &Arc<dyn GraphRepository>,
    vector: &Arc<dyn VectorRepository>,
    snapshot: Option<&Arc<dyn SnapshotRepository>>,
) -> Result<usize, String> {
    let mut total_cleaned = 0usize;

    // 按 project 属性删除（新方案）
    let q1 =
        "MATCH (n) WHERE n.project = 'test-pipeline' DETACH DELETE n RETURN count(*) AS deleted";
    match graph.write_query(q1, HashMap::new()).await {
        Ok(result) => {
            if let Some(arr) = result.as_array().and_then(|a| a.first()) {
                if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                    total_cleaned += c as usize;
                }
            }
        }
        Err(e) => tracing::warn!("cleanup: 删除 test-pipeline 节点失败: {e}"),
    }

    // 按标签前缀删除（旧方案，用于任何残留数据）
    let q2 = "MATCH (n) WHERE any(label IN labels(n) WHERE label STARTS WITH 'test-') DETACH DELETE n RETURN count(*) AS deleted";
    match graph.write_query(q2, HashMap::new()).await {
        Ok(result) => {
            if let Some(arr) = result.as_array().and_then(|a| a.first()) {
                if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                    total_cleaned += c as usize;
                }
            }
        }
        Err(e) => tracing::warn!("cleanup: 删除 test- 标签节点失败: {e}"),
    }

    // 清除 SQLite 快照，使全量重建重新处理所有文件（包括文档）。
    // ②c 修复：失败要记录日志而非吞掉——此处的静默失败会留下过期的增量
    // 进度，导致下一次 `dt build --test` 在 KG 为空时跳过所有文件
    //（2026-08-01 过期进度事件）。
    match snapshot {
        Some(snapshot) => {
            if let Err(e) = snapshot.delete_project("test-pipeline").await {
                tracing::warn!("cleanup: delete_project(file_snapshots) 失败: {e}");
            }
            if let Err(e) = snapshot.clear_llm_progress("test-pipeline").await {
                tracing::warn!("cleanup: clear_llm_progress 失败: {e}");
            }
            if let Err(e) = snapshot.clear_step_progress("test-pipeline").await {
                tracing::warn!("cleanup: clear_step_progress 失败: {e}");
            }
        }
        None => {
            tracing::warn!(
                "cleanup: 无 SQLite 快照存储——增量进度未清除; \
                 下次 `dt build --test` 可能在空 KG 上跳过所有文件"
            );
        }
    }

    tracing::info!(deleted = total_cleaned, "已清理测试数据（graph）");

    // 删除 test- Qdrant 集合
    match vector.list_collections().await {
        Ok(collections) => {
            let test_cols: Vec<String> = collections
                .into_iter()
                .filter(|name| name.starts_with("test-"))
                .collect();
            for name in &test_cols {
                if let Err(e) = vector.delete_collection(name).await {
                    tracing::warn!("cleanup: 删除 Qdrant 集合 {name} 失败: {e}");
                }
            }
            if !test_cols.is_empty() {
                tracing::info!(
                    count = test_cols.len(),
                    "已清理测试 Qdrant 集合"
                );
            }
        }
        Err(e) => tracing::warn!("cleanup: 列出 Qdrant 集合失败: {e}"),
    }

    Ok(total_cleaned)
}
