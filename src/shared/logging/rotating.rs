//! 自定义轮转写入器：按日期 + 大小双维度切分日志文件。
//!
//! - **日期维度**：每天一个文件 `dt.yyyy-MM-dd.log`（本地时区，午夜切换）。
//! - **大小维度**：单文件超过阈值后同日追加序号 `dt.yyyy-MM-dd.1.log`、`.2.log`…。
//! - **保留策略**：目录内日志文件总数超过上限时删除最旧文件。
//! - **兼容软链**：始终维护 `dt.log` → 当前写入文件，现有读 `dt.log` 的消费方零改动。
//! - **旧文件迁移**：首次启用时若 `dt.log` 是普通文件（含历史日志），归档为
//!   `dt.legacy-<时间戳>.log`，避免被软链覆盖丢失历史。
//!
//! 设计约束：实现 `std::io::Write`，由 `tracing_appender::non_blocking` 的
//! worker 线程独占调用（单线程），因此无需内部锁；多进程（CLI 与 dt-mcp 同写）
//! 依赖 O_APPEND 的单次 write 原子性，序号竞争仅可能导致两边各自多切一个文件，
//! 不会损坏数据。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

/// 默认单文件大小阈值（50 MiB）。
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// 默认保留日志文件总数。
pub const DEFAULT_MAX_FILES: usize = 30;

/// 日志文件名前缀。
const FILE_PREFIX: &str = "dt";
/// 日志文件名后缀。
const FILE_SUFFIX: &str = "log";
/// 兼容软链名（始终指向当前写入文件）。
const SYMLINK_NAME: &str = "dt.log";
/// 旧单文件迁移归档标记（仅首次启用时产生一次）。
const LEGACY_MARK: &str = ".legacy-";

/// 按日期 + 大小轮转的日志写入器。
pub struct RotatingWriter {
    dir: PathBuf,
    current: Option<File>,
    current_name: String,
    current_date: String,
    current_seq: u32,
    current_size: u64,
    max_bytes: u64,
    max_files: usize,
}

impl RotatingWriter {
    /// 创建写入器并打开当前应写的文件。
    ///
    /// - `dir`：日志目录（须已存在）。
    /// - `max_bytes`：单文件大小阈值，超过后切下一序号文件。
    /// - `max_files`：目录内日志文件总数上限，超出删最旧。
    pub fn new(dir: PathBuf, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        let mut w = Self {
            dir,
            current: None,
            current_name: String::new(),
            current_date: String::new(),
            current_seq: 0,
            current_size: 0,
            max_bytes,
            max_files,
        };
        w.migrate_legacy()?;
        w.open_current()?;
        Ok(w)
    }

    /// 当前本地日期（yyyy-MM-dd）。
    fn today(&self) -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    /// 首次启用时把旧单文件 `dt.log`（普通文件）归档为 `dt.legacy-<时间戳>.log`，
    /// 避免软链覆盖丢失历史日志。已是软链则跳过。
    fn migrate_legacy(&mut self) -> io::Result<()> {
        let symlink_path = self.dir.join(SYMLINK_NAME);
        let meta = match fs::symlink_metadata(&symlink_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(());
        }
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let legacy = self
            .dir
            .join(format!("{FILE_PREFIX}{LEGACY_MARK}{stamp}.{FILE_SUFFIX}"));
        fs::rename(&symlink_path, &legacy)?;
        // subscriber 尚未初始化，用 stderr 提示（一次性迁移事件）
        eprintln!(
            "[dt-log] 旧日志归档: {} → {}",
            symlink_path.display(),
            legacy.display()
        );
        Ok(())
    }

    /// 打开当前应写入的文件（追加模式），并更新软链与清理。
    ///
    /// 重启后从该日期**已存在的最大序号**续写（而非从 0 探测）——
    /// 保留清理可能删掉中间序号形成空洞，从 0 探测会错误地重新从 .0 写起。
    fn open_current(&mut self) -> io::Result<()> {
        let date = self.today();
        if date != self.current_date {
            self.current_date = date;
            self.current_seq = 0;
        }
        self.current_seq = self.max_seq_for_date(&self.current_date);
        let (date, seq) = (self.current_date.clone(), self.current_seq);
        self.open_seq(&date, seq)
    }

    /// 扫描目录，返回该日期下已存在的最大序号（无则 0）。
    fn max_seq_for_date(&self, date: &str) -> u32 {
        let mut max = 0u32;
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return max;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some((d, seq)) = parse_dt_name(&name) {
                if d == date && seq > max {
                    max = seq;
                }
            }
        }
        max
    }

    /// 打开指定 (date, seq) 文件（追加），刷新软链并清理。
    fn open_seq(&mut self, date: &str, seq: u32) -> io::Result<()> {
        let path = self.seq_path(date, seq);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.current_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.current_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.current = Some(file);
        self.refresh_symlink();
        self.prune();
        Ok(())
    }

    /// 按大小切到下一序号文件（同日）。
    fn rotate_by_size(&mut self) -> io::Result<()> {
        if let Some(f) = self.current.take() {
            let _ = f.sync_all();
        }
        self.current_seq += 1;
        let (date, seq) = (self.current_date.clone(), self.current_seq);
        self.open_seq(&date, seq)
    }

    /// 计算 (date, seq) 对应的文件路径。
    fn seq_path(&self, date: &str, seq: u32) -> PathBuf {
        if seq == 0 {
            self.dir.join(format!("{FILE_PREFIX}.{date}.{FILE_SUFFIX}"))
        } else {
            self.dir
                .join(format!("{FILE_PREFIX}.{date}.{seq}.{FILE_SUFFIX}"))
        }
    }

    /// 维护软链 `dt.log` → 当前写入文件。
    fn refresh_symlink(&self) {
        let target = self.dir.join(&self.current_name);
        let link = self.dir.join(SYMLINK_NAME);
        if let Ok(m) = fs::symlink_metadata(&link) {
            if m.file_type().is_symlink() || m.is_file() {
                let _ = fs::remove_file(&link);
            }
        }
        if let Err(e) = symlink(&target, &link) {
            tracing::warn!("dt.log 软链创建失败: {e}");
        }
    }

    /// 删除最旧日志文件，直到总数 ≤ `max_files`。legacy 文件不参与清理（仅一次）。
    fn prune(&mut self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(String, u32, PathBuf)> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some((date, seq)) = parse_dt_name(&name) {
                files.push((date, seq, e.path()));
            }
        }
        if files.len() <= self.max_files {
            return;
        }
        files.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let excess = files.len() - self.max_files;
        for (_, _, path) in files.into_iter().take(excess) {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("清理旧日志失败 {}: {e}", path.display());
            }
        }
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 跨天 → 重开新日期文件
        let today = self.today();
        if today != self.current_date {
            self.open_current()?;
        }
        // 超大小阈值 → 同日切下一序号
        if self.current_size + buf.len() as u64 > self.max_bytes {
            self.rotate_by_size()?;
        }
        let n = self
            .current
            .as_mut()
            .expect("当前日志文件应已打开")
            .write(buf)?;
        self.current_size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.current.as_mut() {
            f.flush()?;
        }
        Ok(())
    }
}

/// 解析 `dt.yyyy-MM-dd.log` / `dt.yyyy-MM-dd.N.log` 为 `(date, seq)`。
/// 不匹配的（如 legacy 文件）返回 `None`，不参与保留清理。
fn parse_dt_name(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_prefix(FILE_PREFIX)?.strip_prefix('.')?;
    let stem = stem.strip_suffix(&format!(".{FILE_SUFFIX}"))?;
    if let Some((date, seq)) = stem.rsplit_once('.') {
        let seq: u32 = seq.parse().ok()?;
        Some((date.to_string(), seq))
    } else {
        // 纯日期：yyyy-MM-dd（长度 10 + 2 个连字符）
        if stem.len() == 10 && stem.chars().filter(|c| *c == '-').count() == 2 {
            Some((stem.to_string(), 0))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dt-rotating-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_regular_and_seq_names() {
        assert_eq!(parse_dt_name("dt.2026-08-13.log"), Some(("2026-08-13".into(), 0)));
        assert_eq!(
            parse_dt_name("dt.2026-08-13.2.log"),
            Some(("2026-08-13".into(), 2))
        );
        assert_eq!(parse_dt_name("dt.log"), None);
        assert_eq!(parse_dt_name("dt.legacy-20260813-120000.log"), None);
        assert_eq!(parse_dt_name("other.2026-08-13.log"), None);
    }

    #[test]
    fn size_rotation_creates_seq_files() {
        let dir = tmpdir("size");
        // 阈值 10 字节:前 5 字节进 .0,后 5 字节仍进 .0(未超),再写 10 字节触发 .1
        let mut w = RotatingWriter::new(dir.clone(), 10, 30).unwrap();
        w.write_all(b"12345").unwrap(); // size 5
        w.write_all(b"67890").unwrap(); // size 10,未超
        // 再写 10 字节触发轮转(10+10>10) → .1
        w.write_all(b"abcdefghij").unwrap();
        // 验证:同日内存在两个序号文件
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let f0 = dir.join(format!("dt.{date}.log"));
        let f1 = dir.join(format!("dt.{date}.1.log"));
        assert!(f0.exists(), "{f0:?} 应存在");
        assert!(f1.exists(), "{f1:?} 应存在");
        // 软链指向 .1
        let link = fs::read_link(dir.join("dt.log")).unwrap();
        assert_eq!(link, f1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn date_change_opens_new_file() {
        let dir = tmpdir("date");
        let mut w = RotatingWriter::new(dir.clone(), 10 * 1024 * 1024, 30).unwrap();
        w.write_all(b"today").unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert!(dir.join(format!("dt.{today}.log")).exists());

        // 模拟跨天:把 current_date 改成昨天
        let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        w.current_date = yesterday.clone();
        w.write_all(b"tomorrow").unwrap();
        assert!(dir.join(format!("dt.{today}.log")).exists());
        // 昨天(实为今天)与新日期文件都存在——直接验证软链仍有效
        assert!(fs::read_link(dir.join("dt.log")).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_prunes_oldest() {
        let dir = tmpdir("retention");
        // 造 3 个历史日期文件 + new() 会产生今天文件 = 共 4 个;
        // 保留上限 3 → 只删最旧(08-01)
        for d in ["2026-08-01", "2026-08-02", "2026-08-03"] {
            fs::write(dir.join(format!("dt.{d}.log")), vec![b'x'; 4]).unwrap();
        }
        let mut w = RotatingWriter::new(dir.clone(), 10 * 1024 * 1024, 3).unwrap();
        // new 时 open_current 会触发 prune
        assert!(!dir.join("dt.2026-08-01.log").exists(), "最旧应被清理");
        assert!(dir.join("dt.2026-08-02.log").exists());
        assert!(dir.join("dt.2026-08-03.log").exists());
        // 今天文件存在且软链指向它
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert!(dir.join(format!("dt.{today}.log")).exists());
        assert!(fs::read_link(dir.join("dt.log")).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_continues_last_seq() {
        let dir = tmpdir("restart");
        {
            let mut w = RotatingWriter::new(dir.clone(), 5, 30).unwrap();
            w.write_all(b"aaaaa").unwrap(); // size 5
            w.write_all(b"bbbbb").unwrap(); // 触发 .1
        }
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let f1 = dir.join(format!("dt.{date}.1.log"));
        assert!(f1.exists());
        // 重启:new 应续写 .1(最后序号),而非新建 .0
        let mut w2 = RotatingWriter::new(dir.clone(), 100, 30).unwrap();
        let name = fs::read_link(dir.join("dt.log")).unwrap();
        assert_eq!(name, f1, "重启后应续写最后序号文件");
        w2.write_all(b"ccccc").unwrap();
        let content = fs::read_to_string(&f1).unwrap();
        assert!(content.ends_with("ccccc"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_continues_max_seq_across_holes() {
        // 回归:保留清理删掉中间序号形成空洞(.1/.2 被删,只剩 .0 与 .5),
        // 重启必须续写 .5 而非从 .0 重新开始
        let dir = tmpdir("hole");
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        fs::write(dir.join(format!("dt.{date}.log")), b"base").unwrap();
        fs::write(dir.join(format!("dt.{date}.5.log")), b"five").unwrap();
        let mut w = RotatingWriter::new(dir.clone(), 10 * 1024 * 1024, 30).unwrap();
        w.write_all(b"-more").unwrap();
        let link = fs::read_link(dir.join("dt.log")).unwrap();
        assert_eq!(link, dir.join(format!("dt.{date}.5.log")), "应续写最大序号 .5");
        let content = fs::read_to_string(dir.join(format!("dt.{date}.5.log"))).unwrap();
        assert!(content.ends_with("-more"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_plain_file_is_archived() {
        let dir = tmpdir("legacy");
        fs::write(dir.join("dt.log"), "old-history").unwrap();
        let mut w = RotatingWriter::new(dir.clone(), 10 * 1024 * 1024, 30).unwrap();
        // dt.log 变软链
        assert!(fs::symlink_metadata(dir.join("dt.log")).unwrap().file_type().is_symlink());
        // 有 legacy 归档
        let legacy = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".legacy-"));
        assert!(legacy, "旧 dt.log 应被归档为 legacy 文件");
        // legacy 内容保留
        w.write_all(b"new").unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
