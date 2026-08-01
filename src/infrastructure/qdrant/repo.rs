//! Vector repository implementations for Qdrant.
//!
//! - `NoopVectorRepo`: no-op implementation for compile-time validation.
//! - `QdrantRepo`: real Qdrant gRPC repository.

use std::hash::{Hash, Hasher};

use crate::domain::error::DtError;
use crate::domain::traits::VectorRepository;
use crate::domain::types::{CollectionInfo, HealthStatus};
use async_trait::async_trait;

use crate::infrastructure::qdrant::client::QdrantClient;

use qdrant_client::qdrant::{
    CreateCollectionBuilder, DeletePointsBuilder, Distance, PointStruct, SearchPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder,
};

// ---------------------------------------------------------------------------
// Noop — compile-time placeholder (all methods return empty/default)
// ---------------------------------------------------------------------------

/// No-op vector repository — returns empty/default for all queries.
/// Enables compile-check of the full stack before real Qdrant integration.
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
// QdrantRepo — real implementation using qdrant-client crate
// ---------------------------------------------------------------------------

/// Real Qdrant gRPC repository.
///
/// Wraps a [`QdrantClient`] and manages collection lifecycle,
/// point upsert, and vector search over the Qdrant gRPC API.
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

        // Check if collection already exists
        let exists = qdrant
            .collection_exists(collection.to_string())
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant collection_exists: {}", e)))?;

        if !exists {
            // Create the collection with Cosine distance and HNSW index
            qdrant
                .create_collection(
                    CreateCollectionBuilder::new(collection.to_string()).vectors_config(
                        VectorParamsBuilder::new(vector_dim as u64, Distance::Cosine).on_disk(true),
                    ),
                )
                .await
                .map_err(|e| DtError::Repository(format!("Qdrant create_collection: {}", e)))?;
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

        let response = qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit).with_payload(true),
            )
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant search: {}", e)))?;

        scored_points_to_json(response.result)
    }

    /// Native filtered search (R7 override): translates the JSON filter into
    /// a server-side Qdrant `Filter` instead of post-filtering client-side.
    async fn search_with_filter(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        let qdrant = self.client.inner();

        let response = qdrant
            .search_points(
                SearchPointsBuilder::new(collection.to_string(), vector, limit)
                    .with_payload(true)
                    .filter(json_to_qdrant_filter(&filter)?),
            )
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant search_with_filter: {}", e)))?;

        scored_points_to_json(response.result)
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
                // Compute a stable numeric ID from the point ID.
                // Method IDs are strings like "dt://entity/...", which must be
                // hashed to a u64 since Qdrant only accepts numeric or standard-UUID IDs.
                let id_num: u64 = p
                    .get("id")
                    .and_then(|v| v.as_u64()) // already numeric → use directly
                    .unwrap_or_else(|| {
                        // Hash the string representation to a u64
                        let s = p
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("idx:{}", idx));
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        s.hash(&mut hasher);
                        hasher.finish()
                    });
                let id = qdrant_client::qdrant::PointId {
                    point_id_options: Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(
                        id_num,
                    )),
                };

                let vector: Vec<f32> = p
                    .get("vector")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .unwrap_or_default();

                let payload_json = p.get("payload").cloned().unwrap_or(serde_json::Value::Null);

                let payload: std::collections::HashMap<String, qdrant_client::qdrant::Value> =
                    serde_json::from_value(payload_json).unwrap_or_default();

                PointStruct::new(id, vector, payload)
            })
            .collect();

        qdrant
            .upsert_points(
                UpsertPointsBuilder::new(collection.to_string(), qdrant_points).wait(true),
            )
            .await
            .map_err(|e| DtError::Repository(format!("Qdrant upsert: {}", e)))?;

        Ok(())
    }

    async fn delete_by_filter(
        &self,
        collection: &str,
        filter: serde_json::Value,
    ) -> Result<(), DtError> {
        let qdrant = self.client.inner();

        // Translate the JSON filter into a native Qdrant filter. An empty
        // filter matches all points (Qdrant semantics) — callers that need
        // selective deletion must pass a non-empty `must` clause.
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
                .and_then(|vc| {
                    if let Some(qdrant_client::qdrant::vectors_config::Config::Params(vp)) =
                        &vc.config
                    {
                        Some(vp.size as u32)
                    } else {
                        None
                    }
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
                Ok(HealthStatus::Unhealthy(format!(
                    "Qdrant health check: {}",
                    e
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert scored points into the repo's JSON hit shape
/// (`[{"id": ..., "score": ..., "payload": {...}}]`).
fn scored_points_to_json(
    points: Vec<qdrant_client::qdrant::ScoredPoint>,
) -> Result<Vec<serde_json::Value>, DtError> {
    let results: Vec<serde_json::Value> = points
        .iter()
        .map(|point| {
            serde_json::json!({
                "id": if let Some(ref id) = point.id {
                    match &id.point_id_options {
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => {
                            serde_json::json!(*n)
                        }
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => {
                            serde_json::json!(u)
                        }
                        None => serde_json::Value::Null,
                    }
                } else {
                    serde_json::Value::Null
                },
                "score": point.score,
                "payload": point.payload,
            })
        })
        .collect();

    Ok(results)
}

/// Translate a Qdrant-style filter JSON
/// (`{"must": [...], "should": [...], "must_not": [...]}` of
/// `{"key": ..., "match": {"value": ...}}` conditions) into a native
/// [`qdrant_client::qdrant::Filter`]. Supported match values: string
/// (keyword), bool, integer. Anything else is rejected with an error so a
/// malformed filter never silently degrades into "match everything".
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

/// Translate one `{"key": ..., "match": {"value": ...}}` condition.
fn json_to_condition(
    cond: &serde_json::Value,
) -> Result<qdrant_client::qdrant::Condition, DtError> {
    let key = cond
        .get("key")
        .and_then(|k| k.as_str())
        .ok_or_else(|| DtError::General(format!("filter condition missing 'key': {cond}")))?;
    let value = cond
        .get("match")
        .and_then(|m| m.get("value"))
        .ok_or_else(|| {
            DtError::General(format!("filter condition missing 'match.value': {cond}"))
        })?;

    match value {
        serde_json::Value::String(s) => {
            Ok(qdrant_client::qdrant::Condition::matches(key, s.clone()))
        }
        serde_json::Value::Bool(b) => Ok(qdrant_client::qdrant::Condition::matches(key, *b)),
        serde_json::Value::Number(n) => {
            let i = n.as_i64().ok_or_else(|| {
                DtError::General(format!("filter match value not an integer: {n}"))
            })?;
            Ok(qdrant_client::qdrant::Condition::matches(key, i))
        }
        other => Err(DtError::General(format!(
            "unsupported filter match value (string/bool/integer only): {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tests
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

        // Spot-check the first condition: Field(project matches keyword).
        let qdrant_client::qdrant::condition::ConditionOneOf::Field(field) =
            f.must[0].condition_one_of.clone().unwrap()
        else {
            panic!("expected field condition");
        };
        assert_eq!(field.key, "project");
        let m = field.r#match.unwrap().match_value.unwrap();
        assert!(matches!(
            m,
            qdrant_client::qdrant::r#match::MatchValue::Keyword(ref k) if k == "offen-pay"
        ));
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
        // NoopVectorRepo does not override `search_with_filter` — the trait
        // default (search + post-filter) applies and returns empty.
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
}
