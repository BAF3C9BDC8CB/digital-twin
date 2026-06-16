use anyhow::Result;

use crate::client::neo4j;

pub async fn rebuild_calls_for_project(project: &str) -> Result<u64> {
    neo4j::create_call_relationships(project).await
}

pub async fn rebuild_calls_for_files(project: &str, file_paths: &[String]) -> Result<u64> {
    neo4j::create_call_relationships_incremental(project, file_paths).await
}
