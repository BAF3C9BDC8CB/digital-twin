//! Memgraph Bolt client — async connection to Memgraph knowledge graph.
//!
//! Uses the Bolt protocol driver (`bolt_driver`, wrapping the `neo4rs` crate)
//! for Memgraph via [`bolt_driver::Graph`].
//! `MemgraphClient` implements [`GraphRepository`] for real Cypher queries.
//! `NoopGraphRepo` is retained as a compile-time/testing placeholder.
//!
//! ## Memgraph compatibility
//!
//! Memgraph does not support the multi-database `db` field in Bolt RUN/BEGIN
//! messages. We set the database name to an empty string `""`, which tells
//! the Bolt driver to omit the `db` field entirely (see `Run::new` and
//! `BoltRequest::begin` in the driver source — they check `!db.is_empty()`
//! before emitting the field).

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use std::collections::HashMap;

/// Real Memgraph Bolt client.
///
/// Wraps a [`bolt_driver::Graph`] connection pool. `Graph` is `Clone + Send + Sync`,
/// so the client can be shared cheaply without `Arc<Mutex<>>`.
#[derive(Clone)]
pub struct MemgraphClient {
    graph: bolt_driver::Graph,
}

impl MemgraphClient {
    /// Establish a Bolt connection to Memgraph.
    ///
    /// # Arguments
    /// * `uri` — Bolt URI, e.g. `"bolt://localhost:7687"`
    /// * `user` — Memgraph username
    /// * `password` — Memgraph password
    ///
    /// # Compatibility note
    ///
    /// Memgraph does not support the `db` Bolt field. We pass `db("")` which
    /// causes the Bolt driver to omit the field from RUN/BEGIN messages
    /// (the driver skips it when the value is empty).
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self, DtError> {
        let config = bolt_driver::ConfigBuilder::default()
            .uri(uri)
            .user(user)
            .password(password)
            .db("") // empty → driver skips the db field (Memgraph-compatible)
            .build()
            .map_err(|e| DtError::Repository(format!("Memgraph config build: {}", e)))?;

        let graph = bolt_driver::Graph::connect(config)
            .await
            .map_err(|e| DtError::Repository(format!("Memgraph connect: {}", e)))?;

        Ok(Self { graph })
    }
}

// ---------------------------------------------------------------------------
// GraphRepository impl
// ---------------------------------------------------------------------------

#[async_trait]
impl GraphRepository for MemgraphClient {
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
            .map_err(|e| DtError::Repository(format!("Memgraph read: {}", e)))?;

        let mut rows = Vec::new();
        while let Ok(Some(row)) = result.next().await {
            // The driver's Row::to uses its custom serde, but it follows
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
        let mut result = self
            .graph
            .execute(q)
            .await
            .map_err(|e| DtError::Repository(format!("Memgraph write: {}", e)))?;
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
        match self.graph.run(bolt_driver::query("RETURN 1")).await {
            Ok(_) => Ok(HealthStatus::Healthy),
            Err(e) => Ok(HealthStatus::Unhealthy(format!(
                "Memgraph unreachable: {}",
                e
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter conversion helpers
// ---------------------------------------------------------------------------

/// Build a Bolt Query from a raw Cypher string and serde_json params.
///
/// Uses the driver's `json` feature which provides `TryFrom<serde_json::Value>
/// for BoltType`, so parameters are converted cleanly.
fn build_query(query_str: &str, params: &HashMap<String, serde_json::Value>) -> bolt_driver::Query {
    let mut q = bolt_driver::query(query_str);
    for (key, val) in params {
        let bolt_val = json_to_bolt(val.clone());
        q = q.param(key.as_str(), bolt_val);
    }
    q
}

/// Convert a `serde_json::Value` to `BoltType`, with proper
/// handling of arrays (Lists) and objects (Maps) for Cypher queries.
fn json_to_bolt(val: serde_json::Value) -> bolt_driver::BoltType {
    if let Ok(bt) = bolt_driver::BoltType::try_from(val.clone()) {
        return bt;
    }
    match val {
        serde_json::Value::Array(arr) => {
            let items: Vec<bolt_driver::BoltType> = arr.into_iter().map(json_to_bolt).collect();
            bolt_driver::BoltType::List(bolt_driver::BoltList { value: items })
        }
        serde_json::Value::Object(obj) => {
            let map: std::collections::HashMap<bolt_driver::BoltString, bolt_driver::BoltType> =
                obj.into_iter()
                    .map(|(k, v)| (bolt_driver::BoltString { value: k }, json_to_bolt(v)))
                    .collect();
            bolt_driver::BoltType::Map(bolt_driver::BoltMap { value: map })
        }
        // Should not reach here, but provide explicit conversions.
        serde_json::Value::String(s) => {
            bolt_driver::BoltType::String(bolt_driver::BoltString { value: s })
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                bolt_driver::BoltType::Integer(bolt_driver::BoltInteger { value: i })
            } else {
                bolt_driver::BoltType::Float(bolt_driver::BoltFloat {
                    value: n.as_f64().unwrap_or(0.0),
                })
            }
        }
        serde_json::Value::Bool(b) => {
            bolt_driver::BoltType::Boolean(bolt_driver::BoltBoolean { value: b })
        }
        serde_json::Value::Null => bolt_driver::BoltType::Null(bolt_driver::BoltNull),
    }
}

// ---------------------------------------------------------------------------
// Noop implementation for compile-time validation & testing
// ---------------------------------------------------------------------------

/// No-op graph repository — returns default/empty values for all queries.
/// Enables compile-check of the full stack before real Memgraph integration.
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
        let result = rt
            .block_on(repo.read_query("RETURN 1", HashMap::new()))
            .unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }

    #[test]
    fn noop_write_returns_null() {
        let repo = NoopGraphRepo;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(repo.write_query("CREATE (n)", HashMap::new()))
            .unwrap();
        assert_eq!(result, serde_json::Value::Null);
    }
}
