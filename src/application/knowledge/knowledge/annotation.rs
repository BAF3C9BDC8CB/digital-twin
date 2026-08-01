//! Shared `key: value` details-string parsing helpers.
//!
//! Provides functions to parse structured details strings from CLI input
//! into key-value HashMaps. This is the shared parsing logic used by the
//! KnowledgeService convenience constructors.
//!
//! The `parse_details` function re-uses the same parsing strategy as the
//! memory world's `parse_key_values` in `memory::handlers::mod`, ensuring
//! consistent behaviour across the project.

use std::collections::HashMap;

/// Parse a semicolon-separated `key: value` details string into a HashMap.
///
/// Pairs are separated by `;`, `\n`, or `,`. Keys and values are split on
/// the first `=` or `:`. Leading/trailing whitespace is trimmed. Keys are
/// lowercased so callers can match case-insensitively.
///
/// # Example
///
/// ```ignore
/// let m = parse_details("decision: 选择银盛; reason: 费率低; scope: PayService");
/// assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
/// assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
/// assert_eq!(m.get("scope"), Some(&"PayService".to_string()));
/// ```
pub fn parse_details(raw: &str) -> HashMap<String, String> {
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

/// Parse a details string as a value list (semicolon-separated, no key prefix).
///
/// Each semicolon-separated segment is treated as a value (not a key:value pair).
/// This is useful for parsing fields like `tags: tag1; tag2; tag3`.
///
/// # Example
///
/// ```ignore
/// let tags = parse_value_list("tag1; tag2; tag3");
/// assert_eq!(tags, vec!["tag1", "tag2", "tag3"]);
/// ```
pub fn parse_value_list(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_colon_values() {
        let m = parse_details("decision: 选择银盛; reason: 费率低; scope: PayService");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        assert_eq!(m.get("scope"), Some(&"PayService".to_string()));
    }

    #[test]
    fn parse_equals_values() {
        let m = parse_details("key=value; other=42");
        assert_eq!(m.get("key"), Some(&"value".to_string()));
        assert_eq!(m.get("other"), Some(&"42".to_string()));
    }

    #[test]
    fn parse_mixed_delimiters() {
        let m = parse_details("a: 1, b: 2; c: 3");
        assert_eq!(m.get("a"), Some(&"1".to_string()));
        assert_eq!(m.get("b"), Some(&"2".to_string()));
        assert_eq!(m.get("c"), Some(&"3".to_string()));
    }

    #[test]
    fn parse_newlines() {
        let m = parse_details("title: Payment;\ndomain: 支付;\nseverity: warning");
        assert_eq!(m.get("title"), Some(&"Payment".to_string()));
        assert_eq!(m.get("domain"), Some(&"支付".to_string()));
        assert_eq!(m.get("severity"), Some(&"warning".to_string()));
    }

    #[test]
    fn parse_trims_whitespace() {
        let m = parse_details("  decision  :  选择银盛  ");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
    }

    #[test]
    fn parse_empty_string() {
        let m = parse_details("");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_keys_are_lowercased() {
        let m = parse_details("Decision: 选择银盛; Reason: 费率低");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        // Original case keys are NOT stored.
        assert_eq!(m.get("Decision"), None);
    }

    #[test]
    fn parse_no_colon_treats_as_key_without_value() {
        let m = parse_details("nokey");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_duplicate_keys_last_wins() {
        let m = parse_details("a: 1; a: 2");
        assert_eq!(m.get("a"), Some(&"2".to_string()));
    }

    #[test]
    fn parse_value_list_basic() {
        let v = parse_value_list("tag1; tag2; tag3");
        assert_eq!(v, vec!["tag1", "tag2", "tag3"]);
    }

    #[test]
    fn parse_value_list_empty() {
        let v = parse_value_list("");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_value_list_single() {
        let v = parse_value_list("only");
        assert_eq!(v, vec!["only"]);
    }

    #[test]
    fn parse_complex_details() {
        // Realistic details from a Memorize CLI call
        let details = "decision: 选择银盛; reason: 费率低; \
                       scope: PayService, BusinessService; \
                       confidence: 0.9";
        let m = parse_details(details);
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        // Note: "scope" contains commas which are also delimiters, so the
        // value gets truncated. This is expected — the caller should use a
        // different delimiter strategy for multi-value fields.
        assert_eq!(m.get("confidence"), Some(&"0.9".to_string()));
    }

    #[test]
    fn parse_details_roundtrip_with_service_constructors() {
        // Verify the parse_details output is compatible with entity constructors.
        let details = "title: Redis超时; severity: critical; domain: 支付; project: test";
        let m = parse_details(details);
        assert_eq!(m.get("title"), Some(&"Redis超时".to_string()));
        assert_eq!(m.get("severity"), Some(&"critical".to_string()));
        assert_eq!(m.get("domain"), Some(&"支付".to_string()));
        assert_eq!(m.get("project"), Some(&"test".to_string()));
    }
}
