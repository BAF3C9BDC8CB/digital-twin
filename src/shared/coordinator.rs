//! WriteCoordinator——三个写入源的并发写安全。
//!
//! 三个写入方（OpenCode Hook → dt update、用户 dt build、cron 同步）
//! 可能并发写入 Memgraph / Qdrant。`WriteCoordinator` 提供文件级锁、
//! 实体级锁以及可选的全局串行化锁，以防止冲突。
//!
//! # 架构
//!
//! - **文件锁**：`DashMap<file_path, Arc<Mutex>>`。两个写入方不能并发更新同一文件。
//! - **实体锁**：`DashMap<entity_id, Arc<Mutex>>`。两个写入方不能并发写入同一实体
//!   （删除时为项目级等）。
//! - **全局闸门**：可选的 `Arc<Semaphore>`，带大量许可。文件/实体操作各消耗 1 个
//!   许可（允许大量并发操作）。全量构建的 `acquire_global()` 消耗全部许可，
//!   阻塞其后所有文件/实体写入方，直到构建完成。
//! - **活动写入计数器**：`AtomicUsize`，供 cron 任务在启动前检查
//!   （例如 nacos-sync 在存在写入时跳过）。
//!
//! # RAII 守卫
//!
//! 所有加锁方法都返回 RAII 守卫（`FileGuard`、`EntityGuard`、`GlobalGuard`）。
//! 守卫被 drop 时会递减活动写入计数器。底层的 `OwnedMutexGuard` /
//! `OwnedSemaphorePermit` 会自动释放锁。

use dashmap::DashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 全局信号量阻塞之前允许的最大并发文件/实体操作数。全局锁
/// （`acquire_global`）会获取全部 1024 个许可，从而在构建结束前
/// 阻塞所有新的文件/实体操作。
const MAX_CONCURRENT_WRITES: u32 = 1024;

// ---------------------------------------------------------------------------
// WriteCoordinator
// ---------------------------------------------------------------------------

/// 协调来自多个写入源的并发写操作。
///
/// # 用法
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
    /// 每个文件的互斥锁。Key = 规范化的文件路径字符串。
    file_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// 每个实体的互斥锁。Key = 实体 ID 字符串。
    entity_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// 全局信号量闸门。文件/实体操作消耗 1 个许可；全量构建
    /// 消耗全部 1024 个许可，阻塞一切操作。
    global_semaphore: Option<Arc<tokio::sync::Semaphore>>,
    /// 当前持有的写守卫数量（所有类型）。供 cron 任务判断
    /// 是否跳过一轮同步。
    active_writes: Arc<AtomicUsize>,
}

impl WriteCoordinator {
    /// 创建仅带文件锁和实体锁的协调器，**不**带全局闸门。
    /// 适用于只需单文件更新、无需全量重建的场景。
    pub fn new() -> Self {
        Self {
            file_locks: DashMap::new(),
            entity_locks: DashMap::new(),
            global_semaphore: None,
            active_writes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 创建**带**全局闸门的协调器。当全量重建（`dt build --full`）
    /// 必须阻塞所有其他写入方时使用。
    ///
    /// 文件/实体操作从信号量消耗 1 个许可（允许大量并发写入方）。
    /// `acquire_global()` 消耗全部许可，在构建完成前阻塞所有新写入方。
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

    /// 获取用于写入特定文件的独占锁。
    ///
    /// 1. 消耗 1 个全局信号量许可（若存在闸门）。若全量构建持有全部许可，
    ///    此调用将阻塞直到构建完成。
    /// 2. 获取文件级互斥锁。
    ///
    /// 返回的 [`FileGuard`] 在 drop 时同时释放两者。
    pub async fn acquire_file(&self, path: &Path) -> FileGuard {
        // 步骤 1：全局闸门（1 个许可）
        let global_permit = if let Some(sem) = &self.global_semaphore {
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("WriteCoordinator 信号量已关闭"),
            )
        } else {
            None
        };

        // 步骤 2：文件级锁
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

    /// 获取用于写入特定实体（例如删除中的项目）的独占锁。
    ///
    /// 与 [`acquire_file`] 相同的两步闸门：全局许可 → 实体锁。
    pub async fn acquire_entity(&self, entity_id: &str) -> EntityGuard {
        // 步骤 1：全局闸门（1 个许可）
        let global_permit = if let Some(sem) = &self.global_semaphore {
            Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("WriteCoordinator 信号量已关闭"),
            )
        } else {
            None
        };

        // 步骤 2：实体级锁
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

    /// 获取全局串行化锁。从信号量消耗全部许可，阻塞其后所有
    /// 文件/实体操作，直到返回的守卫被 drop。
    ///
    /// 若该协调器创建时**没有**全局闸门（即通过 `new()` 创建），
    /// 则返回 `None`。
    pub async fn acquire_global(&self) -> Option<GlobalGuard> {
        if let Some(sem) = &self.global_semaphore {
            let permit = sem
                .clone()
                .acquire_many_owned(MAX_CONCURRENT_WRITES)
                .await
                .expect("WriteCoordinator 信号量已关闭");
            self.active_writes.fetch_add(1, Ordering::SeqCst);
            Some(GlobalGuard {
                _permit: permit,
                active_writes: Arc::clone(&self.active_writes),
            })
        } else {
            None
        }
    }

    /// 若当前持有任何写守卫（文件、实体或全局），返回 `true`。
    ///
    /// cron 同步任务应在启动前调用此方法，若为 `true` 则跳过。
    /// ```ignore
    /// if coordinator.has_active_writes() {
    ///     tracing::warn!("skipped: {} active writes in progress", count);
    ///     return Ok(SyncReport::skipped());
    /// }
    /// ```
    pub fn has_active_writes(&self) -> bool {
        self.active_writes.load(Ordering::SeqCst) > 0
    }

    /// 返回当前活动写入数（用于诊断 / 日志）。
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
// RAII 守卫
// ---------------------------------------------------------------------------

/// 文件级锁的守卫。同时持有全局信号量许可与文件互斥锁守卫。
/// drop 时释放两者并递减活动写入计数器。
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

/// 实体级锁的守卫。同时持有全局信号量许可与实体互斥锁守卫。
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

/// 全局串行化锁的守卫。持有全部信号量许可，
/// 在守卫被 drop 前阻塞所有新的文件/实体写入方。
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
// CoordinatedBuildService——包装模式
// ---------------------------------------------------------------------------

use crate::domain::error::DtError;
use crate::domain::traits::BuildService;
use crate::domain::types::BuildReport;
use async_trait::async_trait;

/// 一个 `BuildService` 装饰器，在委托给内部服务之前
/// 获取 [`WriteCoordinator`] 锁。
///
/// # 加锁策略
///
/// | 方法               | 获取的锁                            |
/// |--------------------|--------------------------------------|
/// | `build()`          | 全局闸门（全部信号量许可）          |
/// | `update_file()`    | 1 个全局许可 + 文件级锁             |
/// | `delete_project()` | 1 个全局许可 + 实体级锁             |
pub struct CoordinatedBuildService {
    inner: Arc<dyn BuildService>,
    coordinator: Arc<WriteCoordinator>,
}

impl CoordinatedBuildService {
    /// 为已有的 `BuildService` 包装写协调能力。
    pub fn new(inner: Arc<dyn BuildService>, coordinator: Arc<WriteCoordinator>) -> Self {
        Self { inner, coordinator }
    }
}

#[async_trait]
impl BuildService for CoordinatedBuildService {
    /// 全量/增量构建——获取全局闸门（全部信号量许可），
    /// 以确保批量写入阶段不会有任何文件级或实体级写入方插入。
    async fn build(&self, project: &str, root: &Path) -> Result<BuildReport, DtError> {
        let _guard = self.coordinator.acquire_global().await;
        self.inner.build(project, root).await
    }

    /// 单文件更新——获取 1 个全局许可 + 文件级锁，
    /// 以防止对同一文件的并发更新。
    async fn update_file(&self, project: &str, path: &Path) -> Result<(), DtError> {
        let _guard = self.coordinator.acquire_file(path).await;
        self.inner.update_file(project, path).await
    }

    /// 项目删除——获取 1 个全局许可 + 项目实体级锁。
    async fn delete_project(&self, project: &str) -> Result<(), DtError> {
        let _guard = self.coordinator.acquire_entity(project).await;
        self.inner.delete_project(project).await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ------------------------------------------------------------------
    // 构造函数测试
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
    // 基本获取 / 释放测试
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

        // 守卫已 drop → 计数递减
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
        // 无全局锁 → 无活动写入
        assert!(!c.has_active_writes());
    }

    // ------------------------------------------------------------------
    // 并发访问测试
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn same_file_concurrent_serializes() {
        let c = Arc::new(WriteCoordinator::new());
        let path = PathBuf::from("/tmp/shared.rs");

        // 跟踪执行顺序
        let order = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let c1 = Arc::clone(&c);
        let order1 = Arc::clone(&order);
        let path1 = path.clone();

        let c2 = Arc::clone(&c);
        let order2 = Arc::clone(&order);
        let path2 = path.clone();

        // 任务 1：获取文件锁，短暂休眠，然后记录
        let t1 = tokio::spawn(async move {
            let _guard = c1.acquire_file(&path1).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            order1.lock().await.push(1);
        });

        // 任务 2：同一文件——必须等待 t1
        let t2 = tokio::spawn(async move {
            let _guard = c2.acquire_file(&path2).await;
            order2.lock().await.push(2);
        });

        let (r1, r2) = tokio::join!(t1, t2);
        r1.unwrap();
        r2.unwrap();

        let final_order = order.lock().await;
        assert_eq!(*final_order, vec![1, 2], "任务 2 必须在任务 1 之后执行");
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

        // 两者都应无死锁地完成（不同文件）
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (r1, r2) = tokio::join!(t1, t2);
            r1.unwrap();
            r2.unwrap();
        })
        .await
        .expect("不同文件锁不应死锁");
    }

    #[tokio::test]
    async fn global_lock_blocks_file_lock() {
        let c = Arc::new(WriteCoordinator::with_global_lock());
        let path = PathBuf::from("/tmp/x.rs");

        // 先获取全局锁（消耗全部 1024 个信号量许可）
        let global_guard = c.acquire_global().await.unwrap();
        assert!(c.has_active_writes());

        // 尝试获取文件锁——在全局锁被 drop（信号量 0 许可）之前
        // 不应完成。
        let c_clone = Arc::clone(&c);
        let path_clone = path.clone();
        let file_task = tokio::spawn(async move {
            let _guard = c_clone.acquire_file(&path_clone).await;
        });

        // 文件任务应被阻塞（全局锁被持有）
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !file_task.is_finished(),
            "文件任务应被全局锁阻塞"
        );

        // drop 全局锁 → 释放全部许可
        drop(global_guard);

        // 现在文件任务应完成
        tokio::time::timeout(std::time::Duration::from_secs(3), file_task)
            .await
            .expect("全局锁释放后文件任务应完成")
            .unwrap();
    }

    #[tokio::test]
    async fn multiple_file_locks_concurrent_with_global_gate() {
        // 带全局闸门时，多个文件操作仍应并发执行
        // （每个操作消耗 1024 个许可中的 1 个）。
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

        // 三者都应无死锁地完成
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (r1, r2, r3) = tokio::join!(t1, t2, t3);
            r1.unwrap();
            r2.unwrap();
            r3.unwrap();
        })
        .await
        .expect("带全局闸门的多个文件锁不应死锁");
    }

    // ------------------------------------------------------------------
    // has_active_writes 测试
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
    // CoordinatedBuildService 测试
    // ------------------------------------------------------------------

    /// 用于测试包装器的简单 BuildService 桩。
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

        // 调用返回后守卫应已被 drop
        assert!(!coordinator.has_active_writes());
    }
}
