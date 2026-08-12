pub mod backup;
pub mod backup_memgraph;
pub mod backup_qdrant;
pub mod backup_sqlite;
pub mod backup_verify;
pub mod cleanup;

// V3 架构：抽取的 CLI 命令处理器
pub mod build;
pub mod event;
pub mod learn;
pub mod memorize;
pub mod search_render;
pub mod sense;
pub mod sync;
