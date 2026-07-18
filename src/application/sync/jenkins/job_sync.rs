//! Synchronise Jenkins Views / Jobs / Builds into the knowledge graph.
//!
//! # Flow
//!
//! 1. Fetch all jobs from Jenkins via [`JenkinsApiClient::list_all_jobs`]
//! 2. For each job, extract view name from `full_name` and create `JenkinsView`
//! 3. Create `JenkinsJob` node linked to its view
//! 4. Fetch all builds via [`JenkinsApiClient::get_all_builds`]
//! 5. Create `JenkinsBuild` nodes linked to the job
//! 6. Chain builds with `[:NEXT_BUILD]` relationships

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::sync::traits::{SyncReport, SyncSource};
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

/// Synchronise Jenkins Views, Jobs, and Builds into Neo4j.
pub struct JobSyncSource {
    client: Arc<JenkinsApiClient>,
    env_name: String,
    job_filter: Option<String>,
}

impl JobSyncSource {
    /// Create a new Jenkins sync source.
    ///
    /// * `client` — authenticated Jenkins API client
    /// * `env_name` — environment label (e.g. "test", "prod")
    /// * `job_filter` — optional: only sync this specific job
    pub fn new(
        client: Arc<JenkinsApiClient>,
        env_name: String,
        job_filter: Option<String>,
    ) -> Self {
        Self {
            client,
            env_name,
            job_filter,
        }
    }

    /// Extract view name from a Jenkins job's `full_name`.
    ///
    /// Jenkins folders use `/` as separator. E.g. `folder/subfolder/jobname`
    /// returns `folder/subfolder`. A top-level job returns `"default"`.
    fn extract_view_name(full_name: &str) -> String {
        if let Some(idx) = full_name.rfind('/') {
            full_name[..idx].to_string()
        } else {
            "default".to_string()
        }
    }

    /// Build a unique node ID for a view.
    fn view_id(env: &str, view_name: &str) -> String {
        format!("dt://jenkins/{env}/view/{view_name}")
    }

    /// Build a unique node ID for a job.
    fn job_id(env: &str, full_name: &str) -> String {
        format!("dt://jenkins/{env}/job/{full_name}")
    }

    /// Build a unique node ID for a build.
    fn build_id(env: &str, full_name: &str, build_number: i64) -> String {
        format!("dt://jenkins/{env}/job/{full_name}/build/{build_number}")
    }
}

impl JobSyncSource {
    /// Sync a single job (for incremental updates after `jcli build`).
    pub async fn sync_job(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError> {
        self.sync(graph).await
    }
}

#[async_trait]
impl SyncSource for JobSyncSource {
    fn name(&self) -> &str {
        "jenkins/jobs"
    }

    async fn sync(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError> {
        let start = std::time::Instant::now();
        let mut report = SyncReport {
            source: format!("jenkins/{}", self.env_name),
            ..SyncReport::default()
        };

        // ── 1. Fetch all jobs ─────────────────────────────────────────────
        let all_jobs = self.client.list_all_jobs().await?;
        let jobs: Vec<_> = if let Some(ref filter) = self.job_filter {
            all_jobs.into_iter().filter(|j| j.name == *filter || j.full_name == *filter).collect()
        } else {
            all_jobs
        };

        if jobs.is_empty() {
            println!("  (no jobs found)");
            report.elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(report);
        }

        // ── 2. Collect unique views ───────────────────────────────────────
        let mut views: Vec<String> = jobs
            .iter()
            .map(|j| Self::extract_view_name(&j.full_name))
            .collect();
        views.sort();
        views.dedup();
        report.namespaces = views.len();

        println!(
            "Jenkins sync ({}): {} views, {} jobs",
            self.env_name,
            views.len(),
            jobs.len(),
        );

        // ── 3. MERGE views ────────────────────────────────────────────────
        for view_name in &views {
            let vid = Self::view_id(&self.env_name, view_name);
            let cypher = r#"
                MERGE (v:JenkinsView {view_id: $view_id})
                ON CREATE SET
                    v.name = $name,
                    v.url = $url,
                    v.env = $env
                ON MATCH SET
                    v.name = $name,
                    v.url = $url,
                    v.env = $env
            "#;
            let mut params = HashMap::new();
            params.insert("view_id".to_string(), serde_json::json!(vid));
            params.insert("name".to_string(), serde_json::json!(view_name));
            params.insert("url".to_string(), serde_json::json!(""));
            params.insert("env".to_string(), serde_json::json!(&self.env_name));
            graph.write_query(cypher, params).await?;
        }

        // ── 4. Process each job ───────────────────────────────────────────
        for job in &jobs {
            let jid = Self::job_id(&self.env_name, &job.full_name);
            let view_name = Self::extract_view_name(&job.full_name);
            let vid = Self::view_id(&self.env_name, &view_name);

            print!("  job: {}... ", job.name);

            // MERGE job node
            let merge_job = r#"
                MERGE (j:JenkinsJob {job_id: $job_id})
                ON CREATE SET
                    j.name = $name,
                    j.url = $url,
                    j.color = $color,
                    j.description = $description,
                    j.full_name = $full_name,
                    j.env = $env
                ON MATCH SET
                    j.name = $name,
                    j.url = $url,
                    j.color = $color,
                    j.description = $description,
                    j.full_name = $full_name,
                    j.env = $env
            "#;
            let mut params = HashMap::new();
            params.insert("job_id".to_string(), serde_json::json!(jid));
            params.insert("name".to_string(), serde_json::json!(&job.name));
            params.insert("url".to_string(), serde_json::json!(&job.url));
            params.insert("color".to_string(), serde_json::json!(&job.color));
            params.insert("description".to_string(), serde_json::json!(&job.description));
            params.insert("full_name".to_string(), serde_json::json!(&job.full_name));
            params.insert("env".to_string(), serde_json::json!(&self.env_name));
            graph.write_query(merge_job, params).await?;
            report.configs += 1;

            // RELATE job → view
            let rel_job_view = r#"
                MATCH (j:JenkinsJob {job_id: $job_id})
                MATCH (v:JenkinsView {view_id: $view_id})
                MERGE (v)-[:CONTAINS]->(j)
            "#;
            let mut params = HashMap::new();
            params.insert("job_id".to_string(), serde_json::json!(jid));
            params.insert("view_id".to_string(), serde_json::json!(vid));
            graph.write_query(rel_job_view, params).await?;
            report.links_created += 1;

            // ── 5. Fetch builds ───────────────────────────────────────────
            let mut builds = self.client.get_all_builds(&job.name, &job.full_name).await?;

            if builds.is_empty() {
                println!("0 builds");
                continue;
            }

            let build_count = builds.len();
            println!("{build_count} builds");

            // Sort builds by number ascending for chain creation
            builds.sort_by_key(|b| b.number);

            for build in &builds {
                let bid = Self::build_id(&self.env_name, &job.full_name, build.number);

                let merge_build = r#"
                    MERGE (b:JenkinsBuild {build_id: $build_id})
                    ON CREATE SET
                        b.number = $number,
                        b.result = $result,
                        b.timestamp = $timestamp,
                        b.duration = $duration,
                        b.url = $url,
                        b.env = $env
                    ON MATCH SET
                        b.number = $number,
                        b.result = $result,
                        b.timestamp = $timestamp,
                        b.duration = $duration,
                        b.url = $url,
                        b.env = $env
                "#;
                let mut params = HashMap::new();
                params.insert("build_id".to_string(), serde_json::json!(bid));
                params.insert("number".to_string(), serde_json::json!(build.number));
                params.insert("result".to_string(), serde_json::json!(build.result));
                params.insert("timestamp".to_string(), serde_json::json!(build.timestamp));
                params.insert("duration".to_string(), serde_json::json!(build.duration));
                params.insert("url".to_string(), serde_json::json!(&build.url));
                params.insert("env".to_string(), serde_json::json!(&self.env_name));
                graph.write_query(merge_build, params).await?;
                report.items_created += 1;

                // RELATE job → build
                let rel_job_build = r#"
                    MATCH (j:JenkinsJob {job_id: $job_id})
                    MATCH (b:JenkinsBuild {build_id: $build_id})
                    MERGE (j)-[:HAS_BUILD]->(b)
                "#;
                let mut params = HashMap::new();
                params.insert("job_id".to_string(), serde_json::json!(&jid));
                params.insert("build_id".to_string(), serde_json::json!(bid));
                graph.write_query(rel_job_build, params).await?;
                report.links_created += 1;
            }

            // ── 6. Create build chain (NEXT_BUILD) ────────────────────────
            for pair in builds.windows(2) {
                let prev_id = Self::build_id(&self.env_name, &job.full_name, pair[0].number);
                let next_id = Self::build_id(&self.env_name, &job.full_name, pair[1].number);

                let chain = r#"
                    MATCH (prev:JenkinsBuild {build_id: $prev_id})
                    MATCH (next:JenkinsBuild {build_id: $next_id})
                    MERGE (prev)-[:NEXT_BUILD]->(next)
                "#;
                let mut params = HashMap::new();
                params.insert("prev_id".to_string(), serde_json::json!(prev_id));
                params.insert("next_id".to_string(), serde_json::json!(next_id));
                graph.write_query(chain, params).await?;
                report.links_created += 1;
            }

            report.services += 1;
        }

        // ── 7. Cleanup orphan builds/jobs (not in current sync) ───────────
        // Only cleanup when syncing ALL jobs (no filter)
        if self.job_filter.is_none() {
            let clean_builds = r#"
                MATCH (b:JenkinsBuild {env: $env})
                WHERE NOT (b)<-[:HAS_BUILD]-(:JenkinsJob)
                DETACH DELETE b
            "#;
            let mut params = HashMap::new();
            params.insert("env".to_string(), serde_json::json!(&self.env_name));
            let _ = graph.write_query(clean_builds, params).await;

            let clean_jobs = r#"
                MATCH (j:JenkinsJob {env: $env})
                WHERE NOT (j)<-[:CONTAINS]-(:JenkinsView)
                DETACH DELETE j
            "#;
            let mut params = HashMap::new();
            params.insert("env".to_string(), serde_json::json!(&self.env_name));
            let _ = graph.write_query(clean_jobs, params).await;
        }

        report.elapsed_ms = start.elapsed().as_millis() as u64;
        report.skipped = false;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_view_name_top_level() {
        assert_eq!(JobSyncSource::extract_view_name("my-job"), "default");
    }

    #[test]
    fn extract_view_name_in_folder() {
        assert_eq!(
            JobSyncSource::extract_view_name("folder/my-job"),
            "folder"
        );
    }

    #[test]
    fn extract_view_name_nested() {
        assert_eq!(
            JobSyncSource::extract_view_name("folder/sub/my-job"),
            "folder/sub"
        );
    }

    #[test]
    fn view_id_format() {
        assert_eq!(
            JobSyncSource::view_id("test", "default"),
            "dt://jenkins/test/view/default"
        );
    }

    #[test]
    fn job_id_format() {
        assert_eq!(
            JobSyncSource::job_id("prod", "my-service"),
            "dt://jenkins/prod/job/my-service"
        );
    }

    #[test]
    fn job_id_format_with_folder() {
        assert_eq!(
            JobSyncSource::job_id("test", "team-a/my-service"),
            "dt://jenkins/test/job/team-a/my-service"
        );
    }

    #[test]
    fn build_id_format() {
        assert_eq!(
            JobSyncSource::build_id("test", "my-service", 42),
            "dt://jenkins/test/job/my-service/build/42"
        );
    }

    #[test]
    fn build_id_format_with_folder() {
        assert_eq!(
            JobSyncSource::build_id("test", "team-a/my-service", 42),
            "dt://jenkins/test/job/team-a/my-service/build/42"
        );
    }
}
