//! LLM 响应 JSON 解析辅助函数。
//!
//! 统一处理 LLM 返回的 JSON 响应，容忍常见的格式变化：
//! - Markdown 代码围栏（\`\`\`json ... \`\`\`）
//! - 前后杂讯文本
//! - 嵌套在说明文字中的 JSON 对象

use serde::de::DeserializeOwned;

/// 从 LLM 响应中提取并解析 JSON 对象。
///
/// 处理流程：
/// 1. 去除前导/尾随空白
/// 2. 剥离 Markdown 围栏（\`\`\`json / \`\`\`）
/// 3. 从第一个 `{` 到最后一个 `}` 截取 JSON 主体
/// 4. 反序列化为目标类型 `T`
///
/// # 参数
/// - `raw`: LLM 原始响应字符串
///
/// # 返回
/// - `Some(T)`: 成功解析的值
/// - `None`: 解析失败（无效 JSON、缺少花括号、反序列化错误）
///
/// # 示例
/// ```
/// use serde::Deserialize;
/// use digital_twin::shared::llm_parse::parse_llm_json;
///
/// #[derive(Deserialize)]
/// struct Decision {
///     value: String,
///     reason: String,
/// }
///
/// let raw = r#"```json
/// {"value": "high", "reason": "架构文档"}
/// ```"#;
///
/// let result: Option<Decision> = parse_llm_json(raw);
/// assert!(result.is_some());
/// assert_eq!(result.unwrap().value, "high");
/// ```
pub fn parse_llm_json<T: DeserializeOwned>(raw: &str) -> Option<T> {
    let trimmed = raw.trim();

    // 剥离 Markdown 围栏
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed)
        .trim_end_matches("```")
        .trim();

    // 提取 JSON 主体（从第一个 { 到最后一个 }）
    let start = body.find('{')?;
    let end = body.rfind('}')?;

    if end <= start {
        return None;
    }

    let json = &body[start..=end];
    serde_json::from_str(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    struct TestResponse {
        value: String,
        reason: String,
    }

    #[test]
    fn parse_clean_json() {
        let raw = r#"{"value": "high", "reason": "架构文档"}"#;
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_some());
        assert_eq!(result.unwrap().value, "high");
    }

    #[test]
    fn parse_json_with_markdown_fence() {
        let raw = "```json\n{\"value\": \"medium\", \"reason\": \"README\"}\n```";
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_some());
        assert_eq!(result.unwrap().value, "medium");
    }

    #[test]
    fn parse_json_with_noise() {
        let raw = "判定结果如下：{\"value\": \"low\", \"reason\": \"流水账\"} 判定完成。";
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_some());
        assert_eq!(result.unwrap().value, "low");
    }

    #[test]
    fn parse_fails_on_invalid_json() {
        let raw = "不是 JSON";
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_none());
    }

    #[test]
    fn parse_fails_on_empty_string() {
        let raw = "";
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_none());
    }

    #[test]
    fn parse_fails_on_missing_braces() {
        let raw = "value: high, reason: test";
        let result: Option<TestResponse> = parse_llm_json(raw);
        assert!(result.is_none());
    }
}
