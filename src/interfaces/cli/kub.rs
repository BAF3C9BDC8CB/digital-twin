//! CLI handler for `dt kub` — Kubernetes operations (pods, logs, download, status).
//!
//! Extracted from main.rs to keep the entrypoint lean.

/// Handle `dt kub` — native K8s operations via kublog.
///
/// `k8s_cfg` must be pre-resolved from config by the caller.
pub async fn handle_kub(
    action: String,
    namespace: String,
    pod: Option<String>,
    since: Option<String>,
    output: Option<String>,
    resource: String,
    k8s_cfg: crate::application::sync::k8s::K8sSyncConfig,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: kub --action {action} --ns {namespace} --pod {:?} --resource {resource}",
        pod,
    );

    let mut k8s = crate::application::plugins::k8s::service::K8sPluginService::new(k8s_cfg);
    match k8s.connect().await {
        Ok(()) => match action.as_str() {
            "pods" | "deploy" | "svc" => {
                let r = if action == "pods" {
                    "pods"
                } else if action == "deploy" {
                    "deploy"
                } else {
                    "svc"
                };
                match k8s.get_status(r, &namespace).await {
                    Ok(out) => println!("{out}"),
                    Err(e) => eprintln!("kub {action}: {e}"),
                }
            }
            "logs" => {
                let p = match pod.as_deref() {
                    Some(p) => p,
                    None => {
                        eprintln!("error: --pod is required for logs");
                        return Ok(());
                    }
                };
                let tail = since.as_deref().and_then(|s| {
                    let val: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
                    val.parse::<u32>().ok()
                });
                match k8s.get_logs(p, &namespace, tail).await {
                    Ok(out) => println!("{out}"),
                    Err(e) => eprintln!("kub logs: {e}"),
                }
            }
            "download" => {
                let p = match pod.as_deref() {
                    Some(p) => p,
                    None => {
                        eprintln!("error: --pod is required for download");
                        return Ok(());
                    }
                };
                let out = match output.as_deref() {
                    Some(o) => o,
                    None => {
                        eprintln!("error: -o/--output is required for download");
                        return Ok(());
                    }
                };
                let tail = since.as_deref().and_then(|s| {
                    let val: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
                    val.parse::<u32>().ok()
                });
                match k8s.download_logs(p, &namespace, tail, out).await {
                    Ok(msg) => println!("{msg}"),
                    Err(e) => eprintln!("kub download: {e}"),
                }
            }
            "status" => match k8s.get_status(&resource, &namespace).await {
                Ok(out) => println!("{out}"),
                Err(e) => eprintln!("kub status: {e}"),
            },
            other => {
                eprintln!(
                    "Unknown kub action: {other}. \
                         Supported: pods, deploy, svc, logs, download, status"
                );
            }
        },
        Err(e) => eprintln!("K8s connection failed: {e}"),
    }

    Ok(())
}
