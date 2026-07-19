//! Memory world: time-dimension management.
//!
//! Provides the entities (Day, Session, MemoryEvent), the [`MemoryService`]
//! trait for time-dimension operations, and an empty [`EventDispatcher`]
//! retained as a no-op fallback.
//!
//! # Architecture
//!
//! All event types are handled by [`HookEngine`] via YAML-driven hooks
//! (see `config/event-hooks.yaml`). The `handlers` module previously
//! contained per-type implementations; all have been migrated to hooks.
//!
//! ```text
//! service::DefaultMemoryService (trait impl)
//!   ├── HookEngine (routes `code_modified`, `jenkins_deploy_completed`, etc.)
//!   ├── dispatcher::EventDispatcher (empty — no-op fallback)
//!   └── entities::MemoryEvent (payload)
//! ```

pub mod dispatcher;
pub mod entities;
pub mod handlers;
pub mod service;
