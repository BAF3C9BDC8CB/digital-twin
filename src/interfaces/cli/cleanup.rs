//! 模式校验与数据清理的 CLI 实现。
//!
//! 提供：
//! - `dt schema init`    — 幂等的模式初始化（约束 + 索引）
//! - `dt clean --confirm` — 清空 Memgraph、Qdrant 与 SQLite 中的所有数据
//! - `dt cleanup --targets reasoning` — 清理陈旧的 Reasoning 节点（Observation/Analysis/Decision）
//! - `dt cleanup --targets memory`    — 归档超出保留期的 Memory 事件
//! - `dt cleanup --targets snapshots` — 删除孤立的 SQLite 快照行
//! - `dt cleanup --targets all`       — 运行所有清理目标
//! - `dt health`         — 检查所有后端服务的健康状态

use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::HealthStatus;
use crate::infrastructure::memgraph::schema::{clean_all, init_schema};
use crate::infrastructure::memgraph::{CleanReport, SchemaInitReport};
use std::time::Instant;

// ---------------------------------------------------------------------------
// dt schema init
// ---------------------------------------------------------------------------

/// 运行 `dt schema init`——通过 Memgraph 创建所有约束和索引。
///
/// 当 `graph` 为 `None` 时，回退到 `NoopGraphRepo`（no-op，供测试使用）。
pub async fn run_schema_init(graph: Option<&dyn GraphRepository>) -> anyhow::Result<()> {
    println!("正在初始化 V2 模式...");
    let report: SchemaInitReport = if let Some(g) = graph {
        init_schema(g).await?
    } else {
        let noop = crate::infrastructure::memgraph::NoopGraphRepo;
        init_schema(&noop).await?
    };

    println!();
    println!("模式初始化完成:");
    println!("  创建的约束        : {}", report.constraints_created);
    println!("  创建的索引        : {}", report.indexes_created);
    println!("  耗时              : {} ms", report.elapsed_ms);

    Ok(())
}

// ---------------------------------------------------------------------------
// dt clean
// ---------------------------------------------------------------------------

/// 运行 `dt clean --confirm`——清空所有后端的所有数据。
///
/// 未加 `--confirm` 时，打印警告并直接退出，不做任何修改。
/// 加上 `--confirm` 后，将清理 Memgraph、Qdrant 与 SQLite。
pub async fn run_clean(
    confirm: bool,
    graph: Option<&dyn GraphRepository>,
    vector: Option<&dyn VectorRepository>,
    snapshot: Option<&dyn SnapshotRepository>,
) -> anyhow::Result<()> {
    if !confirm {
        eprintln!("警告：`dt clean` 将删除以下所有数据：");
        eprintln!("  - Memgraph：所有节点与关系");
        eprintln!("  - Qdrant：所有向量集合");
        eprintln!("  - SQLite：所有文件快照");
        eprintln!();
        eprintln!("此操作不可撤销。");
        eprintln!("请使用 `--confirm` 参数继续。");
        return Ok(());
    }
    let total_start = Instant::now();

    println!("正在清理所有数据...");
    println!();

    // --- Memgraph ---
    let memgraph_report: CleanReport = if let Some(g) = graph {
        clean_all(g).await?
    } else {
        let noop = crate::infrastructure::memgraph::NoopGraphRepo;
        clean_all(&noop).await?
    };

    println!("Memgraph:");
    println!("  删除的节点          : {}", memgraph_report.nodes_deleted);
    println!(
        "  删除的关系          : {}",
        memgraph_report.relationships_deleted
    );
    println!("  耗时                : {} ms", memgraph_report.elapsed_ms);

    // --- Qdrant ---
    let mut removed_collections: Vec<String> = Vec::new();
    let mut qdrant_err: Option<String> = None;
    if let Some(v) = vector {
        match v.list_collections().await {
            Ok(collections) => {
                for name in &collections {
                    match v.delete_collection(name).await {
                        Ok(()) => removed_collections.push(name.clone()),
                        Err(e) => {
                            qdrant_err = Some(format!("删除集合 {name} 失败: {e}"));
                            break;
                        }
                    }
                }
            }
            Err(e) => qdrant_err = Some(format!("列出集合失败: {e}")),
        }
    } else {
        qdrant_err = Some("vector 后端未连接 (noop)".to_string());
    }

    println!();
    println!("Qdrant:");
    println!("  移除的集合          : {}", removed_collections.len());
    for name in &removed_collections {
        println!("    - {name}");
    }
    if let Some(e) = &qdrant_err {
        println!("  ⚠️ {e}");
    }

    // --- SQLite ---
    let snapshots_cleared = if let Some(s) = snapshot {
        match s.clear_all().await {
            Ok(n) => {
                println!("  已删除快照/进度行    : {n}");
                true
            }
            Err(e) => {
                println!("  ⚠️ SQLite 清空失败: {e}");
                false
            }
        }
    } else {
        println!("  ⚠️ snapshot 后端未连接 (noop)");
        false
    };
    println!();
    println!("SQLite:");
    println!(
        "  已清空的快照        : {}",
        if snapshots_cleared { "yes" } else { "no" }
    );

    // --- 汇总报告 ---
    let total_elapsed = total_start.elapsed().as_millis() as u64;

    let combined = CleanReport {
        nodes_deleted: memgraph_report.nodes_deleted,
        relationships_deleted: memgraph_report.relationships_deleted,
        qdrant_collections_removed: removed_collections.len(),
        snapshots_cleared,
        reasoning_stale_deleted: 0,
        memory_archived: 0,
        snapshots_orphaned: 0,
        elapsed_ms: total_elapsed,
    };

    println!();
    println!("清理完成:");
    println!("  删除的节点               : {}", combined.nodes_deleted);
    println!(
        "  删除的关系               : {}",
        combined.relationships_deleted
    );
    println!(
        "  移除的 Qdrant 集合       : {}",
        combined.qdrant_collections_removed
    );
    println!(
        "  已清空的快照             : {}",
        if combined.snapshots_cleared {
            "yes"
        } else {
            "no"
        }
    );
    println!("  总耗时                   : {} ms", combined.elapsed_ms);

    Ok(())
}

// ---------------------------------------------------------------------------
// check_health! 宏——对任何实现 health_check() 的类型做统一健康检查
// ---------------------------------------------------------------------------

/// 检查任何暴露 `health_check()` 方法的仓库/服务的健康状态。
macro_rules! check_health {
    ($name:expr, $repo:expr) => {{
        let start = std::time::Instant::now();
        let status = $repo.health_check().await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match status {
            Ok(HealthStatus::Healthy) => {
                (true, format!("✅ {:<8}: 健康 ({} ms)", $name, latency_ms))
            }
            Ok(HealthStatus::Degraded(reason)) => (
                false,
                format!("⚠️  {:<8}: 降级 — {} ({} ms)", $name, reason, latency_ms),
            ),
            Ok(HealthStatus::Unhealthy(reason)) => (
                false,
                format!("❌ {:<8}: 不健康 — {} ({} ms)", $name, reason, latency_ms),
            ),
            Err(e) => (
                false,
                format!("❌ {:<8}: 错误 — {} ({} ms)", $name, e, latency_ms),
            ),
        }
    }};
}

// ---------------------------------------------------------------------------
// dt health
// ---------------------------------------------------------------------------

/// 运行 `dt health`——检查所有后端服务的健康状态。
///
/// 探测 Memgraph、Qdrant、SQLite 的
/// 可用性与延迟。
///
/// 当某服务为 `None` 时，报告为"未配置后端"。
pub async fn run_health(
    graph: Option<&dyn GraphRepository>,
    vector: Option<&dyn VectorRepository>,
    snapshot: Option<&dyn SnapshotRepository>,
    embed: Option<&dyn EmbedService>,
) -> anyhow::Result<()> {
    println!("正在检查后端健康状态...");
    println!();

    let mut all_healthy = true;

    // --- Memgraph ---
    let (healthy, detail) = if let Some(g) = graph {
        check_health!("Memgraph", g)
    } else {
        (false, "  ❌ Memgraph : 未配置后端".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- Qdrant ---
    let (healthy, detail) = if let Some(v) = vector {
        check_health!("Qdrant", v)
    } else {
        (false, "  ❌ Qdrant   : 未配置后端".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- SQLite ---
    let (healthy, detail) = if let Some(s) = snapshot {
        check_health!("SQLite", s)
    } else {
        (false, "  ❌ SQLite   : 未配置后端".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- SiliconFlow ---
    let (healthy, detail) = if let Some(e) = embed {
        check_health!("SiliconFlow", e)
    } else {
        (false, "  ❌ SiliconFlow : 未配置后端".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    println!();
    if all_healthy {
        println!("所有后端均健康。");
    } else {
        println!("有一个或多个后端降级或不健康。");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DtError;

    #[tokio::test]
    async fn run_schema_init_succeeds_with_noop() {
        let result = run_schema_init(None).await;
        assert!(result.is_ok(), "使用 noop 仓库时 schema init 应成功");
    }

    #[tokio::test]
    async fn run_clean_without_confirm_prints_warning() {
        // 不应 panic 或报错——它只是警告并退出。
        let result = run_clean(false, None, None, None).await;
        assert!(result.is_ok(), "不带 --confirm 的 clean 应成功（仅警告）");
    }

    #[tokio::test]
    async fn run_clean_with_confirm_succeeds() {
        let result = run_clean(true, None, None, None).await;
        assert!(result.is_ok(), "使用 noop 仓库时 clean --confirm 应成功");
    }

    #[tokio::test]
    async fn run_health_succeeds() {
        let result = run_health(None, None, None, None).await;
        assert!(result.is_ok(), "使用 noop 仓库时健康检查应成功");
    }
}
