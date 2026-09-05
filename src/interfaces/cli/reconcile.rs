//! `dt reconcile` 的 CLI 处理器——图 vs 向量存量对账（方案 §4.1）。
//!
//! 纯只读巡检：对每个 (project, 类型) 桶，比较 Memgraph 节点数与 Qdrant
//! 对应集合中该 project 的向量数，标出"有图无向量"（图多向量少）桶与
//! "有向量无图"孤儿。不写任何数据；修复动作由 S2（`--fix`）另行实现。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::domain::traits::{GraphRepository, VectorRepository};

/// 单行对账结果。
#[derive(Debug)]
struct Bucket {
    /// project（根别名，兼容期字段名仍叫 project）。
    project: String,
    /// 图节点顶层标签。
    label: String,
    /// Memgraph 节点数。
    graph_count: i64,
    /// Qdrant 对应集合中该 project 的向量数（scroll 过滤计数）。
    vector_count: i64,
    /// 该桶对齐状态。
    status: BucketStatus,
}

#[derive(Debug, PartialEq)]
enum BucketStatus {
    /// 两侧数量一致（|Δ|/图 < 1%）。
    Aligned,
    /// 图多向量少（含图有向量完全没有）。
    GraphOnly,
    /// 向量多（图少/无——可能向量集合含该 project 但图侧标签映射不全）。
    VectorOnly,
    /// 向量侧无法对账（集合缺失或 scroll 失败）。
    Skipped,
}

/// 图标签 → Qdrant 集合的映射（与 kg_bridge 的写入约定一致）。
fn collection_for_label(label: &str) -> &'static str {
    match label {
        "Method" => "code_methods",
        "Class" => "code_classes",
        "Document" => "doc_chunks",
        _ => "kg_nodes",
    }
}

/// 运行 `dt reconcile`。
///
/// 参数均为 Option：后端不可用时对账跳过对应侧并在报告里说明，
/// 遵循方案"任何一路失败不影响整体"的降级精神。
pub async fn run_reconcile(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    json: bool,
) -> anyhow::Result<()> {
    if !json {
        println!("=== dt reconcile: 图(Memgraph) vs 向量(Qdrant) 对账 ===\n");
    }

    // ---- 1. 图侧计数: 按 (project, 顶层标签) ----
    let mut graph_buckets: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut total_graph_nodes: i64 = 0;
    if let Some(g) = &graph {
        let q = "MATCH (n) \
                 WHERE n.project IS NOT NULL \
                 RETURN n.project AS project, labels(n)[0] AS label, count(*) AS cnt \
                 ORDER BY project, label";
        match g.read_query(q, Default::default()).await {
            Ok(v) => {
                // Memgraph 驱动返回 JSON 数组行；解析两种形状。
                let mut parsed = false;
                if let Some(rows) = v.as_array() {
                    parsed = true;
                    for row in rows {
                        let project = row
                            .pointer("/project")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let label = row
                            .pointer("/label")
                            .and_then(|x| x.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let cnt = row.pointer("/cnt").and_then(|x| x.as_i64()).unwrap_or(0);
                        *graph_buckets.entry((project, label)).or_insert(0) += cnt;
                        total_graph_nodes += cnt;
                    }
                }
                if !parsed {
                    if let Some(data) = v.pointer("/data").and_then(|x| x.as_array()) {
                        for row in data {
                            if let Some(arr) = row.as_array() {
                                if arr.len() >= 3 {
                                    let project = arr[0].as_str().unwrap_or("").to_string();
                                    let label = arr[1].as_str().unwrap_or("?").to_string();
                                    let cnt = arr[2].as_i64().unwrap_or(0);
                                    *graph_buckets.entry((project, label)).or_insert(0) += cnt;
                                    total_graph_nodes += cnt;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("  ⚠️ 图侧计数失败（Memgraph 查询不可用）: {e}");
            }
        }
    } else {
        println!("  ⚠️ 图数据库未连接——图侧计数跳过");
    }

    // ---- 2. 向量侧: collection 总量 ----
    let mut collection_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut vector_total: i64 = 0;
    if let Some(v) = &vector {
        match v.list_collections().await {
            Ok(cols) => {
                for c in &cols {
                    if let Ok(info) = v.collection_info(c).await {
                        collection_counts.insert(c.clone(), info.points_count as i64);
                        vector_total += info.points_count as i64;
                    }
                }
            }
            Err(e) => {
                println!("  ⚠️ 向量侧集合列表失败: {e}");
            }
        }
    } else {
        println!("  ⚠️ 向量库未连接——向量侧计数跳过");
    }

    if !json {
        println!(
            "[图侧] 共 {} 个节点（按 project+标签分组 {} 桶）",
            total_graph_nodes,
            graph_buckets.len()
        );
        println!(
            "[向量侧] 共 {} 个向量，集合: {:?}",
            vector_total,
            collection_counts.keys().collect::<Vec<_>>()
        );
        println!();
    }

    // ---- 3. 桶级对账（向量侧按 project 过滤 scroll 精确计数）----
    // 只在图侧出现的桶做精确核对；为控制 scroll 开销，每个桶至多取
    // MAX_SCROLL 个 payload——超出即视为"至少 MAX_SCROLL 条"，足以判断对齐。
    const MAX_SCROLL: usize = 200_000;
    let mut buckets: Vec<Bucket> = Vec::new();
    for ((project, label), gcnt) in &graph_buckets {
        let coll = collection_for_label(label);
        // 向量计数: 该集合内 payload.project == 本桶 project 的点数。
        let vcnt = if let Some(v) = &vector {
            let filter = serde_json::json!({
                "must": [{ "key": "project", "match": { "value": project } }]
            });
            match v.scroll_payloads(coll, Some(filter), MAX_SCROLL).await {
                Ok(payloads) => payloads.len() as i64,
                Err(_) => -1, // 集合缺失/失败
            }
        } else {
            -1
        };

        let g = *gcnt;
        let status = if vcnt < 0 {
            BucketStatus::Skipped
        } else if g == 0 {
            BucketStatus::Skipped
        } else if vcnt == 0 {
            BucketStatus::GraphOnly
        } else {
            let diff = (g as f64 - vcnt as f64).abs() / g as f64;
            if diff < 0.01 {
                BucketStatus::Aligned
            } else if vcnt < g {
                BucketStatus::GraphOnly
            } else {
                BucketStatus::VectorOnly
            }
        };
        buckets.push(Bucket {
            project: project.clone(),
            label: label.clone(),
            graph_count: g,
            vector_count: if vcnt < 0 { 0 } else { vcnt },
            status,
        });
    }

    // ---- 4. 输出 ----
    // 每 project 取图侧节点数前几大标签展示，避免 306 桶刷屏；--json 全量。
    if json {
        let out: Vec<serde_json::Value> = buckets
            .iter()
            .map(|b| {
                serde_json::json!({
                    "project": b.project,
                    "label": b.label,
                    "graph_count": b.graph_count,
                    "vector_count": b.vector_count,
                    "status": match b.status {
                        BucketStatus::Aligned => "aligned",
                        BucketStatus::GraphOnly => "graph_only",
                        BucketStatus::VectorOnly => "vector_only",
                        BucketStatus::Skipped => "skipped",
                    },
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "graph_total": total_graph_nodes,
                "vector_total": vector_total,
                "collections": collection_counts,
                "buckets": out,
            }))?
        );
        return Ok(());
    }

    // 文本模式：按 project 聚合并只展示该 project 图节点数最大的桶。
    // 先排序：project 相同则图节点数降序。
    let mut ordered: Vec<&Bucket> = buckets.iter().collect();
    ordered.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then(b.graph_count.cmp(&a.graph_count))
    });

    println!(
        "{:<20} {:<12} {:>10} {:>10}  {}",
        "project", "类型", "图节点", "向量(同project)", "状态"
    );
    println!(
        "{:-<20} {:-<12} {:->10} {:->10}  {:-<8}",
        "", "", "", "", ""
    );

    let mut prev_project: Option<String> = None;
    let mut shown_in_project = 0usize;
    let mut total_shown = 0usize;
    for b in &ordered {
        let p_is_new = prev_project.as_deref() != Some(b.project.as_str());
        if p_is_new {
            shown_in_project = 0;
            prev_project = Some(b.project.clone());
        }
        // 每 project 只展示图节点数 top 4，避免小标签刷屏。
        if shown_in_project >= 4 {
            continue;
        }
        shown_in_project += 1;
        total_shown += 1;
        let status_s = match b.status {
            BucketStatus::Aligned => "✅ 对齐",
            BucketStatus::GraphOnly => "⚠️ 有图无向量",
            BucketStatus::VectorOnly => "⚠️ 向量>图",
            BucketStatus::Skipped => "⬜ 跳过",
        };
        println!(
            "{:<20} {:<12} {:>10} {:>10}  {}",
            if b.project.is_empty() {
                "(无project)"
            } else {
                &b.project
            },
            b.label,
            b.graph_count,
            b.vector_count,
            status_s,
        );
        if total_shown >= 120 {
            println!("  ... (仅显示部分桶，用 --json 看全量 {})", buckets.len());
            break;
        }
    }

    // ---- 5. 汇总判定 ----
    println!("\n[判定]");
    let graph_only: Vec<&Bucket> = buckets
        .iter()
        .filter(|b| b.status == BucketStatus::GraphOnly)
        .collect();
    let aligned: Vec<&Bucket> = buckets
        .iter()
        .filter(|b| b.status == BucketStatus::Aligned)
        .collect();
    let skipped: Vec<&Bucket> = buckets
        .iter()
        .filter(|b| b.status == BucketStatus::Skipped)
        .collect();

    if graph_only.is_empty() {
        println!("  ✅ 无\"有图无向量\"桶——图/向量按 (project,类型) 对齐。");
    } else {
        println!(
            "  ⚠️ {} / {} 个桶\"有图无向量\"（累计缺 {} 个向量）：",
            graph_only.len(),
            buckets.len(),
            graph_only
                .iter()
                .map(|b| b.graph_count - b.vector_count)
                .sum::<i64>()
        );
        // 列出缺口最大的 10 个
        let mut top: Vec<&&Bucket> = graph_only
            .iter()
            .filter(|b| b.graph_count - b.vector_count > 0)
            .collect();
        top.sort_by(|a, b| (b.graph_count - b.vector_count).cmp(&(a.graph_count - a.vector_count)));
        for b in top.into_iter().take(10) {
            println!(
                "     - {:<16} {:<10} 图 {} / 向量 {}（缺 {}）",
                b.project,
                b.label,
                b.graph_count,
                b.vector_count,
                b.graph_count - b.vector_count
            );
        }
    }
    println!(
        "  ℹ️  已对齐 {} 桶，跳过 {} 桶（向量集合缺失/失败）。",
        aligned.len(),
        skipped.len()
    );
    println!(
        "     修复动作见方案 S2（`.hermes` 补向量、Entity 补 kg_nodes、孤儿清理）——尚未实现。"
    );

    Ok(())
}
