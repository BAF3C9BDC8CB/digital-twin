//! dt sense —— 环境感知（只读）：定位 → 状态 → 简报/发现报告。

pub mod locate;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};

/// 三态状态机（S-D2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SenseStatus {
    Indexed,
    RegisteredNotIndexed,
    Unregistered,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectRef {
    pub name: String,
    pub path: String,
    pub registered: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub methods: u64,
    pub classes: u64,
    pub vectors: u64,
    pub last_build: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DirStat {
    pub dir: String,
    pub methods: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LangStat {
    pub ext: String,
    pub pct: u8,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyEntity {
    pub name: String,
    pub kind: String,
    pub source: String,
    pub in_degree: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Candidate {
    pub path: String,
    pub suggested_name: String,
    pub build_cmd: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SenseReport {
    pub status: SenseStatus,
    pub project: Option<ProjectRef>,
    pub stats: Option<Stats>,
    pub dirs: Vec<DirStat>,
    pub languages: Vec<LangStat>,
    pub key_entities: Vec<KeyEntity>,
    pub candidates: Vec<Candidate>,
    pub degraded: Vec<String>,
}

pub struct SenseService {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    /// ~/.config/digital-twin/ignored_dirs.yaml（discover 用；由 CLI 解析 home 传入）
    pub ignored_dirs_file: Option<PathBuf>,
}

impl SenseService {
    /// 永不失败：后端缺失走 degraded。input 存在性由调用方（CLI）校验。
    pub async fn sense(&self, input: &Path, projects: &[(String, PathBuf)]) -> SenseReport {
        let mut degraded: Vec<String> = Vec::new();

        let Some((name, root)) = locate::match_project(input, projects) else {
            return SenseReport {
                status: SenseStatus::Unregistered,
                project: None,
                stats: None,
                dirs: vec![],
                languages: vec![],
                key_entities: vec![],
                candidates: vec![], // T6 discover 填充
                degraded,
            };
        };

        // --- stats: Memgraph 方法/类计数 ---
        let mut methods = 0u64;
        let mut classes = 0u64;
        if let Some(g) = &self.graph {
            let mut params = std::collections::HashMap::new();
            params.insert("p".to_string(), serde_json::Value::String(name.to_string()));
            if let Ok(v) = g
                .read_query(
                    "MATCH (m:Method {project: $p}) RETURN count(m) AS c",
                    params.clone(),
                )
                .await
            {
                methods = v
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|r| r.get("c"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0);
            }
            if let Ok(v) = g
                .read_query("MATCH (c:Class {project: $p}) RETURN count(c) AS c", params)
                .await
            {
                classes = v
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|r| r.get("c"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0);
            }
        } else {
            degraded.push("memgraph".into());
        }

        // --- stats: Qdrant 向量（同时是 T4 目录画像原料） ---
        let mut payloads: Vec<serde_json::Value> = vec![];
        if let Some(v) = &self.vector {
            let filter =
                serde_json::json!({"must": [{"key": "project", "match": {"value": name}}]});
            match v
                .scroll_payloads("code_methods", Some(filter), 100_000)
                .await
            {
                Ok(p) => payloads = p,
                Err(_) => degraded.push("qdrant".into()),
            }
        } else {
            degraded.push("qdrant".into());
        }
        let vectors = payloads.len() as u64;

        // --- stats: SQLite 最近构建 ---
        let mut last_build = None;
        if let Some(s) = &self.snapshot {
            if let Ok(snaps) = s.list_snapshots(name).await {
                last_build = snaps.into_iter().map(|f| f.updated_at).max();
            }
        } else {
            degraded.push("snapshot".into());
        }

        // --- 三态（S-D2 + spec §6 Qdrant 缺失时 Memgraph 兜底） ---
        let status = if vectors > 0 || methods > 0 {
            SenseStatus::Indexed
        } else {
            SenseStatus::RegisteredNotIndexed
        };

        SenseReport {
            status,
            project: Some(ProjectRef {
                name: name.to_string(),
                path: root.display().to_string(),
                registered: true,
            }),
            stats: Some(Stats {
                methods,
                classes,
                vectors,
                last_build,
            }),
            dirs: vec![],
            languages: vec![],
            key_entities: vec![],
            candidates: vec![],
            degraded,
        }
    }
}

#[cfg(test)]
pub(crate) mod stubs {
    use crate::domain::error::DtError;
    use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
    use crate::domain::types::{FileSnapshot, HealthStatus};
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// read_query 响应脚本：按 query 含的关键词匹配返回固定 JSON。
    pub struct StubGraph {
        pub method_count: u64,
        pub class_count: u64,
        pub calls_rows: Vec<serde_json::Value>,
        pub heuristic_rows: Vec<serde_json::Value>,
    }

    #[async_trait]
    impl GraphRepository for StubGraph {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            if query.contains("CALLS") {
                Ok(serde_json::Value::Array(self.calls_rows.clone()))
            } else if query.contains("ENDS WITH") {
                Ok(serde_json::Value::Array(self.heuristic_rows.clone()))
            } else if query.contains("Method") {
                Ok(serde_json::json!([{ "c": self.method_count }]))
            } else if query.contains("Class") {
                Ok(serde_json::json!([{ "c": self.class_count }]))
            } else {
                Ok(serde_json::json!([]))
            }
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub struct StubVector {
        pub payloads: Vec<serde_json::Value>,
    }

    #[async_trait]
    impl VectorRepository for StubVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
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
            _c: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: "stub".into(),
                points_count: 0,
                vector_dim: 1024,
                model_version: "stub".into(),
            })
        }
        async fn delete_collection(&self, _c: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
        async fn scroll_payloads(
            &self,
            _c: &str,
            _f: Option<serde_json::Value>,
            _max: usize,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.payloads.clone())
        }
    }

    pub struct StubSnapshot {
        pub updated_at: Option<String>,
    }

    #[async_trait]
    impl SnapshotRepository for StubSnapshot {
        async fn get_snapshot(
            &self,
            _p: &str,
            _path: &str,
        ) -> Result<Option<FileSnapshot>, DtError> {
            Ok(None)
        }
        async fn save_snapshots(&self, _p: &str, _s: &[FileSnapshot]) -> Result<(), DtError> {
            Ok(())
        }
        async fn delete_project(&self, _p: &str) -> Result<u64, DtError> {
            Ok(0)
        }
        async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError> {
            Ok(match &self.updated_at {
                Some(ts) => vec![FileSnapshot {
                    file_path: "a.java".into(),
                    project: project.into(),
                    file_sha1: "h".into(),
                    file_mtime: 0.0,
                    method_count: 1,
                    updated_at: ts.clone(),
                }],
                None => vec![],
            })
        }
        async fn mark_llm_analyzed(&self, _p: &str, _f: &str, _h: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn is_llm_analyzed(&self, _p: &str, _f: &str, _h: &str) -> Result<bool, DtError> {
            Ok(false)
        }
        async fn clear_llm_progress(&self, _p: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use stubs::*;

    fn projects() -> Vec<(String, PathBuf)> {
        vec![("dt".into(), PathBuf::from("/data/myProject/digital-twin-v2"))]
    }

    fn svc(vectors: usize, methods: u64) -> SenseService {
        let payloads = (0..vectors)
            .map(|i| {
                serde_json::json!({
                    "file_path": format!("/data/myProject/digital-twin-v2/src/m{}.rs", i),
                    "project": "dt"
                })
            })
            .collect();
        SenseService {
            graph: Some(Arc::new(StubGraph {
                method_count: methods,
                class_count: 3,
                calls_rows: vec![],
                heuristic_rows: vec![],
            })),
            vector: Some(Arc::new(StubVector { payloads })),
            snapshot: Some(Arc::new(StubSnapshot {
                updated_at: Some("2026-08-01T10:00:00".into()),
            })),
            ignored_dirs_file: None,
        }
    }

    #[tokio::test]
    async fn indexed_when_vectors_exist() {
        let r = svc(10, 8)
            .sense(Path::new("/data/myProject/digital-twin-v2"), &projects())
            .await;
        assert_eq!(r.status, SenseStatus::Indexed);
        let s = r.stats.unwrap();
        assert_eq!(s.vectors, 10);
        assert_eq!(s.methods, 8);
        assert_eq!(s.classes, 3);
        assert_eq!(s.last_build.as_deref(), Some("2026-08-01T10:00:00"));
        assert!(r.degraded.is_empty());
    }

    #[tokio::test]
    async fn registered_not_indexed_when_empty() {
        let r = svc(0, 0)
            .sense(Path::new("/data/myProject/digital-twin-v2"), &projects())
            .await;
        assert_eq!(r.status, SenseStatus::RegisteredNotIndexed);
        assert_eq!(r.project.as_ref().unwrap().name, "dt");
    }

    #[tokio::test]
    async fn unregistered_when_no_match() {
        let r = svc(0, 0).sense(Path::new("/home/luis/x"), &projects()).await;
        assert_eq!(r.status, SenseStatus::Unregistered);
        assert!(r.project.is_none());
    }

    #[tokio::test]
    async fn memgraph_fallback_when_vector_down() {
        let mut s = svc(0, 5);
        s.vector = None; // Qdrant 不可用
        let r = s
            .sense(Path::new("/data/myProject/digital-twin-v2"), &projects())
            .await;
        assert_eq!(r.status, SenseStatus::Indexed); // Memgraph 方法数 >0 兜底
        assert!(r.degraded.contains(&"qdrant".to_string()));
    }

    #[tokio::test]
    async fn all_backends_down_still_registered_not_indexed() {
        let s = SenseService {
            graph: None,
            vector: None,
            snapshot: None,
            ignored_dirs_file: None,
        };
        let r = s
            .sense(Path::new("/data/myProject/digital-twin-v2"), &projects())
            .await;
        assert_eq!(r.status, SenseStatus::RegisteredNotIndexed);
        assert_eq!(r.degraded.len(), 3);
    }
}
