//! 将 Jenkins 的 Views / Jobs / Builds 同步到知识图谱。
//!
//! # 流程
//!
//! 1. 通过 [`JenkinsApiClient::list_views`] 获取所有视图及其嵌套作业
//! 2. 根据真实的 Jenkins 视图名（JAVA、VUE、...）创建 `JenkinsView` 节点
//! 3. 通过扁平的 `/api/json?tree=jobs[...]` 端点获取全部作业，以保证
//!    覆盖完整（某些视图类型不会展开其作业列表）
//! 4. 对每个作业，通过 [`JenkinsApiClient::get_all_builds`] 获取所有构建
//! 5. 创建 `JenkinsJob` + `JenkinsBuild` 节点
//! 6. 为视图中包含的作业创建 view→job 的 `[:CONTAINS]` 关系
//! 7. 用 `[:NEXT_BUILD]` 关系将构建串联成链
//!
//! # 设计
//!
//! - 视图名来自 Jenkins API，而非从作业 full_name 合成
//! - 一个作业可以属于多个视图（Jenkins API 原样返回）
//! - 无论是否属于某个视图，所有作业都会同步——view→job 映射仅用于
//!   `[:CONTAINS]` 关系（没有视图的作业依然存在）
//! - 无 `env` 字段——环境（test/prod）由视图名决定，不作为节点属性存储
//! - entity_id URI 遵循 `dt://jenkins/{type}/...` 模式

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::sync::traits::{SyncReport, SyncSource};
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

/// 将 Jenkins 的 Views、Jobs 和 Builds 同步到 Memgraph。
pub struct JobSyncSource {
    client: Arc<JenkinsApiClient>,
    job_filter: Option<String>,
}

impl JobSyncSource {
    /// 创建新的 Jenkins 同步源。
    ///
    /// * `client` — 已认证的 Jenkins API 客户端
    /// * `job_filter` — 可选：仅同步该特定作业
    pub fn new(client: Arc<JenkinsApiClient>, job_filter: Option<String>) -> Self {
        Self { client, job_filter }
    }

    /// 为视图构建唯一节点 ID。
    fn view_id(view_name: &str) -> String {
        format!("dt://jenkins/view/{view_name}")
    }

    /// 为作业构建唯一节点 ID。
    ///
    /// 当 `full_name` 为空（顶层作业）时回退到 `name`。
    fn job_id(full_name: &str, name: &str) -> String {
        let id = if full_name.is_empty() {
            name
        } else {
            full_name
        };
        format!("dt://jenkins/job/{id}")
    }

    /// 为构建构建唯一节点 ID。
    ///
    /// 当 `full_name` 为空时回退到 `name`。
    fn build_id(full_name: &str, name: &str, build_number: i64) -> String {
        let id = if full_name.is_empty() {
            name
        } else {
            full_name
        };
        format!("dt://jenkins/job/{id}/build/{build_number}")
    }
}

impl JobSyncSource {
    /// 同步单个作业（用于 `jcli build` 之后的增量更新）。
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

        // ── 1. 获取视图 → 用于 View 节点 + CONTAINS 映射 ──────────
        let all_views = self.client.list_views().await?;
        let views: Vec<_> = all_views.into_iter().filter(|v| v.name != "all").collect();
        report.namespaces = views.len();

        // 构建 view→job 映射（哪些作业属于哪些视图）
        let mut job_to_views: HashMap<String, Vec<String>> = HashMap::new();
        for view in &views {
            for job in &view.jobs {
                let key = if job.full_name.is_empty() {
                    job.name.clone()
                } else {
                    job.full_name.clone()
                };
                job_to_views.entry(key).or_default().push(view.name.clone());
            }
        }

        // ── 2. 合并视图节点 ──────────────────────────────────────────
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
            params.insert(
                "description".to_string(),
                serde_json::json!(&view.description),
            );
            graph.write_query(cypher, params).await?;
        }

        // ── 3. 获取全部作业（扁平列表）以保证覆盖完整 ─────
        let all_jobs = self.client.list_all_jobs().await?;

        // 若设置了过滤条件则应用之
        let jobs: Vec<_> = if let Some(ref filter) = self.job_filter {
            all_jobs
                .into_iter()
                .filter(|j| j.name == *filter || j.full_name == *filter)
                .collect()
        } else {
            all_jobs
        };

        if jobs.is_empty() {
            println!("  (未找到匹配的作业)");
            report.elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(report);
        }

        println!("Jenkins 同步: {} 个视图, {} 个作业", views.len(), jobs.len(),);

        // ── 4. 逐个处理作业 ──────────────────────────────────────────
        for job in &jobs {
            let jid = Self::job_id(&job.full_name, &job.name);
            let key = if job.full_name.is_empty() {
                &job.name
            } else {
                &job.full_name
            };

            print!("  作业: {}... ", job.name);

            // 合并作业节点
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
            params.insert(
                "description".to_string(),
                serde_json::json!(&job.description),
            );
            params.insert("full_name".to_string(), serde_json::json!(&job.full_name));
            graph.write_query(merge_job, params).await?;
            report.configs += 1;

            // 关联作业 → 其视图（若有视图包含该作业）
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

            // ── 5. 获取构建 ──────────────────────────────────────────
            let mut builds = match self.client.get_all_builds(&job.name, &job.full_name).await {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("{}: {e}", job.full_name);
                    eprintln!("  ⚠ 获取构建失败: {msg}");
                    report.add_error(&msg);
                    report.configs -= 1;
                    continue;
                }
            };

            if builds.is_empty() {
                println!("0 个构建");
                continue;
            }

            println!("{} 个构建", builds.len());

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

            // ── 6. 创建构建链（NEXT_BUILD） ────────────────────────
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

        // ── 7. 清理：仅删除没有父作业的构建 ──────────────────
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
        assert_eq!(JobSyncSource::view_id("JAVA"), "dt://jenkins/view/JAVA");
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
