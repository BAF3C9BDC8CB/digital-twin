//! CLI handler for `dt event` — fire a named hook with a JSON context.
//!
//! Calls `hook_engine.fire(hook_name, context)` directly instead of
//! routing through the old MemoryService dispatcher.

use std::sync::Arc;

use crate::application::hooks::{HookContext, HookEngine};

/// Handle `dt event` — fires a named hook.
///
/// `hook_name` identifies the hook (e.g. `code_modified`,
/// `jenkins_deploy_completed`). `context_json` is a JSON object with
/// fields that the hook's side-effect templates can reference.
pub async fn handle_event(
    hook_name: String,
    context_json: String,
    hook_engine: Option<Arc<HookEngine>>,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: event --hook {hook_name}");

    let engine = match hook_engine {
        Some(e) => e,
        None => {
            eprintln!("Hook engine not available — event cannot be fired");
            return Ok(());
        }
    };

    let ctx: HookContext = match serde_json::from_str(&context_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse context JSON: {e}");
            return Ok(());
        }
    };

    let results = engine.fire(&hook_name, ctx).await;
    for r in &results {
        if !r.success {
            tracing::warn!(
                "[hook] {hook_name} failed for label {}: {}",
                r.label,
                r.error.as_deref().unwrap_or("unknown"),
            );
        }
    }

    println!("Event fired: hook={hook_name} results={}", results.len());
    Ok(())
}
