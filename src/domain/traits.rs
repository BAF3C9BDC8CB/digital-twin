//! 数字孪生系统的核心 trait 抽象。
//!
//! 定义构成各层之间契约的仓库、服务与插件 trait。

use crate::domain::error::DtError;
use crate::domain::types::{FileSnapshot, HealthStatus, ParseResult};
use async_trait::async_trait;
use std::path::Path;

/// 图数据库（Bolt 驱动）的仓库抽象。
#[async_trait]
pub trait GraphRepository: Send + Sync + 'static {
    /// 执行只读 Cypher 查询。
    async fn read_query(
        &self,
        query: &str,
        params: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, crate::domain::error::DtError>;

    /// 执行写 Cypher 查询。
    async fn write_query(
        &self,
        query: &str,
        params: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, crate::domain::error::DtError>;

    /// 检查连接健康状态。
    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError>;
}

/// Qdrant 向量数据库（gRPC 驱动）的仓库抽象。
#[async_trait]
pub trait VectorRepository: Send + Sync + 'static {
    /// 确保集合存在，必要时自动创建。
    async fn ensure_collection(
        &self,
        collection: &str,
        vector_dim: u32,
    ) -> Result<(), crate::domain::error::DtError>;

    /// 根据 embedding 向量搜索最近邻。
    async fn search(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<serde_json::Value>, crate::domain::error::DtError>;

    /// 带 payload 过滤器搜索最近邻（R7）。
    ///
    /// `filter` 使用 Qdrant 过滤器 JSON 结构
    /// (`{"must": [{"key": ..., "match": {"value": ...}}], "should": [...],
    /// "must_not": [...]}`)。
    ///
    /// 默认实现会调用 [`VectorRepository::search`]，然后按返回结果的 `payload`
    /// 过滤——结果正确但较慢。支持原生过滤器的后端（例如 Qdrant）应覆盖此
    /// 方法，改用服务端过滤查询。为保持向后兼容，现有
    /// [`VectorRepository::search`] 签名保持不变。
    async fn search_with_filter(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, crate::domain::error::DtError> {
        let hits = self.search(collection, vector, limit).await?;
        Ok(hits
            .into_iter()
            .filter(|hit| payload_matches_filter(hit.get("payload"), &filter))
            .collect())
    }

    /// 向集合中写入（upsert）数据点。
    async fn upsert(
        &self,
        collection: &str,
        points: Vec<serde_json::Value>,
    ) -> Result<(), crate::domain::error::DtError>;

    /// 删除满足过滤条件的数据点。
    async fn delete_by_filter(
        &self,
        collection: &str,
        filter: serde_json::Value,
    ) -> Result<(), crate::domain::error::DtError>;

    /// 列出所有集合名称。
    async fn list_collections(&self) -> Result<Vec<String>, crate::domain::error::DtError>;

    /// 获取指定集合的详细信息。
    async fn collection_info(
        &self,
        name: &str,
    ) -> Result<crate::domain::types::CollectionInfo, crate::domain::error::DtError>;

    /// 删除集合及其全部数据点。
    async fn delete_collection(&self, name: &str) -> Result<(), crate::domain::error::DtError>;

    /// 检查连接健康状态。
    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError>;
}

/// 对搜索结果命中的 payload 应用 Qdrant 风格过滤器 JSON 进行后置过滤。
///
/// 支持 `must` / `should` / `must_not` 子句数组，每个子句包含
/// `{"key": <field>, "match": {"value": <scalar>}}` 条件。缺失的 payload
/// 永远不会匹配 `must` 条件。该函数支撑默认的
/// [`VectorRepository::search_with_filter`] 实现。
fn payload_matches_filter(payload: Option<&serde_json::Value>, filter: &serde_json::Value) -> bool {
    let clause_matches = |clause: &serde_json::Value| -> bool {
        let key = clause.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let expected = clause.get("match").and_then(|m| m.get("value"));
        match expected {
            Some(expected) => payload.and_then(|p| p.get(key)) == Some(expected),
            // 不支持的条件结构——不排除该命中结果。
            None => true,
        }
    };

    let must_ok = filter
        .get("must")
        .and_then(|c| c.as_array())
        .map(|conds| conds.iter().all(clause_matches))
        .unwrap_or(true);
    let must_not_ok = filter
        .get("must_not")
        .and_then(|c| c.as_array())
        .map(|conds| !conds.iter().any(clause_matches))
        .unwrap_or(true);
    let should_ok = match filter.get("should").and_then(|c| c.as_array()) {
        // 仅当不存在 `must` 子句时 `should` 才起约束作用（Qdrant 语义）；
        // 存在 `must` 时它只是加权（boost），而非过滤。
        Some(conds) if !conds.is_empty() && filter.get("must").is_none() => {
            conds.iter().any(clause_matches)
        }
        _ => true,
    };

    must_ok && must_not_ok && should_ok
}

/// SQLite 文件快照（变更检测）的仓库抽象。
#[async_trait]
pub trait SnapshotRepository: Send + Sync + 'static {
    /// 获取指定文件最后已知的快照。
    async fn get_snapshot(
        &self,
        project: &str,
        path: &str,
    ) -> Result<Option<FileSnapshot>, DtError>;

    /// 保存（upsert）一个或多个文件快照。
    async fn save_snapshots(
        &self,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError>;

    /// 删除某个项目的全部快照。
    async fn delete_project(&self, project: &str) -> Result<u64, DtError>;

    /// 列出某个项目的全部快照。
    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError>;

    /// 将文件标记为已完成 LLM 分析，并记录当前文件哈希。
    async fn mark_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<(), DtError>;

    /// 检查文件是否已使用相同内容完成过 LLM 分析。
    /// 仅当此前已分析过 **且** 文件哈希一致时才返回 `true`。
    async fn is_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<bool, DtError>;

    /// 清除某个项目的全部 LLM 分析进度（用于全量重建）。
    async fn clear_llm_progress(&self, project: &str) -> Result<(), DtError>;

    /// 以文件内容哈希为键，将文件的某个流水线步骤标记为已完成。
    /// 步骤：`"tree_sitter"`、`"chunk"`、`"hanlp"`、`"llm"`、`"embed"`、`"store"`。
    ///
    /// 默认为空操作：未实现步骤进度跟踪的仓库只会永不跳过任何步骤
    /// （安全回退——全量重新处理）。
    async fn mark_step_done(
        &self,
        _project: &str,
        _file_path: &str,
        _step: &str,
        _file_hash: &str,
    ) -> Result<(), DtError> {
        Ok(())
    }

    /// 检查文件是否已使用相同内容哈希完成某个流水线步骤。
    /// 仅当进度表中存在完全相同的步骤+文件+哈希组合时才返回 `true`。
    ///
    /// 默认为 `false`：未跟踪时，任何步骤都不会被视为已完成。
    async fn is_step_done(
        &self,
        _project: &str,
        _file_path: &str,
        _step: &str,
        _file_hash: &str,
    ) -> Result<bool, DtError> {
        Ok(false)
    }

    /// 清除某个项目的全部流水线步骤进度（用于全量重建）。
    ///
    /// 默认为空操作（没有跟踪任何内容，也就无需清除）。
    async fn clear_step_progress(&self, _project: &str) -> Result<(), DtError> {
        Ok(())
    }

    /// 为给定的项目相对路径删除全部按文件记录的状态（文件快照 + 流水线步骤进度）。
    /// 用于文件从磁盘上被删除的场景：清除变更检测基线，使删除操作不会在
    /// 后续每次构建时被重复上报，并让之后重新创建的文件以全新状态被处理，
    /// 而不是被跳过为“已完成”。
    ///
    /// 返回被删除的行数（仅供参考）。
    ///
    /// 默认为空操作并返回 `0`：未记录按文件状态的仓库只需保持原有
    /// （无害的）重复上报行为。
    async fn delete_file_progress(
        &self,
        _project: &str,
        _paths: &[String],
    ) -> Result<u64, DtError> {
        Ok(0)
    }

    /// 检查存储健康状态。
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// Embedding 服务抽象。
#[async_trait]
pub trait EmbedService: Send + Sync + 'static {
    /// 为一组文本生成 embedding。
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError>;

    /// 检查服务健康状态。
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// LLM（聊天补全）服务抽象。
#[async_trait]
pub trait LlmService: Send + Sync + 'static {
    /// 发送聊天补全请求。
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError>;

    /// 检查服务健康状态。
    async fn health_check(&self) -> Result<HealthStatus, DtError>;

    /// 返回该提供方的能力声明。
    fn capabilities(&self) -> LlmCapabilities;
}

/// Rerank 服务抽象。
#[async_trait]
pub trait RerankService: Send + Sync + 'static {
    /// 按查询对文档进行重排（rerank）。按原始顺序返回相关性分数。
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError>;

    /// 检查服务健康状态。
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// 提供方能力声明。
#[derive(Debug, Clone, Default)]
pub struct LlmCapabilities {
    /// 支持 embedding。
    pub embed: bool,
    /// 支持重排（rerank）。
    pub rerank: bool,
    /// 支持 LLM 聊天补全。
    pub chat: bool,
    /// 单次响应的最大 token 数。
    pub max_tokens: u32,
}

/// 解析策略 trait——按编程语言分别实现。
///
/// 每种语言的解析器可独立判断其能否处理某个文件，并从源码文本中产出
/// 已解析的实体。
#[async_trait]
pub trait ParseStrategy: Send + Sync {
    /// 返回该解析器处理的语言。
    fn language(&self) -> crate::domain::types::Language;

    /// 若该解析器可以处理给定文件，则返回 `true`。
    fn can_parse(&self, path: &Path) -> bool;

    /// 将源码文本解析为方法和类。
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError>;
}

/// 构建服务抽象——编排整个构建流水线。
#[async_trait]
pub trait BuildService: Send + Sync + 'static {
    /// 对项目执行全量/增量构建。
    async fn build(
        &self,
        project: &str,
        root: &Path,
    ) -> Result<crate::domain::types::BuildReport, DtError>;

    /// 单文件更新（用于实时 hook 触发）。
    async fn update_file(&self, project: &str, path: &Path) -> Result<(), DtError>;

    /// 移除某个项目的全部数据。
    async fn delete_project(&self, project: &str) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 trait 是对象安全的（可被用作 `dyn Trait`）。
    #[test]
    fn traits_are_object_safe() {
        // 如果这些 trait 不是对象安全的，编译时就会失败。
        fn _accept_graph(_: &dyn GraphRepository) {}
        fn _accept_vector(_: &dyn VectorRepository) {}
        fn _accept_snapshot(_: &dyn SnapshotRepository) {}
        fn _accept_embed(_: &dyn EmbedService) {}
        fn _accept_parse(_: &dyn ParseStrategy) {}
        fn _accept_llm(_: &dyn LlmService) {}
        fn _accept_rerank(_: &dyn RerankService) {}
    }

    /// 只实现 `search` 的桩仓库——用于验证默认 `search_with_filter`
    /// 后置过滤逻辑（R7 向后兼容）。
    struct StubVectorRepo {
        hits: Vec<serde_json::Value>,
    }

    #[async_trait]
    impl VectorRepository for StubVectorRepo {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }

        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
        }

        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }

        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }

        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }

        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.to_string(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }

        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn default_search_with_filter_post_filters_by_payload() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a", "type": "Service"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b", "type": "Service"}}),
                serde_json::json!({"score": 0.7, "payload": {"type": "Service"}}), // 无 project 键
            ],
        };
        let hits = repo
            .search_with_filter(
                "kg_nodes",
                vec![0.0],
                5,
                serde_json::json!({"must": [{"key": "project", "match": {"value": "a"}}]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["payload"]["project"], serde_json::json!("a"));
    }

    #[tokio::test]
    async fn default_search_with_filter_must_not_excludes() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b"}}),
            ],
        };
        let hits = repo
            .search_with_filter(
                "c",
                vec![0.0],
                5,
                serde_json::json!({"must_not": [{"key": "project", "match": {"value": "b"}}]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["payload"]["project"], serde_json::json!("a"));
    }

    #[tokio::test]
    async fn default_search_with_filter_should_without_must_is_disjunctive() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b"}}),
                serde_json::json!({"score": 0.7, "payload": {"project": "c"}}),
            ],
        };
        let hits = repo
            .search_with_filter(
                "c",
                vec![0.0],
                5,
                serde_json::json!({"should": [
                    {"key": "project", "match": {"value": "a"}},
                    {"key": "project", "match": {"value": "b"}},
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn default_search_with_filter_empty_filter_passes_all() {
        let repo = StubVectorRepo {
            hits: vec![serde_json::json!({"score": 0.9, "payload": {"project": "a"}})],
        };
        let hits = repo
            .search_with_filter("c", vec![0.0], 5, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }
}
