//! Digital Twin V2 守护进程 — 单 crate 分层架构。
//!
//! 分层 DDD 架构:
//!   domain/          → 实体、值对象、领域 trait（零依赖）
//!   infrastructure/   → Memgraph、Qdrant、SQLite、scanner、parser
//!   application/      → 用例编排（build、sync、context、knowledge）
//!   interfaces/       → CLI 命令处理器
//!

pub mod application;
pub mod domain;
pub mod infrastructure;
pub mod interfaces;
pub mod shared;
