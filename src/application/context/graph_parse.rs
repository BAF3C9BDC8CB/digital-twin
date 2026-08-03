//! 统一的图谱查询结果解析器 — 同时支持 Bolt 驱动与 HTTP API 格式。
//!
//! - Bolt 驱动：行对象的 `Value::Array`（每行是一个 JSON 对象）
//! - HTTP API：`{"results":[{"data":[{"row":[...]}]}]}`（旧版）

/// 将图谱查询结果解析为行对象 Vec。
///
/// Bolt 格式：`[{"col1": v1, "col2": v2}, ...]` — 原样返回。
/// HTTP 格式：`{"results":[{"data":[{"row":[val1, val2, ...]}]}]}` — 使用第一个
/// 结果的 `columns` 字段中的列名，将每个行数组转换为对象。
pub fn parse_graph_rows(raw: &serde_json::Value) -> Vec<serde_json::Value> {
    // Bolt 驱动格式：行对象数组。
    if let Some(rows) = raw.as_array() {
        return rows.clone();
    }

    // HTTP API 格式：{"results: [{"columns": [...], "data": [{"row": [...]}]}]}
    let Some(results) = raw.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let Some(first) = results.first() else {
        return Vec::new();
    };

    let columns: Vec<String> = first
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|cols| {
            cols.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for result in results {
        let Some(data) = result.get("data").and_then(|d| d.as_array()) else {
            continue;
        };
        for row_val in data {
            let Some(row) = row_val.get("row").and_then(|r| r.as_array()) else {
                continue;
            };
            let mut obj = serde_json::Map::new();
            for (i, val) in row.iter().enumerate() {
                if let Some(col) = columns.get(i) {
                    obj.insert(col.clone(), val.clone());
                }
            }
            out.push(serde_json::Value::Object(obj));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_bolt_array_format() {
        let raw = json!([
            {"name": "foo", "type": "Method"},
            {"name": "bar", "type": "Class"}
        ]);
        let rows = parse_graph_rows(&raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "foo");
    }

    #[test]
    fn parse_http_format() {
        let raw = json!({
            "results": [{
                "columns": ["name", "type"],
                "data": [
                    {"row": ["foo", "Method"]},
                    {"row": ["bar", "Class"]}
                ]
            }]
        });
        let rows = parse_graph_rows(&raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "foo");
        assert_eq!(rows[0]["type"], "Method");
    }

    #[test]
    fn parse_empty_returns_empty() {
        assert!(parse_graph_rows(&json!([])).is_empty());
        assert!(parse_graph_rows(&json!({})).is_empty());
        assert!(parse_graph_rows(&json!(null)).is_empty());
    }
}
