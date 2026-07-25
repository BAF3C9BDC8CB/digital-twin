//! Unified graph query result parser — supports both Bolt driver and HTTP API formats.
//!
//! - Bolt driver: `Value::Array` of row objects (each row is a JSON object)
//! - HTTP API: `{"results":[{"data":[{"row":[...]}]}]}` (legacy)

/// Parse graph query result into a Vec of row objects.
///
/// Bolt format: `[{"col1": v1, "col2": v2}, ...]` — returns as-is.
/// HTTP format: `{"results":[{"data":[{"row":[val1, val2, ...]}]}]}` — converts each row array
/// to an object using column names from the first result's `columns` field.
pub fn parse_graph_rows(raw: &serde_json::Value) -> Vec<serde_json::Value> {
    // Bolt driver format: Array of row objects.
    if let Some(rows) = raw.as_array() {
        return rows.clone();
    }

    // HTTP API format: {"results: [{"columns": [...], "data": [{"row": [...]}]}]}
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
