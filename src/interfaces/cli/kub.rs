//! `dt kub` 的 CLI 处理器——Kubernetes 操作（pods、logs、download、status）。
//!
//! 从 main.rs 抽取，保持入口文件精简。

/// 处理 `dt kub`——通过 kublog 进行原生 K8s 操作。
///
/// `k8s_cfg` 必须由调用方从配置中预先解析。
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
                        eprintln!("错误：logs 需要 --pod 参数");
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
                        eprintln!("错误：download 需要 --pod 参数");
                        return Ok(());
                    }
                };
                let out = match output.as_deref() {
                    Some(o) => o,
                    None => {
                        eprintln!("错误：download 需要 -o/--output 参数");
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
                    "未知的 kub 操作: {other}. \
                         支持的操作: pods、deploy、svc、logs、download、status"
                );
            }
        },
        Err(e) => eprintln!("K8s 连接失败: {e}"),
    }

    Ok(())
}
