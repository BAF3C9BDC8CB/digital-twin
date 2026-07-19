//! Synchronise Jenkins Views / Jobs / Builds into the knowledge graph.
//!
//! # Flow
//!
//! 1. Fetch all views with nested jobs via [`JenkinsApiClient::list_views`]
//! 2. Create `JenkinsView` nodes from real Jenkins view names (JAVA, VUE, ...)
//! 3. Fetch all jobs via the flat `/api/json?tree=jobs[...]` endpoint for
//!    comprehensive coverage (some view types don't expand their job list)
//! 4. For each job, fetch all builds via [`JenkinsApiClient::get_all_builds`]
//! 5. Create `JenkinsJob` + `JenkinsBuild` nodes
//! 6. Create `[:CONTAINS]` relationships from view→job for jobs found in views
//! 7. Chain builds with `[:NEXT_BUILD]` relationships
//!
//! # Design
//!
//! - View names come from Jenkins API, not synthesized from job full_name
//! - A job can belong to multiple views (as returned by Jenkins API)
//! - All jobs are synced regardless of view membership — view→job mapping is
//!   purely for the `[:CONTAINS]` relationship (a job without a view still exists)
//! - No `env` field — environment (test/prod) is determined by view name, not
//!   stored as a property on the node
//! - entity_id URIs follow `dt://jenkins/{type}/...` pattern

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
    job_filter: Option<String>,
}

impl JobSyncSource {
    /// Create a new Jenkins sync source.
    ///
    /// * `client` — authenticated Jenkins API client
    /// * `job_filter` — optional: only sync this specific job
    pub fn new(client: Arc<JenkinsApiClient>, job_filter: Option<String>) -> Self {
        Self { client, job_filter }
    }

    /// Build a unique node ID for a view.
    fn view_id(view_name: &str) -> String {
        format!("dt://jenkins/view/{view_name}")
    }

    /// Build a unique node ID for a job.
    ///
    /// Falls back to `name` when `full_name` is empty (top-level jobs).
    fn job_id(full_name: &str, name: &str) -> String {
        let id = if full_name.is_empty() { name } else { full_name };
        format!("dt://jenkins/job/{id}")
    }

    /// Build a unique node ID for a build.
    ///
    /// Falls back to `name` when `full_name` is empty.
    fn build_id(full_name: &str, name: &str, build_number: i64) -> String {
        let id = if full_name.is_empty() { name } else { full_name };
        format!("dt://jenkins/job/{id}/build/{build_number}")
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
            source: "jenkins".into(),
            ..SyncReport::default()
        };

        // ── 1. Fetch views → for View nodes + CONTAINS mapping ──────────
        let all_views = self.client.list_views().await?;
        let views: Vec<_> = all_views.into_iter().filter(|v| v.name != "all").collect();
        report.namespaces = views.len();

        // Build view→job mapping (which jobs belong to which views)
        let mut job_to_views: HashMap<String, Vec<String>> = HashMap::new();
        for view in &views {
            for job in &view.jobs {
                let key = if job.full_name.is_empty() { job.name.clone() } else { job.full_name.clone() };
                job_to_views.entry(key).or_default().push(view.name.clone());
            }
        }

        // ── 2. MERGE view nodes ──────────────────────────────────────────
        for view in &views {
            let vid = Self::view_id(&view.name);
            let cypher = r#"
                MERGE (v:JenkinsView {view_id: $view_id})
                ON CREATE SET
                    v.name = $name,
                    v.description = $description
                ON MATCH SET
                    v.name = $name,
                    v.description = $description
            "#;
            let mut params = HashMap::new();
            params.insert("view_id".to_string(), serde_json::json!(vid));
            params.insert("name".to_string(), serde_json::json!(&view.name));
            params.insert("description".to_string(), serde_json::json!(&view.description));
            graph.write_query(cypher, params).await?;
        }

        // ── 3. Fetch ALL jobs (flat list) for comprehensive coverage ─────
        let all_jobs = self.client.list_all_jobs().await?;

        // Apply job filter if set
        let jobs: Vec<_> = if let Some(ref filter) = self.job_filter {
            all_jobs.into_iter().filter(|j| j.name == *filter || j.full_name == *filter).collect()
        } else {
            all_jobs
        };

        if jobs.is_empty() {
            println!("  (no matching jobs found)");
            report.elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(report);
        }

        println!(
            "Jenkins sync: {} views, {} jobs",
            views.len(),
            jobs.len(),
        );

        // ── 4. Process each job ──────────────────────────────────────────
        for job in &jobs {
            let jid = Self::job_id(&job.full_name, &job.name);
            let key = if job.full_name.is_empty() { &job.name } else { &job.full_name };

            print!("  job: {}... ", job.name);

            // MERGE job node
            let merge_job = r#"
                MERGE (j:JenkinsJob {job_id: $job_id})
                ON CREATE SET
                    j.name = $name,
                    j.url = $url,
                    j.color = $color,
                    j.description = $description,
                    j.full_name = $full_name
                ON MATCH SET
                    j.name = $name,
                    j.url = $url,
                    j.color = $color,
                    j.description = $description,
                    j.full_name = $full_name
            "#;
            let mut params = HashMap::new();
            params.insert("job_id".to_string(), serde_json::json!(jid));
            params.insert("name".to_string(), serde_json::json!(&job.name));
            params.insert("url".to_string(), serde_json::json!(&job.url));
            params.insert("color".to_string(), serde_json::json!(&job.color));
            params.insert("description".to_string(), serde_json::json!(&job.description));
            params.insert("full_name".to_string(), serde_json::json!(&job.full_name));
            graph.write_query(merge_job, params).await?;
            report.configs += 1;

            // RELATE job → its views (if any view carries this job)
            if let Some(view_names) = job_to_views.get(key) {
                for vn in view_names {
                    let vid = Self::view_id(vn);
                    let rel = r#"
                        MATCH (j:JenkinsJob {job_id: $job_id})
                        MATCH (v:JenkinsView {view_id: $view_id})
                        MERGE (v)-[:CONTAINS]->(j)
                    "#;
                    let mut params = HashMap::new();
                    params.insert("job_id".to_string(), serde_json::json!(&jid));
                    params.insert("view_id".to_string(), serde_json::json!(vid));
                    graph.write_query(rel, params).await?;
                    report.links_created += 1;
                }
            }

            // ── 5. Fetch builds ──────────────────────────────────────────
            let mut builds = match self.client.get_all_builds(&job.name, &job.full_name).await {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("{}: {e}", job.full_name);
                    eprintln!("  ⚠ builds fetch failed: {msg}");
                    report.add_error(&msg);
                    report.configs -= 1;
                    continue;
                }
            };

            if builds.is_empty() {
                println!("0 builds");
                continue;
            }

            println!("{} builds", builds.len());

            builds.sort_by_key(|b| b.number);

            for build in &builds {
                let bid = Self::build_id(&job.full_name, &job.name, build.number);

                let merge_build = r#"
                    MERGE (b:JenkinsBuild {build_id: $build_id})
                    ON CREATE SET
                        b.number = $number,
                        b.result = $result,
                        b.timestamp = $timestamp,
                        b.duration = $duration,
                        b.url = $url
                    ON MATCH SET
                        b.number = $number,
                        b.result = $result,
                        b.timestamp = $timestamp,
                        b.duration = $duration,
                        b.url = $url
                "#;
                let mut params = HashMap::new();
                params.insert("build_id".to_string(), serde_json::json!(bid));
                params.insert("number".to_string(), serde_json::json!(build.number));
                params.insert("result".to_string(), serde_json::json!(build.result));
                params.insert("timestamp".to_string(), serde_json::json!(build.timestamp));
                params.insert("duration".to_string(), serde_json::json!(build.duration));
                params.insert("url".to_string(), serde_json::json!(&build.url));
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
                let prev_id = Self::build_id(&job.full_name, &job.name, pair[0].number);
                let next_id = Self::build_id(&job.full_name, &job.name, pair[1].number);

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

        // ── 7. Cleanup: only builds without parent jobs ──────────────────
        if self.job_filter.is_none() {
            let clean_builds = r#"
                MATCH (b:JenkinsBuild)
                WHERE NOT (b)<-[:HAS_BUILD]-(:JenkinsJob)
                DETACH DELETE b
            "#;
            let _ = graph.write_query(clean_builds, HashMap::new()).await;
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
    fn view_id_format() {
        assert_eq!(
            JobSyncSource::view_id("JAVA"),
            "dt://jenkins/view/JAVA"
        );
    }

    #[test]
    fn view_id_format_with_dash() {
        assert_eq!(
            JobSyncSource::view_id("JAVA-TEST"),
            "dt://jenkins/view/JAVA-TEST"
        );
    }

    #[test]
    fn job_id_format() {
        assert_eq!(
            JobSyncSource::job_id("my-service", "my-service"),
            "dt://jenkins/job/my-service"
        );
    }

    #[test]
    fn job_id_empty_full_name() {
        assert_eq!(
            JobSyncSource::job_id("", "top-level-job"),
            "dt://jenkins/job/top-level-job"
        );
    }

    #[test]
    fn job_id_format_with_folder() {
        assert_eq!(
            JobSyncSource::job_id("team-a/my-service", "my-service"),
            "dt://jenkins/job/team-a/my-service"
        );
    }

    #[test]
    fn build_id_format() {
        assert_eq!(
            JobSyncSource::build_id("my-service", "my-service", 42),
            "dt://jenkins/job/my-service/build/42"
        );
    }

    #[test]
    fn build_id_empty_full_name() {
        assert_eq!(
            JobSyncSource::build_id("", "top-level-job", 7),
            "dt://jenkins/job/top-level-job/build/7"
        );
    }

    #[test]
    fn build_id_format_with_folder() {
        assert_eq!(
            JobSyncSource::build_id("team-a/my-service", "my-service", 42),
            "dt://jenkins/job/team-a/my-service/build/42"
        );
    }
}
