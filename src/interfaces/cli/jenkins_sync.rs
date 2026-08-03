//! `dt jc-sync` 的 CLI 处理器——将 Jenkins Views、Jobs、Builds
//! 同步到知识图谱。

use std::sync::Arc;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::sync::jenkins::JobSyncSource;
use crate::application::sync::traits::SyncSource;
use crate::domain::traits::GraphRepository;

/// 处理 `dt jc-sync`——将 Jenkins 数据同步到 KG。
///
/// `job`——可选的作业名过滤器。设置后仅同步该作业。
/// `graph` 必须已预先连接。
pub async fn handle_jenkins_sync(
    job: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    jenkins_url: &str,
    jenkins_user: &str,
    jenkins_token: &str,
) -> anyhow::Result<()> {
    let client = Arc::new(JenkinsApiClient::new(
        jenkins_url,
        jenkins_user,
        jenkins_token,
    ));
    tracing::info!("jenkins-sync 开始: job={job:?}");

    let source = JobSyncSource::new(client, job);

    match graph.as_deref() {
        Some(g) => {
            let report = match source.sync(g).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("jenkins-sync 失败: {e}");
                    return Err(e.into());
                }
            };
            println!(
                "  ✓ {} 个视图, {} 个作业已关联, {} 个构建 (共 {} 次操作, {}ms)",
                report.namespaces,
                report.configs,
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
            eprintln!("  ✗ 图数据库不可用——已跳过");
        }
    }

    println!("\n✓ Jenkins 同步完成");
    tracing::info!("jenkins-sync 完成");
    Ok(())
}
