//! Pipeline test runner — self-contained integration test for the Digital Twin
//! build pipeline.
//!
//! # Architecture
//!
//! The test runner creates test- prefixed nodes and collections, exercises the
//! full build pipeline (code indexing, Nacos/K8s/Jenkins/Knowledge data), then
//! verifies every entity type was stored correctly and reports results.
//!
//! ```text
//!                   TestRunner
//!     ┌───────────────────────────────────────────┐
//!     │  build_test_data() → verify_test_data()    │
//!     │  → cleanup() (unless --keep)               │
//!     └───────────────────────────────────────────┘
//!                        │
//!               ┌────────┴────────┐
//!               │   TestReport    │
//!               │  (colored TTY)  │
//!               └─────────────────┘
//! ```

pub mod cleanup;
pub mod report;
pub mod runner;

pub use runner::TestRunner;
pub use report::TestReport;
