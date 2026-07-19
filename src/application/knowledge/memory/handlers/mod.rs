//! Event handler implementations for each [`EventType`].
//!
//! Each handler:
//! 1. Parses structured data from [`MemoryEvent::details`] (key=value pairs)
//! 2. Builds a Cypher MERGE statement with the correct label and properties
//! 3. Creates relationship edges to target entities
//! 4. Executes the write via [`GraphRepository::write_query`]

pub mod deployment;

pub use deployment::DeploymentHandler;

use std::collections::HashMap;
use sha2::{Digest, Sha256};

/// Parse a `key=value` / `key: value` string into a [`HashMap`].
///
/// Pairs are separated by `;` or `,`. Leading/trailing whitespace is
/// trimmed. Keys are lowercased so callers can match case-insensitively.
///
/// # Example
///
/// ```ignore
/// let m = parse_key_values("branch: main; env: prod; status: success");
/// assert_eq!(m.get("branch"), Some(&"main".to_string()));
/// ```
pub(crate) fn parse_key_values(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in raw.split([';', '\n', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Split on first `=` or `:` to allow both delimiters.
        if let Some(pos) = part.find(['=', ':']) {
            let key = part[..pos].trim().to_lowercase();
            let value = part[pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

/// Build a deterministic event node ID via SHA-256 of (prefix, entity_id, details).
///
/// Unlike the old timestamp-based format, this produces the same ID for
/// identical events, enabling natural deduplication via `MERGE` in Cypher.
///
/// Format: `dt://event/{prefix}/{sha256_hex_short}`
pub(crate) fn make_event_id(
    prefix: &str,
    entity_id: &str,
    details: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(b":");
    hasher.update(entity_id.as_bytes());
    hasher.update(b":");
    hasher.update(details.as_bytes());
    let hash = hex::encode(hasher.finalize());
    format!("dt://event/{}/{}", prefix, &hash[..16])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semicolons_and_commas() {
        let m = parse_key_values("branch: main; env: prod, status: success");
        assert_eq!(m.get("branch"), Some(&"main".to_string()));
        assert_eq!(m.get("env"), Some(&"prod".to_string()));
        assert_eq!(m.get("status"), Some(&"success".to_string()));
    }

    #[test]
    fn parse_equals_sign() {
        let m = parse_key_values("key=value; other=42");
        assert_eq!(m.get("key"), Some(&"value".to_string()));
        assert_eq!(m.get("other"), Some(&"42".to_string()));
    }

    #[test]
    fn parse_empty_string() {
        let m = parse_key_values("");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_newlines() {
        let m = parse_key_values("a: 1\nb: 2");
        assert_eq!(m.get("a"), Some(&"1".to_string()));
        assert_eq!(m.get("b"), Some(&"2".to_string()));
    }

    #[test]
    fn parse_trims_whitespace() {
        let m = parse_key_values("  key  :  value with spaces  ");
        assert_eq!(m.get("key"), Some(&"value with spaces".to_string()));
    }

    #[test]
    fn make_event_id_format() {
        let id = make_event_id("deploy", "my-job", "branch: main; env: prod");
        assert!(id.starts_with("dt://event/deploy/"));
        // Hash-based ID should be 16 hex chars after prefix
        let rest = &id["dt://event/deploy/".len()..];
        assert_eq!(rest.len(), 16);
    }

    #[test]
    fn make_event_id_is_deterministic() {
        let id1 = make_event_id("deploy", "my-job", "branch: main; env: prod");
        let id2 = make_event_id("deploy", "my-job", "branch: main; env: prod");
        assert_eq!(id1, id2, "same input must produce same ID");
    }

    #[test]
    fn make_event_id_different_per_input() {
        let id_a = make_event_id("deploy", "job-a", "details");
        let id_b = make_event_id("deploy", "job-b", "details");
        assert_ne!(id_a, id_b, "different entity_id must produce different ID");
    }
}
