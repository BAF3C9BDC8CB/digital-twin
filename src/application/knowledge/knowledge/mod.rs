//! Knowledge world: concept, pattern, and experience management.
//!
//! Provides entities (Knowledge, KnowledgeVersion, Experience, Concept,
//! Domain, Playbook), the [`KnowledgeService`] trait for knowledge-dimension
//! operations, and a [`DefaultKnowledgeService`] backed by [`GraphRepository`].
//!
//! # Architecture
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
//! # Entity relationships
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
