//! Qdrant storage backend (gRPC protocol).
//!
//! Uses `qdrant-client` crate for async gRPC communication.
//!
//! ## Module overview
//!
//! - `client` — low-level gRPC client wrapper
//! - `collection` — naming, configuration, and model-versioning
//! - `repo` — [`VectorRepository`] trait implementations

pub mod client;
pub mod collection;
pub mod repo;

pub use client::QdrantClient;
pub use collection::{CollectionConfig, Distance};
pub use repo::{NoopVectorRepo, QdrantRepo};
