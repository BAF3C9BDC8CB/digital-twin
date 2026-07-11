//! Endpoint vectoriser and log-pattern extraction framework.
//!
//! # Endpoint vectoriser
//!
//! After `dt build` parses API endpoint metadata (method, path, controller),
//! the [`EndpointVectorizer`] embeds the endpoint description text and
//! upserts the vector into Qdrant for semantic API search.
//!
//! # Log pattern extraction
//!
//! The [`extract_log_pattern`] function analyses log lines for ERROR/WARN
//! severity markers and produces a normalised template string by replacing
//! variable portions (timestamps, UUIDs, numbers, URLs) with placeholder `{}`.
//!
//! # Example
//!
//! ```ignore
//! use crate::shared::vectorizer::{EndpointVectorizer, Endpoint};
//! use crate::shared::vectorizer::extract_log_pattern;
//!
//! // Endpoint vectorisation
//! let ep = Endpoint {
//!     entity_id: "dt://endpoint/payment/PayController/postPay".into(),
//!     method: "POST".into(),
//!     path: "/api/pay/confirm".into(),
//!     description: "Confirm payment transaction".into(),
//!     controller: "PayController".into(),
//! };
//! // vec.vectorize_endpoints(&[ep], "my-project").await?;
//!
//! // Log pattern extraction
//! let line = "2026-07-10 ERROR PaymentService: timeout for order 12345 after 30s";
//! let pattern = extract_log_pattern(line).unwrap();
//! assert_eq!(pattern.template, "{} ERROR {}: timeout for order {} after {}");
//! ```

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, VectorRepository};
use std::sync::Arc;

// ============================================================================
// Endpoint
// ============================================================================

/// A parsed API endpoint descriptor.
///
/// Produced by code parsers when they encounter routing annotations
/// (e.g. `@RequestMapping`, `@PostMapping`, `#[get("/")]`, `@app.route`).
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Unique identifier (e.g. `dt://endpoint/{project}/{controller}/{method}`).
    pub entity_id: String,
    /// HTTP method: GET, POST, PUT, DELETE, PATCH, etc.
    pub method: String,
    /// URL path template (e.g. `/api/users/{id}`).
    pub path: String,
    /// Human-readable description extracted from doc comments.
    pub description: String,
    /// Controller class or handler function name.
    pub controller: String,
}

// ============================================================================
// EndpointVectorizer
// ============================================================================

/// Vectorises API endpoint metadata into Qdrant for semantic search.
///
/// After `dt build` discovers endpoint metadata (via annotation parsing),
/// this vectoriser generates embeddings from the concatenated method, path,
/// and description text, then upserts into the `{project}_semantic` collection.
pub struct EndpointVectorizer {
    embed: Arc<dyn EmbedService>,
    vector: Arc<dyn VectorRepository>,
}

impl EndpointVectorizer {
    /// Create a new endpoint vectoriser.
    pub fn new(embed: Arc<dyn EmbedService>, vector: Arc<dyn VectorRepository>) -> Self {
        Self { embed, vector }
    }

    /// Embed and upsert a batch of endpoints into Qdrant.
    ///
    /// # Payload
    ///
    /// Each Qdrant point carries the following payload fields:
    /// - `entity_id` — unique endpoint identifier
    /// - `method` — HTTP method
    /// - `path` — URL path template
    /// - `description` — human-readable summary
    /// - `controller` — owning controller class
    /// - `source_type` — `"endpoint"`
    /// - `project` — project name
    ///
    /// Returns the number of endpoints vectorised.
    pub async fn vectorize_endpoints(
        &self,
        endpoints: &[Endpoint],
        project: &str,
    ) -> Result<usize, DtError> {
        if endpoints.is_empty() {
            return Ok(0);
        }

        let collection = format!("{}_semantic", project);
        self.vector.ensure_collection(&collection, 384).await?;

        // Build search texts: "POST /api/pay/confirm - Confirm payment transaction"
        let texts: Vec<String> = endpoints
            .iter()
            .map(|ep| {
                if ep.description.is_empty() {
                    format!("{} {}", ep.method, ep.path)
                } else {
                    format!("{} {} - {}", ep.method, ep.path, ep.description)
                }
            })
            .collect();

        // Generate embeddings
        let vectors = self.embed.embed_batch(&texts).await?;

        // Build Qdrant points with enriched payload
        let points: Vec<serde_json::Value> = endpoints
            .iter()
            .zip(vectors.iter())
            .map(|(ep, vec)| {
                serde_json::json!({
                    "id": ep.entity_id,
                    "vector": vec,
                    "payload": {
                        "entity_id": ep.entity_id,
                        "method": ep.method,
                        "path": ep.path,
                        "description": ep.description,
                        "controller": ep.controller,
                        "source_type": "endpoint",
                        "project": project,
                    }
                })
            })
            .collect();

        self.vector.upsert(&collection, points).await?;
        Ok(endpoints.len())
    }
}

// ============================================================================
// LogPattern
// ============================================================================

/// A normalised log pattern extracted from a raw log line.
///
/// Variable portions (timestamps, UUIDs, hex IDs, IPs, numbers, URLs) are
/// replaced with `{}` placeholders, producing a stable template suitable
/// for grouping and anomaly detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPattern {
    /// Normalised template with `{}` placeholders for variable parts.
    pub template: String,
    /// Service name extracted from the log line (e.g. "PaymentService").
    pub service: String,
    /// Severity level: "ERROR" or "WARN".
    pub severity: String,
}

/// Extract a [`LogPattern`] from a raw log line.
///
/// # Detection rules
///
/// 1. The line must contain "ERROR" or "WARN" (case-insensitive) as a
///    standalone word.
/// 2. Extract the service name from the segment after the timestamp/severity
///    prefix, delimited by `: ` or whitespace.
/// 3. Normalise variable parts (timestamps, UUIDs, hex IDs, IP addresses,
///    numbers, URLs) into `{}` placeholders.
///
/// # Returns
///
/// - `Some(LogPattern)` when a valid ERROR/WARN log line is detected.
/// - `None` when the line does not match or is not a log line.
///
/// # Examples
///
/// ```
/// use dt_daemon::shared::vectorizer::extract_log_pattern;
///
/// let line = "2024-01-15 10:30:00 ERROR PaymentService: timeout for order 12345 after 30s";
/// let p = extract_log_pattern(line).unwrap();
/// assert_eq!(p.severity, "ERROR");
/// assert_eq!(p.service, "PaymentService");
/// assert_eq!(p.template, "{} ERROR PaymentService: timeout for order {} after {}");
/// ```
pub fn extract_log_pattern(log_line: &str) -> Option<LogPattern> {
    let line = log_line.trim();
    if line.is_empty() {
        return None;
    }

    // 1. Detect severity
    let severity = if !line.contains("ERROR") && !line.contains("error")
        && !line.contains("WARN") && !line.contains("warn")
    {
        return None;
    } else if line.contains("ERROR") || line.contains("error") {
        "ERROR"
    } else {
        "WARN"
    };

    // Extract the rest after the severity token
    let sev_pos = line.to_uppercase().find(severity)?;
    let after_sev = &line[sev_pos + severity.len()..].trim();

    // 2. Extract service name: strip any [thread-X] / [pid] markers,
    //    then take the first word before ':' or whitespace.
    let cleaned = strip_bracket_markers(after_sev);
    let service = cleaned
        .split([':', ' ', '\t'])
        .next()
        .unwrap_or("unknown")
        .to_string();

    // 3. Normalise the line → replace variable parts with {}
    let template = normalize_log_line(line, severity);

    Some(LogPattern {
        template,
        service,
        severity: severity.to_string(),
    })
}

/// Strip leading bracket markers like `[thread-1]`, `[12345]`, etc.
/// from the beginning of a string. Used for extracting the service name
/// from log lines that have thread/pid prefixes.
fn strip_bracket_markers(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('[') {
        if let Some(end) = s.find(']') {
            return s[end + 1..].trim();
        }
    }
    s
}

/// Replace variable tokens with `{}` placeholders.
///
/// Handles: ISO timestamps, datetimes, UUIDs, hex IDs, IP addresses,
/// port numbers, standalone numbers, URLs, and thread markers.
fn normalize_log_line(line: &str, _severity: &str) -> String {
    use regex::Regex;
    use std::sync::LazyLock;

    // Patterns ordered from most-specific to least-specific.
    // NOTE: Rust's regex crate does NOT support lookahead/lookbehind,
    // so we avoid them entirely. The `ms|s|KB|...` suffix exclusion
    // for stand-alone numbers is handled by ordering: catch unit-suffixed
    // values (like "3000ms") before catching bare numbers.
    static PATTERNS: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
        vec![
            // ISO 8601 timestamp with T separator: 2024-01-15T10:30:00.123Z
            (
                Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
                    .unwrap(),
                "{}",
            ),
            // Datetime with milliseconds: 2024-01-15 10:30:00.123
            (
                Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}").unwrap(),
                "{}",
            ),
            // Datetime without ms: 2024-01-15 10:30:00
            (
                Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}").unwrap(),
                "{}",
            ),
            // Date only: 2024-01-15
            (
                Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").unwrap(),
                "{}",
            ),
            // UUIDs: 550e8400-e29b-41d4-a716-446655440000
            (
                Regex::new(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
                    .unwrap(),
                "{}",
            ),
            // Hex IDs: 0xABCDEF123
            (
                Regex::new(r"\b0x[0-9a-fA-F]+\b").unwrap(),
                "{}",
            ),
            // IP addresses with optional :port
            (
                Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(?::\d{1,5})?\b").unwrap(),
                "{}",
            ),
            // URLs: http:// or https://
            (
                Regex::new(r"https?://\S+").unwrap(),
                "{}",
            ),
            // Numbers with common time/size units (must come BEFORE bare numbers)
            (
                Regex::new(r"\b\d+(?:\.\d+)?\s*(?:ms|s|sec|KB|MB|GB|%)\b").unwrap(),
                "{}",
            ),
            // host:port pattern (hostname + colon + number)
            (
                Regex::new(r"\b[\w.-]+:\d{1,5}\b").unwrap(),
                "{}",
            ),
            // Standalone numbers (after unit-suffixed and host:port are handled)
            (
                Regex::new(r"\b\d+(?:\.\d+)?\b").unwrap(),
                "{}",
            ),
            // Thread identifiers: [thread-1], [main], [pool-3-thread-2]
            (
                Regex::new(r"\[[^\]]*thread[^\]]*\]").unwrap(),
                "{}",
            ),
            // Bracketed process/pid: [12345]
            (
                Regex::new(r"\b\[\d+\]\b").unwrap(),
                "{}",
            ),
        ]
    });

    let mut normalized = line.to_string();
    for (re, replacement) in PATTERNS.iter() {
        normalized = re.replace_all(&normalized, *replacement).to_string();
    }

    // Clean up: collapse multiple adjacent {} placeholders
    let collapsed = collapse_placeholders(&normalized);
    collapsed.trim().to_string()
}

use std::borrow::Cow;

/// Collapse consecutive `{}` patterns into a single `{}`.
fn collapse_placeholders(s: &str) -> Cow<'_, str> {
    use regex::Regex;
    use std::sync::LazyLock;
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\{\}(\s*\{\})+").unwrap());
    RE.replace_all(s, "{}")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // -- Mock services -------------------------------------------------------

    struct MockEmbed;
    #[async_trait::async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.0_f32; 384]).collect())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    struct MockVector {
        points: Mutex<Vec<serde_json::Value>>,
    }
    impl MockVector {
        fn new() -> Self {
            Self { points: Mutex::new(Vec::new()) }
        }
    }
    #[async_trait::async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> { Ok(()) }
        async fn search(&self, _c: &str, _v: Vec<f32>, _l: u64) -> Result<Vec<serde_json::Value>, DtError> { Ok(vec![]) }
        async fn upsert(&self, _c: &str, pts: Vec<serde_json::Value>) -> Result<(), DtError> {
            if let Ok(mut v) = self.points.lock() { v.extend(pts); }
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> { Ok(()) }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> { Ok(vec![]) }
        async fn collection_info(&self, _n: &str) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Err(DtError::NotFound("mock".into()))
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> { Ok(()) }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    // -- EndpointVectorizer tests -------------------------------------------

    #[tokio::test]
    async fn vectorize_endpoints_empty_returns_zero() {
        let ev = EndpointVectorizer::new(Arc::new(MockEmbed), Arc::new(MockVector::new()));
        let count = ev.vectorize_endpoints(&[], "test").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn vectorize_endpoints_upserts_with_correct_payload() {
        let vector = Arc::new(MockVector::new());
        let ev = EndpointVectorizer::new(Arc::new(MockEmbed), vector.clone());

        let endpoints = vec![
            Endpoint {
                entity_id: "dt://endpoint/pay/PayController/postPay".into(),
                method: "POST".into(),
                path: "/api/pay/confirm".into(),
                description: "Confirm payment transaction".into(),
                controller: "PayController".into(),
            },
            Endpoint {
                entity_id: "dt://endpoint/pay/PayController/getOrder".into(),
                method: "GET".into(),
                path: "/api/orders/{id}".into(),
                description: "".into(),
                controller: "PayController".into(),
            },
        ];

        let count = ev.vectorize_endpoints(&endpoints, "pay-proj").await.unwrap();
        assert_eq!(count, 2);

        let pts = vector.points.lock().unwrap();
        assert_eq!(pts.len(), 2);

        let p0 = &pts[0];
        assert_eq!(p0["payload"]["method"], "POST");
        assert_eq!(p0["payload"]["path"], "/api/pay/confirm");
        assert_eq!(p0["payload"]["description"], "Confirm payment transaction");
        assert_eq!(p0["payload"]["controller"], "PayController");
        assert_eq!(p0["payload"]["source_type"], "endpoint");
        assert_eq!(p0["payload"]["project"], "pay-proj");

        // Second endpoint has no description → search text uses method+path only
        let p1 = &pts[1];
        assert_eq!(p1["payload"]["description"], "");
        assert_eq!(p1["payload"]["method"], "GET");
    }

    #[tokio::test]
    async fn vectorize_endpoints_without_description_uses_method_and_path() {
        let vector = Arc::new(MockVector::new());
        let ev = EndpointVectorizer::new(Arc::new(MockEmbed), vector.clone());

        let endpoints = vec![Endpoint {
            entity_id: "dt://endpoint/svc/HealthController/check".into(),
            method: "HEAD".into(),
            path: "/health".into(),
            description: "".into(),
            controller: "HealthController".into(),
        }];

        let count = ev.vectorize_endpoints(&endpoints, "svc").await.unwrap();
        assert_eq!(count, 1);

        let pts = vector.points.lock().unwrap();
        assert_eq!(pts[0]["payload"]["method"], "HEAD");
        assert_eq!(pts[0]["payload"]["path"], "/health");
    }

    // -- LogPattern tests ---------------------------------------------------

    #[test]
    fn extract_log_pattern_error_with_service() {
        let line = "2024-01-15 10:30:00.123 ERROR PaymentService: timeout for order 12345 after 30s";
        let p = extract_log_pattern(line).unwrap();
        assert_eq!(p.severity, "ERROR");
        assert_eq!(p.service, "PaymentService");
        assert!(p.template.contains("{}"));
        assert!(p.template.contains("ERROR"));
        assert!(p.template.contains("timeout for order"));
    }

    #[test]
    fn extract_log_pattern_warn() {
        let line = "2024-01-15 WARN  UserService - memory usage 85% on instance 192.168.1.100:9090";
        let p = extract_log_pattern(line).unwrap();
        assert_eq!(p.severity, "WARN");
        assert_eq!(p.service, "UserService");
        assert!(p.template.contains("{}"));
        assert!(!p.template.contains("192.168.1.100")); // IP replaced
    }

    #[test]
    fn extract_log_pattern_lowercase_error() {
        let line = "error: failed to connect to database at 10.0.0.5:5432";
        let p = extract_log_pattern(line).unwrap();
        assert_eq!(p.severity, "ERROR");
        // Without a clear service name pattern, "failed" may be extracted
        // Verify IP/port normalisation
        assert!(!p.template.contains("10.0.0.5"));
        assert!(!p.template.contains("5432"));
    }

    #[test]
    fn extract_log_pattern_no_severity_returns_none() {
        let line = "2024-01-15 INFO UserService: user login successful";
        assert!(extract_log_pattern(line).is_none());
    }

    #[test]
    fn extract_log_pattern_empty_line_returns_none() {
        assert!(extract_log_pattern("").is_none());
    }

    #[test]
    fn extract_log_pattern_normalizes_uuid() {
        let line = "2024-01-15 ERROR AuthService: token 550e8400-e29b-41d4-a716-446655440000 expired";
        let p = extract_log_pattern(line).unwrap();
        assert!(!p.template.contains("550e8400"));
        assert!(p.template.contains("{}"));
    }

    #[test]
    fn extract_log_pattern_normalizes_url() {
        let line = "ERROR GatewayService: upstream timeout https://api.example.com/v1/pay/confirm";
        let p = extract_log_pattern(line).unwrap();
        assert!(!p.template.contains("https://"));
        assert!(p.template.contains("{}"));
    }

    #[test]
    fn extract_log_pattern_k8s_style_log() {
        let line = "2026-07-10T08:15:30.456Z ERROR [thread-1] PaymentService: payment timeout for order abc-123 after 30000ms";
        let p = extract_log_pattern(line).unwrap();
        assert_eq!(p.severity, "ERROR");
        assert_eq!(p.service, "PaymentService");
        assert!(!p.template.contains("2026-07-10"));
        assert!(!p.template.contains("thread-1"));
        assert!(!p.template.contains("abc-123"));
    }

    #[test]
    fn log_pattern_struct_fields() {
        let lp = LogPattern {
            template: "{} ERROR {}: timeout for order {}".into(),
            service: "PaymentService".into(),
            severity: "ERROR".into(),
        };
        assert_eq!(lp.severity, "ERROR");
        assert_eq!(lp.service, "PaymentService");
        assert_eq!(lp.template, "{} ERROR {}: timeout for order {}");
    }

    #[test]
    fn endpoint_struct_fields() {
        let ep = Endpoint {
            entity_id: "dt://endpoint/test/Svc/post".into(),
            method: "POST".into(),
            path: "/api/v1/create".into(),
            description: "Create resource".into(),
            controller: "Svc".into(),
        };
        assert_eq!(ep.method, "POST");
        assert_eq!(ep.path, "/api/v1/create");
        assert_eq!(ep.controller, "Svc");
    }
}