pub mod chunker;
pub mod collections;
pub mod coordinator;
pub mod logging;

use std::path::PathBuf;

/// 跨平台 home 目录：Unix 读 `HOME`，Windows 读 `USERPROFILE`（次选 `HOMEDRIVE+HOMEPATH`）。
///
/// 本项目统一用该函数解析 `~/.config/digital-twin/...` 等用户级路径，
/// 避免 Windows 上 `HOME` 未设置导致配置加载失败。
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE");
    let path = std::env::var_os("HOMEPATH");
    if let (Some(drive), Some(path)) = (drive, path) {
        // ⚠️ 不能用 join: HOMEPATH 以 `\` 开头, Windows 上 join 会把它当作
        // root-relative 路径而丢弃盘符(D: + \Users\x → \Users\x)。字符串拼接
        // 得到 D:\Users\x, 两种平台一致。
        let mut s = drive.to_string_lossy().into_owned();
        s.push_str(&path.to_string_lossy());
        return Some(PathBuf::from(s));
    }
    None
}
