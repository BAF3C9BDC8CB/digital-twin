//! 所有向量化操作的全局优先级队列。
//!
//! ## 动机
//!
//! 此前，向量化调用是临时且无协调的：
//! - 搜索查询直接调用 `embed.embed_batch()`（阻塞）
//! - 构建流水线使用 `buffer_unordered(3)`（并发受限）
//! - KG 同步使用 `SyncAccumulator`（唯一带队列的路径）
//!
//! 这导致 GPU 饥饿：用户搜索可能被正在运行的同步任务阻塞，
//! 而低优先级同步又浪费 GPU 周期去向量化单项。
//!
//! ## 设计
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
//! - **HIGH**：用户搜索/查询——优先处理，绝不批量（1 条文本）
//! - **NORMAL**：代码索引（build）——最多批量 32，超时极短
//! - **LOW**：KG 同步——最多批量 64，500ms 空闲超时，即发即忘
//!
//! worker 使用带 `biased` 的 `tokio::select!`，因此 HIGH 始终先于
//! NORMAL 被轮询，NORMAL 先于 LOW。在批次处理间隙，
//! worker 会出让并检查更高优先级通道。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::JoinHandle;

use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;

// ---------------------------------------------------------------------------
// 优先级
// ---------------------------------------------------------------------------

/// 向量化请求的优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// 后台同步——可等待，大批量。
    Low = 0,
    /// 代码索引（build）——适中批量。
    Normal = 1,
    /// 用户搜索/查询——立即处理，不批量。
    High = 2,
}

impl Priority {
    /// 该优先级对应的 gRPC metadata 头值。
    pub fn as_header(&self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Low => "low",
        }
    }
}

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// LOW 优先级（同步）的最大批量。
const LOW_BATCH: usize = 64;

/// NORMAL 优先级（build）的最大批量。
const NORMAL_BATCH: usize = 32;

/// 刷新 LOW 批次前的空闲超时（毫秒）。
const LOW_FLUSH_MS: u64 = 500;

/// 刷新 NORMAL 批次前的空闲超时（毫秒）。
const NORMAL_FLUSH_MS: u64 = 100;

// ---------------------------------------------------------------------------
// EmbedTask——内部请求负载
// ---------------------------------------------------------------------------

/// 在优先级队列中流转的向量化请求。
struct EmbedTask {
    texts: Vec<String>,
    priority: Priority,
    /// 响应通道——LOW 即发即忘请求为 None。
    response: Option<oneshot::Sender<Result<Vec<Vec<f32>>, DtError>>>,
}

// ---------------------------------------------------------------------------
// SyncItem——用于 LOW 优先级批量同步
// ---------------------------------------------------------------------------

/// 需要同步到 Qdrant 的 KG 节点（仅 LOW 优先级）。
#[derive(Debug, Clone)]
pub struct SyncItem {
    pub label: String,
    pub prop_key: String,
    pub prop_value: String,
}

// ---------------------------------------------------------------------------
// VectorQueue
// ---------------------------------------------------------------------------

/// 向量化的全局优先级队列。
///
/// 所有向量化操作都必须经过该队列。队列为进程级全局：
/// 在 `main.rs` 中创建一次，并通过 `Arc` 共享。
pub struct VectorQueue {
    /// HIGH 通道：用户搜索。
    hi_tx: mpsc::UnboundedSender<EmbedTask>,
    /// NORMAL 通道：代码索引（build）。
    norm_tx: mpsc::UnboundedSender<EmbedTask>,
    /// LOW 通道：后台同步。
    lo_tx: mpsc::UnboundedSender<EmbedTask>,
    /// 立即刷新 LOW 通道的信号。
    flush_notify: Arc<Notify>,
    /// 后台 worker 句柄。
    _handle: JoinHandle<()>,
    /// 用于直接 gRPC 调用的向量化服务引用。
    embed: Arc<dyn EmbedService>,
}

impl VectorQueue {
    /// 启动后台 worker 并返回队列句柄。
    ///
    /// worker 运行直到所有发送端被丢弃。
    pub fn spawn(embed: Arc<dyn EmbedService>) -> Self {
        let embed2 = embed.clone();
        let (hi_tx, mut hi_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let (norm_tx, mut norm_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let (lo_tx, mut lo_rx) = mpsc::unbounded_channel::<EmbedTask>();
        let flush_notify = Arc::new(Notify::new());
        let flush_notify2 = flush_notify.clone();

        let handle = tokio::spawn(async move {
            // LOW 通道：累积的批次缓冲区。
            let mut lo_buf: Vec<EmbedTask> = Vec::with_capacity(LOW_BATCH);
            // NORMAL 通道：累积的批次缓冲区。
            let mut norm_buf: Vec<EmbedTask> = Vec::with_capacity(NORMAL_BATCH);

            loop {
                // ── biased select：先 HIGH，再 NORMAL，后 LOW ──
                tokio::select! {
                    biased;

                    // HIGH 优先级——立即处理，一次一个。
                    task_opt = hi_rx.recv() => {
                        match task_opt {
                            Some(task) => {
                                let _ = process_high(&embed2, task).await;
                            }
                            None => {
                                // 所有发送端已丢弃——刷新剩余并退出。
                                flush_batch(&embed2, &mut norm_buf).await;
                                flush_batch(&embed2, &mut lo_buf).await;
                                break;
                            }
                        }
                    }

                    // NORMAL 优先级——累积批次，达阈值或超时后刷新。
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

                    // LOW 优先级——累积批次，达阈值或收到信号后刷新。
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

                    // LOW 刷新信号。
                    _ = flush_notify2.notified() => {
                        flush_batch(&embed2, &mut lo_buf).await;
                    }

                    // NORMAL 空闲超时。
                    _ = tokio::time::sleep(Duration::from_millis(NORMAL_FLUSH_MS)), if !norm_buf.is_empty() => {
                        flush_batch(&embed2, &mut norm_buf).await;
                    }

                    // LOW 空闲超时。
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
    // 公共 API
    // ------------------------------------------------------------------

    /// 以 HIGH 优先级向量化文本（用户搜索）。
    ///
    /// 立即处理——向量化完成即返回。
    /// 绝不批量：每次调用都是独立的 gRPC 请求。
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
        rx.await
            .map_err(|_| DtError::Repository("队列已关闭".into()))?
    }

    /// 以 NORMAL 优先级向量化文本（代码索引 / build）。
    ///
    /// 为提升 GPU 效率，可与最多 32 个其他 NORMAL 请求合并批量。
    /// 批次处理完成后返回。
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
        rx.await
            .map_err(|_| DtError::Repository("队列已关闭".into()))?
    }

    /// 以 LOW 优先级将文本入队（后台同步）。非阻塞。
    ///
    /// 用于调用方不需要向量化结果的即发即忘操作。
    /// 进程退出前请调用 [`flush_low`](Self::flush_low) 清空队列。
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

    /// 通知 LOW 通道立即刷新并短暂等待。
    ///
    /// 进程退出前调用，确保排队的同步条目被处理。
    /// 可安全多次调用。
    pub async fn flush_low(&self) {
        self.flush_notify.notify_one();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// 返回底层向量化服务的引用。
    ///
    /// 供需要在调用向量化前设置 gRPC metadata（优先级头）的组件使用。
    /// 优先使用 [`embed_high`]、[`embed_normal`]
    /// 或 [`enqueue_low`]。
    pub fn embed_service(&self) -> &Arc<dyn EmbedService> {
        &self.embed
    }
}

// ---------------------------------------------------------------------------
// 批次处理辅助函数
// ---------------------------------------------------------------------------

/// 立即处理单个 HIGH 优先级任务。
async fn process_high(embed: &Arc<dyn EmbedService>, task: EmbedTask) {
    let result = embed.embed_batch(&task.texts).await;
    if let Some(tx) = task.response {
        let _ = tx.send(result);
    }
}

/// 向量化批次中的所有文本并响应每个调用方。
async fn flush_batch(embed: &Arc<dyn EmbedService>, buffer: &mut Vec<EmbedTask>) {
    if buffer.is_empty() {
        return;
    }

    let tasks: Vec<EmbedTask> = buffer.drain(..).collect();
    let count = tasks.len();

    // 收集所有任务的全部文本。
    let mut all_texts: Vec<String> = Vec::new();
    let mut idx_map: Vec<(usize, usize)> = Vec::with_capacity(count); // (task_idx, start)
    for task in &tasks {
        let start = all_texts.len();
        all_texts.extend(task.texts.clone());
        idx_map.push((start, task.texts.len()));
    }

    tracing::debug!(
        "[vec-queue] 正在刷新 {} 个任务，共 {} 条文本",
        count,
        all_texts.len(),
    );

    // 所有文本一次 gRPC 调用。
    match embed.embed_batch(&all_texts).await {
        Ok(all_vecs) => {
            // 将向量分发给每个任务。
            for (ti, task) in tasks.into_iter().enumerate() {
                if let Some(tx) = task.response {
                    let (start, len) = idx_map[ti];
                    let task_vecs: Vec<Vec<f32>> = all_vecs[start..start + len].to_vec();
                    let _ = tx.send(Ok(task_vecs));
                }
            }
        }
        Err(e) => {
            // 将错误传播给所有等待方。
            let err_msg = format!("{e}");
            for task in tasks {
                if let Some(tx) = task.response {
                    let _ = tx.send(Err(DtError::Repository(err_msg.clone())));
                }
            }
        }
    }
}
