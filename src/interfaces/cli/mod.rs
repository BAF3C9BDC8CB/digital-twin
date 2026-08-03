pub mod archive;
pub mod backup;
pub mod backup_memgraph;
pub mod backup_qdrant;
pub mod backup_sqlite;
pub mod backup_verify;
pub mod cleanup;

// V3 architecture: extracted CLI command handlers
pub mod build;
pub mod context;
pub mod event;
pub mod jcli;
pub mod jenkins_sync;
pub mod kub;
pub mod learn;
pub mod memorize;
pub mod search_render;
pub mod sync;
pub mod thread;
