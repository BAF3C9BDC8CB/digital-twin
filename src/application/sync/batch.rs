//! 批量累积的异步同步 worker——负责 KG → Qdrant。
//!
//! 收集单个同步请求（label + key + value），从图数据库获取节点，
//! 并将向量化委托给全局 [`VectorQueue`](super::queue::VectorQueue)，
//! 以实现按优先级感知的 GPU 调度。
//!
//! 本模块只处理 fetch + upsert 流水线。向量化通过全局队列完成，
//! 这样用户搜索（HIGH 通道）始终优先于后台同步（LOW 通道）。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use super::kg_bridge::KgBridge;
use super::queue::VectorQueue;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

const MAX_BATCH: usize = 64;
const FLUSH_TIMEOUT_MS: u64 = 500;

// ---------------------------------------------------------------------------
// SyncItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SyncItem {
    label: String,
    prop_key: String,
    prop_value: String,
}

// ---------------------------------------------------------------------------
// SyncAccumulator
// ---------------------------------------------------------------------------

/// 收集 KG 节点以进行后台同步。
///
/// 从图数据库获取节点，并传给 `KgBridge::process_batch`，
/// 后者通过全局 VectorQueue（LOW 优先级）完成向量化并 upsert 到
/// Qdrant。
pub struct SyncAccumulator {
    tx: mpsc::UnboundedSender<SyncItem>,
    flush_notify: Arc<Notify>,
    _handle: JoinHandle<()>,
}

impl SyncAccumulator {
    /// 启动一个后台 worker。
    ///
    /// `bridge` 必须已配置 [`VectorQueue`]，这样其 `process_batch`
    /// 调用会经由全局优先级队列完成向量化。
    pub fn spawn(bridge: Arc<KgBridge>, _queue: Arc<VectorQueue>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<SyncItem>();
        let flush_notify = Arc::new(Notify::new());
        let flush_notify2 = flush_notify.clone();

        let handle = tokio::spawn(async move {
            let mut buffer: Vec<SyncItem> = Vec::with_capacity(MAX_BATCH);

            loop {
                let item: Option<SyncItem> = tokio::select! {
                    item_opt = rx.recv() => item_opt,
                    _ = flush_notify2.notified() => {
                        flush(&bridge, &mut buffer).await;
                        continue;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(FLUSH_TIMEOUT_MS)) => {
                        flush(&bridge, &mut buffer).await;
                        continue;
                    }
                };

                let item = match item {
                    Some(it) => it,
                    None => {
                        flush(&bridge, &mut buffer).await;
                        break;
                    }
                };

                buffer.push(item);
                while buffer.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(next) => buffer.push(next),
                        Err(_) => break,
                    }
                }
                if buffer.len() >= MAX_BATCH {
                    flush(&bridge, &mut buffer).await;
                }
            }
        });

        Self {
            tx,
            flush_notify,
            _handle: handle,
        }
    }

    /// 将节点入队以进行后台（LOW 优先级）同步。绝不阻塞。
    pub fn enqueue(&self, label: &str, key: &str, value: &str) -> bool {
        self.tx
            .send(SyncItem {
                label: label.to_string(),
                prop_key: key.to_string(),
                prop_value: value.to_string(),
            })
            .is_ok()
    }

    /// 通知 worker 立即 flush，并短暂等待。
    pub async fn flush(&self) {
        self.flush_notify.notify_one();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 从图数据库获取节点，再委托给 KgBridge 完成向量化与 upsert。
async fn flush(bridge: &Arc<KgBridge>, buffer: &mut Vec<SyncItem>) {
    if buffer.is_empty() {
        return;
    }
    let items: Vec<SyncItem> = buffer.drain(..).collect();

    let mut nodes = Vec::with_capacity(items.len());
    let mut fetched = 0usize;
    let mut skipped = 0usize;
    for item in &items {
        match bridge
            .fetch_node(&item.label, &item.prop_key, &item.prop_value)
            .await
        {
            Ok(Some(node)) => {
                nodes.push(node);
                fetched += 1;
            }
            Ok(None) => {
                skipped += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "[sync-acc] 获取 {} {}={} 失败: {e}",
                    item.label,
                    item.prop_key,
                    item.prop_value
                );
            }
        }
    }

    tracing::info!(
        task = "sync",
        items = items.len(),
        fetched,
        skipped,
        stage = "flush_start",
        "向量同步队列 flush（图获取阶段）"
    );

    if nodes.is_empty() {
        return;
    }

    tracing::debug!("[sync-acc] 正在 flush {} 个节点", nodes.len());

    if let Err(e) = bridge.process_batch(&nodes).await {
        tracing::warn!("[sync-acc] {} 个节点 upsert 失败: {e}", nodes.len());
    }
}
