//! SQLite 快照存储后端。
//!
//! 存储 SHA1 文件哈希，用于增量变更检测。

pub mod repo;

pub use repo::{MemorySnapshotRepo, SqliteRepo};
