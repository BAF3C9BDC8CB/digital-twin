//! Memory world: time-dimension management.
//!
//! Provides the entities (Day, Session, MemoryEvent) and the [`MemoryService`]
//! trait for time-dimension operations.
//!
//! # Architecture
//!
//! All event types are handled by [`HookEngine`] via YAML-driven hooks
//! (see `config/event-hooks.yaml`). The `record_event` method on
//! [`MemoryService`] routes each [`EventType`] to the appropriate hook
//! name for backward compatibility.
//!
//! ```text
//! service::DefaultMemoryService (trait impl)
//!   ├── HookEngine (routes `code_modified`, `jenkins_deploy_completed`, etc.)
//!   └── entities::MemoryEvent (payload)
//! ```

pub mod entities;
pub mod service;
