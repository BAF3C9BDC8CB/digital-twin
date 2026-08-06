//! 文件监听守护进程 — 监控项目源码目录的变更。
//!
//! 使用 `notify` crate 实现跨平台文件系统事件（Linux 上为 inotify，
//! macOS 上为 FSEvents，BSD 上为 kqueue）。事件经过防抖（100 ms），
//! 使对同一文件的快速连续写入（编辑器和构建工具的典型行为）
//! 合并为一次 `dt update` 调用。
//!
//! # 架构
//!
//! 1. `FileWatcher::start()` 派生一个专用 OS 线程，持有
//!    `notify::RecommendedWatcher`。监听线程将原始文件系统事件
//!    送入 `std::sync::mpsc` 通道。
//! 2. 内部循环从该通道读取，过滤源码文件，应用防抖，
//!    解析所属项目，并把 `FileChangeEvent` 值推入 `tokio::sync::mpsc` 通道。
//! 3. 消费方（通常是 `dt-daemon` 的 `watch` 子命令）从该通道读取，
//!    并为每个事件调度 `UpdateRunner::run()`。
//!
//! # PID 文件
//!
//! 启动时，监听器将自身 OS 进程 ID 写入 `/var/run/dt-watch.pid`。
//! `dt watch --status` 与 `dt watch --stop` 使用该文件来检查或
//! 通知守护进程。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{Event, RecursiveMode, Watcher};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 监听器监控的源码文件扩展名。
const SOURCE_EXTENSIONS: &[&str] = &[
    "java", "py", "ts", "tsx", "go", "rs", "php", "js", "jsx", "mjs", "cjs", "kt", "kts", "swift",
    "scala", "rb", "cpp", "cc", "cxx", "c", "h", "hpp", "cs", "fs", "fsx", "vue", "svelte",
];

/// 内容永不监听的目录名。
const IGNORE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "build",
    "__pycache__",
    ".venv",
    "dist",
    ".next",
    "vendor",
    ".idea",
    ".vscode",
    "coverage",
    ".nyc_output",
    "out",
    "classes",
    ".turbo",
    ".cache",
    ".tmp",
    "tmp",
    "cache",
    "generated-sources",
    "generated-test-sources",
    ".nuxt",
    ".output",
];

// ---------------------------------------------------------------------------
// 公共类型
// ---------------------------------------------------------------------------

/// 监听器发出的单个文件系统变更事件。
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    /// 逻辑项目名称（来自 config.yaml）。
    pub project_name: String,
    /// 项目根目录（绝对路径）。
    pub project_root: PathBuf,
    /// 变更的文件（绝对路径）。
    pub file_path: PathBuf,
    /// 文件发生了什么。
    pub kind: FileChangeKind,
}

/// 文件变更的粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Created,
    Modified,
    Deleted,
}

impl FileChangeKind {
    /// 与 `UpdateRunner` 接受的 `--type` 值对应的可读标签。
    pub fn as_op_type(&self) -> &'static str {
        match self {
            FileChangeKind::Created => "create",
            FileChangeKind::Modified => "modify",
            FileChangeKind::Deleted => "delete",
        }
    }
}

/// 监听器当前状态的快照。
#[derive(Debug, Clone)]
pub struct WatcherStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub watched_dirs: usize,
    pub events_processed: u64,
}

// ---------------------------------------------------------------------------
// FileWatcher
// ---------------------------------------------------------------------------

/// 监控项目源码树的跨平台文件系统监听器。
///
/// # 生命周期
///
/// ```ignore
/// let watcher = FileWatcher::new(projects, pid_file);
/// let mut rx = watcher.start()?;              // 派生 OS 线程
/// while let Some(event) = rx.recv().await {
///     // 将事件分发到更新流水线
/// }
/// // 丢弃 rx → 监听线程退出并移除 PID 文件。
/// ```
pub struct FileWatcher {
    /// (project_name, project_root) 对。
    projects: Vec<(String, PathBuf)>,
    /// `projects` 中实际存在于磁盘上的根目录子集。
    watched_dirs: Vec<PathBuf>,
    /// PID 文件路径。
    pid_file: PathBuf,
    /// 防抖窗口（毫秒）。
    debounce_ms: u64,
    /// 监听线程激活时置为 `true`。
    running: Arc<AtomicBool>,
    /// 每发出一个事件便单调递增。
    events_processed: Arc<AtomicU64>,
}

impl FileWatcher {
    // ------------------------------------------------------------------
    // 构造函数
    // ------------------------------------------------------------------

    /// 创建监控给定项目目录的新监听器。
    ///
    /// `projects` 是 `(name, root_path)` 元组列表。不存在的目录会被
    /// 静默跳过；状态中的 `watched_dirs` 只统计可解析的路径。
    pub fn new(projects: Vec<(String, PathBuf)>, pid_file: PathBuf) -> Self {
        let watched_dirs: Vec<PathBuf> = projects
            .iter()
            .map(|(_, root)| root.clone())
            .filter(|p| p.exists() && p.is_dir())
            .collect();

        Self {
            projects,
            watched_dirs,
            pid_file,
            debounce_ms: 100,
            running: Arc::new(AtomicBool::new(false)),
            events_processed: Arc::new(AtomicU64::new(0)),
        }
    }

    // ------------------------------------------------------------------
    // 公共辅助函数
    // ------------------------------------------------------------------

    /// 当 `path` 指向值得跟踪的源码文件时返回 `true`。
    ///
    /// 当文件的扩展名在 [`SOURCE_EXTENSIONS`] 中**且**没有任何路径
    /// 组件匹配 [`IGNORE_DIRS`] 中的条目时，该文件被视为源码。
    pub fn is_source_file(path: &Path) -> bool {
        // ---- 扩展名检查 ----
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SOURCE_EXTENSIONS.contains(&ext) {
            return false;
        }

        // ---- 忽略目录检查（遍历组件） ----
        for component in path.components() {
            if let Component::Normal(name) = component {
                if let Some(s) = name.to_str() {
                    if IGNORE_DIRS.contains(&s) {
                        return false;
                    }
                }
            }
        }

        true
    }

    // ------------------------------------------------------------------
    // 生命周期
    // ------------------------------------------------------------------

    /// 启动后台监听线程。
    ///
    /// 写入 PID 文件，然后派生一个持有 `notify` 监听实例的专用 OS 线程。
    /// 返回一个 Tokio 多生产者单消费者接收器，产出 [`FileChangeEvent`] 值。
    ///
    /// # 错误
    ///
    /// 若监听器已在运行或 PID 文件无法写入，则返回 `Err`。
    pub fn start(&self) -> anyhow::Result<mpsc::UnboundedReceiver<FileChangeEvent>> {
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("监听器已在运行");
        }

        // ---- PID 文件 ----
        let pid = std::process::id();
        if let Some(parent) = self.pid_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::File::create(&self.pid_file)?;
        writeln!(f, "{pid}")?;

        // ---- notify 线程与 tokio 世界之间的通道 ----
        let (tx, rx) = mpsc::unbounded_channel();

        // ---- 为线程克隆 Arc ----
        let projects = self.projects.clone();
        let watched_dirs = self.watched_dirs.clone();
        let debounce_ms = self.debounce_ms;
        let running = Arc::clone(&self.running);
        let events_processed = Arc::clone(&self.events_processed);
        let pid_file = self.pid_file.clone();

        std::thread::Builder::new()
            .name("dt-watch".into())
            .spawn(move || {
                run_watcher_loop(
                    &projects,
                    &watched_dirs,
                    tx,
                    debounce_ms,
                    &running,
                    &events_processed,
                    &pid_file,
                );
            })?;

        Ok(rx)
    }

    /// 通过向 PID 文件中存储的 PID 发送 `SIGTERM` 来停止正在运行的
    /// 监听进程。
    ///
    /// # 平台
    ///
    /// 在 Unix 上，该函数调用 `kill -TERM <pid>`。在非 Unix 目标上
    /// 返回错误。
    pub fn stop(&self) -> anyhow::Result<()> {
        let pid_str = fs::read_to_string(&self.pid_file)
            .map_err(|e| anyhow::anyhow!("无法读取 PID 文件 {}: {e}", self.pid_file.display()))?;
        let pid: i32 = pid_str
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{} 中的 PID 无效: {e}", self.pid_file.display()))?;

        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()?;
            if !status.success() {
                anyhow::bail!("kill -TERM {pid} 返回非零退出码");
            }
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = pid;
            anyhow::bail!("stop 仅在 Unix 系统上受支持");
        }
    }

    /// 返回监听器状态的快照。
    ///
    /// `running` 标志由 PID 文件中的 PID 是否对应一个存活进程
    /// （在 Linux 上通过 `/proc/<pid>` 检查）得出。
    pub fn status(&self) -> WatcherStatus {
        let pid = fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        let running = pid.is_some_and(|p| {
            // 检查 /proc/<pid> — 不依赖 libc 即可跨 Linux 使用。
            Path::new(&format!("/proc/{p}")).exists()
        });

        WatcherStatus {
            running,
            pid,
            watched_dirs: self.watched_dirs.len(),
            events_processed: self.events_processed.load(Ordering::SeqCst),
        }
    }
}

// ---------------------------------------------------------------------------
// 监听线程内部循环
// ---------------------------------------------------------------------------

/// 在专用 OS 线程中运行。创建 `notify` 监听器，订阅所有项目根目录，
/// 并将防抖后的源码事件送入 `tx`。
fn run_watcher_loop(
    projects: &[(String, PathBuf)],
    watched_dirs: &[PathBuf],
    tx: mpsc::UnboundedSender<FileChangeEvent>,
    debounce_ms: u64,
    running: &AtomicBool,
    events_processed: &AtomicU64,
    pid_file: &Path,
) {
    // ---- 通过 MPSC 桥接创建 notify 监听器 ----
    let (notify_tx, notify_rx) = std::sync::mpsc::channel::<Event>();

    let mut watcher = match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let _ = notify_tx.send(event);
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("创建文件监听器失败: {e}");
            running.store(false, Ordering::SeqCst);
            let _ = fs::remove_file(pid_file);
            return;
        }
    };

    // ---- 订阅项目根目录 ----
    let mut watched_count = 0usize;
    for dir in watched_dirs {
        match watcher.watch(dir, RecursiveMode::Recursive) {
            Ok(()) => {
                watched_count += 1;
                tracing::info!("正在监听: {}", dir.display());
            }
            Err(e) => {
                tracing::warn!("监听 {} 失败: {e}", dir.display());
            }
        }
    }

    if watched_count == 0 {
        tracing::error!("没有可监听的目录 — 退出");
        running.store(false, Ordering::SeqCst);
        let _ = fs::remove_file(pid_file);
        return;
    }

    tracing::info!("文件监听器已启动 — {watched_count} 个目录");

    // ---- 防抖状态 ----
    let debounce_duration = Duration::from_millis(debounce_ms);
    let mut last_event: HashMap<PathBuf, Instant> = HashMap::new();

    // ---- 主事件循环 ----
    loop {
        let event = match notify_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(e) => e,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 周期性刷新过期的防抖条目以限制内存。
                let now = Instant::now();
                last_event.retain(|_, t| now.duration_since(*t) < debounce_duration * 5);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // ---- 一次性对事件类型分类 ----
        let kind = classify_event_kind(&event.kind);

        for path in &event.paths {
            // 源码文件过滤
            if !FileWatcher::is_source_file(path) {
                continue;
            }

            // 防抖
            let now = Instant::now();
            if let Some(last) = last_event.get(path) {
                if now.duration_since(*last) < debounce_duration {
                    continue;
                }
            }
            last_event.insert(path.clone(), now);

            // 解析项目
            let (project_name, project_root) = match resolve_project(path, projects) {
                Some(p) => p,
                None => continue,
            };

            // 跳过非文件事件（如仅元数据）
            let Some(kind) = kind else {
                continue;
            };

            let change = FileChangeEvent {
                project_name,
                project_root,
                file_path: path.clone(),
                kind,
            };

            if tx.send(change).is_err() {
                // 消费方丢弃了接收器 — 有序关闭。
                break;
            }
            events_processed.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ---- 清理 ----
    running.store(false, Ordering::SeqCst);
    let _ = fs::remove_file(pid_file);
    tracing::info!("文件监听器已停止");
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 将 `notify::EventKind` 映射到我们的简化枚举。
///
/// 对不表示实际文件内容变更的事件类型（如 `Access`、`Other`）
/// 返回 `None`。
fn classify_event_kind(kind: &EventKind) -> Option<FileChangeKind> {
    match kind {
        EventKind::Create(_) => Some(FileChangeKind::Created),
        EventKind::Modify(_) => Some(FileChangeKind::Modified),
        EventKind::Remove(_) => Some(FileChangeKind::Deleted),
        _ => None,
    }
}

/// 通过最长前缀匹配找到拥有 `file_path` 的项目。
fn resolve_project(file_path: &Path, projects: &[(String, PathBuf)]) -> Option<(String, PathBuf)> {
    projects
        .iter()
        .filter(|(_, root)| file_path.starts_with(root))
        .max_by_key(|(_, root)| root.as_os_str().len())
        .cloned()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // is_source_file
    // ------------------------------------------------------------------

    #[test]
    fn source_file_recognised_by_extension() {
        for ext in SOURCE_EXTENSIONS {
            let path = PathBuf::from(format!("src/main.{ext}"));
            assert!(FileWatcher::is_source_file(&path), "扩展名 .{ext} 应被识别");
        }
    }

    #[test]
    fn non_source_files_are_filtered() {
        assert!(!FileWatcher::is_source_file(Path::new("logo.png")));
        assert!(!FileWatcher::is_source_file(Path::new("data.json")));
        assert!(!FileWatcher::is_source_file(Path::new("README.md")));
        assert!(!FileWatcher::is_source_file(Path::new("Dockerfile")));
    }

    #[test]
    fn ignore_dir_components_block_file() {
        assert!(!FileWatcher::is_source_file(Path::new(
            "node_modules/react/index.js"
        )));
        assert!(!FileWatcher::is_source_file(Path::new(
            "target/debug/build/output.rs"
        )));
        assert!(!FileWatcher::is_source_file(Path::new(
            ".git/hooks/pre-commit.ts"
        )));
        assert!(!FileWatcher::is_source_file(Path::new("dist/bundle.js")));
    }

    #[test]
    fn deep_nested_source_is_ok() {
        assert!(FileWatcher::is_source_file(Path::new(
            "src/main/java/com/example/Foo.java"
        )));
    }

    // ------------------------------------------------------------------
    // classify_event_kind
    // ------------------------------------------------------------------

    #[test]
    fn classify_creates_modifies_removes() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};

        assert_eq!(
            classify_event_kind(&EventKind::Create(CreateKind::File)),
            Some(FileChangeKind::Created)
        );
        assert_eq!(
            classify_event_kind(&EventKind::Create(CreateKind::Folder)),
            Some(FileChangeKind::Created)
        );

        assert_eq!(
            classify_event_kind(&EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Any
            ))),
            Some(FileChangeKind::Modified)
        );
        assert_eq!(
            classify_event_kind(&EventKind::Modify(ModifyKind::Metadata(
                notify::event::MetadataKind::Any
            ))),
            Some(FileChangeKind::Modified)
        );

        assert_eq!(
            classify_event_kind(&EventKind::Remove(RemoveKind::File)),
            Some(FileChangeKind::Deleted)
        );
        assert_eq!(
            classify_event_kind(&EventKind::Remove(RemoveKind::Folder)),
            Some(FileChangeKind::Deleted)
        );
    }

    #[test]
    fn classify_ignores_access_and_other() {
        use notify::event::AccessKind;

        assert_eq!(
            classify_event_kind(&EventKind::Access(AccessKind::Close(
                notify::event::AccessMode::Write
            ))),
            None
        );
        assert_eq!(classify_event_kind(&EventKind::Other), None);
    }

    // ------------------------------------------------------------------
    // resolve_project
    // ------------------------------------------------------------------

    #[test]
    fn resolve_project_by_prefix() {
        let projects = vec![
            ("a".into(), PathBuf::from("/data/projects/a")),
            ("b".into(), PathBuf::from("/data/projects/b")),
        ];

        assert_eq!(
            resolve_project(Path::new("/data/projects/a/src/main.rs"), &projects).map(|(n, _)| n),
            Some("a".into())
        );
        assert_eq!(
            resolve_project(Path::new("/data/projects/b/pkg/main.go"), &projects).map(|(n, _)| n),
            Some("b".into())
        );
    }

    #[test]
    fn resolve_project_longest_prefix_wins() {
        let projects = vec![
            ("outer".into(), PathBuf::from("/data")),
            ("inner".into(), PathBuf::from("/data/projects/x")),
        ];

        let (name, _) =
            resolve_project(Path::new("/data/projects/x/src/main.rs"), &projects).unwrap();
        assert_eq!(name, "inner", "最长前缀应匹配 inner");
    }

    #[test]
    fn resolve_project_returns_none_for_unmatched() {
        let projects = vec![("a".into(), PathBuf::from("/data/projects/a"))];
        assert!(resolve_project(Path::new("/other/main.rs"), &projects).is_none());
    }

    // ------------------------------------------------------------------
    // FileWatcher 构造与状态
    // ------------------------------------------------------------------

    #[test]
    fn watcher_new_with_missing_dirs() {
        let projects = vec![("test".into(), PathBuf::from("/tmp/dt-watcher-nonexistent"))];
        let pid_file = PathBuf::from("/tmp/dt-watch-test-nonexistent.pid");
        let watcher = FileWatcher::new(projects, pid_file);

        let status = watcher.status();
        assert!(!status.running);
        assert_eq!(status.watched_dirs, 0);
        assert_eq!(status.events_processed, 0);
    }

    #[test]
    fn watcher_new_with_existing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let projects = vec![("test".into(), root)];
        let pid_file = dir.path().join("dt-watch.pid");
        let watcher = FileWatcher::new(projects, pid_file);

        let status = watcher.status();
        assert!(!status.running);
        assert_eq!(status.watched_dirs, 1);
        assert_eq!(status.events_processed, 0);
    }

    #[test]
    fn watcher_status_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let projects = vec![("test".into(), dir.path().to_path_buf())];
        let pid_file = dir.path().join("dt-watch-nonexistent.pid");
        let watcher = FileWatcher::new(projects, pid_file);

        let status = watcher.status();
        assert!(!status.running);
        assert!(status.pid.is_none());
    }

    // ------------------------------------------------------------------
    // FileChangeEvent / FileChangeKind
    // ------------------------------------------------------------------

    #[test]
    fn change_event_fields() {
        let ev = FileChangeEvent {
            project_name: "proj".into(),
            project_root: PathBuf::from("/proj"),
            file_path: PathBuf::from("/proj/src/main.rs"),
            kind: FileChangeKind::Modified,
        };
        assert_eq!(ev.project_name, "proj");
        assert_eq!(ev.kind, FileChangeKind::Modified);
    }

    #[test]
    fn change_kind_op_types() {
        assert_eq!(FileChangeKind::Created.as_op_type(), "create");
        assert_eq!(FileChangeKind::Modified.as_op_type(), "modify");
        assert_eq!(FileChangeKind::Deleted.as_op_type(), "delete");
    }
}
