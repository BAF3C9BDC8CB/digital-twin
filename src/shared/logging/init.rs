//! Tracing-subscriber 初始化，带 JSON 结构化文件输出（异步写入 + 日期/大小轮转）。
//!
//! 配置分层的 subscriber：
//! - 第 1 层：env-filter，用于动态级别控制（`RUST_LOG` / `DT_LOG_LEVEL`）
//! - 第 2 层：写入文件的 JSON 格式输出器（non-blocking 异步 + RotatingWriter 轮转）
//! - 第 3 层：stderr 兜底（人类可读，non-blocking 异步通道），供开发使用
//!
//! # 异步语义
//!
//! 文件层与 stderr 层均通过 `tracing_appender::non_blocking` 异步写入：
//! 调用方 `tracing::info!` 只把事件投递到内存队列即返回，由后台 worker
//! 线程负责实际落盘——日志写入不会阻塞业务操作。
//! `LogGuard` 持有 worker guard，进程退出（drop）时冲刷队列，保证日志不丢。
//!
//! # 文件轮转（RotatingWriter）
//!
//! - 日期维度：每天一个 `dt.yyyy-MM-dd.log`（本地时区午夜切换）。
//! - 大小维度：单文件超阈值后同日追加序号 `dt.yyyy-MM-dd.1.log`…。
//! - 保留：目录内日志文件总数超限删最旧；`dt.log` 软链恒指向当前文件。

use std::fs;
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use crate::shared::logging::rotating::{RotatingWriter, DEFAULT_MAX_BYTES, DEFAULT_MAX_FILES};

/// 持有 non-blocking worker 的 guard，进程结束（drop）时冲刷队列。
///
/// 必须存活到进程退出（如 `main` 中的 `let _guard = init_logging()?;`），
/// 否则提前 drop 会导致队列中的日志未落盘即被丢弃。
pub struct LogGuard {
    /// 文件层 worker guard。
    _file: WorkerGuard,
    /// stderr 层 worker guard。
    _stderr: WorkerGuard,
}

/// 使用本地时区（而非 UTC）记录时间戳。
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = chrono::Local::now();
        write!(w, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3f%:z"))
    }
}

/// 默认日志目录。若不可写，则回退到 `/tmp`。
const LOG_DIR: &str = "/var/log/digital-twin";

/// 初始化统一日志管线（异步写入）。
///
/// # 写入目标
///
/// 1. **主要**：JSON 行 → `$LOG_DIR/dt.log`（non-blocking 异步）。
/// 2. **兜底**：若 `LOG_DIR` 不可写，则写入 `/tmp/dt.log`。
/// 3. **stderr**：供开发使用的人类可读紧凑输出（non-blocking 异步）。
///
/// # 环境变量
///
/// | 变量                     | 默认值                | 说明                               |
/// |--------------------------|----------------------|------------------------------------|
/// | `DT_LOG_DIR`             | `/var/log/digital-twin` | 覆盖日志目录                   |
/// | `RUST_LOG`               | `info`               | 按模块过滤（tracing EnvFilter）    |
/// | `DT_LOG_LEVEL`           | `info`               | RUST_LOG 未设置时的兜底值          |
/// | `DT_LOG_STDERR`          | `warn`               | stderr 层级别（debug 恢复详细输出）|
/// | `DT_LOG_MAX_BYTES`       | `52428800` (50 MiB)  | 单文件大小阈值,超限同日切序号文件  |
/// | `DT_LOG_RETENTION_FILES` | `30`                 | 目录内日志文件总数上限,超出删最旧  |
pub fn init_logging() -> anyhow::Result<LogGuard> {
    // 解析日志目录，必要时创建；不可写时静默回退 /tmp（不污染终端）
    let log_dir = std::env::var("DT_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(LOG_DIR));
    let log_dir = if log_dir.exists() || fs::create_dir_all(&log_dir).is_ok() {
        log_dir
    } else {
        PathBuf::from("/tmp")
    };

    // ── 轮转参数（env 可覆盖）─────────────────────────────────────
    let max_bytes = std::env::var("DT_LOG_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_BYTES);
    let max_files = std::env::var("DT_LOG_RETENTION_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_FILES);

    let rotating = RotatingWriter::new(log_dir.clone(), max_bytes, max_files)?;

    // ── 异步写入通道（non-blocking）──────────────────────────────
    // 事件投递到内存队列即返回，worker 线程负责实际写入；
    // guard 在进程退出时冲刷队列，保证日志不丢。
    let (file_writer, file_guard) = tracing_appender::non_blocking(rotating);
    let (stderr_writer, stderr_guard) = tracing_appender::non_blocking(std::io::stderr());

    // ── 环境过滤 ──────────────────────────────────────────────────
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| {
            tracing_subscriber::EnvFilter::try_new(
                std::env::var("DT_LOG_LEVEL").unwrap_or_else(|_| "info".into()),
            )
        })
        .unwrap_or_else(|_| "info".into());

    // ── JSON 文件层 ────────────────────────────────────────────
    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_writer(file_writer)
        .with_ansi(false)
        .with_timer(LocalTimer);

    // ── stderr 层（紧凑、人类可读）──────────────────────────
    // 默认只输出 WARN 及以上，INFO 走日志文件；DT_LOG_STDERR 可覆盖。
    // 注意：fmt layer 默认写 stdout——必须显式指定 stderr（U-D4 stdout 纯净约束）。
    let stderr_filter_str = std::env::var("DT_LOG_STDERR").unwrap_or_else(|_| "warn".into());
    let stderr_filter = tracing_subscriber::EnvFilter::try_new(&stderr_filter_str)
        .unwrap_or_else(|_| "warn".into());
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(true)
        .compact()
        .with_writer(stderr_writer)
        .with_timer(LocalTimer)
        .with_filter(stderr_filter);

    // ── 组装 ────────────────────────────────────────────────────
    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing subscriber 初始化失败：{}", e))?;

    tracing::info!(
        log_dir = %log_dir.display(),
        "日志初始化完成（异步写入 + 日期/大小轮转）"
    );

    Ok(LogGuard {
        _file: file_guard,
        _stderr: stderr_guard,
    })
}
