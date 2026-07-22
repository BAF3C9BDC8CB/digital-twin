//! Sensitive configuration value handling with automatic log redaction.
//!
//! # Usage
//!
//! ```ignore
//! let s = SecretString::from_config("env:MEMGRAPH_PASSWORD");
//! let pw = s.resolve()?;
//! println!("{:?}", s);  // "***"
//! ```

use std::env;
use std::fmt;

use crate::domain::error::DtError;

/// Sensitive configuration value with three source backends.
///
/// - `Env("MEMGRAPH_PASSWORD")`: read from environment variable
/// - `Vault("secret/memgraph")`: read from a secret manager (future)
/// - `Plain("my-password")`: plain text (dev only; refused in production)
///
/// The `Debug` and `Display` implementations output `"***"` so that
/// passwords are never accidentally leaked through log / format strings.
#[derive(Clone)]
pub enum SecretString {
    /// Resolve from environment variable.
    Env(String),
    /// Resolve from external vault / secret manager (not yet implemented).
    Vault(String),
    /// Plain text value.
    Plain(String),
}

impl SecretString {
    /// Parse a configuration value into a `SecretString`.
    ///
    /// Prefix rules:
    /// - `"env:VAR_NAME"` → `SecretString::Env("VAR_NAME")`
    /// - `"vault:path"`   → `SecretString::Vault("path")`
    /// - anything else     → `SecretString::Plain(value)`
    pub fn from_config(s: &str) -> Self {
        if let Some(var) = s.strip_prefix("env:") {
            Self::Env(var.to_string())
        } else if let Some(path) = s.strip_prefix("vault:") {
            Self::Vault(path.to_string())
        } else {
            Self::Plain(s.to_string())
        }
    }

    /// Resolve the actual secret value.
    ///
    /// - `Env` reads `std::env::var`
    /// - `Vault` returns an error (not yet implemented)
    /// - `Plain` returns the value as-is
    pub fn resolve(&self) -> Result<String, DtError> {
        match self {
            Self::Env(var) => env::var(var)
                .map_err(|e| DtError::Config(format!("env var {var} not set: {e}"))),
            Self::Vault(path) => Err(DtError::Config(format!(
                "vault backend not yet implemented (path: {path})"
            ))),
            Self::Plain(val) => Ok(val.clone()),
        }
    }

    /// Returns `true` if this is a plain-text password.
    ///
    /// Production deployments should call this at startup and refuse to
    /// proceed when plain passwords are present.
    pub fn is_plain(&self) -> bool {
        matches!(self, Self::Plain(_))
    }
}

// ---------------------------------------------------------------------------
// Debug / Display — always redacted
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
// Tests
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
