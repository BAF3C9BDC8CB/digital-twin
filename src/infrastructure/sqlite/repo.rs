//! 用于变更检测的 SQLite 快照存储。
//!
//! 使用本地 SQLite 数据库实现 `SnapshotRepository`。
//! 存储已索引文件的 SHA1 哈希，以检测构建之间的变化。

use crate::domain::error::DtError;
use crate::domain::traits::SnapshotRepository;
use crate::domain::types::FileSnapshot;
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::sync::Mutex;

/// 基于 SQLite 的快照仓库。
///
/// 使用本地 `lazy.db` 文件跟踪文件哈希，
/// 通过比较当前文件内容与先前索引的快照实现增量构建。
pub struct SqliteRepo {
    conn: Mutex<Connection>,
}

impl SqliteRepo {
    /// 在给定路径打开或创建 SQLite 数据库。
    pub fn open(db_path: &str) -> Result<Self, DtError> {
        let conn = Connection::open(db_path)
            .map_err(|e| DtError::Repository(format!("SQLite 打开失败: {e}")))?;

        // 启用 WAL 模式以获得更好的并发读取
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| DtError::Repository(format!("SQLite pragma 失败: {e}")))?;

        let repo = Self {
            conn: Mutex::new(conn),
        };
        repo.ensure_schema()?;
        Ok(repo)
    }

    /// 在表不存在时创建 snapshots 表。
    fn ensure_schema(&self) -> Result<(), DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_snapshots (
                file_path    TEXT NOT NULL,
                project      TEXT NOT NULL,
                file_sha1    TEXT NOT NULL,
                file_mtime   REAL NOT NULL,
                method_count INTEGER DEFAULT 0,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (file_path, project)
            );
            CREATE TABLE IF NOT EXISTS build_progress (
                file_path     TEXT NOT NULL,
                project       TEXT NOT NULL,
                stage         TEXT NOT NULL DEFAULT 'llm_analysis',
                file_sha1     TEXT NOT NULL DEFAULT '',
                completed_at  TEXT NOT NULL,
                PRIMARY KEY (file_path, project, stage)
            );
            CREATE TABLE IF NOT EXISTS pipeline_progress (
                file_path     TEXT NOT NULL,
                project       TEXT NOT NULL,
                step          TEXT NOT NULL,
                file_hash     TEXT NOT NULL,
                completed_at  TEXT NOT NULL,
                PRIMARY KEY (file_path, project, step)
            );",
        )
        .map_err(|e| DtError::Repository(format!("SQLite schema: {e}")))?;
        // Existing installations predate file_sha1 on build_progress.  Keep
        // them compatible so failed LLM work can be retried incrementally.
        let has_file_sha1: bool = conn
            .prepare("PRAGMA table_info(build_progress)")
            .and_then(|mut stmt| {
                let mut rows = stmt.query([])?;
                let mut found = false;
                while let Some(row) = rows.next()? {
                    let name: String = row.get(1)?;
                    if name == "file_sha1" {
                        found = true;
                        break;
                    }
                }
                Ok(found)
            })
            .map_err(|e| DtError::Repository(format!("SQLite schema inspect: {e}")))?;
        if !has_file_sha1 {
            conn.execute(
                "ALTER TABLE build_progress ADD COLUMN file_sha1 TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|e| DtError::Repository(format!("SQLite schema migrate: {e}")))?;
        }
        Ok(())
    }
}

#[async_trait]
impl SnapshotRepository for SqliteRepo {
    async fn get_snapshot(
        &self,
        project: &str,
        path: &str,
    ) -> Result<Option<FileSnapshot>, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT file_path, project, file_sha1, file_mtime, method_count, updated_at
                 FROM file_snapshots WHERE file_path = ?1 AND project = ?2",
            )
            .map_err(|e| DtError::Repository(e.to_string()))?;

        let result = stmt
            .query_row(params![path, project], |row| {
                Ok(FileSnapshot {
                    file_path: row.get(0)?,
                    project: row.get(1)?,
                    file_sha1: row.get(2)?,
                    file_mtime: row.get(3)?,
                    method_count: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .ok();

        Ok(result)
    }

    async fn save_snapshots(
        &self,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        if snapshots.is_empty() {
            return Ok(());
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DtError::Repository(e.to_string()))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO file_snapshots
                     (file_path, project, file_sha1, file_mtime, method_count, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| DtError::Repository(e.to_string()))?;

            for snap in snapshots {
                stmt.execute(params![
                    snap.file_path,
                    project, // 使用 project 参数保持一致
                    snap.file_sha1,
                    snap.file_mtime,
                    snap.method_count,
                    snap.updated_at,
                ])
                .map_err(|e| DtError::Repository(e.to_string()))?;
            }
        }

        tx.commit()
            .map_err(|e| DtError::Repository(format!("SQLite 提交: {e}")))?;

        Ok(())
    }

    async fn delete_project(&self, project: &str) -> Result<u64, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let count = conn
            .execute(
                "DELETE FROM file_snapshots WHERE project = ?1",
                params![project],
            )
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(count as u64)
    }

    async fn clear_all(&self) -> Result<u64, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let snap_count = conn
            .execute("DELETE FROM file_snapshots", [])
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let prog_count = conn
            .execute("DELETE FROM build_progress", [])
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let pipe_count = conn
            .execute("DELETE FROM pipeline_progress", [])
            .map_err(|e| DtError::Repository(e.to_string()))?;
        // pipeline_tasks 由 TaskStore（独立连接）维护；表可能尚未创建，
        // 因此对“no such table”宽容——其余错误照常上报。
        let task_count = match conn.execute("DELETE FROM pipeline_tasks", []) {
            Ok(n) => n,
            Err(e) if e.to_string().contains("no such table") => 0,
            Err(e) => return Err(DtError::Repository(e.to_string())),
        };
        Ok((snap_count + prog_count + pipe_count + task_count) as u64)
    }

    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT file_path, project, file_sha1, file_mtime, method_count, updated_at
                 FROM file_snapshots WHERE project = ?1",
            )
            .map_err(|e| DtError::Repository(e.to_string()))?;

        let rows = stmt
            .query_map(params![project], |row| {
                Ok(FileSnapshot {
                    file_path: row.get(0)?,
                    project: row.get(1)?,
                    file_sha1: row.get(2)?,
                    file_mtime: row.get(3)?,
                    method_count: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|e| DtError::Repository(e.to_string()))?;

        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row.map_err(|e| DtError::Repository(e.to_string()))?);
        }

        Ok(snapshots)
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        // 测试能否从数据库读取
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        match conn.execute_batch("SELECT 1") {
            Ok(_) => Ok(HealthStatus::Healthy),
            Err(e) => Ok(HealthStatus::Unhealthy(e.to_string())),
        }
    }

    async fn mark_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<(), DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO build_progress (file_path, project, stage, file_sha1, completed_at)
             VALUES (?1, ?2, 'llm_analysis', ?3, ?4)",
            params![file_path, project, file_sha1, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| DtError::Repository(format!("mark_llm_analyzed: {e}")))?;
        Ok(())
    }

    async fn is_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<bool, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM build_progress
                 WHERE file_path = ?1 AND project = ?2 AND stage = 'llm_analysis'
                 AND file_sha1 = ?3",
            )
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let exists = stmt
            .exists(params![file_path, project, file_sha1])
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(exists)
    }

    async fn clear_llm_progress(&self, project: &str) -> Result<(), DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        conn.execute(
            "DELETE FROM build_progress WHERE project = ?1",
            params![project],
        )
        .map_err(|e| DtError::Repository(format!("clear_llm_progress: {e}")))?;
        Ok(())
    }

    async fn mark_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<(), DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO pipeline_progress (file_path, project, step, file_hash, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file_path,
                project,
                step,
                file_hash,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| DtError::Repository(format!("mark_step_done: {e}")))?;
        Ok(())
    }

    async fn is_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<bool, DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pipeline_progress
                 WHERE file_path = ?1 AND project = ?2 AND step = ?3
                 AND file_hash = ?4",
            )
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let exists = stmt
            .exists(params![file_path, project, step, file_hash])
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(exists)
    }

    async fn clear_step_progress(&self, project: &str) -> Result<(), DtError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        conn.execute(
            "DELETE FROM pipeline_progress WHERE project = ?1",
            params![project],
        )
        .map_err(|e| DtError::Repository(format!("clear_step_progress: {e}")))?;
        Ok(())
    }

    async fn delete_file_progress(&self, project: &str, paths: &[String]) -> Result<u64, DtError> {
        if paths.is_empty() {
            return Ok(0);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let placeholders = std::iter::repeat_n("?", paths.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut removed = 0u64;
        for table in ["file_snapshots", "pipeline_progress"] {
            let sql =
                format!("DELETE FROM {table} WHERE project = ?1 AND file_path IN ({placeholders})");
            let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(paths.len() + 1);
            values.push(project.to_string().into());
            values.extend(paths.iter().map(|p| p.clone().into()));
            removed += conn
                .execute(&sql, rusqlite::params_from_iter(values))
                .map_err(|e| DtError::Repository(format!("delete_file_progress: {e}")))?
                as u64;
        }
        Ok(removed)
    }
}

/// 用于测试的内存快照仓库。
pub struct MemorySnapshotRepo {
    snapshots: Mutex<Vec<FileSnapshot>>,
    progress: Mutex<HashSet<(String, String, String)>>,
    /// 流水线步骤进度：(project, file_path, step, file_hash)
    step_progress: Mutex<HashSet<(String, String, String, String)>>,
}

impl MemorySnapshotRepo {
    /// 创建新的内存快照仓库（用于测试）。
    pub fn new() -> Self {
        Self {
            snapshots: Mutex::new(Vec::new()),
            progress: Mutex::new(HashSet::new()),
            step_progress: Mutex::new(HashSet::new()),
        }
    }
}

impl Default for MemorySnapshotRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SnapshotRepository for MemorySnapshotRepo {
    async fn get_snapshot(
        &self,
        project: &str,
        path: &str,
    ) -> Result<Option<FileSnapshot>, DtError> {
        let snapshots = self
            .snapshots
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(snapshots
            .iter()
            .find(|s| s.project == project && s.file_path == path)
            .cloned())
    }

    async fn save_snapshots(
        &self,
        _project: &str,
        new_snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        for new in new_snapshots {
            snapshots.retain(|s| !(s.project == new.project && s.file_path == new.file_path));
            snapshots.push(new.clone());
        }
        Ok(())
    }

    async fn delete_project(&self, project: &str) -> Result<u64, DtError> {
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        let before = snapshots.len();
        snapshots.retain(|s| s.project != project);
        Ok((before - snapshots.len()) as u64)
    }

    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError> {
        let snapshots = self
            .snapshots
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(snapshots
            .iter()
            .filter(|s| s.project == project)
            .cloned()
            .collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }

    async fn mark_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<(), DtError> {
        let mut progress = self
            .progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        progress.insert((
            project.to_string(),
            file_path.to_string(),
            file_sha1.to_string(),
        ));
        Ok(())
    }

    async fn is_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<bool, DtError> {
        let progress = self
            .progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(progress.contains(&(
            project.to_string(),
            file_path.to_string(),
            file_sha1.to_string(),
        )))
    }

    async fn clear_llm_progress(&self, project: &str) -> Result<(), DtError> {
        let mut progress = self
            .progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        progress.retain(|(p, _, _)| p != project);
        Ok(())
    }

    async fn mark_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<(), DtError> {
        let mut sp = self
            .step_progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        sp.insert((
            project.to_string(),
            file_path.to_string(),
            step.to_string(),
            file_hash.to_string(),
        ));
        Ok(())
    }

    async fn is_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<bool, DtError> {
        let sp = self
            .step_progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        Ok(sp.contains(&(
            project.to_string(),
            file_path.to_string(),
            step.to_string(),
            file_hash.to_string(),
        )))
    }

    async fn clear_step_progress(&self, project: &str) -> Result<(), DtError> {
        let mut sp = self
            .step_progress
            .lock()
            .map_err(|e| DtError::Repository(e.to_string()))?;
        sp.retain(|(p, _, _, _)| p != project);
        Ok(())
    }

    async fn delete_file_progress(&self, project: &str, paths: &[String]) -> Result<u64, DtError> {
        let mut removed = 0u64;
        {
            let mut snapshots = self
                .snapshots
                .lock()
                .map_err(|e| DtError::Repository(e.to_string()))?;
            let before = snapshots.len();
            snapshots.retain(|s| !(s.project == project && paths.contains(&s.file_path)));
            removed += (before - snapshots.len()) as u64;
        }
        {
            let mut sp = self
                .step_progress
                .lock()
                .map_err(|e| DtError::Repository(e.to_string()))?;
            let before = sp.len();
            sp.retain(|(p, f, _, _)| !(p == project && paths.contains(f)));
            removed += (before - sp.len()) as u64;
        }
        Ok(removed)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(project: &str, path: &str, hash: &str) -> FileSnapshot {
        FileSnapshot {
            file_path: path.to_string(),
            project: project.to_string(),
            file_sha1: hash.to_string(),
            file_mtime: 1.0,
            method_count: 0,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn sqlite_repo_save_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_string_lossy().to_string();

        let repo = SqliteRepo::open(&db_path).unwrap();

        // 保存一个快照
        let snap = make_snapshot("test-proj", "src/main.rs", "abc123");
        repo.save_snapshots("test-proj", &[snap]).await.unwrap();

        // 取回来
        let retrieved = repo.get_snapshot("test-proj", "src/main.rs").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().file_sha1, "abc123");
    }

    #[tokio::test]
    async fn sqlite_repo_list_project() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test2.db").to_string_lossy().to_string();

        let repo = SqliteRepo::open(&db_path).unwrap();

        let s1 = make_snapshot("proj-a", "a.rs", "aaa");
        let s2 = make_snapshot("proj-a", "b.rs", "bbb");
        let s3 = make_snapshot("proj-b", "c.rs", "ccc");

        repo.save_snapshots("proj-a", &[s1, s2, s3]).await.unwrap();

        let snapshots_a = repo.list_snapshots("proj-a").await.unwrap();
        assert_eq!(snapshots_a.len(), 3);
    }

    #[tokio::test]
    async fn sqlite_repo_delete_project() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test3.db").to_string_lossy().to_string();

        let repo = SqliteRepo::open(&db_path).unwrap();
        let snap = make_snapshot("to-delete", "x.rs", "ddd");
        repo.save_snapshots("to-delete", &[snap]).await.unwrap();

        let deleted = repo.delete_project("to-delete").await.unwrap();
        assert_eq!(deleted, 1);

        let remaining = repo.list_snapshots("to-delete").await.unwrap();
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn memory_repo_works() {
        let repo = MemorySnapshotRepo::new();
        let snap = make_snapshot("test", "f.rs", "xyz");
        repo.save_snapshots("test", &[snap.clone()]).await.unwrap();

        let got = repo.get_snapshot("test", "f.rs").await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().file_sha1, "xyz");
    }

    #[tokio::test]
    async fn health_check() {
        let repo = MemorySnapshotRepo::new();
        assert!(repo.health_check().await.unwrap().is_healthy());
    }

    #[tokio::test]
    async fn sqlite_delete_file_progress_removes_snapshot_and_step_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test4.db").to_string_lossy().to_string();
        let repo = SqliteRepo::open(&db_path).unwrap();

        repo.save_snapshots(
            "proj",
            &[
                make_snapshot("proj", "docs/a.md", "h1"),
                make_snapshot("proj", "docs/b.md", "h2"),
            ],
        )
        .await
        .unwrap();
        repo.mark_step_done("proj", "docs/a.md", "store", "h1")
            .await
            .unwrap();
        repo.mark_step_done("proj", "docs/b.md", "store", "h2")
            .await
            .unwrap();

        let removed = repo
            .delete_file_progress("proj", &["docs/a.md".to_string()])
            .await
            .unwrap();
        // 1 行 file_snapshots + 1 行 pipeline_progress。
        assert_eq!(removed, 2);

        // 被删除的路径在两个表中都已消失……
        assert!(repo
            .get_snapshot("proj", "docs/a.md")
            .await
            .unwrap()
            .is_none());
        assert!(!repo
            .is_step_done("proj", "docs/a.md", "store", "h1")
            .await
            .unwrap());
        // ……而未动的路径在两个表中都保留。
        assert!(repo
            .get_snapshot("proj", "docs/b.md")
            .await
            .unwrap()
            .is_some());
        assert!(repo
            .is_step_done("proj", "docs/b.md", "store", "h2")
            .await
            .unwrap());

        // 空输入是 no-op。
        assert_eq!(repo.delete_file_progress("proj", &[]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn memory_delete_file_progress_removes_both_stores() {
        let repo = MemorySnapshotRepo::new();
        repo.save_snapshots("p", &[make_snapshot("p", "x.md", "h")])
            .await
            .unwrap();
        repo.mark_step_done("p", "x.md", "chunk", "h")
            .await
            .unwrap();

        let removed = repo
            .delete_file_progress("p", &["x.md".to_string()])
            .await
            .unwrap();
        assert_eq!(removed, 2);
        assert!(repo.get_snapshot("p", "x.md").await.unwrap().is_none());
        assert!(!repo.is_step_done("p", "x.md", "chunk", "h").await.unwrap());
    }
}
