# `dt jc-sync` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `dt jc-sync` command that syncs Jenkins Views, Jobs, and build history into the Neo4j knowledge graph.

**Architecture:** Follows the existing `nacos-sync` pattern: a `SyncSource` trait implementation (`JobSyncSource`) that fetches data via the existing `JenkinsApiClient` and writes to KG via `GraphRepository`. CLI handler in `src/interfaces/cli/jenkins_sync.rs`. Command registered in `main.rs` Commands enum.

**Tech Stack:** Rust, reqwest (Jenkins HTTP API), Neo4j (bolt), serde_json

## Global Constraints

- All node labels use PascalCase: `JenkinsView`, `JenkinsJob`, `JenkinsBuild`
- Node ID format: `dt://jenkins/{env}/{view_name}` / `dt://jenkins/{env}/{job_name}` / `dt://jenkins/{env}/{job_name}/build/{number}`
- Follow existing `SyncSource` trait + `SyncReport` pattern exactly
- Use `MERGE` + `ON CREATE SET` / `ON MATCH SET` for idempotent writes
- Reuse existing `JenkinsApiClient`; add new methods, don't modify existing ones

---

## File Structure

### New files
| File | Responsibility |
|------|---------------|
| `src/application/sync/jenkins/mod.rs` | Module exports, re-export `JobSyncSource` |
| `src/application/sync/jenkins/job_sync.rs` | `JobSyncSource`: fetch Jenkins data, write to KG |
| `src/interfaces/cli/jenkins_sync.rs` | CLI handler for `dt jc-sync` |

### Modified files
| File | Change |
|------|--------|
| `src/application/plugins/jenkins/client.rs` | Add `list_all_jobs()` and `get_all_builds()` structured methods |
| `src/application/sync/mod.rs` | Add `pub mod jenkins;` |
| `src/infrastructure/neo4j/schema.rs` | Add 3 Jenkins constraints + fulltext labels |
| `src/interfaces/cli/mod.rs` | Add `pub mod jenkins_sync;` |
| `src/main.rs` | Add `JcSync` command variant + handler |

---

### Task 1: Add structured API methods to JenkinsApiClient

**Files:**
- Modify: `src/application/plugins/jenkins/client.rs`

**Interfaces:**
- Produces: `JenkinsJobInfo` struct, `JenkinsBuildInfo` struct,
  `JenkinsApiClient::list_all_jobs()` → `Result<Vec<JenkinsJobInfo>, DtError>`,
  `JenkinsApiClient::get_all_builds(job_name)` → `Result<Vec<JenkinsBuildInfo>, DtError>`

- [ ] **Step 1: Add response structs and new methods after line 265 (before `impl Default`)**

```rust
// ── Structured response types for jc-sync ───────────────────────────────

/// Structured Jenkins job info for sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsJobInfo {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub full_name: String,
}

/// Structured Jenkins build info for sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JenkinsBuildInfo {
    pub number: i64,
    pub result: Option<String>,
    pub timestamp: i64,
    pub duration: i64,
    pub url: String,
}

impl JenkinsApiClient {
    /// Fetch all jobs with full details (for jc-sync).
    pub async fn list_all_jobs(&self) -> Result<Vec<JenkinsJobInfo>, DtError> {
        let json = self
            .get_json("/api/json?tree=jobs[name,url,color,description,fullName]")
            .await?;
        let jobs: Vec<JenkinsJobInfo> = json["jobs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|j| serde_json::from_value(j.clone()).unwrap_or_else(|_| JenkinsJobInfo {
                        name: j["name"].as_str().unwrap_or("?").to_string(),
                        url: j["url"].as_str().unwrap_or("").to_string(),
                        color: j["color"].as_str().unwrap_or("").to_string(),
                        description: j["description"].as_str().unwrap_or("").to_string(),
                        full_name: j["fullName"].as_str().unwrap_or("").to_string(),
                    }))
                    .collect()
            })
            .unwrap_or_default();
        Ok(jobs)
    }

    /// Fetch all builds for a job (for jc-sync).
    pub async fn get_all_builds(&self, job_name: &str) -> Result<Vec<JenkinsBuildInfo>, DtError> {
        let encoded = urlencoding(job_name);
        let json = self
            .get_json(&format!(
                "/job/{}/api/json?tree=builds[number,result,timestamp,duration,url]",
                encoded
            ))
            .await?;
        let builds: Vec<JenkinsBuildInfo> = json["builds"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|b| serde_json::from_value(b.clone()).unwrap_or_else(|_| JenkinsBuildInfo {
                        number: b["number"].as_i64().unwrap_or(0),
                        result: b["result"].as_str().map(|s| s.to_string()),
                        timestamp: b["timestamp"].as_i64().unwrap_or(0),
                        duration: b["duration"].as_i64().unwrap_or(0),
                        url: b["url"].as_str().unwrap_or("").to_string(),
                    }))
                    .collect()
            })
            .unwrap_or_default();
        Ok(builds)
    }
}
```

- [ ] **Step 2: Build to verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: Compilation succeeds (may have warnings).

- [ ] **Step 3: Commit**

```bash
git add src/application/plugins/jenkins/client.rs
git commit -m "feat(jenkins): add structured API methods for jc-sync"
```

---

### Task 2: Create Jenkins sync module (JobSyncSource)

**Files:**
- Create: `src/application/sync/jenkins/mod.rs`
- Create: `src/application/sync/jenkins/job_sync.rs`

**Interfaces:**
- Consumes: `JenkinsApiClient::list_all_jobs()`, `JenkinsApiClient::get_all_builds()`
- Consumes: `SyncSource` trait, `SyncReport`, `GraphRepository`
- Produces: `JobSyncSource` struct implementing `SyncSource`

- [ ] **Step 1: Create `src/application/sync/jenkins/mod.rs`**

```rust
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
```

- [ ] **Step 2: Create `src/application/sync/jenkins/job_sync.rs`**

```rust
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
    fn job_id(env: &str, job_name: &str) -> String {
        format!("dt://jenkins/{env}/job/{job_name}")
    }

    /// Build a unique node ID for a build.
    fn build_id(env: &str, job_name: &str, build_number: i64) -> String {
        format!("dt://jenkins/{env}/job/{job_name}/build/{build_number}")
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
            report.links_created += 1;
        }

        // ── 4. Process each job ───────────────────────────────────────────
        for job in &jobs {
            let jid = Self::job_id(&self.env_name, &job.name);
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
            let builds = self.client.get_all_builds(&job.name).await?;

            if builds.is_empty() {
                println!("0 builds");
                continue;
            }

            let build_count = builds.len();
            println!("{build_count} builds");

            // Sort builds by number ascending for chain creation
            let mut sorted_builds = builds.clone();
            sorted_builds.sort_by_key(|b| b.number);

            for build in &sorted_builds {
                let bid = Self::build_id(&self.env_name, &job.name, build.number);

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
            for pair in sorted_builds.windows(2) {
                let prev_id = Self::build_id(&self.env_name, &job.name, pair[0].number);
                let next_id = Self::build_id(&self.env_name, &job.name, pair[1].number);

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
            JobSyncSource::job_id("prod", "my-job"),
            "dt://jenkins/prod/job/my-job"
        );
    }

    #[test]
    fn build_id_format() {
        assert_eq!(
            JobSyncSource::build_id("test", "my-job", 42),
            "dt://jenkins/test/job/my-job/build/42"
        );
    }
}
```

- [ ] **Step 3: Build to verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: Compilation succeeds (with warnings about unused import of `HashMap` — will be used in later wiring).

- [ ] **Step 4: Commit**

```bash
git add src/application/sync/jenkins/
git commit -m "feat(jenkins): add JobSyncSource for syncing views/jobs/builds"
```

---

### Task 3: Create CLI handler for jc-sync

**Files:**
- Create: `src/interfaces/cli/jenkins_sync.rs`

**Interfaces:**
- Consumes: `JobSyncSource`, `JenkinsApiClient`
- Produces: `handle_jenkins_sync()` function

- [ ] **Step 1: Create `src/interfaces/cli/jenkins_sync.rs`**

```rust
//! CLI handler for `dt jc-sync` — synchronise Jenkins Views, Jobs, Builds
//! into the knowledge graph.

use std::sync::Arc;

use crate::application::sync::jenkins::JobSyncSource;
use crate::application::sync::traits::SyncSource;
use crate::domain::traits::GraphRepository;
use crate::application::plugins::jenkins::client::JenkinsApiClient;

/// Handle `dt jc-sync` — synchronise Jenkins data into the KG.
///
/// `env` — optional environment filter ("test" or "prod").
/// If `None`, syncs both "test" and "prod".
///
/// `job` — optional job name filter. If set, only syncs that job.
///
/// `graph` must be pre-connected.
pub async fn handle_jenkins_sync(
    env: Option<String>,
    job: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    jenkins_url: &str,
    jenkins_user: &str,
    jenkins_token: &str,
) -> anyhow::Result<()> {
    let envs: Vec<&str> = match env.as_deref() {
        Some(e) => vec![e],
        None => vec!["test", "prod"],
    };

    let client = Arc::new(JenkinsApiClient::new(jenkins_url, jenkins_user, jenkins_token));

    for env_name in &envs {
        println!("\n── Jenkins sync: {env_name} ──");

        let source = JobSyncSource::new(client.clone(), env_name.to_string(), job.clone());

        match graph.as_deref() {
            Some(g) => {
                let report = source.sync(g).await?;
                println!(
                    "  ✓ {} views, {} jobs, {} builds ({} links, {}ms)",
                    report.namespaces,
                    report.services,
                    report.items_created,
                    report.links_created,
                    report.elapsed_ms,
                );
                if !report.errors.is_empty() {
                    for err in &report.errors {
                        eprintln!("  ⚠ {err}");
                    }
                }
            }
            None => {
                eprintln!("  ✗ Neo4j unavailable — skipping");
            }
        }
    }

    println!("\n✓ Jenkins sync complete");
    Ok(())
}
```

- [ ] **Step 2: Build to verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: Compilation succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/interfaces/cli/jenkins_sync.rs
git commit -m "feat(jenkins): add CLI handler for dt jc-sync"
```

---

### Task 4: Register jc-sync in main.rs and module files

**Files:**
- Modify: `src/main.rs`
- Modify: `src/application/sync/mod.rs`
- Modify: `src/interfaces/cli/mod.rs`

- [ ] **Step 1: Add `pub mod jenkins_sync;` to `src/interfaces/cli/mod.rs`**

After line 18 (`pub mod jcli;`), add:
```rust
pub mod jenkins_sync;
```

- [ ] **Step 2: Add `pub mod jenkins;` to `src/application/sync/mod.rs`**

After line 2 (`pub mod traits;`), add:
```rust
pub mod jenkins;
```

- [ ] **Step 3: Add `JcSync` variant to Commands enum in `src/main.rs`**

After the `Jcli` variant (line 529), add:
```rust
/// Synchronize Jenkins Views, Jobs, and Builds to Knowledge Graph.
JcSync {
    /// Target environment (test, prod). Default: sync all.
    #[arg(long = "env")]
    env: Option<String>,

    /// Specific job name to sync. Default: sync all jobs.
    #[arg(long = "job")]
    job: Option<String>,
},
```

- [ ] **Step 4: Add match arm for JcSync in `src/main.rs`**

After the Jcli handler (after line 1620 `return Ok(());`), add:
```rust
        // ---- CLI mode: dt jc-sync ----
        Some(Commands::JcSync { env, job }) => {
            let config = load_config();
            let jenkins_creds = config.as_ref().and_then(|c| {
                let j = &c.services.jenkins;
                let url = j.url.as_deref()?;
                let user = j.user.as_deref()?;
                let token = j.token.as_deref()?;
                if url.is_empty() { return None; }
                Some((url.to_string(), user.to_string(), token.to_string()))
            });

            match jenkins_creds {
                Some((url, user, token)) => {
                    let graph = connect_graph().await;
                    dt_daemon::interfaces::cli::jenkins_sync::handle_jenkins_sync(
                        env, job, graph, &url, &user, &token,
                    )
                    .await?;
                }
                None => {
                    eprintln!("Jenkins not configured in config.yaml (services.jenkins). Add jenkins section with url/user/token to enable.");
                }
            }
            return Ok(());
        }
```

- [ ] **Step 5: Build to verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: Compilation succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/application/sync/mod.rs src/interfaces/cli/mod.rs
git commit -m "feat(jenkins): register dt jc-sync command"
```

---

### Task 5: Add Neo4j schema constraints for Jenkins

**Files:**
- Modify: `src/infrastructure/neo4j/schema.rs`

- [ ] **Step 1: Add 3 Jenkins constraints to `CONSTRAINT_STATEMENTS`**

After line 75 (nacos_config_id_unique), add:
```rust
    // ── Jenkins ──
    "CREATE CONSTRAINT jenkins_view_id_unique IF NOT EXISTS FOR (n:JenkinsView) REQUIRE n.view_id IS UNIQUE",
    "CREATE CONSTRAINT jenkins_job_id_unique IF NOT EXISTS FOR (n:JenkinsJob) REQUIRE n.job_id IS UNIQUE",
    "CREATE CONSTRAINT jenkins_build_id_unique IF NOT EXISTS FOR (n:JenkinsBuild) REQUIRE n.build_id IS UNIQUE",
```

- [ ] **Step 2: Add Jenkins labels to fulltext index**

Replace the `FULLTEXT_INDEX_STATEMENT` line (112) with:
```rust
FOR (n:Server|Database|NacosConfig|NacosService|K8sDeployment|K8sService|Service|ServiceInstance|Method|Class|Module|Knowledge|Concept|Experience|Playbook|Document|Thread|ConfigKey|Endpoint|JenkinsView|JenkinsJob|JenkinsBuild)
```

- [ ] **Step 3: Update test assertion counts**

In the test `init_schema_creates_all_constraints` (line 306), change the expected constraint count from 27 to 30:
```rust
        // 30 constraints + 1 fulltext index + 1 regular index
        assert_eq!(report.constraints_created, 30);
```

And on line 316, change:
```rust
        assert_eq!(write_calls.len(), 32); // 30 constraints + 2 indexes
```

And in `init_schema_is_idempotent_via_if_not_exists` (line 328):
```rust
        assert_eq!(report.constraints_created, 30);
```

- [ ] **Step 4: Build to verify compilation + test**

```bash
cargo build 2>&1 | head -20
cargo test --lib infrastructure::neo4j::schema::tests 2>&1 | tail -15
```

Expected: Tests pass with updated assertion counts.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/neo4j/schema.rs
git commit -m "feat(neo4j): add Jenkins constraints and fulltext index labels"
```

---

### Task 6: Wire jcli build to trigger incremental sync

**Files:**
- Modify: `src/interfaces/cli/jcli.rs`
- Modify: `src/application/sync/jenkins/job_sync.rs`

- [ ] **Step 1: Add `sync_job()` method to `JobSyncSource` for single-job incremental sync**

Add to `src/application/sync/jenkins/job_sync.rs` before the `#[async_trait]` impl block:

```rust
impl JobSyncSource {
    /// Sync a single job (for incremental updates after `jcli build`).
    pub async fn sync_job(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError> {
        self.sync(graph).await
    }
}
```

- [ ] **Step 2: Modify `handle_jcli` in `src/interfaces/cli/jcli.rs` to accept a graph parameter**

Change the function signature to accept an optional graph:
```rust
pub async fn handle_jcli(
    action: String,
    job: Option<String>,
    build: Option<String>,
    limit: Option<u32>,
    params: Option<String>,
    env: String,
    jenkins_url: &str,
    jenkins_user: &str,
    jenkins_token: &str,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
```

Add the import at the top:
```rust
use std::sync::Arc;
use crate::domain::traits::GraphRepository;
use crate::application::sync::jenkins::JobSyncSource;
use crate::application::sync::traits::SyncSource;
```

After the `"build"` arm's success (`Ok(out) => { println!("{out}") }`), add incremental sync:
```rust
                    // After successful build, incrementally sync this job
                    if let Some(ref job_name) = job {
                        if let Some(ref g) = graph {
                            let client = crate::application::plugins::jenkins::client::JenkinsApiClient::new(
                                jenkins_url, jenkins_user, jenkins_token,
                            );
                            let source = JobSyncSource::new(
                                Arc::new(client),
                                env.clone(),
                                Some(job_name.clone()),
                            );
                            match source.sync_job(g.as_ref()).await {
                                Ok(r) => tracing::info!(
                                    "jcli build: incremental sync for {job_name}: {} builds",
                                    r.items_created,
                                ),
                                Err(e) => tracing::warn!(
                                    "jcli build: incremental sync failed for {job_name}: {e}",
                                ),
                            }
                        }
                    }
```

- [ ] **Step 3: Update the Jcli match arm in `src/main.rs` to pass `graph`**

Change the Jcli handler in main.rs (around line 1609-1614) to pass graph:
```rust
            match jenkins_creds {
                Some((url, user, token)) => {
                    let graph = connect_graph().await;
                    dt_daemon::interfaces::cli::jcli::handle_jcli(
                        action, job, build, limit, params, env, &url, &user, &token, graph,
                    )
                    .await?;
                }
```

- [ ] **Step 4: Build to verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: Compilation succeeds.

- [ ] **Step 5: Commit**

```bash
git add src/interfaces/cli/jcli.rs src/main.rs
git commit -m "feat(jenkins): wire jcli build to trigger incremental jc-sync"
```

---

## Verification

```bash
# Full build
cargo build 2>&1 | tail -5

# Run tests
cargo test --lib 2>&1 | tail -20

# Run jc-sync (requires Jenkins connectivity)
dt jc-sync --env test

# Verify in KG
dt cypher "MATCH (v:JenkinsView) RETURN v.name, v.env LIMIT 10"
dt cypher "MATCH (j:JenkinsJob)-[:HAS_BUILD]->(b:JenkinsBuild) RETURN j.name, count(b) AS builds LIMIT 10"
```
