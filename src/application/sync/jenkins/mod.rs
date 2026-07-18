//! Jenkins synchronisation module.
//!
//! Provides [`JobSyncSource`] — syncs Jenkins Views, Jobs, and build history
//! into the knowledge graph.
//!
//! # Node types created
//!
//! - `JenkinsView` — a Jenkins view (namespace group)
//! - `JenkinsJob` — a Jenkins job
//! - `JenkinsBuild` — a single build of a job
//!
//! # Relationships
//!
//! - `(:JenkinsView)-[:CONTAINS]->(:JenkinsJob)`
//! - `(:JenkinsJob)-[:HAS_BUILD]->(:JenkinsBuild)`
//! - `(:JenkinsBuild)-[:NEXT_BUILD]->(:JenkinsBuild)` (ordered chain)

pub mod job_sync;

pub use job_sync::JobSyncSource;
