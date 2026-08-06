//! `dt jcli` 的 CLI 处理器——Jenkins CI/CD 操作（list、params、history、log、build）。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::application::sync::jenkins::JobSyncSource;
use crate::domain::traits::GraphRepository;

/// 处理 `dt jcli`——通过 jcli 进行原生 Jenkins 操作。
///
/// Jenkins 凭据必须由调用方从配置中预先解析。
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
                    eprintln!("错误：params 需要 --job 参数");
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
                    eprintln!("错误：history 需要 --job 参数");
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
                    eprintln!("错误：log 需要 --job 参数");
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
                    eprintln!("错误：build 需要 --job 参数");
                    return Ok(());
                }
            };
            // 解析 params 字符串 "k=v,k2=v2" → Vec<(&str, &str)>
            let parsed_params: Vec<(&str, &str)> = params
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.split_once('='))
                .collect();
            // 若 env=production，则添加提示
            if env == "production" {
                println!("⚠️  正在为 {j} 触发生产构建");
            }
            match jenkins.trigger_build(j, &parsed_params).await {
                Ok(out) => {
                    println!("{out}");
                    // 构建成功后，对该作业进行增量同步
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
                                        "jcli build: 对 {job_name} 进行增量同步: {} 个构建",
                                        r.items_created,
                                    );
                                    tracing::info!(
                                        "jcli build: 记录 {job_name} 在 {env} 环境中的部署事件",
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("jcli build: 对 {job_name} 的增量同步失败: {e}",)
                                }
                            }
                        }
                    }
                }
                Err(e) => eprintln!("jcli build: {e}"),
            }
        }
        other => {
            eprintln!(
                "未知的 jcli 操作: {other}. \
                 支持的操作: list、params、history、log、build"
            );
        }
    }

    Ok(())
}
