//! `dt logs` — 统一查看 digital-twin 日志（纯文本格式，tail/grep 语义）。
//!
//! 用法:
//! ```text
//! dt logs               # 打印最后 50 行
//! dt logs -f            # 实时跟随（类似 tail -f），Ctrl-C 退出
//! dt logs -k 搜索       # 只显示包含"搜索"的行（-k 可重复，多关键词取并集）
//! dt logs -n 200        # 显示最后 200 行
//! dt logs -f -k 搜索    # 跟随并只看搜索日志
//! ```
//!
//! 日志文件解析与 `init_logging` 一致：`DT_LOG_DIR`（默认 /var/log/digital-twin）/dt.log，
//! 目录不可用时回退 /tmp/dt.log。`dt.log` 软链恒指向当天文件；follow 模式检测
//! 到轮转（日期切换/软链指向变化）后自动重开新文件继续，不中断。

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 与 init_logging 一致的日志路径解析（读场景）。
fn resolve_log_path() -> PathBuf {
    let dir = std::env::var("DT_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/log/digital-twin"));
    if dir.is_dir() {
        dir.join("dt.log")
    } else {
        PathBuf::from("/tmp/dt.log")
    }
}

/// 读取文件末尾最多 `lines` 行。
fn tail_lines(path: &Path, lines: usize) -> std::io::Result<Vec<String>> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    if size == 0 {
        return Ok(Vec::new());
    }
    // 从末尾向前扫 1MiB 窗口（50 行普通文本日志远小于此），不足则整文件。
    let start = size.saturating_sub(1024 * 1024);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let lines_all: Vec<&str> = text.lines().collect();
    let tail: Vec<String> = lines_all
        .iter()
        .rev()
        .take(lines)
        .rev()
        .map(|s| s.to_string())
        .collect();
    Ok(tail)
}

/// 从 offset 起读取新增字节，按行拆分返回。
fn read_increment(f: &mut File, offset: u64, size: u64) -> std::io::Result<Vec<String>> {
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity((size - offset) as usize);
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    Ok(text.lines().map(|s| s.to_string()).collect())
}

/// 行是否命中关键词（任一命中即显示；空关键词 = 显示全部）。
fn matches(row: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return true;
    }
    let lower = row.to_lowercase();
    keywords
        .iter()
        .any(|k| row.contains(k) || lower.contains(&k.to_lowercase()))
}

/// 当前文件身份（inode），用于轮转检测。
fn file_ident(path: &Path) -> Option<(u64, u64)> {
    let md = fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((md.ino(), md.dev()))
    }
    #[cfg(not(unix))]
    {
        Some((md.len(), 0))
    }
}

/// `dt logs` 主入口。
pub fn handle_logs(follow: bool, keywords: &[String], lines: usize) -> anyhow::Result<()> {
    let path = resolve_log_path();
    if !path.is_file() {
        anyhow::bail!(
            "日志文件不存在: {} (可用 DT_LOG_DIR 指定实际目录)",
            path.display()
        );
    }

    let n = lines.max(1);
    let tail = tail_lines(&path, n)?;
    for row in tail {
        if matches(&row, keywords) {
            println!("{row}");
        }
    }

    if !follow {
        return Ok(());
    }

    // ── 实时跟随 ─────────────────────────────────────────────
    let mut seen: u64 = fs::metadata(&path)?.len();
    let mut ident = file_ident(&path);
    loop {
        std::thread::sleep(Duration::from_millis(500));

        // 轮转检测：inode/dev 变化 → 重开新文件，从头读
        let cur = file_ident(&path);
        if cur.is_some() && cur != ident {
            println!("--- dt.log 已轮转，切换到新文件 ---");
            seen = 0;
            ident = cur;
        }

        if let Ok(md) = fs::metadata(&path) {
            let size = md.len();
            if size > seen {
                if let Ok(mut f) = File::open(&path) {
                    match read_increment(&mut f, seen, size) {
                        Ok(rows) => {
                            for row in rows {
                                if matches(&row, keywords) {
                                    println!("{row}");
                                }
                            }
                        }
                        Err(e) => eprintln!("dt logs: 读取增量失败: {e}"),
                    }
                }
                seen = size;
            }
        }
    }
}
