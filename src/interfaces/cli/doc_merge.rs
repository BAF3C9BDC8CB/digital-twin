//! `dt merge-docs` 的 CLI 处理器 —— 文档主题归并。
//!
//! 扫描全库（或指定项目）文档，按实体重叠度识别同主题文档，
//! 建立 `SAME_TOPIC_AS` 边。

use std::sync::Arc;

use crate::application::sync::doc_merge::merge_documents_by_topic;
use crate::domain::traits::GraphRepository;

/// 处理 `dt merge-docs`——跨路径/跨项目识别同主题文档并建边。
pub async fn handle_doc_merge(
    projects: Vec<String>,
    threshold: Option<f64>,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    let Some(graph) = graph else {
        anyhow::bail!("图数据库不可用——merge-docs 需要连接 Memgraph");
    };
    let threshold =
        threshold.unwrap_or(crate::application::sync::doc_merge::TOPIC_SIMILARITY_THRESHOLD);
    if !(0.0..=1.0).contains(&threshold) {
        anyhow::bail!("--threshold 必须在 0.0~1.0 之间，收到 {threshold}");
    }

    let proj_desc = if projects.is_empty() {
        "全部项目".to_string()
    } else {
        format!("项目: {}", projects.join(", "))
    };
    println!("文档主题归并: {proj_desc}, 阈值={threshold}");
    println!("方法: 实体重叠系数 (|A∩B|/min(|A|,|B|), 归一化后跨项目对齐)");

    let report = merge_documents_by_topic(graph.as_ref(), &projects, threshold).await?;

    println!(
        "归并完成: 扫描 {} 篇文档, 比较 {} 对, 建立 {} 条 SAME_TOPIC_AS 边 (已存在跳过 {}), {}ms",
        report.documents_scanned,
        report.pairs_compared,
        report.topics_merged,
        report.edges_existing_skipped,
        report.elapsed_ms,
    );
    Ok(())
}
