//! Memgraph Bolt 客户端——到 Memgraph 知识图谱的异步连接。
//!
//! 通过 [`bolt_driver::Graph`] 使用 Bolt 协议驱动（`bolt_driver`，包装
//! `neo4rs` crate）访问 Memgraph。
//! `MemgraphClient` 实现了 [`GraphRepository`]，用于真实的 Cypher 查询。
//! `NoopGraphRepo` 保留作为编译期/测试占位实现。
//!
//! ## Memgraph 兼容性
//!
//! Memgraph 不支持 Bolt RUN/BEGIN 消息中的多数据库 `db` 字段。
//! 我们将数据库名设为空字符串 `""`，这会告诉 Bolt 驱动完全省略 `db`
//! 字段（见驱动源码中的 `Run::new` 与 `BoltRequest::begin`——它们在
//! 发出该字段前会检查 `!db.is_empty()`）。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use std::collections::HashMap;

/// 真实的 Memgraph Bolt 客户端。
///
/// 包装一个 [`bolt_driver::Graph`] 连接池。`Graph` 是 `Clone + Send + Sync`，
/// 因此客户端无需 `Arc<Mutex<>>` 即可廉价共享。
#[derive(Clone)]
pub struct MemgraphClient {
    graph: bolt_driver::Graph,
}

impl MemgraphClient {
    /// 建立到 Memgraph 的 Bolt 连接。
    ///
    /// # 参数
    /// * `uri` —— Bolt URI，如 `"bolt://localhost:7687"`
    /// * `user` —— Memgraph 用户名
    /// * `password` —— Memgraph 密码
    ///
    /// # 兼容性说明
    ///
    /// Memgraph 不支持 `db` Bolt 字段。我们传入 `db("")`，
    /// 使 Bolt 驱动在 RUN/BEGIN 消息中省略该字段
    /// （驱动在值为空时跳过它）。
    pub async fn connect(uri: &str, user: &str, password: &str) -> Result<Self, DtError> {
        let config = bolt_driver::ConfigBuilder::default()
            .uri(uri)
            .user(user)
            .password(password)
            .db("") // 空值→驱动省略 db 字段（兼容 Memgraph）
            .build()
            .map_err(|e| DtError::Repository(format!("Memgraph 配置构建: {}", e)))?;

        let graph = bolt_driver::Graph::connect(config)
            .await
            .map_err(|e| DtError::Repository(format!("Memgraph 连接: {}", e)))?;

        Ok(Self { graph })
    }
}

// ---------------------------------------------------------------------------
// GraphRepository 实现
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
            .map_err(|e| DtError::Repository(format!("Memgraph 读取: {}", e)))?;

        let mut rows = Vec::new();
        while let Ok(Some(row)) = result.next().await {
            // 驱动的 Row::to 使用其自定义 serde，但它遵循标准 serde 协议，
            // 因此我们可以直接反序列化到 serde_json::Value。
            match row.to::<serde_json::Value>() {
                Ok(val) => rows.push(val),
                Err(_) => {
                    // 回退：如果 row.to 失败，跳过该行。
                    // 这发生在无法表示为纯 JSON 值的节点/关系类型上。
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
            .map_err(|e| DtError::Repository(format!("Memgraph 写入: {}", e)))?;
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
                "Memgraph 不可达: {}",
                e
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// 参数转换辅助函数
// ---------------------------------------------------------------------------

/// 从原始 Cypher 字符串与 serde_json 参数构建 Bolt Query。
///
/// 使用驱动的 `json` 特性，它提供 `TryFrom<serde_json::Value>
/// for BoltType`，因此参数可以被干净地转换。
fn build_query(query_str: &str, params: &HashMap<String, serde_json::Value>) -> bolt_driver::Query {
    let mut q = bolt_driver::query(query_str);
    for (key, val) in params {
        let bolt_val = json_to_bolt(val.clone());
        q = q.param(key.as_str(), bolt_val);
    }
    q
}

/// 将 `serde_json::Value` 转换为 `BoltType`，正确处理数组（Lists）与
/// 对象（Maps），以便用于 Cypher 查询。
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
        // 不应到达这里，但提供显式转换。
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
// 用于编译期校验与测试的 Noop 实现
// ---------------------------------------------------------------------------

/// No-op 图仓库——所有查询都返回默认/空值。
/// 在真正接入 Memgraph 之前，可用于对完整技术栈做编译期检查。
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
// 测试
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
