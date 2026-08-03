//! 流水线处理器结果的灵活输出容器。
//!
//! `ProcessorOutput` 包装一个 `HashMap<String, serde_json::Value>`，作为
//! 流水线阶段间通用的数据交换格式。每个处理器将其结果写入其中一个实例，
//! 然后合并到共享的 [`PipelineContext`](super::context::PipelineContext) 中。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 一个灵活的、与 JSON 兼容的处理器输出容器。
///
/// 通过底层的 `HashMap<String, serde_json::Value>` 支持任意嵌套的键值访问。
/// 值可以是任意 JSON 类型（对象、数组、字符串、数字、布尔值、null）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessorOutput {
    inner: HashMap<String, serde_json::Value>,
}

impl ProcessorOutput {
    /// 创建空的输出容器。
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// 将键设置为一个可序列化的值。
    ///
    /// 任何实现了 `serde::Serialize` 的类型都可以存储：
    ///
    /// ```ignore
    /// let mut out = ProcessorOutput::new();
    /// out.set("name", "tree-sitter");
    /// out.set("count", 42);
    /// out.set("nested", serde_json::json!({"a": [1, 2, 3]}));
    /// ```
    pub fn set(&mut self, key: &str, value: impl Serialize) {
        self.inner.insert(
            key.to_string(),
            serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        );
    }

    /// 按键获取值的引用。
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.get(key)
    }

    /// 将另一个 `ProcessorOutput` 合并到当前容器中，用 `other` 中的值
    /// 覆盖已有的键。
    pub fn merge(&mut self, other: ProcessorOutput) {
        self.inner.extend(other.inner);
    }

    /// 当容器中没有任何条目时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 消费该包装器并返回底层的 map。
    pub fn into_inner(self) -> HashMap<String, serde_json::Value> {
        self.inner
    }

    /// 借用底层的 map。
    pub fn as_inner(&self) -> &HashMap<String, serde_json::Value> {
        &self.inner
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_is_empty() {
        let out = ProcessorOutput::new();
        assert!(out.is_empty());
    }

    #[test]
    fn set_and_get_string() {
        let mut out = ProcessorOutput::new();
        out.set("name", "tree-sitter");
        assert_eq!(out.get("name"), Some(&json!("tree-sitter")));
    }

    #[test]
    fn set_and_get_integer() {
        let mut out = ProcessorOutput::new();
        out.set("lines", 128);
        assert_eq!(out.get("lines"), Some(&json!(128)));
    }

    #[test]
    fn set_and_get_nested_json() {
        let mut out = ProcessorOutput::new();
        out.set(
            "entities",
            json!({ "methods": ["foo", "bar"], "classes": ["Baz"] }),
        );
        let val = out.get("entities").unwrap();
        assert_eq!(val["methods"][0], json!("foo"));
        assert_eq!(val["classes"][0], json!("Baz"));
    }

    #[test]
    fn merge_overwrites_existing() {
        let mut a = ProcessorOutput::new();
        a.set("x", 1);
        a.set("y", 2);

        let mut b = ProcessorOutput::new();
        b.set("y", 99);
        b.set("z", 3);

        a.merge(b);
        assert_eq!(a.get("x"), Some(&json!(1)));
        assert_eq!(a.get("y"), Some(&json!(99))); // 已被覆盖
        assert_eq!(a.get("z"), Some(&json!(3)));
    }

    #[test]
    fn round_trip_serialization() {
        let mut out = ProcessorOutput::new();
        out.set("key", "value");
        let json = serde_json::to_string(&out).unwrap();
        let deserialized: ProcessorOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.get("key"), Some(&json!("value")));
    }

    #[test]
    fn into_inner_consumes() {
        let mut out = ProcessorOutput::new();
        out.set("a", 1);
        let map = out.into_inner();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("a"), Some(&json!(1)));
    }

    #[test]
    fn default_is_empty() {
        let out = ProcessorOutput::default();
        assert!(out.is_empty());
    }
}
