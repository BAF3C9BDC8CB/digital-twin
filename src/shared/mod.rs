pub mod chunker;
pub mod collections;
pub mod coordinator;
pub mod llm_parse;
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
        // ⚠️ 不能用 join: HOMEPATH 以 `\\` 开头, Windows 上 join 会把它当作
        // root-relative 路径而丢弃盘符(D: + \Users\x → \Users\x)。字符串拼接
        // 得到 D:\Users\x, 两种平台一致。
        let mut s = drive.to_string_lossy().into_owned();
        s.push_str(&path.to_string_lossy());
        return Some(PathBuf::from(s));
    }
    None
}

/// 轻量 .env 加载器：读取 `~/.config/digital-twin/.env`（若存在），
/// 将其中 `KEY=VALUE` 行注入进程环境（仅当该变量尚未设置时）。
///
/// 用于把 API key / 后端地址等密钥移出配置文件后仍能零配置运行；
/// 实际密钥只存在于 0600 权限的 .env，不进入任何 git 提交。
/// 显式 shell export 优先于 .env（不覆盖已存在的变量）。
pub fn load_dotenv_if_present() {
    let Some(home) = home_dir() else { return };
    let path = home.join(".config/digital-twin/.env");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() || v.is_empty() {
                continue;
            }
            // 已存在的环境变量（shell export 优先）不覆盖
            if std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
    }
}
