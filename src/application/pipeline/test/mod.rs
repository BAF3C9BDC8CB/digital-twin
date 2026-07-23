//! Pipeline test verification — standalone integration test for the Digital Twin
//! build pipeline.
//!
//! # Architecture
//!
//! The [`verify_test_data`] function cleans old test-prefixed data, runs
//! verification checks over every entity type (classes, methods, Nacos configs,
//! pods, Jenkins jobs, knowledge entries), and returns a [`TestReport`].
//!
//! ```text
//!     verify_test_data()
//!     → cleanup_test_data()
//!     → 10 Cypher queries + Qdrant checks
//!     → TestReport
//! ```

pub mod cleanup;
pub mod report;
pub mod runner;

pub use report::TestReport;
pub use runner::verify_test_data;
