//! 敏感配置值的处理，并自动在日志中打码。
//!
//! # 用法
//!
//! ```ignore
//! let s = SecretString::from_config("env:MEMGRAPH_PASSWORD");
//! let pw = s.resolve()?;
//! println!("{:?}", s);  // "***"
//! ```

use std::env;
use std::fmt;

use crate::domain::error::DtError;

/// 敏感配置值，支持三种来源后端。
///
/// - `Env("MEMGRAPH_PASSWORD")`：从环境变量读取
/// - `Vault("secret/memgraph")`：从密钥管理器读取（未来支持）
/// - `Plain("my-password")`：明文（仅限开发环境；生产环境拒绝使用）
///
/// `Debug` 与 `Display` 实现输出 `"***"`，确保密码绝不会通过日志/
/// 格式化字符串意外泄露。
#[derive(Clone)]
pub enum SecretString {
    /// 从环境变量解析。
    Env(String),
    /// 从外部 vault / 密钥管理器解析（尚未实现）。
    Vault(String),
    /// 明文值。
    Plain(String),
}

impl SecretString {
    /// 将配置值解析为 `SecretString`。
    ///
    /// 前缀规则：
    /// - `"env:VAR_NAME"` → `SecretString::Env("VAR_NAME")`
    /// - `"vault:path"`   → `SecretString::Vault("path")`
    /// - 其他任何值       → `SecretString::Plain(value)`
    pub fn from_config(s: &str) -> Self {
        if let Some(var) = s.strip_prefix("env:") {
            Self::Env(var.to_string())
        } else if let Some(path) = s.strip_prefix("vault:") {
            Self::Vault(path.to_string())
        } else {
            Self::Plain(s.to_string())
        }
    }

    /// 解析实际的密钥值。
    ///
    /// - `Env` 读取 `std::env::var`
    /// - `Vault` 返回错误（尚未实现）
    /// - `Plain` 原样返回值
    pub fn resolve(&self) -> Result<String, DtError> {
        match self {
            Self::Env(var) => {
                env::var(var).map_err(|e| DtError::Config(format!("环境变量 {var} 未设置：{e}")))
            }
            Self::Vault(path) => Err(DtError::Config(format!(
                "vault 后端尚未实现（路径：{path}）"
            ))),
            Self::Plain(val) => Ok(val.clone()),
        }
    }

    /// 若这是明文密码，则返回 `true`。
    ///
    /// 生产部署应在启动时调用此方法，当存在明文密码时拒绝继续运行。
    pub fn is_plain(&self) -> bool {
        matches!(self, Self::Plain(_))
    }
}

// ---------------------------------------------------------------------------
// Debug / Display —— 始终打码
// ---------------------------------------------------------------------------

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_env_prefix() {
        let s = SecretString::from_config("env:MEMGRAPH_PASSWORD");
        assert!(matches!(s, SecretString::Env(ref v) if v == "MEMGRAPH_PASSWORD"));
    }

    #[test]
    fn from_config_vault_prefix() {
        let s = SecretString::from_config("vault:secret/memgraph");
        assert!(matches!(s, SecretString::Vault(ref v) if v == "secret/memgraph"));
    }

    #[test]
    fn from_config_plain() {
        let s = SecretString::from_config("plaintext");
        assert!(matches!(s, SecretString::Plain(ref v) if v == "plaintext"));
    }

    #[test]
    fn is_plain_detection() {
        assert!(SecretString::from_config("password").is_plain());
        assert!(!SecretString::from_config("env:PW").is_plain());
        assert!(!SecretString::from_config("vault:foo").is_plain());
    }

    #[test]
    fn plain_resolve_returns_value() {
        let s = SecretString::from_config("secret");
        assert_eq!(s.resolve().unwrap(), "secret");
    }

    #[test]
    fn vault_resolve_is_error() {
        let s = SecretString::from_config("vault:secret/memgraph");
        assert!(s.resolve().is_err());
    }

    #[test]
    fn debug_is_redacted() {
        let s = SecretString::from_config("env:SUPER_SECRET");
        assert_eq!(format!("{:?}", s), "***");
    }

    #[test]
    fn display_is_redacted() {
        let s = SecretString::from_config("env:SUPER_SECRET");
        assert_eq!(format!("{}", s), "***");
    }
}
