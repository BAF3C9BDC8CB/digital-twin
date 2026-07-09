use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::config;

/// dt-embed CLI 命令名
const EMBED_CMD: &str = "dt-embed";
/// Unix domain socket 路径（与 Python daemon 保持一致）
const SOCKET_PATH: &str = "/tmp/dt-embed.sock";
/// 等待 daemon 启动的最大时间
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(30);
/// 轮询间隔
const POLL_INTERVAL: Duration = Duration::from_millis(200);

// ── 帧协议 ────────────────────────────────────────────────────────────────
// 4 字节大端长度 + JSON payload

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_frame(stream: &mut UnixStream, data: &[u8]) -> Result<()> {
    let len = data.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(data)?;
    stream.flush()?;
    Ok(())
}

// ── Daemon 管理 ────────────────────────────────────────────────────────────

/// 检查 daemon 是否在运行（尝试连接 socket）
fn daemon_running() -> bool {
    UnixStream::connect(SOCKET_PATH).is_ok()
}

/// 启动 dt-embed --daemon 作为独立后台进程
fn start_daemon() -> Result<()> {
    let cmd = std::env::var("EMBED_CMD").unwrap_or_else(|_| EMBED_CMD.to_string());

    eprintln!("[dt_embed.daemon] 启动 {} --daemon (后台进程)...", cmd);

    // 使用 nohup 风格启动：完全脱离父进程
    let child = Command::new(&cmd)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("启动 {} --daemon 失败: {}（请确认 dt-embed 已安装）", cmd, e))?;

    // 不等待子进程，让它成为孤儿进程（由 init 收养）
    // child 被 drop 后进程继续运行
    drop(child);

    // 轮询等待 socket 出现
    let start = std::time::Instant::now();
    while start.elapsed() < DAEMON_START_TIMEOUT {
        if daemon_running() {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    Err(anyhow!(
        "daemon 启动超时（{}s），socket {} 未出现",
        DAEMON_START_TIMEOUT.as_secs(),
        SOCKET_PATH
    ))
}

/// 连接到 daemon，如未运行则自动启动
fn connect_daemon() -> Result<UnixStream> {
    if !daemon_running() {
        start_daemon()?;
    }
    UnixStream::connect(SOCKET_PATH)
        .map_err(|e| anyhow!("连接 daemon socket {} 失败: {}", SOCKET_PATH, e))
}

// ── 公开 API ──────────────────────────────────────────────────────────────

pub async fn health() -> Result<(String, usize)> {
    let cfg = config::load();
    // 尝试快速检测 daemon 是否在运行
    let running = UnixStream::connect(SOCKET_PATH).is_ok();
    let status = if running { "running" } else { "stopped" };
    Ok((
        format!("{} ({})", cfg.services.embed_server.model, status),
        cfg.services.embed_server.dim,
    ))
}

/// 调用 dt-embed daemon（Unix socket）编码文本，返回向量列表。
/// daemon 未运行时会自动启动。
pub async fn embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }

    let total = texts.len();
    println!("  [embed] {} texts → daemon socket", total);

    let result = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
        let mut stream = connect_daemon()?;

        // 构建请求
        let request = serde_json::json!({"texts": texts});
        let req_bytes = serde_json::to_vec(&request)?;
        write_frame(&mut stream, &req_bytes)?;

        // 设置读取超时（大量向量可能需较长时间）
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;

        // 读取响应
        let resp_bytes = read_frame(&mut stream)?;
        let response: serde_json::Value = serde_json::from_slice(&resp_bytes)?;

        if let Some(err) = response.get("error") {
            return Err(anyhow!("{}", err));
        }

        let vectors: Vec<Vec<f32>> =
            serde_json::from_value(response["vectors"].clone()).unwrap_or_default();
        Ok(vectors)
    })
    .await??;

    println!("  [embed] done: {} vectors", result.len());
    Ok(result)
}
