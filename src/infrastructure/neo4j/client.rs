//! Neo4j Bolt client — async connection to Neo4j knowledge graph.
//!
//! Uses `neo4rs` crate for async Bolt communication via [`neo4rs::Graph`].
//! `Neo4jClient` implements [`GraphRepository`] for real Cypher queries.
//! `NoopGraphRepo` is retained as a compile-time/testing placeholder.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use crate::domain::types::HealthStatus;
use std::collections::HashMap;

/// Real Neo4j Bolt client using `neo4rs`.
///
/// Wraps a [`neo4rs::Graph`] connection pool. `Graph` is `Clone + Send + Sync`,
/// so the client can be shared cheaply without `Arc<Mutex<>>`.
#[derive(Clone)]
pub struct Neo4jClient {
    graph: neo4rs::Graph,
}

impl Neo4jClient {
    /// Establish a Bolt connection to Neo4j.
    ///
    /// # Arguments
    /// * `uri` — Bolt URI, e.g. `"bolt://localhost:7687"`
    /// * `user` — Neo4j username
    /// * `password` — Neo4j password
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self, DtError> {
        let config = neo4rs::ConfigBuilder::default()
            .uri(uri)
            .user(user)
            .password(password)
            .db("neo4j")
            .build()
            .map_err(|e| DtError::Repository(format!("Neo4j config build: {}", e)))?;

        let graph = neo4rs::Graph::connect(config)
            .await
            .map_err(|e| DtError::Repository(format!("Neo4j connect: {}", e)))?;

        Ok(Self { graph })
    }
}

// ---------------------------------------------------------------------------
// GraphRepository impl
// ---------------------------------------------------------------------------

#[async_trait]
impl GraphRepository for Neo4jClient {
    async fn read_query(
        &self,
        query: &str,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, DtError> {
        let q = build_query(query, &params);
        let mut result = self
            .graph
            .execute(q)
            .await
            .map_err(|e| DtError::Repository(format!("Neo4j read: {}", e)))?;

        let mut rows = Vec::new();
        while let Ok(Some(row)) = result.next().await {
            // neo4rs Row::to uses the crate's custom serde, but it follows
            // standard serde protocol so we can deserialize directly into
            // serde_json::Value.
            match row.to::<serde_json::Value>() {
                Ok(val) => rows.push(val),
                Err(_) => {
                    // Fallback: if row.to fails, skip this row.
                    // This happens for node/relation types that can't be
                    // represented as plain JSON values.
                }
            }
        }
        Ok(serde_json::Value::Array(rows))
    }

    async fn write_query(
        &self,
        query: &str,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, DtError> {
        let q = build_query(query, &params);
        let mut result = self.graph
            .execute(q)
            .await
            .map_err(|e| DtError::Repository(format!("Neo4j write: {}", e)))?;
        let mut rows = Vec::new();
        while let Ok(Some(row)) = result.next().await {
            match row.to::<serde_json::Value>() {
                Ok(val) => rows.push(val),
                Err(_) => {}
            }
        }
        if rows.is_empty() {
            Ok(serde_json::Value::Null)
        } else {
            Ok(serde_json::Value::Array(rows))
        }
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        match self.graph.run(neo4rs::query("RETURN 1")).await {
            Ok(_) => Ok(HealthStatus::Healthy),
            Err(e) => Ok(HealthStatus::Unhealthy(format!(
                "Neo4j unreachable: {}",
                e
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter conversion helpers
// ---------------------------------------------------------------------------

/// Build a `neo4rs::Query` from a raw Cypher string and serde_json params.
///
/// Uses the `json` feature of neo4rs which provides `TryFrom<serde_json::Value>
/// for BoltType`, so parameters are converted cleanly.
fn build_query(
    query_str: &str,
    params: &HashMap<String, serde_json::Value>,
) -> neo4rs::Query {
    let mut q = neo4rs::query(query_str);
    for (key, val) in params {
        let bolt_val = json_to_bolt(val.clone());
        q = q.param(key.as_str(), bolt_val);
    }
    q
}

/// Convert a `serde_json::Value` to `neo4rs::BoltType`, with proper
/// handling of arrays (Lists) and objects (Maps) for Cypher queries.
fn json_to_bolt(val: serde_json::Value) -> neo4rs::BoltType {
    if let Ok(bt) = neo4rs::BoltType::try_from(val.clone()) {
        return bt;
    }
    match val {
        serde_json::Value::Array(arr) => {
            let items: Vec<neo4rs::BoltType> = arr.into_iter().map(json_to_bolt).collect();
            neo4rs::BoltType::List(neo4rs::BoltList { value: items })
        }
        serde_json::Value::Object(obj) => {
            let map: std::collections::HashMap<neo4rs::BoltString, neo4rs::BoltType> = obj
                .into_iter()
                .map(|(k, v)| (neo4rs::BoltString { value: k }, json_to_bolt(v)))
                .collect();
            neo4rs::BoltType::Map(neo4rs::BoltMap { value: map })
        }
        // Should not reach here, but provide explicit conversions.
        serde_json::Value::String(s) => neo4rs::BoltType::String(neo4rs::BoltString { value: s }),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                neo4rs::BoltType::Integer(neo4rs::BoltInteger { value: i })
            } else {
                neo4rs::BoltType::Float(neo4rs::BoltFloat { value: n.as_f64().unwrap_or(0.0) })
            }
        }
        serde_json::Value::Bool(b) => neo4rs::BoltType::Boolean(neo4rs::BoltBoolean { value: b }),
        serde_json::Value::Null => neo4rs::BoltType::Null(neo4rs::BoltNull),
    }
}

// ---------------------------------------------------------------------------
// Noop implementation for compile-time validation & testing
// ---------------------------------------------------------------------------

/// No-op graph repository — returns default/empty values for all queries.
/// Enables compile-check of the full stack before real Neo4j integration.
pub struct NoopGraphRepo;

#[async_trait]
impl GraphRepository for NoopGraphRepo {
    async fn read_query(
        &self,
        _query: &str,
        _params: HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, DtError> {
        Ok(serde_json::Value::Null)
    }

    async fn write_query(
        &self,
        _query: &str,
        _params: HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, DtError> {
        Ok(serde_json::Value::Null)
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_health_check_returns_healthy() {
        let repo = NoopGraphRepo;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let status = rt.block_on(repo.health_check()).unwrap();
        assert!(matches!(status, HealthStatus::Healthy));
    }

    #[test]
    fn noop_read_returns_null() {
        let repo = NoopGraphRepo;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(repo.read_query("RETURN 1", HashMap::new())).unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn noop_write_returns_null() {
        let repo = NoopGraphRepo;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(repo.write_query("CREATE (n)", HashMap::new())).unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }
}
