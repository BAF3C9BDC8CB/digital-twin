//! Memgraph storage backend (Bolt protocol).
//!
//! Uses the Bolt protocol driver for Memgraph communication.
//!
//! ## Modules
//! - `client` — `MemgraphClient` Bolt client + `NoopGraphRepo` placeholder.
//! - `schema` — Schema initialization (will be created in Task 3.2).
//!
//! ## Memgraph compatibility
//!
//! Memgraph does not support the multi-database `db` field in Bolt messages.
//! The client passes `db("")` to the driver, which tells it to omit the
//! field entirely (the driver skips it when the value is empty).

pub mod client;
pub mod schema;

pub use client::{MemgraphClient, NoopGraphRepo};
pub use schema::{CleanReport, SchemaInitReport};
