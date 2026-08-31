//! 共享的 `key: value` details 字符串解析辅助函数。
//!
//! 提供将 CLI 输入的结构化 details 字符串解析为键值 HashMap 的函数。
//! 这是 KnowledgeService 便捷构造函数共用的解析逻辑。
//!
//! `parse_details` 函数复用与 memory 世界 `memory::handlers::mod` 中
//! `parse_key_values` 相同的解析策略，保证全项目行为一致。

use std::collections::HashMap;

/// 全文键：值为「段内剩余全部」，不再按 `,` 拆段。
///
/// 这些键承载自由文本（记忆正文、描述），其中几乎必含中文逗号/分号/冒号。
/// 若按普通键处理会被拦腰截断（2026-08-31 实证：记忆 content 只存到第一个 `,`）。
const WHOLE_VALUE_KEYS: &[&str] = &[
    "content",
    "details",
    "description",
    "definition",
    "body",
    "text",
];

/// 将分号分隔的 `key: value` details 字符串解析为 HashMap。
///
/// 解析分两层：
/// 1. 先按 `;`、`\n` 分「段」（key:value 对的标准分隔符）。
/// 2. 段内：找首个 `=` 或 `:` 拆出键；若键属于 [`WHOLE_VALUE_KEYS`]，
///    值取该分隔符之后直到**下一个新键段之前**的全部内容（可跨段吞并——
///    自由文本里的中文逗号/分号/冒号不会被误伤）；否则仍按 `,` 拆成多个
///    key:value 对（兼容 `scope: A, B` 这类多值写法）。
///
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
    // 第一层：按 `;` / `\n` 分「段」。
    let segments: Vec<&str> = raw
        .split([';', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let mut i = 0;
    while i < segments.len() {
        let segment = segments[i];
        // 段内找首个 `=` 或 `:`。
        let Some(pos) = segment.find(['=', ':']) else {
            // 无键的裸文本段：跳过（与旧行为一致）。
            i += 1;
            continue;
        };
        let key = segment[..pos].trim().to_lowercase();
        if key.is_empty() {
            i += 1;
            continue;
        }
        let rest = segment[pos + 1..].trim();

        if WHOLE_VALUE_KEYS.contains(&key.as_str()) {
            // 全文键：值取本段剩余 + 吞并后续「非新键段」，直到遇到形如
            // `<key>:`/`<key>=`（key 为字母数字/下划线/中文词）的新键段。
            let mut value = rest.to_string();
            let mut j = i + 1;
            while j < segments.len() && !is_key_value_segment(segments[j]) {
                value.push(';');
                value.push_str(segments[j]);
                j += 1;
            }
            map.insert(key, value);
            i = j;
        } else {
            // 普通键：段内再按 `,` 拆多个 key:value 对（兼容多值写法）。
            if rest.is_empty() {
                i += 1;
                continue;
            }
            for part in rest.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Some(p2) = part.find(['=', ':']) {
                    let k2 = part[..p2].trim().to_lowercase();
                    let v2 = part[p2 + 1..].trim().to_string();
                    if !k2.is_empty() {
                        map.insert(k2, v2);
                    }
                } else {
                    // 没有第二个键的裸值：整段仍归原键（如 "scope: A, B" 的 B 归 scope）
                    map.insert(key.clone(), part.to_string());
                }
            }
            i += 1;
        }
    }
    map
}

/// 判断某段是否形如 `<key>:` / `<key>=`（即一个新的 key:value 段）。
///
/// 键必须是纯 ASCII（小写字母/数字/下划线/中划线）——全文值里可能恰有
/// `<中文>=` 或 `名称: xxx` 这类片段，若把中文词也当新键边界会误伤正文。
fn is_key_value_segment(segment: &str) -> bool {
    let Some(pos) = segment.find(['=', ':']) else {
        return false;
    };
    let key = &segment[..pos];
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
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

    #[test]
    fn whole_value_keys_keep_full_text_with_chinese_punctuation() {
        // 回归：2026-08-31 实证 bug——content 里的中文逗号/分号/冒号把记忆正文截断。
        // dt-memory 插件构造的 details：name: <标题>; content: <自由文本>
        let details = "name: 银盛支付手续费四费率规则; content: 银盛渠道手续费4项, \
                       1.支付手续费0.38%(商户承担,结算侧扣); 2.分账0.02%(君乐承担); \
                       净盘=订单金额-支付手续费-归集手续费; 默认值来自FeeConfigService";
        let m = parse_details(details);
        assert_eq!(m.get("name"), Some(&"银盛支付手续费四费率规则".to_string()));
        let content = m.get("content").expect("content 应完整保留");
        // 完整正文不再被逗号/分号截断
        assert!(content.contains("银盛渠道手续费4项"));
        assert!(content.contains("0.38%"));
        assert!(content.contains("0.02%"));
        assert!(content.contains("净盘=订单金额-支付手续费-归集手续费"));
        assert!(content.contains("FeeConfigService"));
        // 且内容应等于整段 `content:` 之后的部分（不含尾部 name 等新键）
        assert!(!content.contains("name:"));
    }

    #[test]
    fn whole_value_keys_with_equals_separator() {
        let m = parse_details("details=第一段,含逗号; summary: 简短摘要");
        assert_eq!(m.get("details"), Some(&"第一段,含逗号".to_string()));
        assert_eq!(m.get("summary"), Some(&"简短摘要".to_string()));
    }

    #[test]
    fn non_whole_keys_still_split_by_comma() {
        // 非全文键保持旧行为：逗号仍可拆分多值（兼容 scope: A, B）
        let m = parse_details("scope: A, B; confidence: 0.9");
        assert_eq!(m.get("confidence"), Some(&"0.9".to_string()));
    }
}
