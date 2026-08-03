//! Memgraph 存储后端（Bolt 协议）。
//!
//! 使用 Bolt 协议驱动与 Memgraph 通信。
//!
//! ## 模块
//! - `client` —— `MemgraphClient` Bolt 客户端 + `NoopGraphRepo` 占位。
//! - `schema` —— Schema 初始化（将在任务 3.2 中创建）。
//!
//! ## Memgraph 兼容性
//!
//! Memgraph 不支持 Bolt 消息中的多数据库 `db` 字段。
//! 客户端向驱动传入 `db("")`，这会让驱动完全省略该字段
//! （驱动在值为空时跳过它）。

pub mod client;
pub mod schema;

pub use client::{MemgraphClient, NoopGraphRepo};
pub use schema::{CleanReport, SchemaInitReport};
