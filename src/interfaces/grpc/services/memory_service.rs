//! DtCore 服务的 Memory（RecordEvent）gRPC 处理器。
//!
//! 委托 [`DefaultMemoryService`] 在知识图谱中创建 Day → Session → Event 链。

use crate::application::hooks::HookEngine;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::service::{DefaultMemoryService, MemoryService};
use crate::domain::traits::GraphRepository;
use crate::proto::dt::common;
use crate::proto::dt::core::*;
use std::sync::Arc;
use tonic::Status;

/// `RecordEvent` RPC 的处理器——将事件持久化到时间维度。
pub async fn handle_record_event(
    req: EventRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    hook_engine: Option<Arc<HookEngine>>,
) -> Result<common::Empty, Status> {
    let graph = graph.ok_or_else(|| Status::unavailable("图后端不可用"))?;

    let project = if req.project.is_empty() {
        "unknown"
    } else {
        &req.project
    };

    let parsed_type = parse_event_type(&req.r#type).ok_or_else(|| {
        Status::invalid_argument(format!(
            "未知的事件类型: {}. \
             支持的类型: Modification、Deployment、ConfigChange、BugFix、Decision、Conversation",
            req.r#type
        ))
    })?;

    let session_id = format!("{}-grpc", chrono::Utc::now().format("%Y-%m-%d"));

    let event = MemoryEvent {
        event_type: parsed_type,
        entity_id: req.entity_id.clone(),
        entity_type: if req.entity_type.is_empty() {
            "Unknown".to_string()
        } else {
            req.entity_type.clone()
        },
        project: project.to_string(),
        details: req.details.clone(),
        session_id,
        timestamp: chrono::Utc::now(),
    };

    let memory_svc = DefaultMemoryService::new(graph, hook_engine);
    memory_svc
        .record_event(&event)
        .await
        .map_err(|e| Status::internal(format!("record_event 失败: {e}")))?;

    Ok(common::Empty {})
}

/// 从字符串解析 EventType（不区分大小写）。
///
/// 与 `main.rs::parse_event_type` 中的逻辑一致。
fn parse_event_type(s: &str) -> Option<EventType> {
    match s.to_lowercase().as_str() {
        "modification" => Some(EventType::Modification),
        "deployment" => Some(EventType::Deployment),
        "configchange" => Some(EventType::ConfigChange),
        "bugfix" => Some(EventType::BugFix),
        "decision" => Some(EventType::Decision),
        "conversation" => Some(EventType::Conversation),
        _ => None,
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

    struct MinimalGraphRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for MinimalGraphRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn record_event_requires_graph() {
        let req = EventRequest {
            r#type: "Deployment".into(),
            entity_id: "test-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: "job: test; branch: main".into(),
        };
        let result = handle_record_event(req, None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn record_event_invalid_type_errors() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(MinimalGraphRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = EventRequest {
            r#type: "bogus".into(),
            entity_id: "test-1".into(),
            entity_type: "Bogus".into(),
            project: "test".into(),
            details: "".into(),
        };
        let result = handle_record_event(req, Some(graph), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn record_event_deployment_succeeds() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(MinimalGraphRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = EventRequest {
            r#type: "Deployment".into(),
            entity_id: "my-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: "job: my-job; env: test; branch: main".into(),
        };
        let result = handle_record_event(req, Some(graph), None).await;
        assert!(result.is_ok());
        // 无 hook_engine 时，事件会被静默丢弃（记录为警告）。
        // 现在由 hook 系统处理所有事件类型；旧处理器已移除。
        assert_eq!(write.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn parse_event_type_mapping() {
        assert_eq!(parse_event_type("Deployment"), Some(EventType::Deployment));
        assert_eq!(parse_event_type("DEPLOYMENT"), Some(EventType::Deployment));
        assert_eq!(parse_event_type("deployment"), Some(EventType::Deployment));
        assert_eq!(
            parse_event_type("Conversation"),
            Some(EventType::Conversation)
        );
        assert_eq!(parse_event_type("unknown"), None);
    }
}
