//! Digital Twin V2 daemon — single-crate layered architecture.
//!
//! Layered DDD architecture:
//!   domain/          → entities, value objects, domain traits (zero deps)
//!   infrastructure/   → Memgraph, Qdrant, SQLite, scanner, parser
//!   application/      → use-case orchestration (build, sync, context, knowledge, plugins)
//!   interfaces/       → gRPC server + CLI command handlers
//!   shared/           → logging, coordinator, chunker

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod interfaces;
pub mod proto;
pub mod shared;
