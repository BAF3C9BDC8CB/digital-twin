//! Qdrant 的向量仓库实现。
//!
//! - `NoopVectorRepo`：用于编译期校验的 no-op 实现。
//! - `QdrantRepo`：真实的 Qdrant gRPC 仓库。

use std::hash::{Hash, Hasher};

use crate::domain::error::DtError;
use crate::domain::traits::VectorRepository;
use crate::domain::types::{CollectionInfo, HealthStatus};
use crate::shared::collections::{VECTOR_NAME_BASE, VECTOR_NAME_LLM};
use async_trait::async_trait;

use crate::infrastructure::qdrant::client::QdrantClient;

use qdrant_client::qdrant::{
    point_id, vectors_config, CreateCollectionBuilder, DeletePointsBuilder, Distance,
    GetPointsBuilder, PointId, PointStruct, RetrievedPoint, SearchPointsBuilder,
    SetPayloadPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder, VectorParamsMap,
    VectorsConfig, VectorsOutput,
};

/// 判断 Qdrant 错误是否为"集合不存在"。
///
/// Qdrant gRPC 对不存在的集合返回形如
/// `Not found: Collection 'xxx' doesn't exist!` 的错误。
/// 对搜索/滚动而言，集合不存在 = 该世界尚无数据，应视为空结果
/// 而非硬错误（例如 `dt clean` 后未重建时搜索应优雅返回空）。
fn collection_missing(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("doesn't exist")
        || lower.contains("does not exist")
        || lower.contains("not found")
}

// ---------------------------------------------------------------------------
// Noop——编译期占位（所有方法返回空/默认值）
// ---------------------------------------------------------------------------

/// No-op 向量仓库——所有查询都返回空/默认值。
/// 在真正接入 Qdrant 之前，可用于对完整技术栈做编译期检查。
pub struct NoopVectorRepo;

#[async_trait]
impl VectorRepository for NoopVectorRepo {
    async fn ensure_collection(&self, _collection: &str, _vector_dim: u32) -> Result<(), DtError> {
        Ok(())
    }

    async fn search(
        &self,
        _collection: &str,
        _vector: Vec<f32>,
        _limit: u64,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        Ok(vec![])
    }

    async fn upsert(
        &self,
        _collection: &str,
        _points: Vec<serde_json::Value>,
    ) -> Result<(), DtError> {
        Ok(())
    }

    async fn delete_by_filter(
        &self,
        _collection: &str,
        _filter: serde_json::Value,
    ) -> Result<(), DtError> {
        Ok(())
    }

    async fn list_collections(&self) -> Result<Vec<String>, DtError> {
        Ok(vec![])
    }

    async fn collection_info(&self, name: &str) -> Result<CollectionInfo, DtError> {
        Ok(CollectionInfo {
            name: name.to_string(),
            points_count: 0,
            vector_dim: 0,
            model_version: String::new(),
        })
    }

    async fn delete_collection(&self, _name: &str) -> Result<(), DtError> {
        Ok(())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }
}

// ---------------------------------------------------------------------------
// QdrantRepo——使用 qdrant-client crate 的真实实现
// ---------------------------------------------------------------------------

/// 真实的 Qdrant gRPC 仓库。
///
/// 包装 [`QdrantClient`]，并通过 Qdrant gRPC API 管理集合生命周期、
/// 点 upsert 与向量搜索。
pub struct QdrantRepo {
    client: QdrantClient,
}

impl QdrantRepo {
    pub fn new(client: QdrantClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl VectorRepository for QdrantRepo {
    async fn ensure_collection(&self, collection: &str, vector_dim: u32) -> Result<(), DtError> {
        let qdrant = self.client.inner();

        // 检查集合是否已存在
        let exists = qdrant
            .collection_exists(collection.to_string())
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant collection_exists: {}", e)))?;

        if !exists {
            // 仅 code_methods 使用 named vectors 双向量（用户拍板，base 召回 + llm rerank）：
            // - `base`：确定性召回向量（AST 提取 → embed(signature+comment)），必填。
            // - `llm`：LLM 分析文本的 rerank 向量，仅 Phase 2 成功后写入（可选）。
            // 其余集合（kg_nodes/doc_chunks 等）的写入方是单向量（kg_bridge/consolidate
            // 用普通 upsert + 不带 name 的 search），必须保持单向量结构，否则
            // search_with_filter 会报 "Not existing vector name"。
            if collection == crate::shared::collections::CODE_METHODS {
                let mut map = std::collections::HashMap::new();
                for name in [VECTOR_NAME_BASE, VECTOR_NAME_LLM] {
                    map.insert(
                        name.to_string(),
                        VectorParamsBuilder::new(vector_dim as u64, Distance::Cosine).on_disk(true),
                    );
                }
                qdrant
                    .create_collection(
                        CreateCollectionBuilder::new(collection.to_string()).vectors_config(
                            VectorsConfig {
                                config: Some(vectors_config::Config::ParamsMap(VectorParamsMap {
                                    map: map.into_iter().map(|(k, b)| (k, b.build())).collect(),
                                })),
                            },
                        ),
                    )
                    .await
                    .map_err(|e| DtError::Repository(format!("Qdrant create_collection: {e}")))?;
            } else {
                qdrant
                    .create_collection(
                        CreateCollectionBuilder::new(collection.to_string()).vectors_config(
                            VectorParamsBuilder::new(vector_dim as u64, Distance::Cosine)
                                .on_disk(true),
                        ),
                    )
                    .await
                    .map_err(|e| DtError::Repository(format!("Qdrant create_collection: {e}")))?;
            }
        }

        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        let qdrant = self.client.inner();

        match qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit).with_payload(true),
            )
            .await
        {
            Ok(response) => scored_points_to_json(response.result),
            Err(e) if collection_missing(&e.to_string()) => Ok(vec![]),
            Err(e) => Err(DtError::Repository(format!("Qdrant search: {e}"))),
        }
    }

    /// 原生带过滤的搜索（R7 覆写）：将 JSON 过滤条件翻译成
    /// 服务端的 Qdrant `Filter`，而不是在客户端做后置过滤。
    async fn search_with_filter(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        let qdrant = self.client.inner();

        match qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit)
                    .with_payload(true)
                    .filter(json_to_qdrant_filter(&filter)?),
            )
            .await
        {
            Ok(response) => scored_points_to_json(response.result),
            Err(e) if collection_missing(&e.to_string()) => Ok(vec![]),
            Err(e) => Err(DtError::Repository(format!(
                "Qdrant search_with_filter: {e}"
            ))),
        }
    }

    /// 在指定命名向量上搜索最近邻（named vectors 集合，如 code_methods 的 "base"）。
    async fn search_named(
        &self,
        collection: &str,
        vector_name: &str,
        vector: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        let qdrant = self.client.inner();

        match qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit)
                    .with_payload(true)
                    .vector_name(vector_name.to_string()),
            )
            .await
        {
            Ok(response) => scored_points_to_json(response.result),
            Err(e) if collection_missing(&e.to_string()) => Ok(vec![]),
            Err(e) => Err(DtError::Repository(format!("Qdrant search_named: {e}"))),
        }
    }

    /// 在指定命名向量上带过滤搜索（named vectors 集合）。
    async fn search_named_with_filter(
        &self,
        collection: &str,
        vector_name: &str,
        vector: Vec<f32>,
        limit: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        let qdrant = self.client.inner();

        match qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit)
                    .with_payload(true)
                    .vector_name(vector_name.to_string())
                    .filter(json_to_qdrant_filter(&filter)?),
            )
            .await
        {
            Ok(response) => scored_points_to_json(response.result),
            Err(e) if collection_missing(&e.to_string()) => Ok(vec![]),
            Err(e) => Err(DtError::Repository(format!(
                "Qdrant search_named_with_filter: {e}"
            ))),
        }
    }

    /// 按数值点 id 批量拉取指定命名向量（rerank 用）。
    ///
    /// 返回 `[{"id": <u64>, "vector": [f32...]}]`。点缺失该命名向量时不在结果中。
    async fn fetch_vectors(
        &self,
        collection: &str,
        ids: &[u64],
        vector_name: &str,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let qdrant = self.client.inner();
        let point_ids: Vec<PointId> = ids
            .iter()
            .map(|&n| PointId {
                point_id_options: Some(point_id::PointIdOptions::Num(n)),
            })
            .collect();

        match qdrant
            .get_points(
                GetPointsBuilder::new(collection.to_string(), point_ids)
                    .with_payload(false)
                    .with_vectors(qdrant_client::qdrant::VectorsSelector {
                        names: vec![vector_name.to_string()],
                    }),
            )
            .await
        {
            Ok(response) => {
                let mut out = Vec::with_capacity(response.result.len());
                for point in response.result {
                    if let Some(v) = extracted_named_vector(&point, vector_name) {
                        out.push(serde_json::json!({ "id": point_id_to_json(point.id.as_ref()), "vector": v }));
                    }
                }
                Ok(out)
            }
            Err(e) if collection_missing(&e.to_string()) => Ok(vec![]),
            Err(e) => Err(DtError::Repository(format!("Qdrant fetch_vectors: {e}"))),
        }
    }

    /// Scroll all payloads in a collection (no vectors), with optional JSON filter.
    ///
    /// Pages through `ScrollPoints` (256/page) until `max` payloads are collected
    /// or the collection is exhausted. Payload prost maps serialize to plain
    /// JSON the same way as in `scored_points_to_json`.
    async fn scroll_payloads(
        &self,
        collection: &str,
        filter: Option<serde_json::Value>,
        max: usize,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        use qdrant_client::qdrant::ScrollPointsBuilder;

        let qdrant = self.client.inner();
        let mut out = Vec::new();
        let mut offset: Option<qdrant_client::qdrant::PointId> = None;
        let mut first_err: Option<String> = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(collection.to_string())
                .limit(256)
                .with_payload(true)
                .with_vectors(false);
            if let Some(f) = &filter {
                builder = builder.filter(json_to_qdrant_filter(f)?);
            }
            if let Some(o) = offset {
                builder = builder.offset(o);
            }
            match qdrant.scroll(builder).await {
                Ok(resp) => {
                    for point in resp.result {
                        let payload: serde_json::Value = serde_json::to_value(&point.payload)
                            .unwrap_or_else(|_| serde_json::json!({}));
                        out.push(payload);
                        if out.len() >= max {
                            return Ok(out);
                        }
                    }
                    match resp.next_page_offset {
                        Some(next) => offset = Some(next),
                        None => break,
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    // 集合不存在 = 该世界无数据 → 空结果；首页失败且是集合缺失时直接返回空。
                    if collection_missing(&msg) {
                        return Ok(vec![]);
                    }
                    if first_err.is_none() {
                        first_err = Some(msg);
                    }
                    break;
                }
            }
        }
        if let Some(e) = first_err {
            return Err(DtError::Repository(format!("Qdrant scroll_payloads: {e}")));
        }
        Ok(out)
    }

    /// Scroll 返回 `{"id": <数值点 id>, "payload": {...}}` 列表（无向量），可选过滤。
    ///
    /// 与 [`scroll_payloads`] 的区别：结果携带点 id，供上层（如补偿自愈扫描）
    /// 定位需要 `set_payload` 更新的具体点。分页循环（256/页）直到拿满 `max`
    /// 或集合耗尽（next_page_offset 为空）。集合缺失时返回空（与
    /// `scroll_payloads` 一致）。
    async fn scroll_points(
        &self,
        collection: &str,
        filter: Option<serde_json::Value>,
        max: usize,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        use qdrant_client::qdrant::ScrollPointsBuilder;

        let qdrant = self.client.inner();
        let mut out = Vec::new();
        let mut offset: Option<qdrant_client::qdrant::PointId> = None;
        let mut first_err: Option<String> = None;
        loop {
            let mut builder = ScrollPointsBuilder::new(collection.to_string())
                .limit(256)
                .with_payload(true)
                .with_vectors(false);
            if let Some(f) = &filter {
                builder = builder.filter(json_to_qdrant_filter(f)?);
            }
            if let Some(o) = offset {
                builder = builder.offset(o);
            }
            match qdrant.scroll(builder).await {
                Ok(resp) => {
                    for point in resp.result {
                        out.push(serde_json::json!({
                            "id": point_id_to_json(point.id.as_ref()),
                            "payload": serde_json::to_value(&point.payload)
                                .unwrap_or_else(|_| serde_json::json!({})),
                        }));
                        if out.len() >= max {
                            return Ok(out);
                        }
                    }
                    match resp.next_page_offset {
                        Some(next) => offset = Some(next),
                        None => break,
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    // 集合不存在 = 该世界无数据 → 空结果；首页失败且是集合缺失时直接返回空。
                    if collection_missing(&msg) {
                        return Ok(vec![]);
                    }
                    if first_err.is_none() {
                        first_err = Some(msg);
                    }
                    break;
                }
            }
        }
        if let Some(e) = first_err {
            return Err(DtError::Repository(format!("Qdrant scroll_points: {e}")));
        }
        Ok(out)
    }

    async fn upsert(
        &self,
        collection: &str,
        points: Vec<serde_json::Value>,
    ) -> Result<(), DtError> {
        let qdrant = self.client.inner();

        let qdrant_points: Vec<PointStruct> = points
            .iter()
            .enumerate()
            .map(|(idx, p)| {
                // 从点 ID 计算稳定的数值 ID。
                // 方法 ID 是 "dt://entity/..." 这类字符串，由于 Qdrant 只接受
                // 数值或标准 UUID ID，因此必须哈希成 u64。
                let id_num: u64 = p
                    .get("id")
                    .and_then(|v| v.as_u64()) // 已是数值→直接使用
                    .unwrap_or_else(|| {
                        // 将字符串表示哈希成 u64
                        let s = p
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("idx:{idx}"));
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        s.hash(&mut hasher);
                        hasher.finish()
                    });
                let id = PointId {
                    point_id_options: Some(point_id::PointIdOptions::Num(id_num)),
                };

                let payload_json = p.get("payload").cloned().unwrap_or(serde_json::Value::Null);

                let payload: std::collections::HashMap<String, qdrant_client::qdrant::Value> =
                    serde_json::from_value(payload_json).unwrap_or_default();

                // named vectors（双向量）：point 携带 `"vectors": {"base": [...], "llm": [...]}`
                // 对象时走 named vectors 路径（base 必填，llm 可选——仅 Phase 2 成功后才有）。
                // 否则回退到单向量 `"vector"` 字段，兼容既有调用方。
                if let Some(named) = p.get("vectors").and_then(|v| v.as_object()) {
                    let mut vec_map: std::collections::HashMap<String, Vec<f32>> =
                        std::collections::HashMap::new();
                    for (name, arr) in named {
                        let v: Vec<f32> = arr
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                                    .collect()
                            })
                            .unwrap_or_default();
                        vec_map.insert(name.clone(), v);
                    }
                    PointStruct::new(id, vec_map, payload)
                } else {
                    let vector: Vec<f32> = p
                        .get("vector")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect()
                        })
                        .unwrap_or_default();

                    PointStruct::new(id, vector, payload)
                }
            })
            .collect();

        qdrant
            .upsert_points(
                UpsertPointsBuilder::new(collection.to_string(), qdrant_points).wait(true),
            )
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant upsert: {e}")))?;

        Ok(())
    }

    /// 只更新指定点的 payload 字段（保留向量、不重嵌）。
    ///
    /// `payloads` 每项形状：`{"id": <u64>, "payload": {...}}`。
    /// 按 payload 内容分组，每个分组一次 gRPC 请求。集合缺失时降级 `Ok(())`
    /// （与 `collection_missing` 语义一致，例如 `dt clean` 后尚未重建）。
    async fn set_payload(
        &self,
        collection: &str,
        payloads: Vec<serde_json::Value>,
    ) -> Result<(), DtError> {
        if payloads.is_empty() {
            return Ok(());
        }
        let qdrant = self.client.inner();

        // 按 payload 内容分组（相同 payload 的多点合并为一个请求）。
        let mut groups: std::collections::HashMap<serde_json::Value, Vec<PointId>> =
            std::collections::HashMap::new();
        for item in &payloads {
            let id_num = item
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| DtError::General(format!("set_payload 项缺少数值 'id': {item}")))?;
            let payload_json = item
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let payload: std::collections::HashMap<String, qdrant_client::qdrant::Value> =
                serde_json::from_value(payload_json).map_err(|e| {
                    DtError::General(format!("set_payload 的 payload 解析失败: {e}"))
                })?;
            let group_key = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
            groups.entry(group_key).or_default().push(PointId {
                point_id_options: Some(point_id::PointIdOptions::Num(id_num)),
            });
        }

        for (payload, ids) in groups {
            // 分组 key 是 serde_json 值，调用前转回 Qdrant payload map。
            let payload_map: std::collections::HashMap<String, qdrant_client::qdrant::Value> =
                serde_json::from_value(payload).unwrap_or_default();
            match qdrant
                .set_payload(
                    SetPayloadPointsBuilder::new(collection.to_string(), payload_map)
                        .points_selector(ids)
                        .wait(true),
                )
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    let msg = e.to_string();
                    // 集合不存在 = 该世界无数据 → 降级 Ok(())，与 collection_missing 语义一致。
                    if !collection_missing(&msg) {
                        return Err(DtError::Repository(format!("Qdrant set_payload: {msg}")));
                    }
                }
            }
        }

        Ok(())
    }

    async fn delete_by_filter(
        &self,
        collection: &str,
        filter: serde_json::Value,
    ) -> Result<(), DtError> {
        let qdrant = self.client.inner();

        // 将 JSON 过滤条件翻译成原生 Qdrant filter。空
        // filter 匹配所有点（Qdrant 语义）——需要选择性删除的
        // 调用方必须传入非空的 `must` 子句。
        qdrant
            .delete_points(
                DeletePointsBuilder::new(collection.to_string())
                    .points(json_to_qdrant_filter(&filter)?)
                    .wait(true),
            )
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant delete_points: {}", e)))?;

        Ok(())
    }

    async fn list_collections(&self) -> Result<Vec<String>, DtError> {
        let qdrant = self.client.inner();

        let response = qdrant
            .list_collections()
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant list_collections: {}", e)))?;

        let names: Vec<String> = response
            .collections
            .iter()
            .map(|c| c.name.clone())
            .collect();

        Ok(names)
    }

    async fn collection_info(&self, name: &str) -> Result<CollectionInfo, DtError> {
        let qdrant = self.client.inner();

        let response = qdrant
            .collection_info(name.to_string())
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant collection_info: {}", e)))?;

        let info = response.result.as_ref();

        Ok(CollectionInfo {
            name: name.to_string(),
            points_count: info.and_then(|r| r.points_count).unwrap_or(0),
            vector_dim: info
                .and_then(|r| r.config.as_ref())
                .and_then(|c| c.params.as_ref())
                .and_then(|p| p.vectors_config.as_ref())
                .and_then(|vc| match &vc.config {
                    // 单向量配置：直接取 size。
                    Some(qdrant_client::qdrant::vectors_config::Config::Params(vp)) => {
                        Some(vp.size as u32)
                    }
                    // named vectors 配置：取 base 向量的 size（base 为必填召回向量）。
                    Some(qdrant_client::qdrant::vectors_config::Config::ParamsMap(map)) => map
                        .map
                        .get(VECTOR_NAME_BASE)
                        .map(|vp| vp.size as u32)
                        .or_else(|| map.map.values().next().map(|vp| vp.size as u32)),
                    None => None,
                })
                .unwrap_or(0),
            model_version: String::new(),
        })
    }

    async fn delete_collection(&self, name: &str) -> Result<(), DtError> {
        let qdrant = self.client.inner();

        qdrant
            .delete_collection(name.to_string())
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant delete_collection: {}", e)))?;

        Ok(())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        let qdrant = self.client.inner();

        match qdrant.health_check().await {
            Ok(reply) => {
                tracing::info!(
                    "Qdrant 健康状态正常: version={}, title={}",
                    reply.version,
                    reply.title
                );
                Ok(HealthStatus::Healthy)
            }
            Err(e) => {
                tracing::error!("Qdrant 健康检查失败: {}", e);
                Ok(HealthStatus::Unhealthy(format!("Qdrant 健康检查: {}", e)))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 将评分后的点转换为仓库的 JSON 命中形状
/// （`[{"id": ..., "score": ..., "payload": {...}}]`）。
fn scored_points_to_json(
    points: Vec<qdrant_client::qdrant::ScoredPoint>,
) -> Result<Vec<serde_json::Value>, DtError> {
    let results: Vec<serde_json::Value> = points
        .iter()
        .map(|point| {
            serde_json::json!({
                "id": point_id_to_json(point.id.as_ref()),
                "score": point.score,
                "payload": point.payload,
            })
        })
        .collect();

    Ok(results)
}

/// 将 Qdrant 风格的过滤器 JSON
/// （`{"must": [...], "should": [...], "must_not": [...]}`，其中每个条件为
/// `{"key": ..., "match": {"value": ...}}`）翻译成原生
/// [`qdrant_client::qdrant::Filter`]。支持的匹配值：字符串
/// （keyword）、布尔、整数。其他任何值都会报错，
/// 这样畸形过滤器就不会悄悄退化为"匹配所有"。
fn json_to_qdrant_filter(
    filter: &serde_json::Value,
) -> Result<qdrant_client::qdrant::Filter, DtError> {
    let parse_conditions =
        |clause: &str| -> Result<Vec<qdrant_client::qdrant::Condition>, DtError> {
            let Some(conds) = filter.get(clause).and_then(|c| c.as_array()) else {
                return Ok(vec![]);
            };
            conds.iter().map(json_to_condition).collect()
        };

    Ok(qdrant_client::qdrant::Filter {
        should: parse_conditions("should")?,
        must: parse_conditions("must")?,
        must_not: parse_conditions("must_not")?,
        ..Default::default()
    })
}

/// 翻译一个 `{"key": ..., "match": {"value": ...}}` 条件。
///
/// 扩展支持（Phase 2 补偿自愈扫描依赖）：
/// - `{"key": ..., "is_empty": true}` → [`Condition::is_empty`]
///   （匹配字段缺失或为空值，例如 `llm_analysis` 未写入的点）；
/// - `{"key": ..., "is_null": true}` → [`Condition::is_null`]（匹配 null 值）。
/// `is_empty` / `is_null` 与 `match` 互斥，同时出现返回错误。
fn json_to_condition(
    cond: &serde_json::Value,
) -> Result<qdrant_client::qdrant::Condition, DtError> {
    let key = cond
        .get("key")
        .and_then(|k| k.as_str())
        .ok_or_else(|| DtError::General(format!("过滤条件缺少 'key': {cond}")))?;

    let is_empty = cond
        .get("is_empty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_null = cond
        .get("is_null")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_match = cond.get("match").is_some();

    if (is_empty || is_null) && has_match {
        return Err(DtError::General(format!(
            "过滤条件 'is_empty'/'is_null' 与 'match' 互斥: {cond}"
        )));
    }
    if is_empty {
        return Ok(qdrant_client::qdrant::Condition::is_empty(key));
    }
    if is_null {
        return Ok(qdrant_client::qdrant::Condition::is_null(key));
    }

    let value = cond
        .get("match")
        .and_then(|m| m.get("value"))
        .ok_or_else(|| DtError::General(format!("过滤条件缺少 'match.value': {cond}")))?;

    match value {
        serde_json::Value::String(s) => {
            Ok(qdrant_client::qdrant::Condition::matches(key, s.clone()))
        }
        serde_json::Value::Bool(b) => Ok(qdrant_client::qdrant::Condition::matches(key, *b)),
        serde_json::Value::Number(n) => {
            let i = n
                .as_i64()
                .ok_or_else(|| DtError::General(format!("过滤匹配值不是整数: {n}")))?;
            Ok(qdrant_client::qdrant::Condition::matches(key, i))
        }
        other => Err(DtError::General(format!(
            "不支持的过滤匹配值（仅支持 string/bool/integer）: {other}"
        ))),
    }
}

/// 将 Qdrant 点 ID 转成 JSON（数值 id 保持数值、UUID id 转字符串、缺失为 null）。
fn point_id_to_json(id: Option<&qdrant_client::qdrant::PointId>) -> serde_json::Value {
    match id.and_then(|i| i.point_id_options.as_ref()) {
        Some(point_id::PointIdOptions::Num(n)) => serde_json::json!(*n),
        Some(point_id::PointIdOptions::Uuid(u)) => serde_json::json!(u),
        None => serde_json::Value::Null,
    }
}

/// 从 RetrievedPoint 中提取指定命名向量的稠密数据（rerank 用）。
///
/// Qdrant named vectors 的 gRPC 结构：`point.vectors -> VectorsOutput`，
/// 单/命名向量分别用 `vectors` 或 `named_vectors` 字段承载。只取稠密向量，
/// 稀疏（sparse）向量返回 None（本系统只用稠密）。
fn extracted_named_vector(point: &RetrievedPoint, vector_name: &str) -> Option<Vec<f32>> {
    let vectors = point.vectors.as_ref()?;
    use qdrant_client::qdrant::vectors_output::VectorsOptions;
    match vectors.vectors_options.as_ref()? {
        VectorsOptions::Vectors(named) => {
            let v = named.vectors.get(vector_name)?;
            // VectorOutput 是扁平 prost struct，稠密数据在 data 字段
            // （deprecated 标记仅提示用 into_vector，字段本身仍可用）。
            Some(v.data.iter().map(|x| *x as f32).collect())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_translates_must_clauses() {
        let filter = serde_json::json!({
            "must": [
                {"key": "project", "match": {"value": "offen-pay"}},
                {"key": "degraded", "match": {"value": false}},
                {"key": "block_index", "match": {"value": 3}},
            ]
        });
        let f = json_to_qdrant_filter(&filter).unwrap();
        assert_eq!(f.must.len(), 3);
        assert!(f.should.is_empty());
        assert!(f.must_not.is_empty());

        // 抽查第一个条件：Field(project matches keyword)。
        let qdrant_client::qdrant::condition::ConditionOneOf::Field(field) =
            f.must[0].condition_one_of.clone().unwrap()
        else {
            panic!("预期 field 条件");
        };
        assert_eq!(field.key, "project");
        let m = field.r#match.unwrap().match_value.unwrap();
        assert!(matches!(
            m,
            qdrant_client::qdrant::r#match::MatchValue::Keyword(ref k) if k == "offen-pay"
        ));
    }

    #[test]
    fn collection_missing_detects_qdrant_errors() {
        // Qdrant gRPC 对不存在的集合返回 "doesn't exist" / "Not found" 错误。
        assert!(collection_missing(
            "Not found: Collection `code_methods` doesn't exist!"
        ));
        assert!(collection_missing("Collection 'kg_nodes' does not exist"));
        assert!(collection_missing("Not found: Collection xyz"));
        // 其他错误不应误判。
        assert!(!collection_missing("connection refused"));
        assert!(!collection_missing("failed to connect to qdrant"));
        assert!(!collection_missing(""));
    }

    #[test]
    fn filter_translates_all_clause_kinds() {
        let filter = serde_json::json!({
            "must": [{"key": "a", "match": {"value": "x"}}],
            "should": [{"key": "b", "match": {"value": "y"}}],
            "must_not": [{"key": "c", "match": {"value": "z"}}],
        });
        let f = json_to_qdrant_filter(&filter).unwrap();
        assert_eq!(f.must.len(), 1);
        assert_eq!(f.should.len(), 1);
        assert_eq!(f.must_not.len(), 1);
    }

    #[test]
    fn filter_empty_json_yields_empty_filter() {
        let f = json_to_qdrant_filter(&serde_json::json!({})).unwrap();
        assert!(f.must.is_empty() && f.should.is_empty() && f.must_not.is_empty());
    }

    #[test]
    fn filter_rejects_missing_key() {
        let filter = serde_json::json!({"must": [{"match": {"value": "x"}}]});
        assert!(json_to_qdrant_filter(&filter).is_err());
    }

    #[test]
    fn filter_rejects_unsupported_match_value() {
        let filter = serde_json::json!({"must": [{"key": "k", "match": {"value": [1, 2]}}]});
        assert!(json_to_qdrant_filter(&filter).is_err());
    }

    #[test]
    fn noop_uses_default_search_with_filter() {
        // NoopVectorRepo 未覆写 `search_with_filter`——trait
        // 默认实现（search + 后置过滤）生效并返回空。
        let repo = NoopVectorRepo;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hits = rt
            .block_on(repo.search_with_filter(
                "kg_nodes",
                vec![0.0],
                5,
                serde_json::json!({"must": [{"key": "project", "match": {"value": "a"}}]}),
            ))
            .unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn noop_scroll_payloads_returns_empty() {
        let repo = NoopVectorRepo;
        let out = repo
            .scroll_payloads("code_methods", None, 100)
            .await
            .expect("default scroll impl");
        assert!(out.is_empty());
    }

    #[test]
    fn filter_translates_is_empty_and_is_null() {
        // 补偿自愈扫描依赖：`llm_analysis` 缺失 → is_empty；null 值 → is_null。
        let filter = serde_json::json!({
            "must": [
                {"key": "llm_analysis", "is_empty": true},
                {"key": "llm_status", "is_null": true},
            ]
        });
        let f = json_to_qdrant_filter(&filter).unwrap();
        assert_eq!(f.must.len(), 2);

        let qdrant_client::qdrant::condition::ConditionOneOf::IsEmpty(empty) =
            f.must[0].condition_one_of.clone().unwrap()
        else {
            panic!("预期 is_empty 条件");
        };
        assert_eq!(empty.key, "llm_analysis");

        let qdrant_client::qdrant::condition::ConditionOneOf::IsNull(is_null) =
            f.must[1].condition_one_of.clone().unwrap()
        else {
            panic!("预期 is_null 条件");
        };
        assert_eq!(is_null.key, "llm_status");
    }

    #[test]
    fn filter_rejects_is_empty_with_match() {
        // is_empty/is_null 与 match 互斥——同时出现必须报错，避免畸形过滤
        // 悄悄退化为"匹配所有"。
        let filter = serde_json::json!({
            "must": [
                {"key": "llm_analysis", "is_empty": true, "match": {"value": "x"}},
            ]
        });
        assert!(json_to_qdrant_filter(&filter).is_err());
    }

    #[tokio::test]
    async fn noop_scroll_points_returns_empty() {
        // NoopVectorRepo 未覆写 scroll_points——trait 默认实现返回空。
        let repo = NoopVectorRepo;
        let out = repo
            .scroll_points("code_methods", None, 100)
            .await
            .expect("default scroll impl");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn noop_set_payload_is_noop() {
        // NoopVectorRepo 未覆写 set_payload——trait 默认实现为空操作。
        let repo = NoopVectorRepo;
        repo.set_payload(
            "code_methods",
            vec![serde_json::json!({"id": 42u64, "payload": {"llm_status": "success"}})],
        )
        .await
        .expect("default set_payload impl");
    }
}
