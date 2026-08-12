//! `dt event` 的 CLI 处理器——以 JSON 上下文触发指定 hook。
//!
//! 直接调用 `hook_engine.fire(hook_name, context)`，不再经由
//! 旧的 MemoryService 分发器路由。

use std::sync::Arc;

use crate::application::hooks::{HookContext, HookEngine};
use crate::application::sync::kg_bridge::KgBridge;

/// 处理 `dt event`——触发指定 hook。
///
/// `hook_name` 标识 hook（例如 `code_modified`、
/// `jenkins_deploy_completed`）。`context_json` 是 JSON 对象，
/// hook 副作用模板可引用其中的字段。
///
/// hook 触发后，`kg_bridge` 会触发对 Qdrant 的增量同步，
/// 拾取事件创建/修改的节点。
pub async fn handle_event(
    hook_name: String,
    context_json: String,
    hook_engine: Option<Arc<HookEngine>>,
    kg_bridge: Option<Arc<KgBridge>>,
) -> anyhow::Result<()> {
    tracing::info!("dt CLI: event --hook {hook_name}");

    let engine = match hook_engine {
        Some(e) => e,
        None => {
            eprintln!("Hook 引擎不可用——无法触发事件");
            return Ok(());
        }
    };

    let ctx: HookContext = match serde_json::from_str(&context_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("解析 context JSON 失败: {e}");
            return Ok(());
        }
    };

    let results = engine.fire(&hook_name, ctx).await;
    for r in &results {
        if !r.success {
            tracing::warn!(
                "[hook] {hook_name} 对标签 {} 触发失败: {}",
                r.label,
                r.error.as_deref().unwrap_or("未知"),
            );
        }
    }

    println!("事件已触发: hook={hook_name} results={}", results.len());

    // ── 自动同步到 Qdrant（后台、非阻塞）──────────────────────
    // HookEngine::fire 写入节点时 _kg_synced_at 为 NULL，因此
    // 增量同步会拾取全部节点。我们在后台任务中执行，
    // 以便 CLI 立即返回。
    if let Some(bridge) = kg_bridge {
        tokio::spawn(async move {
            if let Err(e) = bridge.sync_incremental().await {
                tracing::warn!("[auto-sync] 后台增量同步失败: {e}");
            }
        });
    }

    Ok(())
}
