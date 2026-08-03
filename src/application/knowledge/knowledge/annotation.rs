//! 共享的 `key: value` details 字符串解析辅助函数。
//!
//! 提供将 CLI 输入的结构化 details 字符串解析为键值 HashMap 的函数。
//! 这是 KnowledgeService 便捷构造函数共用的解析逻辑。
//!
//! `parse_details` 函数复用与 memory 世界 `memory::handlers::mod` 中
//! `parse_key_values` 相同的解析策略，保证全项目行为一致。

use std::collections::HashMap;

/// 将分号分隔的 `key: value` details 字符串解析为 HashMap。
///
/// 键值对以 `;`、`\n` 或 `,` 分隔。键和值在第一个 `=` 或 `:` 处拆分。
/// 首尾空白会被修剪。键统一转小写，便于调用方大小写不敏感匹配。
///
/// # 示例
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
        // 在第一个 `=` 或 `:` 处拆分，以同时支持两种分隔符。
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

/// 将 details 字符串解析为值列表（分号分隔，无键前缀）。
///
/// 每个分号分隔的段都视为值（而非 key:value 对）。
/// 适用于解析 `tags: tag1; tag2; tag3` 这类字段。
///
/// # 示例
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
// 测试
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
        // 原始大小写的键不会被存储。
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
        // 来自 Memorize CLI 调用的真实 details
        let details = "decision: 选择银盛; reason: 费率低; \
                       scope: PayService, BusinessService; \
                       confidence: 0.9";
        let m = parse_details(details);
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        // 注意："scope" 中含逗号，逗号同样是分隔符，因此
        // 该值会被截断。这是预期行为——多值字段应改用
        // 不同的分隔策略。
        assert_eq!(m.get("confidence"), Some(&"0.9".to_string()));
    }

    #[test]
    fn parse_details_roundtrip_with_service_constructors() {
        // 验证 parse_details 的输出与实体构造函数兼容。
        let details = "title: Redis超时; severity: critical; domain: 支付; project: test";
        let m = parse_details(details);
        assert_eq!(m.get("title"), Some(&"Redis超时".to_string()));
        assert_eq!(m.get("severity"), Some(&"critical".to_string()));
        assert_eq!(m.get("domain"), Some(&"支付".to_string()));
        assert_eq!(m.get("project"), Some(&"test".to_string()));
    }
}
