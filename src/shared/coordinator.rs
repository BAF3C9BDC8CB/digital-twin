//! WriteCoordinator — concurrent write safety for three ingestion sources.
//!
//! Three writers (OpenCode Hook → dt update, user dt build, cron syncs)
//! can concurrently write to Memgraph / Qdrant. The `WriteCoordinator` provides
//! file-level locks, entity-level locks, and an optional global serialization
//! lock to prevent conflicts.
//!
//! # Architecture
//!
//! - **File locks**: `DashMap<file_path, Arc<Mutex>>`. Two writers cannot
//!   concurrently update the same file.
//! - **Entity locks**: `DashMap<entity_id, Arc<Mutex>>`. Two writers cannot
//!   concurrently write the same entity (project-level for deletes, etc.).
//! - **Global gate**: Optional `Arc<Semaphore>` with a large permit count.
//!   File/entity ops each consume 1 permit (allowing many concurrent ops).
//!   A full-build `acquire_global()` consumes ALL permits, blocking every
//!   subsequent file/entity writer until the build completes.
//! - **Active-writes counter**: `AtomicUsize` for cron jobs to check before
//!   starting (e.g. nacos-sync skips if writes are in progress).
//!
//! # RAII Guards
//!
//! All lock methods return RAII guards (`FileGuard`, `EntityGuard`, `GlobalGuard`).
//! On drop, the guard decrements the active-writes counter. The underlying
//! `OwnedMutexGuard` / `OwnedSemaphorePermit` release the lock automatically.

use dashmap::DashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Maximum number of concurrent file/entity operations allowed before the
/// global semaphore blocks. The global lock (`acquire_global`) acquires all
/// 1024 permits, effectively blocking every new file/entity operation until
/// the build finishes.
const MAX_CONCURRENT_WRITES: u32 = 1024;

// ---------------------------------------------------------------------------
// WriteCoordinator
// ---------------------------------------------------------------------------

/// Coordinates concurrent writes from multiple ingestion sources.
///
/// # Usage
///
/// ```ignore
/// let coordinator = Arc::new(WriteCoordinator::with_global_lock());
///
/// // Single file update — file-level lock (acquires 1 global permit)
/// let _guard = coordinator.acquire_file(path).await;
/// // ... write to Memgraph/Qdrant ...
///
/// // Full build — global lock (acquires ALL global permits)
/// let _guard = coordinator.acquire_global().await;
/// // ... batch writes — no file ops can start ...
///
/// // Cron sync — check before starting
/// if coordinator.has_active_writes() {
///     tracing::warn!("skipped: active writes in progress");
///     return;
/// }
/// ```
pub struct WriteCoordinator {
    /// Per-file mutexes. Key = canonical file path string.
    file_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Per-entity mutexes. Key = entity ID string.
    entity_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Global semaphore gate. File/entity ops consume 1 permit; full builds
    /// consume all 1024 permits, blocking everything.
    global_semaphore: Option<Arc<tokio::sync::Semaphore>>,
    /// Number of currently held write guards (all types). Used by cron jobs
    /// to decide whether to skip a sync cycle.
    active_writes: Arc<AtomicUsize>,
}

impl WriteCoordinator {
    /// Create a coordinator with file and entity locks, **without** a global
    /// gate. Suitable when only single-file updates are expected and there
    /// are no full-project rebuilds.
    pub fn new() -> Self {
        Self {
            file_locks: DashMap::new(),
            entity_locks: DashMap::new(),
            global_semaphore: None,
            active_writes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Create a coordinator **with** a global gate. Use when full-project
    /// rebuilds (`dt build --full`) must block all other writers.
    ///
    /// File/entity ops consume 1 permit from the semaphore (allowing many
    /// concurrent writers). `acquire_global()` consumes all permits,
    /// blocking every new writer until the build finishes.
    pub fn with_global_lock() -> Self {
        Self {
            file_locks: DashMap::new(),
            entity_locks: DashMap::new(),
            global_semaphore: Some(Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_WRITES as usize,
            ))),
            active_writes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Acquire an exclusive lock for writing a specific file.
    ///
    /// 1. Consumes 1 global semaphore permit (if gate exists). If a full
    ///    build holds all permits, this call blocks until the build finishes.
    /// 2. Acquires the file-level mutex.
    ///
    /// The returned [`FileGuard`] releases both on drop.
    pub async fn acquire_file(&self, path: &Path) -> FileGuard {
        // Step 1: global gate (1 permit)
        let global_permit = if let Some(sem) = &self.global_semaphore {
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("WriteCoordinator semaphore closed"),
            )
        } else {
            None
        };

        // Step 2: file-level lock
        let key = path.to_string_lossy().to_string();
        let lock = self
            .file_locks
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let file_guard = lock.lock_owned().await;

        self.active_writes.fetch_add(1, Ordering::SeqCst);
        FileGuard {
            _file_guard: file_guard,
            _global_permit: global_permit,
            active_writes: Arc::clone(&self.active_writes),
        }
    }

    /// Acquire an exclusive lock for writing a specific entity (e.g. a project
    /// during deletion).
    ///
    /// Same two-step gating as [`acquire_file`]: global permit → entity lock.
    pub async fn acquire_entity(&self, entity_id: &str) -> EntityGuard {
        // Step 1: global gate (1 permit)
        let global_permit = if let Some(sem) = &self.global_semaphore {
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("WriteCoordinator semaphore closed"),
            )
        } else {
            None
        };

        // Step 2: entity-level lock
        let key = entity_id.to_string();
        let lock = self
            .entity_locks
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let entity_guard = lock.lock_owned().await;

        self.active_writes.fetch_add(1, Ordering::SeqCst);
        EntityGuard {
            _entity_guard: entity_guard,
            _global_permit: global_permit,
            active_writes: Arc::clone(&self.active_writes),
        }
    }

    /// Acquire the global serialization lock. Consumes ALL permits from the
    /// semaphore, blocking every subsequent file/entity operation until the
    /// returned guard is dropped.
    ///
    /// Returns `None` if this coordinator was created **without** a global
    /// gate (i.e. via `new()`).
    pub async fn acquire_global(&self) -> Option<GlobalGuard> {
        if let Some(sem) = &self.global_semaphore {
            let permit = sem
                .clone()
                .acquire_many_owned(MAX_CONCURRENT_WRITES)
                .await
                .expect("WriteCoordinator semaphore closed");
            self.active_writes.fetch_add(1, Ordering::SeqCst);
            Some(GlobalGuard {
                _permit: permit,
                active_writes: Arc::clone(&self.active_writes),
            })
        } else {
            None
        }
    }

    /// Returns `true` if any write guard is currently held (file, entity, or
    /// global).
    ///
    /// Cron sync jobs should call this before starting and skip if `true`.
    /// ```ignore
    /// if coordinator.has_active_writes() {
    ///     tracing::warn!("skipped: {} active writes in progress", count);
    ///     return Ok(SyncReport::skipped());
    /// }
    /// ```
    pub fn has_active_writes(&self) -> bool {
        self.active_writes.load(Ordering::SeqCst) > 0
    }

    /// Return the current count of active writes (for diagnostics / logging).
    pub fn active_write_count(&self) -> usize {
        self.active_writes.load(Ordering::SeqCst)
    }
}

impl Default for WriteCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RAII Guards
// ---------------------------------------------------------------------------

/// Guard for a file-level lock. Holds both the global semaphore permit and
/// the file mutex guard. Dropping releases both and decrements the active
/// writes counter.
pub struct FileGuard {
    _file_guard: tokio::sync::OwnedMutexGuard<()>,
    _global_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    active_writes: Arc<AtomicUsize>,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        self.active_writes.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Guard for an entity-level lock. Holds both the global semaphore permit
/// and the entity mutex guard.
pub struct EntityGuard {
    _entity_guard: tokio::sync::OwnedMutexGuard<()>,
    _global_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    active_writes: Arc<AtomicUsize>,
}

impl Drop for EntityGuard {
    fn drop(&mut self) {
        self.active_writes.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Guard for the global serialization lock. Holds all semaphore permits,
/// blocking every new file/entity writer until this guard is dropped.
pub struct GlobalGuard {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active_writes: Arc<AtomicUsize>,
}

impl Drop for GlobalGuard {
    fn drop(&mut self) {
        self.active_writes.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// CoordinatedBuildService — Wrap pattern
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::BuildService;
use crate::domain::types::BuildReport;

/// A `BuildService` decorator that acquires [`WriteCoordinator`] locks
/// before delegating to the inner service.
///
/// # Locking strategy
///
/// | Method            | Lock acquired                           |
/// |-------------------|-----------------------------------------|
/// | `build()`         | Global gate (all semaphore permits)     |
/// | `update_file()`   | 1 global permit + file-level lock       |
/// | `delete_project()`| 1 global permit + entity-level lock     |
pub struct CoordinatedBuildService {
    inner: Arc<dyn BuildService>,
    coordinator: Arc<WriteCoordinator>,
}

impl CoordinatedBuildService {
    /// Wrap an existing `BuildService` with write coordination.
    pub fn new(inner: Arc<dyn BuildService>, coordinator: Arc<WriteCoordinator>) -> Self {
        Self {
            inner,
            coordinator,
        }
    }
}

#[async_trait]
impl BuildService for CoordinatedBuildService {
    /// Full/incremental build — acquires the global gate (all semaphore
    /// permits) so that no file-level or entity-level writer can interleave
    /// during the bulk write phase.
    async fn build(&self, project: &str, root: &Path) -> Result<BuildReport, DtError> {
        let _guard = self.coordinator.acquire_global().await;
        self.inner.build(project, root).await
    }

    /// Single-file update — acquires 1 global permit + file-level lock to
    /// prevent concurrent updates to the same file.
    async fn update_file(&self, project: &str, path: &Path) -> Result<(), DtError> {
        let _guard = self.coordinator.acquire_file(path).await;
        self.inner.update_file(project, path).await
    }

    /// Project deletion — acquires 1 global permit + entity-level lock for
    /// the project.
    async fn delete_project(&self, project: &str) -> Result<(), DtError> {
        let _guard = self.coordinator.acquire_entity(project).await;
        self.inner.delete_project(project).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ------------------------------------------------------------------
    // Constructor tests
    // ------------------------------------------------------------------

    #[test]
    fn new_creates_without_global_lock() {
        let c = WriteCoordinator::new();
        assert!(!c.has_active_writes());
    }

    #[test]
    fn with_global_lock_creates_with_global_lock() {
        let c = WriteCoordinator::with_global_lock();
        assert!(!c.has_active_writes());
    }

    #[test]
    fn default_equals_new() {
        let c = WriteCoordinator::default();
        assert!(!c.has_active_writes());
    }

    // ------------------------------------------------------------------
    // Basic acquire / release tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn acquire_file_lock_and_release() {
        let c = WriteCoordinator::new();
        let path = PathBuf::from("/tmp/test.rs");

        assert!(!c.has_active_writes());

        {
            let _guard = c.acquire_file(&path).await;
            assert!(c.has_active_writes());
            assert_eq!(c.active_write_count(), 1);
        }

        // Guard dropped → count decremented
        assert!(!c.has_active_writes());
        assert_eq!(c.active_write_count(), 0);
    }

    #[tokio::test]
    async fn acquire_entity_lock_and_release() {
        let c = WriteCoordinator::new();

        {
            let _guard = c.acquire_entity("project:x").await;
            assert!(c.has_active_writes());
        }

        assert!(!c.has_active_writes());
    }

    #[tokio::test]
    async fn acquire_global_lock_and_release() {
        let c = WriteCoordinator::with_global_lock();

        {
            let guard = c.acquire_global().await;
            assert!(guard.is_some());
            assert!(c.has_active_writes());
        }

        assert!(!c.has_active_writes());
    }

    #[tokio::test]
    async fn acquire_global_returns_none_when_no_global_lock() {
        let c = WriteCoordinator::new();
        let guard = c.acquire_global().await;
        assert!(guard.is_none());
        // No global lock → no active write
        assert!(!c.has_active_writes());
    }

    // ------------------------------------------------------------------
    // Concurrent access tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn same_file_concurrent_serializes() {
        let c = Arc::new(WriteCoordinator::new());
        let path = PathBuf::from("/tmp/shared.rs");

        // Track execution order
        let order = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let c1 = Arc::clone(&c);
        let order1 = Arc::clone(&order);
        let path1 = path.clone();

        let c2 = Arc::clone(&c);
        let order2 = Arc::clone(&order);
        let path2 = path.clone();

        // Task 1: acquires file lock, sleeps briefly, then records
        let t1 = tokio::spawn(async move {
            let _guard = c1.acquire_file(&path1).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            order1.lock().await.push(1);
        });

        // Task 2: same file — must wait for t1
        let t2 = tokio::spawn(async move {
            let _guard = c2.acquire_file(&path2).await;
            order2.lock().await.push(2);
        });

        let (r1, r2) = tokio::join!(t1, t2);
        r1.unwrap();
        r2.unwrap();

        let final_order = order.lock().await;
        assert_eq!(*final_order, vec![1, 2], "task 2 must execute after task 1");
    }

    #[tokio::test]
    async fn different_files_concurrent_do_not_block() {
        let c = Arc::new(WriteCoordinator::new());

        let path_a = PathBuf::from("/tmp/a.rs");
        let path_b = PathBuf::from("/tmp/b.rs");

        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let c1 = Arc::clone(&c);
        let barrier1 = Arc::clone(&barrier);
        let t1 = tokio::spawn(async move {
            let _guard = c1.acquire_file(&path_a).await;
            barrier1.wait().await;
        });

        let c2 = Arc::clone(&c);
        let barrier2 = Arc::clone(&barrier);
        let t2 = tokio::spawn(async move {
            let _guard = c2.acquire_file(&path_b).await;
            barrier2.wait().await;
        });

        // Both should complete without deadlock (different files)
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (r1, r2) = tokio::join!(t1, t2);
            r1.unwrap();
            r2.unwrap();
        })
        .await
        .expect("different file locks should not deadlock");
    }

    #[tokio::test]
    async fn global_lock_blocks_file_lock() {
        let c = Arc::new(WriteCoordinator::with_global_lock());
        let path = PathBuf::from("/tmp/x.rs");

        // Acquire global lock first (consumes all 1024 semaphore permits)
        let global_guard = c.acquire_global().await.unwrap();
        assert!(c.has_active_writes());

        // Attempt to acquire file lock — should not complete until global
        // lock is dropped (semaphore has 0 permits).
        let c_clone = Arc::clone(&c);
        let path_clone = path.clone();
        let file_task = tokio::spawn(async move {
            let _guard = c_clone.acquire_file(&path_clone).await;
        });

        // The file task should be blocked (global lock held)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !file_task.is_finished(),
            "file task should be blocked by global lock"
        );

        // Drop global lock → release all permits
        drop(global_guard);

        // Now file task should complete
        tokio::time::timeout(std::time::Duration::from_secs(3), file_task)
            .await
            .expect("file task should complete after global lock released")
            .unwrap();
    }

    #[tokio::test]
    async fn multiple_file_locks_concurrent_with_global_gate() {
        // With global gate, multiple file operations should still be concurrent
        // (each consumes 1 permit out of 1024).
        let c = Arc::new(WriteCoordinator::with_global_lock());

        let barrier = Arc::new(tokio::sync::Barrier::new(3));

        let t1 = {
            let c = Arc::clone(&c);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let _guard = c.acquire_file(Path::new("/tmp/a.rs")).await;
                barrier.wait().await;
            })
        };

        let t2 = {
            let c = Arc::clone(&c);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let _guard = c.acquire_file(Path::new("/tmp/b.rs")).await;
                barrier.wait().await;
            })
        };

        let t3 = {
            let c = Arc::clone(&c);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let _guard = c.acquire_entity("project:x").await;
                barrier.wait().await;
            })
        };

        // All three should complete without deadlock
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (r1, r2, r3) = tokio::join!(t1, t2, t3);
            r1.unwrap();
            r2.unwrap();
            r3.unwrap();
        })
        .await
        .expect("multiple file locks with global gate should not deadlock");
    }

    // ------------------------------------------------------------------
    // has_active_writes tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn has_active_writes_tracks_multiple_guards() {
        let c = WriteCoordinator::new();
        let path1 = PathBuf::from("/tmp/a.rs");
        let path2 = PathBuf::from("/tmp/b.rs");

        assert!(!c.has_active_writes());

        let g1 = c.acquire_file(&path1).await;
        assert!(c.has_active_writes());
        assert_eq!(c.active_write_count(), 1);

        let g2 = c.acquire_file(&path2).await;
        assert_eq!(c.active_write_count(), 2);

        drop(g1);
        assert_eq!(c.active_write_count(), 1);

        drop(g2);
        assert!(!c.has_active_writes());
    }

    // ------------------------------------------------------------------
    // CoordinatedBuildService tests
    // ------------------------------------------------------------------

    /// A simple stub BuildService for testing the wrapper.
    struct StubBuildService;

    #[async_trait]
    impl BuildService for StubBuildService {
        async fn build(&self, _project: &str, _root: &Path) -> Result<BuildReport, DtError> {
            Ok(BuildReport {
                project: "stub".into(),
                files_scanned: 0,
                files_changed: 0,
                methods_total: 0,
                methods_new: 0,
                classes_total: 0,
                elapsed_ms: 0,
            })
        }

        async fn update_file(&self, _project: &str, _path: &Path) -> Result<(), DtError> {
            Ok(())
        }

        async fn delete_project(&self, _project: &str) -> Result<(), DtError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn coordinated_build_service_creates_and_delegates() {
        let coordinator = Arc::new(WriteCoordinator::new());
        let inner: Arc<dyn BuildService> = Arc::new(StubBuildService);
        let svc = CoordinatedBuildService::new(inner, coordinator);

        let result = svc.build("test", Path::new("/tmp")).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().project, "stub");
    }

    #[tokio::test]
    async fn coordinated_build_service_acquires_file_lock_on_update() {
        let coordinator = Arc::new(WriteCoordinator::new());
        let inner: Arc<dyn BuildService> = Arc::new(StubBuildService);
        let svc = CoordinatedBuildService::new(inner, Arc::clone(&coordinator));

        assert!(!coordinator.has_active_writes());

        svc.update_file("test", Path::new("/tmp/x.rs"))
            .await
            .unwrap();

        // Guard should have been dropped after the call returned
        assert!(!coordinator.has_active_writes());
    }
}
