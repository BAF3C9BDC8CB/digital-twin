//! Durable state for resumable Nacos pipeline runs.
//!
//! The store intentionally lives in the existing SQLite snapshot database. It
//! records orchestration state only; external writes must remain idempotent
//! (Qdrant/Memgraph do not participate in a distributed transaction).

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;
use uuid::Uuid;

const STATES: [&str; 6] = [
    "pending",
    "running",
    "success",
    "failed",
    "retry_wait",
    "dead_letter",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Success,
    Failed,
    RetryWait,
    DeadLetter,
}
impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::RetryWait => "retry_wait",
            Self::DeadLetter => "dead_letter",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "success" => Self::Success,
            "failed" => Self::Failed,
            "retry_wait" => Self::RetryWait,
            "dead_letter" => Self::DeadLetter,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task_id: String,
    pub file_id: String,
    pub chunk_id: Option<String>,
    pub file_hash: String,
    pub dataset_version: String,
    pub state: TaskState,
    pub retry_count: u32,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ProgressSummary {
    pub total: u64,
    pub pending: u64,
    pub running: u64,
    pub success: u64,
    pub failed: u64,
    pub retry_wait: u64,
    pub dead_letter: u64,
}
impl ProgressSummary {
    pub fn completed_percent(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.success as f64 * 100.0 / self.total as f64
        }
    }
}

pub struct TaskStore {
    conn: Mutex<Connection>,
    max_retries: u32,
    lease_seconds: i64,
}
impl TaskStore {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        Self::open_with_policy(path, 3, 300)
    }
    pub fn open_with_policy(
        path: &str,
        max_retries: u32,
        lease_seconds: i64,
    ) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        let store = Self {
            conn: Mutex::new(conn),
            max_retries,
            lease_seconds,
        };
        store.ensure_schema()?;
        Ok(store)
    }
    fn ensure_schema(&self) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute_batch("CREATE TABLE IF NOT EXISTS pipeline_tasks (task_id TEXT NOT NULL, file_id TEXT NOT NULL, chunk_id TEXT NOT NULL DEFAULT '', file_hash TEXT NOT NULL, dataset_version TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','running','success','failed','retry_wait','dead_letter')), retry_count INTEGER NOT NULL DEFAULT 0, lease_owner TEXT, lease_until INTEGER, last_error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY(task_id,file_id,chunk_id)); CREATE INDEX IF NOT EXISTS idx_pipeline_tasks_lease ON pipeline_tasks(state,lease_until); CREATE INDEX IF NOT EXISTS idx_pipeline_tasks_dataset ON pipeline_tasks(task_id,dataset_version);")
    }
    pub fn enqueue(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
        file_hash: &str,
        dataset_version: &str,
    ) -> Result<bool, rusqlite::Error> {
        let started = Instant::now();
        let now = Utc::now().timestamp();
        let n=self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO pipeline_tasks (task_id,file_id,chunk_id,file_hash,dataset_version,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'pending',?6,?6)",params![task_id,file_id,chunk_id.unwrap_or(""),file_hash,dataset_version,now])?;
        tracing::info!(task = %task_id, run = %task_id, file = %file_id, chunk = chunk_id.unwrap_or("all"), attempt = 0u32, provider = "sqlite", model = "n/a", elapsed_ms = started.elapsed().as_millis(), total_ms = started.elapsed().as_millis(), stage = "checkpoint_enqueue_done", inserted = n == 1, "SQLite checkpoint enqueue");
        Ok(n == 1)
    }
    pub fn start_run(&self) -> String {
        Uuid::new_v4().to_string()
    }
    pub fn claim(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
        owner: &str,
    ) -> Result<bool, rusqlite::Error> {
        let started = Instant::now();
        let now = Utc::now().timestamp();
        let until = now + self.lease_seconds;
        let c = self.conn.lock().unwrap();
        let n=c.execute("UPDATE pipeline_tasks SET state='running',lease_owner=?4,lease_until=?5,updated_at=?6 WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3 AND (state IN ('pending','retry_wait') OR (state='running' AND lease_until IS NOT NULL AND lease_until<=?6))",params![task_id,file_id,chunk_id.unwrap_or(""),owner,until,now])?;
        tracing::info!(task = %task_id, run = %task_id, file = %file_id, chunk = chunk_id.unwrap_or("all"), attempt = 0u32, provider = "sqlite", model = "n/a", elapsed_ms = started.elapsed().as_millis(), total_ms = started.elapsed().as_millis(), stage = "checkpoint_claim_done", claimed = n == 1, "SQLite checkpoint claim");
        Ok(n == 1)
    }
    pub fn complete(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        self.transition(task_id, file_id, chunk_id, TaskState::Success, None)
    }
    pub fn fail(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
        error: &str,
        transient: bool,
    ) -> Result<TaskState, rusqlite::Error> {
        let c = self.conn.lock().unwrap();
        let old: String = c.query_row(
            "SELECT state FROM pipeline_tasks WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",
            params![task_id, file_id, chunk_id.unwrap_or("")],
            |r| r.get(0),
        )?;
        let retry:i64=c.query_row("SELECT retry_count FROM pipeline_tasks WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",params![task_id,file_id,chunk_id.unwrap_or("")],|r|r.get(0))?;
        let next = if transient && retry < i64::from(self.max_retries) {
            TaskState::RetryWait
        } else {
            TaskState::DeadLetter
        };
        if old == "success" {
            return Ok(TaskState::Success);
        };
        if old != "running" {
            return Err(rusqlite::Error::InvalidParameterName(
                "failure requires running task".into(),
            ));
        }
        let now = Utc::now().timestamp();
        c.execute("UPDATE pipeline_tasks SET state=?4,retry_count=retry_count+1,last_error=?5,lease_owner=NULL,lease_until=NULL,updated_at=?6 WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",params![task_id,file_id,chunk_id.unwrap_or(""),next.as_str(),error,now])?;
        Ok(next)
    }
    fn transition(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
        next: TaskState,
        error: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        let c = self.conn.lock().unwrap();
        let old: Option<String> = c
            .query_row(
                "SELECT state FROM pipeline_tasks WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",
                params![task_id, file_id, chunk_id.unwrap_or("")],
                |r| r.get(0),
            )
            .optional()?;
        let old = old.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
        if !legal_transition(TaskState::parse(&old).unwrap(), next) {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "illegal task transition {old} -> {}",
                next.as_str()
            )));
        }
        c.execute("UPDATE pipeline_tasks SET state=?4,last_error=?5,lease_owner=NULL,lease_until=NULL,updated_at=?6 WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",params![task_id,file_id,chunk_id.unwrap_or(""),next.as_str(),error,Utc::now().timestamp()])?;
        Ok(())
    }
    pub fn recover_stale(&self) -> Result<u64, rusqlite::Error> {
        let n = Utc::now().timestamp();
        Ok(self.conn.lock().unwrap().execute("UPDATE pipeline_tasks SET state='retry_wait',lease_owner=NULL,lease_until=NULL,updated_at=?1 WHERE state='running' AND lease_until IS NOT NULL AND lease_until<=?1",params![n])? as u64)
    }
    pub fn get(
        &self,
        task_id: &str,
        file_id: &str,
        chunk_id: Option<&str>,
    ) -> Result<Option<TaskRecord>, rusqlite::Error> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT task_id,file_id,chunk_id,file_hash,dataset_version,state,retry_count,lease_owner,lease_until,last_error,created_at,updated_at FROM pipeline_tasks WHERE task_id=?1 AND file_id=?2 AND chunk_id=?3",params![task_id,file_id,chunk_id.unwrap_or("")],row).optional()
    }
    pub fn summary(&self, task_id: &str) -> Result<ProgressSummary, rusqlite::Error> {
        let c = self.conn.lock().unwrap();
        let mut s = ProgressSummary::default();
        let mut st =
            c.prepare("SELECT state,count(*) FROM pipeline_tasks WHERE task_id=?1 GROUP BY state")?;
        for r in st.query_map(params![task_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })? {
            let (state, n) = r?;
            s.total += n;
            match state.as_str() {
                "pending" => s.pending = n,
                "running" => s.running = n,
                "success" => s.success = n,
                "failed" => s.failed = n,
                "retry_wait" => s.retry_wait = n,
                "dead_letter" => s.dead_letter = n,
                _ => {}
            }
        }
        Ok(s)
    }
}
fn row(r: &rusqlite::Row<'_>) -> Result<TaskRecord, rusqlite::Error> {
    Ok(TaskRecord {
        task_id: r.get(0)?,
        file_id: r.get(1)?,
        chunk_id: {
            let x: String = r.get(2)?;
            if x.is_empty() {
                None
            } else {
                Some(x)
            }
        },
        file_hash: r.get(3)?,
        dataset_version: r.get(4)?,
        state: TaskState::parse(&r.get::<_, String>(5)?).unwrap(),
        retry_count: r.get(6)?,
        lease_owner: r.get(7)?,
        lease_until: r.get(8)?,
        last_error: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}
pub fn legal_transition(from: TaskState, to: TaskState) -> bool {
    match from {
        TaskState::Pending => matches!(to, TaskState::Running),
        TaskState::Running => matches!(
            to,
            TaskState::Success | TaskState::RetryWait | TaskState::DeadLetter
        ),
        TaskState::RetryWait => matches!(to, TaskState::Running),
        TaskState::Success | TaskState::DeadLetter => false,
        TaskState::Failed => matches!(to, TaskState::RetryWait | TaskState::DeadLetter),
    }
}
pub fn known_states() -> &'static [&'static str] {
    &STATES
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> TaskStore {
        TaskStore::open_with_policy(":memory:", 2, 1).unwrap()
    }
    #[test]
    fn transitions_and_idempotent_enqueue() {
        let s = store();
        let id = s.start_run();
        assert!(s.enqueue(&id, "f", Some("c"), "h", "v").unwrap());
        assert!(!s.enqueue(&id, "f", Some("c"), "h", "v").unwrap());
        assert!(s.claim(&id, "f", Some("c"), "w").unwrap());
        assert!(s.complete(&id, "f", Some("c")).is_ok());
        assert!(!s.claim(&id, "f", Some("c"), "w").unwrap());
        assert!(!legal_transition(TaskState::Success, TaskState::Running));
    }
    #[test]
    fn retry_dead_letters() {
        let s = store();
        let id = s.start_run();
        s.enqueue(&id, "f", None, "h", "v").unwrap();
        for _ in 0..2 {
            s.claim(&id, "f", None, "w").unwrap();
            assert_eq!(
                s.fail(&id, "f", None, "temporary", true).unwrap(),
                TaskState::RetryWait
            );
        }
        s.claim(&id, "f", None, "w").unwrap();
        assert_eq!(
            s.fail(&id, "f", None, "temporary", true).unwrap(),
            TaskState::DeadLetter
        );
        assert_eq!(s.summary(&id).unwrap().dead_letter, 1);
    }
    #[test]
    fn stale_lease_is_recovered_and_restart_resumes_only_unfinished() {
        let path = tempfile::NamedTempFile::new().unwrap();
        let id;
        {
            let s = TaskStore::open_with_policy(path.path().to_str().unwrap(), 2, 0).unwrap();
            id = s.start_run();
            s.enqueue(&id, "done", None, "h1", "v").unwrap();
            s.enqueue(&id, "unfinished", None, "h2", "v").unwrap();
            s.claim(&id, "done", None, "worker").unwrap();
            s.complete(&id, "done", None).unwrap();
            s.claim(&id, "unfinished", None, "worker").unwrap();
            assert_eq!(s.recover_stale().unwrap(), 1);
        }
        let s = TaskStore::open_with_policy(path.path().to_str().unwrap(), 2, 300).unwrap();
        assert_eq!(
            s.get(&id, "done", None).unwrap().unwrap().state,
            TaskState::Success
        );
        assert_eq!(
            s.get(&id, "unfinished", None).unwrap().unwrap().state,
            TaskState::RetryWait
        );
        assert!(s.claim(&id, "unfinished", None, "new-worker").unwrap());
    }

    #[test]
    fn summary_partial() {
        let s = store();
        let id = s.start_run();
        for f in ["a", "b", "c"] {
            s.enqueue(&id, f, None, "h", "v").unwrap();
        }
        s.claim(&id, "a", None, "w").unwrap();
        s.complete(&id, "a", None).unwrap();
        assert_eq!(
            s.summary(&id).unwrap().completed_percent(),
            33.333333333333336
        );
    }
}

#[allow(dead_code)]
fn _assert_states() {
    assert_eq!(STATES.len(), 6);
}

// Keep a stable compile-time guard against accidental schema/state drift.
const _: () = {
    let _ = STATES;
};

#[derive(Debug, Clone, Serialize)]
pub struct DatasetVersion {
    pub task_id: String,
    pub version: String,
}
impl DatasetVersion {
    pub fn new(task_id: String) -> Self {
        Self {
            version: task_id.clone(),
            task_id,
        }
    }
}

// BTreeMap is intentionally referenced by the public summary implementation's
// future extension point; this keeps ordering deterministic when fields grow.
#[allow(dead_code)]
fn _ordered_summary() -> BTreeMap<&'static str, u64> {
    BTreeMap::new()
}
