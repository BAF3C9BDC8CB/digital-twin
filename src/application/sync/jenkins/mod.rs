//! Jenkins 同步模块。
//!
//! 提供 [`JobSyncSource`]——将 Jenkins 的 Views、Jobs 和构建历史
//! 同步到知识图谱。
//!
//! # 创建的节点类型
//!
//! - `JenkinsView` — Jenkins 视图（命名空间分组）
//! - `JenkinsJob` — Jenkins 作业
//! - `JenkinsBuild` — 作业的单个构建
//!
//! # 关系
//!
//! - `(:JenkinsView)-[:CONTAINS]->(:JenkinsJob)`
//! - `(:JenkinsJob)-[:HAS_BUILD]->(:JenkinsBuild)`
//! - `(:JenkinsBuild)-[:NEXT_BUILD]->(:JenkinsBuild)`（有序链）

pub mod job_sync;

pub use job_sync::JobSyncSource;
