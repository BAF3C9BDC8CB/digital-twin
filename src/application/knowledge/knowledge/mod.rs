//! Knowledge 世界：概念、模式与经验管理。
//!
//! 提供实体（Knowledge、KnowledgeVersion、Experience、Concept、
//! Domain、Playbook）、面向知识维度操作的 [`KnowledgeService`] trait，
//! 以及由 [`GraphRepository`] 支撑的 [`DefaultKnowledgeService`]。
//!
//! # 架构
//!
//! ```text
//! service::DefaultKnowledgeService (trait impl)
//!   ├── write_knowledge   → MERGE (:Knowledge)
//!   ├── write_experience  → MERGE (:Experience)
//!   ├── write_concept     → MERGE (:Concept)
//!   ├── write_domain      → MERGE (:Domain)
//!   ├── write_playbook    → MERGE (:Playbook)
//!   └── update_knowledge  → CREATE new version + [:EVOLVED_FROM] + (:KnowledgeVersion)
//! ```
//!
//! # 实体关系
//!
//! ```text
//! (:Domain)-[:CONTAINS]->(:Knowledge)
//! (:Domain)-[:CONTAINS]->(:Concept)
//! (:Knowledge)-[:EVOLVED_FROM]->(:Knowledge)
//! (:KnowledgeVersion)-[:RECORDS]->(:Knowledge)
//! (:Playbook)-[:USES_KNOWLEDGE]->(:Knowledge)
//! (:Experience)-[:RELATED_TO]->(:Knowledge)
//! (:Experience)-[:HAPPENED_IN]->(:Session)
//! (:Concept)-[:RELATED_TO]->(:Concept)
//! (:Concept)-[:IMPLEMENTED_BY]->(:Method)
//! ```

pub mod annotation;
pub mod entities;
pub mod service;

pub use annotation::{parse_details, parse_value_list};
pub use entities::{
    Concept, Domain, Experience, ExperienceSeverity, Knowledge, KnowledgeSource, KnowledgeVersion,
    Playbook, Step,
};
pub use service::{
    concept_from_details, domain_from_details, experience_from_details, knowledge_from_details,
    playbook_from_details, DefaultKnowledgeService, KnowledgeService,
};
