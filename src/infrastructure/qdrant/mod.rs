//! Qdrant 存储后端（gRPC 协议）。
//!
//! 使用 `qdrant-client` crate 进行异步 gRPC 通信。
//!
//! ## 模块概览
//!
//! - `client` —— 底层 gRPC 客户端包装
//! - `collection` —— 命名、配置与模型版本管理
//! - `repo` —— [`VectorRepository`] trait 实现

pub mod client;
pub mod collection;
pub mod repo;

pub use client::QdrantClient;
pub use collection::{CollectionConfig, Distance};
pub use repo::{NoopVectorRepo, QdrantRepo};
