//! Global priority queue for ALL vector embedding operations.
//!
//! ## Motivation
//!
//! Previously, embed calls were ad-hoc and uncoordinated:
//! - Search queries called `embed.embed_batch()` directly (blocking)
//! - Build pipeline used `buffer_unordered(3)` (limited concurrency)
//! - KG sync used `SyncAccumulator` (only path with queueing)
//!
//! This led to GPU starvation: user searches could be blocked by a running
//! sync job, and low-priority sync wasted GPU cycles on single-item embeds.
//!
//! ## Design
//!
//! ```text
//!   dt search ──→ [HIGH] ──┐
//!   dt build ───→ [NORMAL]─┤
//!   dt sync ────→ [LOW] ───┤
//!                            │
//!                    ┌───────▼────────────────────────┐
//!                    │     VectorQueue worker          │
//!                    │                                 │
//!                    │  biased select! {               │
//!                    │    HIGH   → embed 1 (immediate) │
//!                    │    NORMAL → batch 32            │
//!                    │    LOW    → batch 64, timeout   │
//!                    │  }                              │
//!                    └────────────────────────────────┘
//! ```
//!
//! - **HIGH**: User search/query — processed next, never batched (1 text)
//! - **NORMAL**: Code indexing (build) — batch up to 32, minimal timeout
//! - **LOW**: KG sync — batch up to 64, 500ms idle timeout, fire-and-forget
//!
//! The worker uses `tokio::select!` with `biased` so HIGH is always polled
//! before NORMAL, and NORMAL before LOW.  Between batch processing, the
//! worker yields and checks higher-priority lanes.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::JoinHandle;

use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Priority of an embed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Background sync — can wait, large batches.
    Low = 0,
    /// Code indexing (build) — moderate batching.
    Normal = 1,
    /// User search / query — process immediately, no batching.
    High = 2,
}

impl Priority {
    /// gRPC metadata header value for this priority.
    pub fn as_header(&self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Low => "low",
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum batch size for LOW priority (sync).
const LOW_BATCH: usize = 64;

/// Maximum batch size for NORMAL priority (build).
const NORMAL_BATCH: usize = 32;

/// Idle timeout before flushing a LOW batch (ms).
const LOW_FLUSH_MS: u64 = 500;

/// Idle timeout before flushing a NORMAL batch (ms).
const NORMAL_FLUSH_MS: u64 = 100;

// ---------------------------------------------------------------------------
// EmbedTask — internal request payload
// ---------------------------------------------------------------------------

/// An embed request travelling through the priority queue.
struct EmbedTask {
    texts: Vec<String>,
    priority: Priority,
    /// Response channel — None for fire-and-forget LOW requests.
    response: Option<oneshot::Sender<Result<Vec<Vec<f32>>, DtError>>>,
}

// ---------------------------------------------------------------------------
// SyncItem — for LOW-priority batch sync
// ---------------------------------------------------------------------------

/// A KG node that needs syncing to Qdrant (LOW priority only).
#[derive(Debug, Clone)]
pub struct SyncItem {
    pub label: String,
    pub prop_key: String,
    pub prop_value: String,
}

// ---------------------------------------------------------------------------
// VectorQueue
// ---------------------------------------------------------------------------

/// Global priority queue for vector embedding.
///
/// All embed operations must go through this queue.  The queue is
/// process-global: created once in `main.rs` and shared via `Arc`.
pub struct VectorQueue {
    /// HIGH lane: user searches.
    hi_tx: mpsc::UnboundedSender<EmbedTask>,
    /// NORMAL lane: code indexing (build).
    norm_tx: mpsc::UnboundedSender<EmbedTask>,
    /// LOW lane: background sync.
    lo_tx: mpsc::UnboundedSender<EmbedTask>,
    /// Signal to flush LOW lane immediately.
    flush_notify: Arc<Notify>,
    /// Background worker handle.
    _handle: JoinHandle<()>,
    /// Reference to the embed service for direct gRPC calls.
    embed: Arc<dyn EmbedService>,
}

impl VectorQueue {
    /// Spawn the background worker and return the queue handle.
    ///
    /// The worker runs until all senders are dropped.
    pub fn spawn(embed: Arc<dyn EmbedService>) -> Self {
        let embed2 = embed.clone();
        let (hi_tx, mut hi_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let (norm_tx, mut norm_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let (lo_tx, mut lo_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let flush_notify = Arc::new(Notify::new());
        let flush_notify2 = flush_notify.clone();

        let handle = tokio::spawn(async move {
            // LOW lane: accumulated batch buffer.
            let mut lo_buf: Vec<EmbedTask> = Vec::with_capacity(LOW_BATCH);
            // NORMAL lane: accumulated batch buffer.
            let mut norm_buf: Vec<EmbedTask> = Vec::with_capacity(NORMAL_BATCH);

            loop {
                // ── biased select: HIGH first, then NORMAL, then LOW ──
                tokio::select! {
                    biased;

                    // HIGH priority — process immediately, one at a time.
                    task_opt = hi_rx.recv() => {
                        match task_opt {
                            Some(task) => {
                                let _ = process_high(&embed2, task).await;
                            }
                            None => {
                                // All senders dropped — flush remaining and exit.
                                flush_batch(&embed2, &mut norm_buf).await;
                                flush_batch(&embed2, &mut lo_buf).await;
                                break;
                            }
                        }
                    }

                    // NORMAL priority — accumulate batch, flush on threshold or timeout.
                    task_opt = norm_rx.recv() => {
                        match task_opt {
                            Some(task) => {
                                norm_buf.push(task);
                                while norm_buf.len() < NORMAL_BATCH {
                                    match norm_rx.try_recv() {
                                        Ok(next) => norm_buf.push(next),
                                        Err(_) => break,
                                    }
                                }
                                if norm_buf.len() >= NORMAL_BATCH {
                                    flush_batch(&embed2, &mut norm_buf).await;
                                }
                            }
                            None => {
                                flush_batch(&embed2, &mut norm_buf).await;
                                flush_batch(&embed2, &mut lo_buf).await;
                                break;
                            }
                        }
                    }

                    // LOW priority — accumulate batch, flush on threshold or signal.
                    task_opt = lo_rx.recv() => {
                        match task_opt {
                            Some(task) => {
                                lo_buf.push(task);
                                while lo_buf.len() < LOW_BATCH {
                                    match lo_rx.try_recv() {
                                        Ok(next) => lo_buf.push(next),
                                        Err(_) => break,
                                    }
                                }
                                if lo_buf.len() >= LOW_BATCH {
                                    flush_batch(&embed2, &mut lo_buf).await;
                                }
                            }
                            None => {
                                flush_batch(&embed2, &mut norm_buf).await;
                                flush_batch(&embed2, &mut lo_buf).await;
                                break;
                            }
                        }
                    }

                    // LOW flush signal.
                    _ = flush_notify2.notified() => {
                        flush_batch(&embed2, &mut lo_buf).await;
                    }

                    // NORMAL idle timeout.
                    _ = tokio::time::sleep(Duration::from_millis(NORMAL_FLUSH_MS)), if !norm_buf.is_empty() => {
                        flush_batch(&embed2, &mut norm_buf).await;
                    }

                    // LOW idle timeout.
                    _ = tokio::time::sleep(Duration::from_millis(LOW_FLUSH_MS)), if !lo_buf.is_empty() => {
                        flush_batch(&embed2, &mut lo_buf).await;
                    }
                }
            }
        });

        Self {
            hi_tx,
            norm_tx,
            lo_tx,
            flush_notify,
            _handle: handle,
            embed,
        }
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Embed text(s) at HIGH priority (user search).
    ///
    /// Processes immediately — returns as soon as embedding is done.
    /// Never batched: each call is a separate gRPC request.
    pub async fn embed_high(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let (tx, rx) = oneshot::channel();
        let _ = self.hi_tx.send(EmbedTask {
            texts: texts.to_vec(),
            priority: Priority::High,
            response: Some(tx),
        });
        rx.await.map_err(|_| DtError::Repository("queue closed".into()))?
    }

    /// Embed text(s) at NORMAL priority (code indexing / build).
    ///
    /// May be batched with up to 32 other NORMAL requests for GPU
    /// efficiency.  Returns when the batch is processed.
    pub async fn embed_normal(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let (tx, rx) = oneshot::channel();
        let _ = self.norm_tx.send(EmbedTask {
            texts: texts.to_vec(),
            priority: Priority::Normal,
            response: Some(tx),
        });
        rx.await.map_err(|_| DtError::Repository("queue closed".into()))?
    }

    /// Enqueue text(s) at LOW priority (background sync).  Non-blocking.
    ///
    /// Use this for fire-and-forget operations where the caller doesn't
    /// need the embedding result.  Call [`flush_low`](Self::flush_low)
    /// to drain the queue before process exit.
    pub fn enqueue_low(&self, texts: Vec<String>) {
        if texts.is_empty() {
            return;
        }
        let _ = self.lo_tx.send(EmbedTask {
            texts,
            priority: Priority::Low,
            response: None,
        });
    }

    /// Signal the LOW lane to flush immediately and wait briefly.
    ///
    /// Call this before process exit to ensure queued sync items are
    /// processed.  Safe to call multiple times.
    pub async fn flush_low(&self) {
        self.flush_notify.notify_one();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// Return a reference to the underlying embed service.
    ///
    /// Used by components that need to set gRPC metadata (priority header)
    /// before calling embed.  Prefer using [`embed_high`], [`embed_normal`],
    /// or [`enqueue_low`] instead.
    pub fn embed_service(&self) -> &Arc<dyn EmbedService> {
        &self.embed
    }
}

// ---------------------------------------------------------------------------
// Batch processing helpers
// ---------------------------------------------------------------------------

/// Process a single HIGH-priority task immediately.
async fn process_high(
    embed: &Arc<dyn EmbedService>,
    task: EmbedTask,
) {
    let result = embed.embed_batch(&task.texts).await;
    if let Some(tx) = task.response {
        let _ = tx.send(result);
    }
}

/// Embed all texts in a batch and respond to each caller.
async fn flush_batch(
    embed: &Arc<dyn EmbedService>,
    buffer: &mut Vec<EmbedTask>,
) {
    if buffer.is_empty() {
        return;
    }

    let tasks: Vec<EmbedTask> = buffer.drain(..).collect();
    let count = tasks.len();

    // Collect all texts from all tasks.
    let mut all_texts: Vec<String> = Vec::new();
    let mut idx_map: Vec<(usize, usize)> = Vec::with_capacity(count); // (task_idx, start)
    for task in &tasks {
        let start = all_texts.len();
        all_texts.extend(task.texts.clone());
        idx_map.push((start, task.texts.len()));
    }

    tracing::debug!(
        "[vec-queue] flushing {} task(s), {} text(s) total",
        count,
        all_texts.len(),
    );

    // One gRPC call for all texts.
    match embed.embed_batch(&all_texts).await {
        Ok(all_vecs) => {
            // Distribute vectors back to each task.
            for (ti, task) in tasks.into_iter().enumerate() {
                if let Some(tx) = task.response {
                    let (start, len) = idx_map[ti];
                    let task_vecs: Vec<Vec<f32>> = all_vecs[start..start + len].to_vec();
                    let _ = tx.send(Ok(task_vecs));
                }
            }
        }
        Err(e) => {
            // Propagate error to all waiters.
            let err_msg = format!("{e}");
            for task in tasks {
                if let Some(tx) = task.response {
                    let _ = tx.send(Err(DtError::Repository(err_msg.clone())));
                }
            }
        }
    }
}
