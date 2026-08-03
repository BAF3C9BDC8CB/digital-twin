//! Tracing-subscriber 初始化，带 JSON 结构化文件输出。
//!
//! 配置分层的 subscriber：
//! - 第 1 层：env-filter，用于动态级别控制（`RUST_LOG` / `DT_LOG_LEVEL`）
//! - 第 2 层：写入文件的 JSON 格式输出器
//! - 第 3 层：stderr 兜底（人类可读），供开发使用

use std::fs;
use std::path::PathBuf;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

/// 默认日志文件名。
const LOG_FILE: &str = "dt-daemon.log";

/// 初始化统一日志管线。
///
/// # 写入目标
///
/// 1. **主要**：JSON 行 → `$LOG_DIR/dt-daemon.log`。
/// 2. **兜底**：若 `LOG_DIR` 不可写，则写入 `/tmp/dt-daemon.log`。
/// 3. **stderr**：供开发使用的人类可读紧凑输出。
///
/// # 环境变量
///
/// | 变量            | 默认值                | 说明                               |
/// |-----------------|----------------------|------------------------------------|
/// | `DT_LOG_DIR`    | `/var/log/digital-twin` | 覆盖日志目录                   |
/// | `RUST_LOG`      | `info`               | 按模块过滤（tracing EnvFilter）    |
/// | `DT_LOG_LEVEL`  | `info`               | RUST_LOG 未设置时的兜底值          |
pub fn init_logging() -> anyhow::Result<()> {
    // 解析日志目录，必要时创建
    let log_dir = std::env::var("DT_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(LOG_DIR));

    let log_file = if log_dir.exists() || fs::create_dir_all(&log_dir).is_ok() {
        log_dir.join(LOG_FILE)
    } else {
        eprintln!(
            "[dt-log] 无法创建 {}，回退到 /tmp/dt-daemon.log",
            log_dir.display()
        );
        PathBuf::from("/tmp/dt-daemon.log")
    };

    // 以追加模式打开（或创建）日志文件
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| anyhow::anyhow!("无法打开日志文件 {}: {}", log_file.display(), e))?;

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
        .with_writer(file)
        .with_ansi(false)
        .with_timer(LocalTimer);

    // ── stderr 层（紧凑、人类可读）──────────────────────────
    // 注意：fmt layer 默认写 stdout——必须显式指定 stderr（U-D4 stdout 纯净约束）。
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(true)
        .compact()
        .with_writer(std::io::stderr)
        .with_timer(LocalTimer);

    // ── 组装 ────────────────────────────────────────────────────
    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing subscriber 初始化失败：{}", e))?;

    tracing::info!(
        log_path = %log_file.display(),
        "日志初始化完成"
    );

    Ok(())
}
