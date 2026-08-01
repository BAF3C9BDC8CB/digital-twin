//! Flexible output container for pipeline processor results.
//!
//! `ProcessorOutput` wraps a `HashMap<String, serde_json::Value>` and serves
//! as the universal data exchange format between pipeline stages. Every
//! processor writes its results into one of these, which are then merged into
//! the shared [`PipelineContext`](super::context::PipelineContext).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A flexible, JSON-compatible container for processor outputs.
///
/// Supports arbitrary nested key-value access via the underlying
/// `HashMap<String, serde_json::Value>`.  Values can be any JSON type
/// (objects, arrays, strings, numbers, booleans, null).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessorOutput {
    inner: HashMap<String, serde_json::Value>,
}

impl ProcessorOutput {
    /// Create an empty output container.
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Set a key to a serializable value.
    ///
    /// Any type that implements `serde::Serialize` can be stored:
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

    /// Get a reference to a value by key.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.inner.get(key)
    }

    /// Merge another `ProcessorOutput` into this one, overwriting existing
    /// keys with the values from `other`.
    pub fn merge(&mut self, other: ProcessorOutput) {
        self.inner.extend(other.inner);
    }

    /// Returns `true` when the container holds no entries.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Consume the wrapper and return the underlying map.
    pub fn into_inner(self) -> HashMap<String, serde_json::Value> {
        self.inner
    }

    /// Borrow the underlying map.
    pub fn as_inner(&self) -> &HashMap<String, serde_json::Value> {
        &self.inner
    }
}

// ---------------------------------------------------------------------------
// Tests
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
        assert_eq!(a.get("y"), Some(&json!(99))); // overwritten
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
