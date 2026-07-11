//! Neo4j storage backend (Bolt protocol).
//!
//! Uses `neo4rs` crate for async Bolt communication.
//!
//! ## Modules
//! - `client` — `NoopGraphRepo` compile-time placeholder (real `neo4rs` client in Phase 1.x).
//! - `repo`  — Schema initialization entry point (`init_schema`, `clean_all`).
//! - `schema` — V2 constraint/index definitions and lifecycle helpers.

pub mod client;
pub mod repo;
pub mod schema;

pub use client::{Neo4jClient, NoopGraphRepo};
pub use repo::{clean_all, init_schema};
pub use schema::{CleanReport, SchemaInitReport};
