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
