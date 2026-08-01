//! CLI handler for `dt jcli` — Jenkins CI/CD operations (list, params, history, log, build).
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::sync::jenkins::JobSyncSource;
use crate::domain::traits::GraphRepository;

/// Handle `dt jcli` — native Jenkins operations via jcli.
///
/// Jenkins credentials must be pre-resolved from config by the caller.
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
    tracing::info!(
        "dt-daemon CLI: jcli --action {action} --job {:?} --build {:?} --limit {:?}",
        job,
        build,
        limit,
    );

    let client = crate::application::plugins::jenkins::client::JenkinsApiClient::new(
        jenkins_url,
        jenkins_user,
        jenkins_token,
    );
    let jenkins = crate::application::plugins::jenkins::service::JenkinsPluginService::new(client);

    match action.as_str() {
        "list" | "jobs" => match jenkins.list_jobs().await {
            Ok(out) => println!("{out}"),
            Err(e) => eprintln!("jcli list: {e}"),
        },
        "params" => {
            let j = match job.as_deref() {
                Some(j) => j,
                None => {
                    eprintln!("error: --job is required for params");
                    return Ok(());
                }
            };
            match jenkins.get_params(j).await {
                Ok(out) => println!("{out}"),
                Err(e) => eprintln!("jcli params: {e}"),
            }
        }
        "history" => {
            let j = match job.as_deref() {
                Some(j) => j,
                None => {
                    eprintln!("error: --job is required for history");
                    return Ok(());
                }
            };
            match jenkins.get_history(j, limit).await {
                Ok(out) => println!("{out}"),
                Err(e) => eprintln!("jcli history: {e}"),
            }
        }
        "log" => {
            let j = match job.as_deref() {
                Some(j) => j,
                None => {
                    eprintln!("error: --job is required for log");
                    return Ok(());
                }
            };
            match jenkins.get_build_log(j, build.as_deref()).await {
                Ok(out) => println!("{out}"),
                Err(e) => eprintln!("jcli log: {e}"),
            }
        }
        "build" => {
            let j = match job.as_deref() {
                Some(j) => j,
                None => {
                    eprintln!("error: --job is required for build");
                    return Ok(());
                }
            };
            // Parse params string "k=v,k2=v2" → Vec<(&str, &str)>
            let parsed_params: Vec<(&str, &str)> = params
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.split_once('='))
                .collect();
            // If env=production, add a comment
            if env == "production" {
                println!("⚠️  Triggering production build for {j}");
            }
            match jenkins.trigger_build(j, &parsed_params).await {
                Ok(out) => {
                    println!("{out}");
                    // After successful build, incrementally sync this job
                    if let Some(ref job_name) = job {
                        if let Some(ref g) = graph {
                            let client =
                                crate::application::plugins::jenkins::client::JenkinsApiClient::new(
                                    jenkins_url,
                                    jenkins_user,
                                    jenkins_token,
                                );
                            let source =
                                JobSyncSource::new(Arc::new(client), Some(job_name.clone()));
                            match source.sync_job(g.as_ref()).await {
                                Ok(r) => {
                                    tracing::info!(
                                        "jcli build: incremental sync for {job_name}: {} builds",
                                        r.items_created,
                                    );
                                    tracing::info!(
                                        "jcli build: deploy event for {job_name} in {env} environment",
                                    );
                                }
                                Err(e) => tracing::warn!(
                                    "jcli build: incremental sync failed for {job_name}: {e}",
                                ),
                            }
                        }
                    }
                }
                Err(e) => eprintln!("jcli build: {e}"),
            }
        }
        other => {
            eprintln!(
                "Unknown jcli action: {other}. \
                 Supported: list, params, history, log, build"
            );
        }
    }

    Ok(())
}
