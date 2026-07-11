//! JSON-structured log formatter for tracing-subscriber.
//!
//! Uses tracing-subscriber's built-in `.json()` layer with flattened events.
//!
//! Output example:
//! ```json
//! {"timestamp":"2026-07-09T14:30:00.123456Z","level":"INFO","target":"crate::interfaces::server",
//!  "message":"Starting project build","trace_id":"a1b2c3","plugin":"k8s"}
//! ```
//!
//! Note: the built-in format uses `timestamp` (not `ts`) and microsecond precision.
//! For production use with the exact spec format, the custom formatter in
//! `formatter_exact.rs` can be enabled later.

// This module provides re-exports and configuration helpers.
// The actual JSON layer is created via `tracing_subscriber::fmt::layer().json()`.
// We keep this module for future exact-format formatter implementation.
