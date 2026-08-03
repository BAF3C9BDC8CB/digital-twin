//! Memory 世界：时间维度管理。
//!
//! 提供实体（Day、Session、MemoryEvent）以及面向时间维度操作的
//! [`MemoryService`] trait。
//!
//! # 架构
//!
//! 所有事件类型都由 [`HookEngine`] 通过 YAML 驱动的 hook 处理
//! （见 `config/event-hooks.yaml`）。[`MemoryService`] 上的 `record_event`
//! 方法将每个 [`EventType`] 路由到对应的 hook 名以保持向后兼容。
//!
//! ```text
//! service::DefaultMemoryService (trait impl)
//!   ├── HookEngine (routes `code_modified`, `jenkins_deploy_completed`, etc.)
//!   └── entities::MemoryEvent (payload)
//! ```

pub mod entities;
pub mod service;
