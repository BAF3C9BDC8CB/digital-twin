//! Batch-accumulating async sync worker for KG → Qdrant.
//!
//! Collects individual sync requests (label + key + value), fetches
//! nodes from Neo4j, and delegates embedding to the global
//! [`VectorQueue`](super::queue::VectorQueue) for priority-aware GPU
//! scheduling.
//!
//! This module ONLY handles the fetch + upsert pipeline.  Embedding
//! is done through the global queue so that user searches (HIGH lane)
//! always preempt background sync (LOW lane).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use super::kg_bridge::KgBridge;
use super::queue::VectorQueue;

// ---------------------------------------------------------------------------
// Constants
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

/// Collects KG nodes for background sync.
///
/// Fetches nodes from Neo4j and passes them to `KgBridge::process_batch`,
/// which embeds via the global VectorQueue (LOW priority) and upserts to
/// Qdrant.
pub struct SyncAccumulator {
    tx: mpsc::UnboundedSender<SyncItem>,
    flush_notify: Arc<Notify>,
    _handle: JoinHandle<()>,
}

impl SyncAccumulator {
    /// Spawn a background worker.
    ///
    /// `bridge` must already be configured with a [`VectorQueue`] so
    /// that its `process_batch` call routes embedding through the
    /// global priority queue.
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

    /// Enqueue a node for background (LOW-priority) sync.  Never blocks.
    pub fn enqueue(&self, label: &str, key: &str, value: &str) -> bool {
        self.tx
            .send(SyncItem {
                label: label.to_string(),
                prop_key: key.to_string(),
                prop_value: value.to_string(),
            })
            .is_ok()
    }

    /// Signal the worker to flush immediately and wait briefly.
    pub async fn flush(&self) {
        self.flush_notify.notify_one();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Fetch nodes from Neo4j, then delegate to KgBridge for embed+upsert.
async fn flush(bridge: &Arc<KgBridge>, buffer: &mut Vec<SyncItem>) {
    if buffer.is_empty() {
        return;
    }
    let items: Vec<SyncItem> = buffer.drain(..).collect();

    let mut nodes = Vec::with_capacity(items.len());
    for item in &items {
        match bridge
            .fetch_node(&item.label, &item.prop_key, &item.prop_value)
            .await
        {
            Ok(Some(node)) => nodes.push(node),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "[sync-acc] fetch {} {}={}: {e}",
                    item.label, item.prop_key, item.prop_value
                );
            }
        }
    }

    if nodes.is_empty() {
        return;
    }

    tracing::debug!("[sync-acc] flushing {} node(s)", nodes.len());

    if let Err(e) = bridge.process_batch(&nodes).await {
        tracing::warn!("[sync-acc] upsert failed for {} nodes: {e}", nodes.len());
    }
}
