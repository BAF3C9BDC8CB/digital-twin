//! SQLite snapshot storage backend.
//!
//! Stores SHA1 file hashes for incremental change detection.

pub mod repo;

pub use repo::{MemorySnapshotRepo, SqliteRepo};
