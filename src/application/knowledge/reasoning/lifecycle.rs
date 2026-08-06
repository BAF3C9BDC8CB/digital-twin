// 推理图节点的生命周期管理。
//
// 两级清理策略：
//
// 阶段 1 —— 标记过期（弃用）：
// 会话结束时，属于该会话的所有推理节点被标记 _stale_at = timestamp()。
// 过期节点在 Context Builder 查询中被排除。
//
// 阶段 2 —— 删除（垃圾回收）：
// 夜间 dt 清理任务删除所有 _stale_at 距今超过 30 天的推理节点。
//
// 受影响的标签：:Observation、:Analysis、:Decision。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;
use std::sync::Arc;

/// 管理推理图节点的生命周期。
///
/// 提供将推理节点标记为过期以及执行垃圾回收清理的方法。
#[async_trait]
pub trait LifecycleManager: Send + Sync {
    /// 将给定会话的所有推理节点标记为过期。
    ///
    /// 为每个 session_id 匹配的 :Observation、
    /// :Analysis 与 :Decision 节点设置 _stale_at = timestamp()。
    ///
    /// 返回被标记的节点数。
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError>;

    /// 删除早于 retention_days 的过期推理节点。
    ///
    /// 移除所有 _stale_at 距今超过 retention_days 的
    /// :Observation、:Analysis 与 :Decision 节点。
    ///
    /// 返回被删除的节点数。
    async fn cleanup_stale(&self, retention_days: u32) -> Result<usize, DtError>;

    /// 确认一个推理决策（设置 verified = true）。
    ///
    /// 将 tentative 的 :Decision 转为适合归档 / Memory 世界
    /// 集成的已确认决策。
    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// DefaultLifecycleManager — 规范实现
// ---------------------------------------------------------------------------

/// 由 GraphRepository 支撑的 LifecycleManager 规范实现。
pub struct DefaultLifecycleManager {
    graph: Arc<dyn GraphRepository>,
}

impl DefaultLifecycleManager {
    /// 创建由给定图仓库支撑的 DefaultLifecycleManager。
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl LifecycleManager for DefaultLifecycleManager {
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError> {
        let now = chrono::Utc::now().to_rfc3339();

        let cypher = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n.session_id = $session_id
              AND n._stale_at IS NULL
            SET n._stale_at = $now
            RETURN count(n) AS marked
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );
        params.insert("now".into(), serde_json::Value::String(now));

        let result = self.graph.write_query(cypher, params).await?;
        let count = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("marked"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        Ok(count)
    }

    async fn cleanup_stale(&self, retention_days: u32) -> Result<usize, DtError> {
        let threshold = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
        let threshold_str = threshold.to_rfc3339();

        let cypher = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n._stale_at IS NOT NULL
              AND n._stale_at < $threshold
            DETACH DELETE n
            RETURN count(n) AS deleted
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert("threshold".into(), serde_json::Value::String(threshold_str));

        let result = self.graph.write_query(cypher, params).await?;
        let count = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("deleted"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        Ok(count)
    }

    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError> {
        let cypher = r#"
            MATCH (d:Decision {decision_id: $decision_id})
            SET d.verified = true
            RETURN d.decision_id
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "decision_id".into(),
            serde_json::Value::String(decision_id.to_string()),
        );

        let result = self.graph.write_query(cypher, params).await?;

        let affected = result.as_array().map(|rows| rows.len()).unwrap_or(0);

        if affected == 0 {
            return Err(DtError::NotFound(format!("未找到决策：{}", decision_id)));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepo {
        fn new(write: Arc<AtomicUsize>, read: Arc<AtomicUsize>) -> Self {
            Self {
                write_count: write,
                read_count: read,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"marked": 3u64}]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    struct ConfirmRepo {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GraphRepository for ConfirmRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"decision_id": "dec://test/001"}]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn LifecycleManager) {}
    }

    #[tokio::test]
    async fn mark_stale_writes_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let mgr = DefaultLifecycleManager::new(repo);

        let count = mgr
            .mark_stale("2026-07-09-001")
            .await
            .expect("mark_stale 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn cleanup_stale_writes_query() {
        // 使用返回 deleted 计数的仓库（字段名与 mark_stale 不同）。
        struct CleanupRepo {
            counter: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl GraphRepository for CleanupRepo {
            async fn read_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::Value::Null)
            }
            async fn write_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                self.counter.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!([{"deleted": 5u64}]))
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CleanupRepo {
            counter: counter.clone(),
        });
        let mgr = DefaultLifecycleManager::new(repo);

        let count = mgr.cleanup_stale(30).await.expect("cleanup_stale 应成功");
        assert!(counter.load(Ordering::SeqCst) >= 1);
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn confirm_decision_sets_verified() {
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(ConfirmRepo {
            counter: counter.clone(),
        });
        let mgr = DefaultLifecycleManager::new(repo);

        mgr.confirm_decision("dec://test/001")
            .await
            .expect("confirm_decision 应成功");
        assert!(counter.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn mark_stale_returns_zero_for_no_matches() {
        // 使用返回 0 个标记的仓库
        struct EmptyRepo;
        #[async_trait]
        impl GraphRepository for EmptyRepo {
            async fn read_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::Value::Null)
            }
            async fn write_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::json!([]))
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        let mgr = DefaultLifecycleManager::new(Arc::new(EmptyRepo));
        let count = mgr
            .mark_stale("no-such-session")
            .await
            .expect("mark_stale 应成功");
        assert_eq!(count, 0);
    }
}
