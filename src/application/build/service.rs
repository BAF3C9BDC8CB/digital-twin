//! Build 服务 — 构建流水线的编排层。
//!
//! 实现 `BuildService` trait，提供：
//! - `build(project, path)` — 全量/增量构建
//! - `update_file(project, path)` — 单文件更新
//! - `delete_project(project)` — 删除项目的所有数据

use crate::domain::error::DtError;
use crate::domain::traits::{
    BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use crate::domain::types::{BatchConfig, BuildReport, ScanConfig};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use super::pipeline::PipelineTemplate;
use super::strategy::full_rebuild::FullRebuildStrategy;
use super::strategy::incremental::IncrementalStrategy;
use super::strategy::BuildStrategy;
use crate::application::pipeline::infer_client::ChatClient;
use crate::infrastructure::parser::ParserRegistry;

/// 默认的构建服务实现。
///
/// 持有所有存储后端、解析器注册表与 embed 服务的引用。
/// 通过选择适当的策略并执行流水线来编排构建。
pub struct BuildServiceImpl {
    parser_registry: Arc<ParserRegistry>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    llm_client: Option<Arc<dyn ChatClient>>,
    llm_model: String,
    target_file: Option<std::path::PathBuf>,
    scan_config: ScanConfig,
    full: bool,
    batch_config: BatchConfig,
    skip_embed: bool,
}

impl BuildServiceImpl {
    /// 创建新的构建服务。
    pub fn new(
        parser_registry: Arc<ParserRegistry>,
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        snapshot: Option<Arc<dyn SnapshotRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
        llm_client: Option<Arc<dyn ChatClient>>,
        llm_model: String,
        target_file: Option<std::path::PathBuf>,
        full: bool,
        batch_config: BatchConfig,
        skip_embed: bool,
    ) -> Self {
        Self {
            parser_registry,
            graph,
            vector,
            snapshot,
            embed,
            llm_client,
            llm_model,
            target_file,
            scan_config: ScanConfig::default(),
            full,
            batch_config,
            skip_embed,
        }
    }

    /// 设置自定义扫描配置。
    pub fn with_scan_config(mut self, config: ScanConfig) -> Self {
        self.scan_config = config;
        self
    }

    /// 选择适当的构建策略。
    fn select_strategy(&self) -> Box<dyn BuildStrategy> {
        if self.full {
            Box::new(FullRebuildStrategy)
        } else if self.snapshot.is_some() {
            Box::new(IncrementalStrategy)
        } else {
            Box::new(FullRebuildStrategy)
        }
    }
}

#[async_trait]
impl BuildService for BuildServiceImpl {
    async fn build(&self, project: &str, root: &Path) -> Result<BuildReport, DtError> {
        let pipeline = PipelineTemplate::new(
            self.parser_registry.clone(),
            self.batch_config.clone(),
            self.llm_client.clone(),
            self.llm_model.clone(),
            self.target_file.clone(),
        )
        .with_skip_embed(self.skip_embed);
        let strategy = self.select_strategy();

        let graph_ref: Option<&dyn GraphRepository> = self.graph.as_ref().map(|r| r.as_ref());

        pipeline
            .execute(
                project,
                root,
                strategy.as_ref(),
                &self.scan_config,
                self.snapshot.clone(),
                graph_ref,
                self.embed.clone(),
                self.vector.clone(),
            )
            .await
    }

    async fn update_file(&self, project: &str, path: &Path) -> Result<(), DtError> {
        // 单文件更新：解析文件并 upsert 到图谱。
        let source = std::fs::read_to_string(path).map_err(DtError::Io)?;

        let result = self.parser_registry.parse_file(&source, path, project)?;

        if let Some(graph) = &self.graph {
            let extraction = super::pipeline::ExtractionResult {
                methods: result.methods,
                classes: result.classes,
                modules: Vec::new(),
                snapshots: Vec::new(),
            };
            // 复用 write_graph 逻辑 - 但 extraction 是私有的
            // 目前单文件更新暂不写入图谱。
            // 正式实现中应使用流水线的写入方法。
            let _ = graph;
            let _ = extraction;
        }

        Ok(())
    }

    async fn delete_project(&self, project: &str) -> Result<(), DtError> {
        if let Some(graph) = &self.graph {
            use std::collections::HashMap;
            let mut params = HashMap::new();
            params.insert(
                "project".to_string(),
                serde_json::Value::String(project.to_string()),
            );

            // 先删除所有出向关系
            let _ = graph
                .write_query(
                    "MATCH (m:Method {project: $project})-[r:CALLS]->() DELETE r",
                    params.clone(),
                )
                .await;
            let _ = graph
                .write_query(
                    "MATCH (c:Class {project: $project})-[r:CONTAINS]->() DELETE r",
                    params.clone(),
                )
                .await;
            // 删除 Method/Class 到 Project 的 BELONGS_TO 关系
            let _ = graph
                .write_query(
                    "MATCH (n {project: $project})-[r:BELONGS_TO]->(:Project) DELETE r",
                    params.clone(),
                )
                .await;
            // 删除实体
            let _ = graph
                .write_query(
                    "MATCH (n) WHERE n.project = $project AND (n:Method OR n:Class OR n:Module) DETACH DELETE n",
                    params.clone(),
                )
                .await;
            // 删除 Project 节点
            let _ = graph
                .write_query("MATCH (p:Project {name: $project}) DETACH DELETE p", params)
                .await;
        }

        if let Some(snapshot) = &self.snapshot {
            let _ = snapshot.delete_project(project).await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_creates() {
        let registry = Arc::new(ParserRegistry::new());
        let service = BuildServiceImpl::new(
            registry,
            None,
            None,
            None,
            None,
            None,
            String::new(),
            None,
            false,
            BatchConfig::default(),
            false,
        );
        // 仅验证它可编译且可构造
        assert_eq!(service.scan_config.max_file_size, 524_288);
    }
}
