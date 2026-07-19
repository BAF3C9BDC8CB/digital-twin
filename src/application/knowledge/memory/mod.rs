//! Memory world: time-dimension management.
//!
//! Provides the entities (Day, Session, MemoryEvent), the [`MemoryService`]
//! trait for time-dimension operations, and an [`EventDispatcher`] for
//! Observer-style event routing.
//!
//! # Architecture
//!
//! Most event types are now handled directly by [`HookEngine`] via YAML-driven
//! hooks (see `config/event-hooks.yaml`). The dispatcher only routes
//! Deployment events through the legacy hand-written handler.
//!
//! ```text
//! service::DefaultMemoryService (trait impl)
//!   ├── HookEngine (routes `code_modified`, `config_changed`, etc.)
//!   ├── dispatcher::EventDispatcher (routes by EventType — Deployment only)
//!   │     └── handlers::DeploymentHandler → (:Deployment)-[:DEPLOYS]->(:ServiceInstance)
//!   └── entities::MemoryEvent (payload)
//! ```

pub mod dispatcher;
pub mod entities;
pub mod handlers;
pub mod service;
