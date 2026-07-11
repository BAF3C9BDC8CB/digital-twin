//! Tracing-subscriber initialization with JSON-structured file output.
//!
//! Configures a layered subscriber:
//! - Layer 1: env-filter for dynamic level control (`RUST_LOG` / `DT_LOG_LEVEL`)
//! - Layer 2: JSON-format writer to a file
//! - Layer 3: stderr fallback (human-readable) for development

use std::fs;
use std::path::PathBuf;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default log directory. If unwritable, falls back to `/tmp`.
const LOG_DIR: &str = "/var/log/digital-twin";

/// Default log file name.
const LOG_FILE: &str = "dt-daemon.log";

/// Initialise the unified logging pipeline.
///
/// # Write targets
///
/// 1. **Primary**: JSON lines → `$LOG_DIR/dt-daemon.log`.
/// 2. **Fallback**: If `LOG_DIR` is unwritable, writes to `/tmp/dt-daemon.log`.
/// 3. **Stderr**: Human-readable compact output for development.
///
/// # Environment
///
/// | Variable        | Default              | Description                        |
/// |-----------------|----------------------|------------------------------------|
/// | `DT_LOG_DIR`    | `/var/log/digital-twin` | Override log directory          |
/// | `RUST_LOG`      | `info`               | Per-module filter (tracing EnvFilter) |
/// | `DT_LOG_LEVEL`  | `info`               | Fallback when RUST_LOG is unset      |
pub fn init_logging() -> anyhow::Result<()> {
    // Resolve the log directory + create if needed
    let log_dir = std::env::var("DT_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(LOG_DIR));

    let log_file = if log_dir.exists() || fs::create_dir_all(&log_dir).is_ok() {
        log_dir.join(LOG_FILE)
    } else {
        eprintln!(
            "[dt-log] cannot create {}, falling back to /tmp/dt-daemon.log",
            log_dir.display()
        );
        PathBuf::from("/tmp/dt-daemon.log")
    };

    // Open (or create) the log file in append mode
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| anyhow::anyhow!("failed to open log file {}: {}", log_file.display(), e))?;

    // ── Env filter ──────────────────────────────────────────────────
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| {
            tracing_subscriber::EnvFilter::try_new(
                std::env::var("DT_LOG_LEVEL").unwrap_or_else(|_| "info".into()),
            )
        })
        .unwrap_or_else(|_| "info".into());

    // ── JSON file layer ────────────────────────────────────────────
    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_writer(file)
        .with_ansi(false);

    // ── Stderr layer (compact, human-readable) ──────────────────────
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(true)
        .compact();

    // ── Assemble ────────────────────────────────────────────────────
    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(stderr_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("tracing subscriber init failed: {}", e))?;

    tracing::info!(
        log_path = %log_file.display(),
        "logging initialised"
    );

    Ok(())
}
