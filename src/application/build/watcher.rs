//! File watcher daemon — monitors project source directories for changes.
//!
//! Uses the `notify` crate for cross-platform file-system events (inotify on
//! Linux, FSEvents on macOS, kqueue on BSD).  Events are debounced (100 ms)
//! so that rapid successive writes to the same file (typical of editors and
//! build tools) are collapsed into a single `dt update` call.
//!
//! # Architecture
//!
//! 1. `FileWatcher::start()` spawns a dedicated OS thread that owns a
//!    `notify::RecommendedWatcher`.  The watcher thread feeds raw FS events
//!    into a `std::sync::mpsc` channel.
//! 2. An inner loop reads from that channel, filters for source-code files,
//!    applies debounce, resolves the owning project, and pushes
//!    `FileChangeEvent` values onto a `tokio::sync::mpsc` channel.
//! 3. The consumer (typically `dt-daemon`'s `watch` subcommand) reads from
//!    that channel and dispatches `UpdateRunner::run()` for each event.
//!
//! # PID file
//!
//! On start, the watcher writes its OS process-id to `/var/run/dt-watch.pid`.
//! `dt watch --status` and `dt watch --stop` use this file to inspect or
//! signal the daemon process.

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
// Constants
// ---------------------------------------------------------------------------

/// Source-code file extensions that the watcher monitors.
const SOURCE_EXTENSIONS: &[&str] = &[
    "java", "py", "ts", "tsx", "go", "rs", "php", "js", "jsx", "mjs", "cjs", "kt", "kts", "swift",
    "scala", "rb", "cpp", "cc", "cxx", "c", "h", "hpp", "cs", "fs", "fsx", "vue", "svelte",
];

/// Directory names whose contents are never watched.
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
// Public types
// ---------------------------------------------------------------------------

/// A single file-system change event emitted by the watcher.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    /// Logical project name (from config.yaml).
    pub project_name: String,
    /// Project root directory (absolute path).
    pub project_root: PathBuf,
    /// Changed file (absolute path).
    pub file_path: PathBuf,
    /// What happened to the file.
    pub kind: FileChangeKind,
}

/// Granularity of a file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Created,
    Modified,
    Deleted,
}

impl FileChangeKind {
    /// Human-readable label matching the `--type` values accepted by
    /// `UpdateRunner`.
    pub fn as_op_type(&self) -> &'static str {
        match self {
            FileChangeKind::Created => "create",
            FileChangeKind::Modified => "modify",
            FileChangeKind::Deleted => "delete",
        }
    }
}

/// Snapshot of the watcher's current state.
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

/// Cross-platform file-system watcher that monitors project source trees.
///
/// # Lifecycle
///
/// ```ignore
/// let watcher = FileWatcher::new(projects, pid_file);
/// let mut rx = watcher.start()?;              // spawns OS thread
/// while let Some(event) = rx.recv().await {
///     // dispatch event to update pipeline
/// }
/// // Drop rx → watcher thread exits & removes PID file.
/// ```
pub struct FileWatcher {
    /// (project_name, project_root) pairs.
    projects: Vec<(String, PathBuf)>,
    /// Subset of `projects` roots that actually exist on disk.
    watched_dirs: Vec<PathBuf>,
    /// Path of the PID file.
    pid_file: PathBuf,
    /// Debounce window in milliseconds.
    debounce_ms: u64,
    /// Set to `true` when the watcher thread is active.
    running: Arc<AtomicBool>,
    /// Monotonically incremented for every event emitted.
    events_processed: Arc<AtomicU64>,
}

impl FileWatcher {
    // ------------------------------------------------------------------
    // Constructors
    // ------------------------------------------------------------------

    /// Create a new watcher that monitors the given project directories.
    ///
    /// `projects` is a list of `(name, root_path)` tuples.  Directories that
    /// do not exist are silently skipped; `watched_dirs` in the status will
    /// only count resolvable paths.
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
    // Public helpers
    // ------------------------------------------------------------------

    /// Returns `true` when `path` refers to a source-code file worth tracking.
    ///
    /// A file is considered source code when its extension is in
    /// [`SOURCE_EXTENSIONS`] **and** no path component matches any entry in
    /// [`IGNORE_DIRS`].
    pub fn is_source_file(path: &Path) -> bool {
        // ---- Extension check ----
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SOURCE_EXTENSIONS.contains(&ext) {
            return false;
        }

        // ---- Ignored-directory check (walk components) ----
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
    // Lifecycle
    // ------------------------------------------------------------------

    /// Start the background watcher thread.
    ///
    /// Writes the PID file, then spawns a dedicated OS thread that owns the
    /// `notify` watcher instance.  Returns a Tokio multi-producer
    /// single-consumer receiver that yields [`FileChangeEvent`] values.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the watcher is already running or the PID file
    /// cannot be written.
    pub fn start(&self) -> anyhow::Result<mpsc::UnboundedReceiver<FileChangeEvent>> {
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("watcher is already running");
        }

        // ---- PID file ----
        let pid = std::process::id();
        if let Some(parent) = self.pid_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::File::create(&self.pid_file)?;
        writeln!(f, "{pid}")?;

        // ---- Channel between the notify thread and the tokio world ----
        let (tx, rx) = mpsc::unbounded_channel();

        // ---- Clone Arcs for the thread ----
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

    /// Stop a running watcher process by sending `SIGTERM` to the PID stored
    /// in the PID file.
    ///
    /// # Platform
    ///
    /// On Unix the function invokes `kill -TERM <pid>`.  On non-Unix targets
    /// it returns an error.
    pub fn stop(&self) -> anyhow::Result<()> {
        let pid_str = fs::read_to_string(&self.pid_file).map_err(|e| {
            anyhow::anyhow!("cannot read PID file {}: {e}", self.pid_file.display())
        })?;
        let pid: i32 = pid_str
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid PID in {}: {e}", self.pid_file.display()))?;

        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()?;
            if !status.success() {
                anyhow::bail!("kill -TERM {pid} returned non-zero exit");
            }
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        #[cfg(not(unix))]
        {
            let _ = pid;
            anyhow::bail!("stop is only supported on Unix systems");
        }
    }

    /// Return a snapshot of the watcher's state.
    ///
    /// The `running` flag is derived from whether the PID in the PID file
    /// corresponds to a live process (checked via `/proc/<pid>` on Linux).
    pub fn status(&self) -> WatcherStatus {
        let pid = fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());

        let running = pid.is_some_and(|p| {
            // Check /proc/<pid> — portable across Linux without libc dependency.
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
// Watcher thread inner loop
// ---------------------------------------------------------------------------

/// Runs in a dedicated OS thread.  Creates a `notify` watcher, subscribes to
/// all project roots, and pumps debounced source-file events into `tx`.
fn run_watcher_loop(
    projects: &[(String, PathBuf)],
    watched_dirs: &[PathBuf],
    tx: mpsc::UnboundedSender<FileChangeEvent>,
    debounce_ms: u64,
    running: &AtomicBool,
    events_processed: &AtomicU64,
    pid_file: &Path,
) {
    // ---- Create notify watcher with an MPSC bridge ----
    let (notify_tx, notify_rx) = std::sync::mpsc::channel::<Event>();

    let mut watcher = match notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let _ = notify_tx.send(event);
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("failed to create file watcher: {e}");
            running.store(false, Ordering::SeqCst);
            let _ = fs::remove_file(pid_file);
            return;
        }
    };

    // ---- Subscribe to project roots ----
    let mut watched_count = 0usize;
    for dir in watched_dirs {
        match watcher.watch(dir, RecursiveMode::Recursive) {
            Ok(()) => {
                watched_count += 1;
                tracing::info!("watching: {}", dir.display());
            }
            Err(e) => {
                tracing::warn!("failed to watch {}: {e}", dir.display());
            }
        }
    }

    if watched_count == 0 {
        tracing::error!("no directories to watch — exiting");
        running.store(false, Ordering::SeqCst);
        let _ = fs::remove_file(pid_file);
        return;
    }

    tracing::info!("file watcher started — {watched_count} directories");

    // ---- Debounce state ----
    let debounce_duration = Duration::from_millis(debounce_ms);
    let mut last_event: HashMap<PathBuf, Instant> = HashMap::new();

    // ---- Main event loop ----
    loop {
        let event = match notify_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(e) => e,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Periodically flush stale debounce entries to bound memory.
                let now = Instant::now();
                last_event.retain(|_, t| now.duration_since(*t) < debounce_duration * 5);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // ---- Classify the event kind once ----
        let kind = classify_event_kind(&event.kind);

        for path in &event.paths {
            // Source-file filter
            if !FileWatcher::is_source_file(path) {
                continue;
            }

            // Debounce
            let now = Instant::now();
            if let Some(last) = last_event.get(path) {
                if now.duration_since(*last) < debounce_duration {
                    continue;
                }
            }
            last_event.insert(path.clone(), now);

            // Resolve project
            let (project_name, project_root) = match resolve_project(path, projects) {
                Some(p) => p,
                None => continue,
            };

            // Skip non-file events (e.g. metadata-only)
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
                // Consumer dropped the receiver — orderly shutdown.
                break;
            }
            events_processed.fetch_add(1, Ordering::Relaxed);
        }
    }

    // ---- Cleanup ----
    running.store(false, Ordering::SeqCst);
    let _ = fs::remove_file(pid_file);
    tracing::info!("file watcher stopped");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a `notify::EventKind` to our simplified enum.
///
/// Returns `None` for event kinds that don't represent actual file content
/// changes (e.g. `Access`, `Other`).
fn classify_event_kind(kind: &EventKind) -> Option<FileChangeKind> {
    match kind {
        EventKind::Create(_) => Some(FileChangeKind::Created),
        EventKind::Modify(_) => Some(FileChangeKind::Modified),
        EventKind::Remove(_) => Some(FileChangeKind::Deleted),
        _ => None,
    }
}

/// Find the project that owns `file_path` by longest-prefix match.
fn resolve_project(file_path: &Path, projects: &[(String, PathBuf)]) -> Option<(String, PathBuf)> {
    projects
        .iter()
        .filter(|(_, root)| file_path.starts_with(root))
        .max_by_key(|(_, root)| root.as_os_str().len())
        .cloned()
}

// ---------------------------------------------------------------------------
// Tests
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
            assert!(
                FileWatcher::is_source_file(&path),
                "extension .{ext} should be recognised"
            );
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
        assert_eq!(name, "inner", "longest prefix should match inner");
    }

    #[test]
    fn resolve_project_returns_none_for_unmatched() {
        let projects = vec![("a".into(), PathBuf::from("/data/projects/a"))];
        assert!(resolve_project(Path::new("/other/main.rs"), &projects).is_none());
    }

    // ------------------------------------------------------------------
    // FileWatcher construction & status
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
