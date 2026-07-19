# Jenkins Deployment Tracking Refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace standalone `(:Deployment)` nodes with three-node model: JenkinsJob → JenkinsBuild → ServiceInstance, linked by relationships.

**Architecture:** The `DeploymentHandler` currently creates `(:Deployment)` nodes. We rewrite it to update/merge `(:JenkinsJob)`, `(:JenkinsBuild)`, and `(:ServiceInstance)` nodes instead, and connect them via `[:LATEST_DEPLOY]` and `[:DEPLOYED_TO]` relationships. `EventType::Deployment` enum value is retained for CLI compatibility.

**Tech Stack:** Rust, Neo4j Cypher, tokio async

## Global Constraints

- `EventType::Deployment` enum value MUST NOT be removed (backward compat)
- The `Day → Session → HAS_EVENT` timeline chain MUST remain unchanged
- Event details format MUST support `build_number` for JenkinsBuild matching
- All existing tests for other handlers MUST continue to pass
- `Deployment` label MUST be removed from `BUSINESS_LABELS` in `kg_bridge.rs`

---

### Task 1: Refactor DeploymentHandler to three-node model

**Files:**
- Modify: `src/application/knowledge/memory/handlers/deployment.rs`
- Modify: `src/application/knowledge/memory/dispatcher.rs` (link_event_to_session: skip for Deployment, handler does it)

**Interfaces:**
- Consumes: `EventHandler` trait, `MemoryEvent { details: "job: X; env: Y; build_number: N; ..." }`
- Produces: Cypher that MATCH/MERGE JenkinsJob + JenkinsBuild + ServiceInstance + relationships + links Session

- [ ] **Step 1.1: Update handler doc and parse `build_number`**

```rust
//! [`DeploymentHandler`] — records deployment by linking JenkinsJob,
//! JenkinsBuild, and ServiceInstance in a three-node model.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `job`            — Jenkins Job name
//! - `env`            — deployment environment ("test" | "prod")
//! - `build_number`   — Jenkins build number (required, used to find/build_id)
//! - `branch`         — git branch
//! - `version`        — artifact version
//! - `params`         — build parameters (JSON)
//! - `status`         — "success" | "failure"
//!
//! Produces Cypher:
//! ```cypher
//! MERGE (job:JenkinsJob {name: $job}) ...
//! MATCH (build:JenkinsBuild {build_id: $build_id}) ...
//! MERGE (si:ServiceInstance {instance_id: $instance_id}) ...
//! MERGE (job)-[:LATEST_DEPLOY]->(si)
//! MERGE (build)-[:DEPLOYED_TO {env, version, deployed_at}]->(si)
//! ```
```

In the `handle()` method, add `build_number` parsing after line 63 (`let status = ...`):

```rust
        let build_number = props
            .get("build_number")
            .cloned()
            .unwrap_or_default();
```

- [ ] **Step 1.2: Replace the Cypher query (includes Session link)**

Replace lines 81-100 (the entire `let cypher = r#"..."#.to_string()` block) with:

```rust
        let build_id = if build_number.is_empty() {
            format!("dt://jenkins/job/{}/build/unknown", job)
        } else {
            format!("dt://jenkins/job/{}/build/{}", job, build_number)
        };

        let cypher = r#"
            // 1. Ensure JenkinsJob exists
            MERGE (job:JenkinsJob {name: $job})
            ON CREATE SET
                job.job_id = $job_id,
                job.full_name = $job,
                job.latest_deploy_env = $env,
                job.latest_deploy_version = $version,
                job.latest_deployed_at = $now
            ON MATCH SET
                job.latest_deploy_env = $env,
                job.latest_deploy_version = $version,
                job.latest_deployed_at = $now

            // 2. Update JenkinsBuild with deployment info
            MERGE (build:JenkinsBuild {build_id: $build_id})
            ON CREATE SET
                build.number = $build_number_int,
                build.deployed_env = $env,
                build.deployed_at = $now,
                build.deployed_version = $version,
                build.result = $status,
                build.timestamp = $timestamp_raw
            ON MATCH SET
                build.deployed_env = $env,
                build.deployed_at = $now,
                build.deployed_version = $version

            // 3. Create ServiceInstance
            MERGE (si:ServiceInstance {instance_id: $instance_id})
            ON CREATE SET
                si.service_name = $job,
                si.env = $env,
                si.updated_at = $now
            ON MATCH SET
                si.service_name = $job,
                si.env = $env,
                si.updated_at = $now

            // 4. Link JenkinsJob -> ServiceInstance (latest deploy, replace old)
            WITH job, build, si
            OPTIONAL MATCH (job)-[old:LATEST_DEPLOY]->()
            DELETE old
            MERGE (job)-[:LATEST_DEPLOY]->(si)

            // 5. Link JenkinsBuild -> ServiceInstance
            MERGE (build)-[:DEPLOYED_TO {env: $env, version: $version, deployed_at: $now}]->(si)

            // 6. Link Session -> JenkinsBuild (timeline, replaces old Deployment link)
            MATCH (s:Session {session_id: $session_id})
            MERGE (s)-[:HAS_EVENT]->(build)
            "#.to_string();
```

- [ ] **Step 1.3: Update params map**

Replace the params HashMap (lines 102-152) with:

```rust
        let mut params = std::collections::HashMap::new();
        params.insert("job".into(), serde_json::Value::String(job.clone()));
        params.insert("job_id".into(), serde_json::Value::String(job_id));
        params.insert("build_id".into(), serde_json::Value::String(build_id));
        params.insert("env".into(), serde_json::Value::String(env));
        params.insert("branch".into(), serde_json::Value::String(branch));
        params.insert("version".into(), serde_json::Value::String(version));
        params.insert("params".into(), serde_json::Value::String(params_json));
        params.insert("status".into(), serde_json::Value::String(status));
        params.insert("now".into(), serde_json::Value::String(event.timestamp.to_rfc3339()));
        params.insert("instance_id".into(), serde_json::Value::String(instance_id));

        // build_number_int: parse for Neo4j integer property (0 if missing/invalid)
        let build_number_int: i64 = build_number.parse().unwrap_or(0);
        params.insert("build_number_int".into(), serde_json::Value::Number(build_number_int.into()));

        // timestamp_raw: epoch millis for JenkinsBuild compatibility
        let timestamp_raw = event.timestamp.timestamp_millis();
        params.insert("timestamp_raw".into(), serde_json::Value::Number(timestamp_raw.into()));

        params.insert("session_id".into(), serde_json::Value::String(event.session_id.clone()));
        params.insert("entity_id".into(), serde_json::Value::String(event.entity_id.clone()));
        params.insert("event_type".into(), serde_json::Value::String(event.event_type.as_str().into()));
        params.insert("details".into(), serde_json::Value::String(event.details.clone()));
        params.insert("project".into(), serde_json::Value::String(event.project.clone()));
```

Also add `job_id` variable after `instance_id` (around line 76-79):

```rust
        let job_id = format!("dt://jenkins/job/{}", job);
```

- [ ] **Step 1.4: Update unit tests**

Replace the `CountingRepo` (lines 169-199) assertions:

```rust
        async fn write_query(
            &self,
            query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            assert!(query.contains("MERGE (job:JenkinsJob"), "should merge JenkinsJob");
            assert!(query.contains("MERGE (build:JenkinsBuild"), "should merge JenkinsBuild");
            assert!(query.contains("MERGE (si:ServiceInstance"), "should merge ServiceInstance");
            assert!(query.contains("LATEST_DEPLOY"), "should create LATEST_DEPLOY");
            assert!(query.contains("DEPLOYED_TO"), "should create DEPLOYED_TO");
            // Must NOT create Deployment nodes
            assert!(!query.contains("MERGE (e:Deployment"), "should NOT create Deployment node");
            Ok(serde_json::Value::Null)
        }
```

Update test events to include `build_number`:

```rust
    #[tokio::test]
    async fn deployment_handler_writes_correct_query() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "job: my-job; env: prod; branch: main; version: v2.0; \
             build_number: 42; params: {\"k\":\"v\"}; status: success",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deployment_handler_defaults_without_build_number() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("job: test-job; env: prod");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
```

Remove the old `deployment_handler_service_from_details` test since `service` field is no longer special (replaced by `job`).

- [ ] **Step 1.5: Update `dispatcher.rs` — skip link for Deployment events**

In `link_event_to_session` (line 136-173), add early return for Deployment:

```rust
pub async fn link_event_to_session(
    graph: &dyn GraphRepository,
    session_id: &str,
    event_node_id: &str,
    event_type: EventType,
) -> Result<(), DtError> {
    // Deployment handler creates the Session->JenkinsBuild link itself
    // so we skip it here.
    if event_type == EventType::Deployment {
        return Ok(());
    }

    let event_id_field = match event_type {
        // ... rest unchanged
```

- [ ] **Step 1.6: Run tests to verify**

```bash
cd /data/myProject/digital-twin-v2 && cargo test --package digital-twin --lib application::knowledge::memory::handlers::deployment::tests 2>&1
```

Expected: all tests pass, no Deployment node created.

- [ ] **Step 1.7: Commit**

```bash
cd /data/myProject/digital-twin-v2 && git add src/application/knowledge/memory/handlers/deployment.rs && git commit -m "refactor: replace Deployment label with three-node JenkinsJob-Build-ServiceInstance model"
```

---

### Task 2: Remove "Deployment" from BUSINESS_LABELS

**Files:**
- Modify: `src/application/sync/kg_bridge.rs`

- [ ] **Step 2.1: Remove `"Deployment"` from BUSINESS_LABELS array**

Line 86: delete `"Deployment",`

- [ ] **Step 2.2: Verify the change**

```bash
cd /data/myProject/digital-twin-v2 && grep -n '"Deployment"' src/application/sync/kg_bridge.rs
```

Expected: only hits are in the `build_search_text` match arm (which should be kept for backward compat with any existing nodes) or comments.

- [ ] **Step 2.3: Commit**

```bash
cd /data/myProject/digital-twin-v2 && git add src/application/sync/kg_bridge.rs && git commit -m "chore: remove Deployment from BUSINESS_LABELS (replaced by three-node model)"
```

---

### Task 3: Fix AGENTS.md and WRITE-EVENTS.md trigger rules

**Files:**
- Modify: `AGENTS.md`
- Modify: `guides/WRITE-EVENTS.md`

- [ ] **Step 3.1: Fix AGENTS.md line 110**

Change:
```
| 5 | Jenkins 部署（`jenkins_build_job` MCP） | **仅生产/stable 环境** | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>, params: <参数>" --project "<项目>"` |
```
To:
```
| 5 | Jenkins 部署（`jenkins_build_job` MCP） | **仅生产/stable 环境** | `dt event --type Deployment --entity-id "<job_name>" --entity-type JenkinsJob --details "job: <job_name>; env: <环境>; build_number: <构建号>; branch: <分支>; version: <版本>" --project "<项目>"` |
```

- [ ] **Step 3.2: Fix WRITE-EVENTS.md line 14**

Change:
```
| Jenkins 部署 | 仅生产/stable 环境 | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>" --project "<项目>"` |
```
To:
```
| Jenkins 部署 | 仅生产/stable 环境 | `dt event --type Deployment --entity-id "<job_name>" --entity-type JenkinsJob --details "job: <job_name>; env: <环境>; build_number: <构建号>; branch: <分支>; version: <版本>" --project "<项目>"` |
```

- [ ] **Step 3.3: Commit**

```bash
cd /data/myProject/digital-twin-v2 && git add AGENTS.md guides/WRITE-EVENTS.md && git commit -m "docs: fix Deployment trigger rules (--type Deploy -> --type Deployment), add build_number"
```
