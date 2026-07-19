# Plan: `dt jc-sync` — Jenkins 同步命令

## Files
- `src/application/sync/jenkins/mod.rs` — **create**
- `src/application/sync/jenkins/job_sync.rs` — **create**
- `src/interfaces/cli/jenkins_sync.rs` — **create**
- `src/application/plugins/jenkins/client.rs` — **modify** (add structured API methods)
- `src/application/sync/mod.rs` — **modify**
- `src/infrastructure/neo4j/schema.rs` — **modify**
- `src/interfaces/cli/mod.rs` — **modify**
- `src/main.rs` — **modify**

## Steps

### 1. Add structured API methods to JenkinsApiClient
- `list_all_jobs()` → returns `Vec<JenkinsJobInfo>` with name, url, color, description, full_name
- `get_job_builds(job_name)` → returns `Vec<JenkinsBuildInfo>` with number, result, timestamp, duration, url
- Parse Jenkins API JSON responses into typed structs

### 2. Create `src/application/sync/jenkins/` module
- `mod.rs` — export JobSyncSource
- `job_sync.rs` — JobSyncSource implementing SyncSource trait
  - Fetch all jobs via client
  - Extract view info from job full_name
  - MERGE JenkinsView nodes
  - MERGE JenkinsJob nodes with env
  - Fetch all builds per job, MERGE JenkinsBuild nodes
  - Create relationships: CONTAINS, HAS_BUILD, NEXT_BUILD
  - Clean orphaned JenkinsJob/JenkinsBuild nodes for this env

### 3. Create `src/interfaces/cli/jenkins_sync.rs`
- `handle_jenkins_sync()` — analogous to `handle_nacos_sync()`
- Accepts env filter + optional job filter
- Connects to Jenkins, runs JobSyncSource, prints report

### 4. Register in main.rs
- Add `JcSync` variant to Commands enum with `--env` and `--job` args
- Add match arm: load config → resolve Jenkins creds → call handler

### 5. Register in module files
- `src/application/sync/mod.rs` — add `pub mod jenkins;`
- `src/interfaces/cli/mod.rs` — add `pub mod jenkins_sync;`

### 6. Add Neo4j schema constraints
- 3 new constraints: JenkinsView, JenkinsJob, JenkinsBuild
- Add these labels to fulltext index
- Update test assertion counts

### 7. Wire jcli build to trigger incremental sync
- After successful `trigger_build()` in jcli handler, call job_sync for that job
- Record dt event --type Deploy

## Verification
```bash
cargo build 2>&1 | head -50
# Then if build succeeds:
dt jc-sync --env test
# Check KG:
MATCH (j:JenkinsJob)-[:HAS_BUILD]->(b:JenkinsBuild) RETURN j.name, count(b) LIMIT 10
```
