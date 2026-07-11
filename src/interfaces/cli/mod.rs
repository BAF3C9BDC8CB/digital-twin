pub mod cleanup;
pub mod archive;
pub mod backup;
pub mod backup_neo4j;
pub mod backup_qdrant;
pub mod backup_sqlite;
pub mod backup_verify;

// V3 architecture: extracted CLI command handlers
pub mod build;
pub mod sync;
pub mod event;
pub mod memorize;
pub mod learn;
pub mod context;
pub mod thread;
pub mod kub;
pub mod jcli;
