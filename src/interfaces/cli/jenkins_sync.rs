//! CLI handler for `dt jc-sync` — synchronise Jenkins Views, Jobs, Builds
//! into the knowledge graph.

use std::sync::Arc;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::sync::jenkins::JobSyncSource;
use crate::application::sync::traits::SyncSource;
use crate::domain::traits::GraphRepository;

/// Handle `dt jc-sync` — synchronise Jenkins data into the KG.
///
/// `job` — optional job name filter. If set, only syncs that job.
/// `graph` must be pre-connected.
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
    tracing::info!("jenkins-sync starting: job={job:?}");

    let source = JobSyncSource::new(client, job);

    match graph.as_deref() {
        Some(g) => {
            let report = match source.sync(g).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("jenkins-sync failed: {e}");
                    return Err(e.into());
                }
            };
            println!(
                "  ✓ {} views, {} jobs linked, {} builds ({} total ops, {}ms)",
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
            eprintln!("  ✗ Graph database unavailable — skipping");
        }
    }

    println!("\n✓ Jenkins sync complete");
    tracing::info!("jenkins-sync complete");
    Ok(())
}
