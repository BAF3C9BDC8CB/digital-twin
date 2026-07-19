//! Memory world: time-dimension management.
//!
//! Provides the entities (Day, Session, MemoryEvent), the [`MemoryService`]
//! trait for time-dimension operations, and an [`EventDispatcher`] for
//! Observer-style event routing.
//!
//! # Architecture
//!
//! ```text
//! service::DefaultMemoryService (trait impl)
//!   ├── HookEngine (routes `config_changed` and other hook events)
//!   ├── dispatcher::EventDispatcher (routes by EventType)
//!   │     ├── handlers::ModificationHandler → (:Modification)-[:AFFECTS]->(:Method|:Class|:NacosConfig)
//!   │     ├── handlers::DeploymentHandler   → (:Deployment)-[:DEPLOYS]->(:ServiceInstance)
//!   │     ├── handlers::BugFixHandler       → (:BugFix)-[:FIXES]->(:Method)
//!   │     └── handlers::DecisionHandler     → (:Decision)-[:BASED_ON]->(:Knowledge)
//!   └── entities::MemoryEvent (payload)
//! ```

pub mod dispatcher;
pub mod entities;
pub mod handlers;
pub mod service;
