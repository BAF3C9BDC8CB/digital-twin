//! 全量重建策略 — 清空项目的所有数据并从零重建。

use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use crate::shared::collections::{CODE_CLASSES, CODE_METHODS, DOC_CHUNKS, KG_NODES};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;

use super::BuildStrategy;

/// 全量重建：处理所有文件，删除该项目之前的全部数据。
pub struct FullRebuildStrategy;

#[async_trait]
impl BuildStrategy for FullRebuildStrategy {
    fn name(&self) -> &'static str {
        "full"
    }

    fn force_rebuild(&self) -> bool {
        true
    }

    async fn select_files(
        &self,
        _root: &Path,
        all_files: &[std::path::PathBuf],
        _snapshot_repo: Option<&dyn SnapshotRepository>,
        _project: &str,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), DtError> {
        // 全量重建中，所有文件都是处理候选
        Ok((all_files.to_vec(), Vec::new()))
    }

    async fn prepare(
        &self,
        graph: Option<&dyn GraphRepository>,
        vector: Option<&dyn VectorRepository>,
        project: &str,
    ) -> Result<(), DtError> {
        // §7.5：先清空该项目的向量 — 提取实体点（kg_nodes）与
        // 证据块（doc_chunks）通过 payload 的 `project` 键按项目隔离。
        // 失败仅记录日志，不致命：后续的 upsert 通过确定性点 ID 幂等。
        if let Some(vector) = vector {
            let project_filter = serde_json::json!({
                "must": [{"key": "project", "match": {"value": project}}],
            });
            // P3-10：全量重建清理包含 code_classes——否则已删除类的旧向量点残留可被召回。
            // 2026-08-31 补：code_methods 也必须清——否则旧方法向量残留导致
            // Memgraph 方法数 ≠ Qdrant 向量数的索引漂移（实证：101254 vs 107302）。
            for collection in [KG_NODES, DOC_CHUNKS, CODE_CLASSES, CODE_METHODS] {
                if let Err(e) = vector
                    .delete_by_filter(collection, project_filter.clone())
                    .await
                {
                    tracing::warn!("[full_rebuild] 清空 {collection} 中 {project} 的向量失败: {e}");
                }
            }
        }

        // 删除该项目的所有 Method、Class 与 Module 节点
        if let Some(graph) = graph {
            let mut params = HashMap::new();
            params.insert(
                "project".to_string(),
                serde_json::Value::String(project.to_string()),
            );

            // 先删除 CALLS 关系
            let _ = graph
                .write_query(
                    "MATCH (m:Method {project: $project})-[r:CALLS]->() DELETE r",
                    params.clone(),
                )
                .await;

            // 删除 CONTAINS 关系
            let _ = graph
                .write_query(
                    "MATCH (c:Class {project: $project})-[r:CONTAINS]->() DELETE r",
                    params.clone(),
                )
                .await;

            // 删除方法
            let _ = graph
                .write_query(
                    "MATCH (m:Method {project: $project}) DETACH DELETE m",
                    params.clone(),
                )
                .await;

            // 删除类
            let _ = graph
                .write_query(
                    "MATCH (c:Class {project: $project}) DETACH DELETE c",
                    params.clone(),
                )
                .await;

            // 删除模块
            let _ = graph
                .write_query(
                    "MATCH (m:Module {project: $project}) DETACH DELETE m",
                    params.clone(),
                )
                .await;

            // 删除 Artifact（含 PART_OF 边由 DETACH 顺带清理）。
            // 注意：同一制品可能被多项目引用（跨项目 DEPENDS_ON），
            // 这里只删「本项目声明」的制品——切片 B 引入外部制品占位后
            // 需改为：删除本项目的 Artifact + 仅清理 project 指向本项目的。
            let _ = graph
                .write_query(
                    "MATCH (a:Artifact {project: $project}) DETACH DELETE a",
                    params,
                )
                .await;
        }

        Ok(())
    }

    async fn update_snapshots(
        &self,
        snapshot_repo: &dyn SnapshotRepository,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        // 全量重建：先清空所有旧快照，再插入新快照
        let _ = snapshot_repo.delete_project(project).await;
        snapshot_repo.save_snapshots(project, snapshots).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_name() {
        let s = FullRebuildStrategy;
        assert_eq!(s.name(), "full");
    }
}
