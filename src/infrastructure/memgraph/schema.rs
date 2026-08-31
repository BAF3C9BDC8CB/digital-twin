//! V2 Schema 初始化——约束、索引与数据生命周期。
//!
//! Memgraph 兼容版本。Memgraph 支持 Cypher 的 `IS UNIQUE` 约束与
//! 常规 b-tree 索引，但**不支持**全文索引。
//! 所有与全文索引相关的代码都已被排除。
//!
//! 提供：
//! - `init_schema()` —— 创建所有唯一性约束与 b-tree 索引。
//! - `clean_all()` —— 清空所有节点与关系（用于开发/测试）。
//!
//! # 数据保留策略（在此记录，由 `dt cleanup` 执行）
//!
//! | Data | TTL | Action when exceeded |
//! |------|-----|---------------------|
//! | Memory.Event (Modification, Deployment, ConfigChange, BugFix, Decision, PodEvent) | 365 days | Archive to `/var/lib/dt/archive/` |
//! | Reasoning (unverified Observation, Analysis, Decision) | Session end | `SET _stale_at = timestamp()`; `dt cleanup` deletes after 30 days |
//! | SQLite snapshots old rows | Latest only | `dt build` auto-deletes `WHERE updated_at < latest_per_file` |
//! | Qdrant orphan points | Follows Memgraph | Entity deleted → corresponding point cleaned by `dt kg-sync` |

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 报告类型
// ---------------------------------------------------------------------------

/// Schema 初始化摘要。
#[derive(Debug, Clone)]
pub struct SchemaInitReport {
    /// 创建（或已存在）的唯一性约束数量。
    pub constraints_created: usize,
    /// 创建（或已存在）的索引数量。
    pub indexes_created: usize,
    /// 整个初始化过程的墙钟耗时。
    pub elapsed_ms: u64,
}

/// 数据清理摘要。
#[derive(Debug, Clone)]
pub struct CleanReport {
    /// 删除的节点数量。
    pub nodes_deleted: usize,
    /// 删除的关系数量。
    pub relationships_deleted: usize,
    /// 移除的 Qdrant 集合数量（仅 Memgraph 时为 0）。
    pub qdrant_collections_removed: usize,
    /// 是否清空了 SQLite 快照（仅 Memgraph 时为 false）。
    pub snapshots_cleared: bool,
    /// 删除的过期 Reasoning 节点数量（Observation/Analysis/Decision）。
    pub reasoning_stale_deleted: usize,
    /// 归档的 Memory World 事件数量（Modification、Deployment 等）。
    pub memory_archived: usize,
    /// 删除的孤立 SQLite 快照行数量。
    pub snapshots_orphaned: usize,
    /// 墙钟耗时。
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// 约束定义（Cypher IF NOT EXISTS——幂等）
// ---------------------------------------------------------------------------

/// 所有 V2 实体唯一性约束。
///
/// 每个实体类型都有一个唯一 ID 约束。复合约束使用
/// `(propA, propB) IS UNIQUE` 记法（Memgraph 支持）。
const CONSTRAINT_STATEMENTS: &[&str] = &[
    // ── 现实世界：代码实体 ──
    "CREATE CONSTRAINT method_id_unique IF NOT EXISTS FOR (n:Method) REQUIRE n.method_id IS UNIQUE",
    "CREATE CONSTRAINT class_id_unique IF NOT EXISTS FOR (n:Class) REQUIRE n.class_id IS UNIQUE",
    "CREATE CONSTRAINT module_id_unique IF NOT EXISTS FOR (n:Module) REQUIRE n.module_id IS UNIQUE",
    // ── 现实世界：基础设施 ──
    "CREATE CONSTRAINT server_id_unique IF NOT EXISTS FOR (n:Server) REQUIRE n.server_id IS UNIQUE",
    "CREATE CONSTRAINT database_id_unique IF NOT EXISTS FOR (n:Database) REQUIRE n.database_id IS UNIQUE",
    "CREATE CONSTRAINT table_name_db_unique IF NOT EXISTS FOR (n:Table) REQUIRE (n.name, n.db) IS UNIQUE",
    // ── 现实世界：配置 ──
    "CREATE CONSTRAINT nacos_config_id_unique IF NOT EXISTS FOR (n:NacosConfig) REQUIRE n.config_id IS UNIQUE",
    "CREATE CONSTRAINT config_key_name_ns_unique IF NOT EXISTS FOR (n:ConfigKey) REQUIRE (n.name, n.namespace) IS UNIQUE",
    // ── Jenkins ──
    "CREATE CONSTRAINT jenkins_view_id_unique IF NOT EXISTS FOR (n:JenkinsView) REQUIRE n.view_id IS UNIQUE",
    "CREATE CONSTRAINT jenkins_job_id_unique IF NOT EXISTS FOR (n:JenkinsJob) REQUIRE n.job_id IS UNIQUE",
    "CREATE CONSTRAINT jenkins_build_id_unique IF NOT EXISTS FOR (n:JenkinsBuild) REQUIRE n.build_id IS UNIQUE",
    // ── 现实世界：API ──
    "CREATE CONSTRAINT endpoint_id_unique IF NOT EXISTS FOR (n:Endpoint) REQUIRE n.endpoint_id IS UNIQUE",
    // ── 现实世界：文档 ──
    "CREATE CONSTRAINT doc_id_unique IF NOT EXISTS FOR (n:Document) REQUIRE n.doc_id IS UNIQUE",
    // ── 现实世界：Service / K8s ──
    "CREATE CONSTRAINT service_id_unique IF NOT EXISTS FOR (n:Service) REQUIRE n.service_id IS UNIQUE",
    "CREATE CONSTRAINT service_instance_id_unique IF NOT EXISTS FOR (n:ServiceInstance) REQUIRE n.instance_id IS UNIQUE",
    "CREATE CONSTRAINT k8s_deployment_name_ns_unique IF NOT EXISTS FOR (n:K8sDeployment) REQUIRE (n.name, n.namespace) IS UNIQUE",
    "CREATE CONSTRAINT k8s_service_name_ns_unique IF NOT EXISTS FOR (n:K8sService) REQUIRE (n.name, n.namespace) IS UNIQUE",
    // ── 知识世界 ──
    "CREATE CONSTRAINT knowledge_id_unique IF NOT EXISTS FOR (n:Knowledge) REQUIRE n.knowledge_id IS UNIQUE",
    "CREATE CONSTRAINT knowledge_version_id_unique IF NOT EXISTS FOR (n:KnowledgeVersion) REQUIRE n.version_id IS UNIQUE",
    "CREATE CONSTRAINT playbook_id_unique IF NOT EXISTS FOR (n:Playbook) REQUIRE n.playbook_id IS UNIQUE",
    "CREATE CONSTRAINT experience_id_unique IF NOT EXISTS FOR (n:Experience) REQUIRE n.experience_id IS UNIQUE",
    "CREATE CONSTRAINT concept_id_unique IF NOT EXISTS FOR (n:Concept) REQUIRE n.concept_id IS UNIQUE",
    "CREATE CONSTRAINT domain_id_unique IF NOT EXISTS FOR (n:Domain) REQUIRE n.domain_id IS UNIQUE",
    // ── 记忆世界 ──
    "CREATE CONSTRAINT day_id_unique IF NOT EXISTS FOR (n:Day) REQUIRE n.day_id IS UNIQUE",
    "CREATE CONSTRAINT session_id_unique IF NOT EXISTS FOR (n:Session) REQUIRE n.session_id IS UNIQUE",
    // ── 数字主线 ──
    "CREATE CONSTRAINT thread_id_unique IF NOT EXISTS FOR (n:Thread) REQUIRE n.thread_id IS UNIQUE",
    "CREATE CONSTRAINT requirement_id_unique IF NOT EXISTS FOR (n:Requirement) REQUIRE n.requirement_id IS UNIQUE",
    // ── 推理世界 ──
    "CREATE CONSTRAINT observation_id_unique IF NOT EXISTS FOR (n:Observation) REQUIRE n.observation_id IS UNIQUE",
    "CREATE CONSTRAINT analysis_id_unique IF NOT EXISTS FOR (n:Analysis) REQUIRE n.analysis_id IS UNIQUE",
];

/// 用于查询性能的常规 b-tree 索引。
///
/// Memgraph 通过 `CREATE INDEX ON :Label(prop)` 支持标签-属性索引。
/// 下面使用的展开式 `IF NOT EXISTS` 语法与较短的
/// `CREATE INDEX ON :Label(prop)` 形式均可使用。
const INDEX_STATEMENTS: &[&str] = &[
    // 加速调用图重建：MATCH (callee:Method {project: $project, name: called_name})
    "CREATE INDEX method_project_name IF NOT EXISTS FOR (n:Method) ON (n.project, n.name)",
    // 2026-08-31 补：UNWIND MERGE 用 method_id/class_id/module_id 匹配，无索引时
    // 每次 MERGE 全表扫描（Memgraph 唯一约束不隐式建索引）——yijianbao 等大项目
    // 8 万方法 × 500 批 → 事务挂起数分钟，表现为"死锁"（全线程 park）。
    "CREATE INDEX idx_method_method_id IF NOT EXISTS FOR (n:Method) ON (n.method_id)",
    "CREATE INDEX idx_class_class_id IF NOT EXISTS FOR (n:Class) ON (n.class_id)",
    "CREATE INDEX idx_module_module_id IF NOT EXISTS FOR (n:Module) ON (n.module_id)",
    // MERGE (p:Project {name}) 高频——同样补索引
    "CREATE INDEX idx_project_name IF NOT EXISTS FOR (n:Project) ON (n.name)",
    // 加速 retriever.rs 中的 CONTAINS 查询
    "CREATE INDEX idx_concept_name IF NOT EXISTS FOR (n:Concept) ON (n.name)",
    "CREATE INDEX idx_concept_title IF NOT EXISTS FOR (n:Concept) ON (n.title)",
    "CREATE INDEX idx_playbook_name IF NOT EXISTS FOR (n:Playbook) ON (n.name)",
    "CREATE INDEX idx_playbook_title IF NOT EXISTS FOR (n:Playbook) ON (n.title)",
    "CREATE INDEX idx_knowledge_name IF NOT EXISTS FOR (n:Knowledge) ON (n.name)",
    "CREATE INDEX idx_knowledge_title IF NOT EXISTS FOR (n:Knowledge) ON (n.title)",
    "CREATE INDEX idx_experience_name IF NOT EXISTS FOR (n:Experience) ON (n.name)",
    "CREATE INDEX idx_experience_title IF NOT EXISTS FOR (n:Experience) ON (n.title)",
    "CREATE INDEX idx_domain_name IF NOT EXISTS FOR (n:Domain) ON (n.name)",
    "CREATE INDEX idx_thread_title IF NOT EXISTS FOR (n:Thread) ON (n.title)",
    "CREATE INDEX idx_observation_obs_id IF NOT EXISTS FOR (n:Observation) ON (n.observation_id)",
    "CREATE INDEX idx_analysis_analysis_id IF NOT EXISTS FOR (n:Analysis) ON (n.analysis_id)",
];

// ---------------------------------------------------------------------------
// Schema 初始化
// ---------------------------------------------------------------------------

/// 在 Memgraph 实例上初始化完整的 V2 schema。
///
/// 创建所有唯一性约束与 b-tree 索引。所有语句都使用
/// `IF NOT EXISTS`，因此该函数可安全地重复调用（幂等）。
///
/// # 参数
/// * `graph` —— 任意 [`GraphRepository`] 实现。
///
/// # 返回
/// [`SchemaInitReport`] 摘要，说明创建了什么以及耗时多久。
pub async fn init_schema(graph: &dyn GraphRepository) -> Result<SchemaInitReport, DtError> {
    let start = std::time::Instant::now();
    let empty_params = HashMap::new();

    let mut constraints_created = 0usize;
    let mut indexes_created = 0usize;

    // --- 唯一性约束 ---
    for stmt in CONSTRAINT_STATEMENTS {
        graph.write_query(stmt, empty_params.clone()).await?;
        constraints_created += 1;
    }

    // --- 常规 b-tree 索引 ---
    for stmt in INDEX_STATEMENTS {
        graph.write_query(stmt, empty_params.clone()).await?;
        indexes_created += 1;
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;

    Ok(SchemaInitReport {
        constraints_created,
        indexes_created,
        elapsed_ms,
    })
}

// ---------------------------------------------------------------------------
// 数据清理
// ---------------------------------------------------------------------------

/// 清空 Memgraph 实例中的**所有**节点与关系。
///
/// # 安全性
/// 这是破坏性操作。请谨慎使用——通常仅在开发/测试环境中。
/// 生产环境应通过 `dt cleanup --confirm` 执行。
///
/// 删除前会先捕获当前节点与关系数量，
/// 以便调用方报告被移除的内容。
pub async fn clean_all(graph: &dyn GraphRepository) -> Result<CleanReport, DtError> {
    let start = std::time::Instant::now();
    let empty_params = HashMap::new();

    // 删除前统计节点数量
    let nodes_deleted = count_nodes(graph).await.unwrap_or(0);

    // 删除前统计关系数量
    let relationships_deleted = count_relationships(graph).await.unwrap_or(0);

    // 删除所有内容
    graph
        .write_query("MATCH (n) DETACH DELETE n", empty_params)
        .await?;

    Ok(CleanReport {
        nodes_deleted,
        relationships_deleted,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted: 0,
        memory_archived: 0,
        snapshots_orphaned: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 统计图中的所有节点。
async fn count_nodes(graph: &dyn GraphRepository) -> Result<usize, DtError> {
    let result = graph
        .read_query("MATCH (n) RETURN count(n) AS total", HashMap::new())
        .await?;
    Ok(extract_count(&result, "total"))
}

/// 统计图中的所有关系。
async fn count_relationships(graph: &dyn GraphRepository) -> Result<usize, DtError> {
    let result = graph
        .read_query("MATCH ()-[r]->() RETURN count(r) AS total", HashMap::new())
        .await?;
    Ok(extract_count(&result, "total"))
}

/// 从 JSON 结果中提取 `usize`——同时处理数组行与标量。
fn extract_count(value: &serde_json::Value, field: &str) -> usize {
    value
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;

    /// 记录查询以供断言的 Mock 仓库。
    struct MockGraphRepo {
        write_calls: std::sync::Mutex<Vec<String>>,
        read_calls: std::sync::Mutex<Vec<String>>,
        should_fail_after: Option<usize>,
    }

    impl MockGraphRepo {
        fn new() -> Self {
            Self {
                write_calls: std::sync::Mutex::new(Vec::new()),
                read_calls: std::sync::Mutex::new(Vec::new()),
                should_fail_after: None,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for MockGraphRepo {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_calls.lock().unwrap().push(query.to_string());
            // 默认返回一个 COUNT 结果为 0
            Ok(serde_json::json!([{"total": 0}]))
        }

        async fn write_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            let mut calls = self.write_calls.lock().unwrap();
            calls.push(query.to_string());

            if let Some(limit) = self.should_fail_after {
                if calls.len() > limit {
                    return Err(DtError::Repository("mock 失败".into()));
                }
            }
            Ok(serde_json::json!({"ok": true}))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn init_schema_creates_all_constraints() {
        let mock = MockGraphRepo::new();
        let report = init_schema(&mock).await.expect("应当成功");

        // 29 个约束 + 17 个常规索引（2026-08-31 补 method_id/class_id/module_id/project name 索引）
        assert_eq!(report.constraints_created, 29);
        assert_eq!(report.indexes_created, 17);
        assert!(report.elapsed_ms < 5_000);

        let write_calls = mock.write_calls.lock().unwrap();
        assert_eq!(write_calls.len(), 46); // 29 个约束 + 17 个索引
        assert!(write_calls[0].contains("method_id_unique"));
        assert!(write_calls[28].contains("analysis_id_unique"));
    }

    #[tokio::test]
    async fn init_schema_is_idempotent_via_if_not_exists() {
        let mock = MockGraphRepo::new();
        // 第一次调用
        init_schema(&mock).await.unwrap();
        // 第二次调用——所有语句都带 IF NOT EXISTS，因此应当成功
        let report2 = init_schema(&mock).await.unwrap();
        assert_eq!(report2.constraints_created, 29);
        assert_eq!(report2.indexes_created, 17);
    }

    #[tokio::test]
    async fn clean_all_deletes_everything() {
        let mock = MockGraphRepo::new();
        let report = clean_all(&mock).await.expect("应当成功");

        // 节点/关系数量为 0（mock 返回 0）
        assert_eq!(report.nodes_deleted, 0);
        assert_eq!(report.relationships_deleted, 0);
        assert_eq!(report.qdrant_collections_removed, 0);
        assert!(!report.snapshots_cleared);
        assert!(report.elapsed_ms < 5_000);

        // 验证 DETACH DELETE 被调用
        let write_calls = mock.write_calls.lock().unwrap();
        assert!(write_calls.iter().any(|s| s.contains("DETACH DELETE n")));
    }

    #[test]
    fn extract_count_handles_empty() {
        assert_eq!(extract_count(&serde_json::json!([]), "total"), 0);
        assert_eq!(extract_count(&serde_json::Value::Null, "total"), 0);
    }

    #[test]
    fn extract_count_handles_array_of_rows() {
        assert_eq!(
            extract_count(&serde_json::json!([{"total": 42}]), "total"),
            42
        );
    }

    #[test]
    fn schema_init_report_debug() {
        let report = SchemaInitReport {
            constraints_created: 26,
            indexes_created: 1,
            elapsed_ms: 123,
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("26"));
        assert!(debug.contains("1"));
        assert!(debug.contains("123"));
    }

    #[test]
    fn clean_report_debug() {
        let report = CleanReport {
            nodes_deleted: 100,
            relationships_deleted: 200,
            qdrant_collections_removed: 3,
            snapshots_cleared: true,
            reasoning_stale_deleted: 5,
            memory_archived: 50,
            snapshots_orphaned: 10,
            elapsed_ms: 456,
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("100"));
        assert!(debug.contains("200"));
        assert!(debug.contains("3"));
        assert!(debug.contains("true"));
        assert!(debug.contains("5"));
        assert!(debug.contains("50"));
        assert!(debug.contains("10"));
        assert!(debug.contains("456"));
    }
}
