//! 内置指标收集，提供 counter、gauge 与 histogram 原语。
//!
//! 所有指标都存储在全局 `MetricsCollector` 单例中。
//! 收集器按指标类型使用独立的类型化 `HashMap`，
//! 避免 trait 对象向下转型的问题。
//!
//! # 宏
//!
//! ```ignore
//! counter!("dt.embed.requests", "status", "ok").inc();
//! gauge!("dt.embed.queue_depth").set(42.0);
//! histogram!("dt.build.duration", "project", "my-app").observe(12.5);
//! ```

use chrono::Utc;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// 指标值类型（基于原子操作、可克隆的句柄）
// ---------------------------------------------------------------------------

/// 单调递增的计数器。每个 `Counter` 句柄指向同一个底层 `AtomicU64`，
/// 因此克隆 `Counter` 会共享该值。
#[derive(Debug, Clone)]
pub struct Counter {
    value: Arc<AtomicU64>,
}

impl Counter {
    pub fn new() -> Self {
        Self {
            value: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 增加 `n`。
    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// 增加 1。
    pub fn inc(&self) {
        self.inc_by(1);
    }

    /// 当前值（某一时刻的读取）。
    pub fn value(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// 某一时刻的仪表（gauge）。`Clone` 共享底层的 `AtomicU64`
/// （以 `f64` 位存储，以保证在 stable Rust 上的可移植性）。
#[derive(Debug, Clone)]
pub struct Gauge {
    bits: Arc<AtomicU64>,
}

impl Gauge {
    pub fn new(initial: f64) -> Self {
        Self {
            bits: Arc::new(AtomicU64::new(initial.to_bits())),
        }
    }

    pub fn set(&self, v: f64) {
        self.bits.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn add(&self, delta: f64) {
        loop {
            let old = self.bits.load(Ordering::Relaxed);
            let new = f64::from_bits(old) + delta;
            if self
                .bits
                .compare_exchange_weak(old, new.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn sub(&self, delta: f64) {
        self.add(-delta);
    }

    pub fn value(&self) -> f64 {
        f64::from_bits(self.bits.load(Ordering::Relaxed))
    }
}

impl Default for Gauge {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// 带固定桶上界的直方图。`Clone` 共享底层桶存储（由 `RwLock` 保护）。
#[derive(Debug, Clone)]
pub struct Histogram {
    inner: Arc<HistogramInner>,
}

#[derive(Debug)]
struct HistogramInner {
    buckets: Vec<f64>,
    counts: RwLock<Vec<u64>>,
    total: AtomicU64,
    sum_bits: AtomicU64,
}

impl Histogram {
    pub fn linear(start: f64, width: f64, n: usize) -> Self {
        let buckets: Vec<f64> = (0..n).map(|i| start + (i as f64 + 1.0) * width).collect();
        let counts = vec![0u64; n];
        Self {
            inner: Arc::new(HistogramInner {
                buckets,
                counts: RwLock::new(counts),
                total: AtomicU64::new(0),
                sum_bits: AtomicU64::new(0.0_f64.to_bits()),
            }),
        }
    }

    pub fn exponential(start: f64, factor: f64, n: usize) -> Self {
        let buckets: Vec<f64> = (0..n).map(|i| start * factor.powi(i as i32 + 1)).collect();
        let counts = vec![0u64; n];
        Self {
            inner: Arc::new(HistogramInner {
                buckets,
                counts: RwLock::new(counts),
                total: AtomicU64::new(0),
                sum_bits: AtomicU64::new(0.0_f64.to_bits()),
            }),
        }
    }

    /// 记录一次观测。
    pub fn observe(&self, value: f64) {
        self.inner.total.fetch_add(1, Ordering::Relaxed);
        // 对 sum 使用原子 CAS
        loop {
            let old = self.inner.sum_bits.load(Ordering::Relaxed);
            let new = f64::from_bits(old) + value;
            if self
                .inner
                .sum_bits
                .compare_exchange_weak(old, new.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        let mut counts = self.inner.counts.write();
        for (i, &upper) in self.inner.buckets.iter().enumerate() {
            if value <= upper {
                counts[i] += 1;
                return;
            }
        }
        if let Some(last) = counts.last_mut() {
            *last += 1;
        }
    }

    pub fn count(&self) -> u64 {
        self.inner.total.load(Ordering::Relaxed)
    }

    pub fn sum(&self) -> f64 {
        f64::from_bits(self.inner.sum_bits.load(Ordering::Relaxed))
    }
}

// ---------------------------------------------------------------------------
// 快照类型
// ---------------------------------------------------------------------------

/// 某一时刻的直方图快照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistogramSnapshot {
    pub count: u64,
    pub sum: f64,
    pub bounds: Vec<f64>,
    pub counts: Vec<u64>,
}

/// 所有已注册指标的完整时刻快照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricSnapshot {
    pub timestamp: String,
    pub gauges: HashMap<String, f64>,
    pub counters: HashMap<String, u64>,
    pub histograms: HashMap<String, HistogramSnapshot>,
}

// ---------------------------------------------------------------------------
// 全局收集器（单例）
// ---------------------------------------------------------------------------

/// 线程安全的全局指标注册表。
pub struct MetricsCollector {
    counters: RwLock<HashMap<String, Counter>>,
    gauges: RwLock<HashMap<String, Gauge>>,
    histograms: RwLock<HashMap<String, Histogram>>,
}

impl MetricsCollector {
    /// 获取（或初始化）全局单例。
    pub fn global() -> &'static Self {
        static INSTANCE: once_cell::sync::OnceCell<MetricsCollector> =
            once_cell::sync::OnceCell::new();
        INSTANCE.get_or_init(|| MetricsCollector {
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        })
    }

    /// 注册或获取计数器。若 key 已存在，则返回
    /// 已有的计数器（共享句柄）。
    pub fn counter(&self, key: &str) -> Counter {
        {
            let map = self.counters.read();
            if let Some(c) = map.get(key) {
                return c.clone();
            }
        }
        let c = Counter::new();
        let mut map = self.counters.write();
        map.entry(key.to_string()).or_insert_with(|| c.clone());
        c
    }

    /// 注册或获取仪表。默认值 = 0.0。
    pub fn gauge(&self, key: &str) -> Gauge {
        {
            let map = self.gauges.read();
            if let Some(g) = map.get(key) {
                return g.clone();
            }
        }
        let g = Gauge::new(0.0);
        let mut map = self.gauges.write();
        map.entry(key.to_string()).or_insert_with(|| g.clone());
        g
    }

    /// 按 key 获取已有仪表。若未注册则返回 `None`。
    #[allow(dead_code)]
    pub fn get_gauge(&self, key: &str) -> Option<Gauge> {
        self.gauges.read().get(key).cloned()
    }

    /// 注册或获取带线性桶的直方图。
    pub fn histogram_linear(&self, key: &str, start: f64, width: f64, n: usize) -> Histogram {
        {
            let map = self.histograms.read();
            if let Some(h) = map.get(key) {
                return h.clone();
            }
        }
        let h = Histogram::linear(start, width, n);
        let mut map = self.histograms.write();
        map.entry(key.to_string()).or_insert_with(|| h.clone());
        h
    }

    /// 注册或获取带指数桶的直方图。
    pub fn histogram_exponential(&self, key: &str, start: f64, factor: f64, n: usize) -> Histogram {
        {
            let map = self.histograms.read();
            if let Some(h) = map.get(key) {
                return h.clone();
            }
        }
        let h = Histogram::exponential(start, factor, n);
        let mut map = self.histograms.write();
        map.entry(key.to_string()).or_insert_with(|| h.clone());
        h
    }

    /// 默认直方图（线性桶 0..10，宽度 1，10 个桶）。
    pub fn histogram(&self, key: &str) -> Histogram {
        self.histogram_linear(key, 0.0, 1.0, 10)
    }

    /// 生成所有已注册指标的某一时刻快照。
    pub fn snapshot(&self) -> MetricSnapshot {
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

        let gauges: HashMap<String, f64> = self
            .gauges
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.value()))
            .collect();

        let counters: HashMap<String, u64> = self
            .counters
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.value()))
            .collect();

        let histograms: HashMap<String, HistogramSnapshot> = self
            .histograms
            .read()
            .iter()
            .map(|(k, h)| {
                let snap = HistogramSnapshot {
                    count: h.count(),
                    sum: h.sum(),
                    bounds: h.inner.buckets.clone(),
                    counts: h.inner.counts.read().clone(),
                };
                (k.clone(), snap)
            })
            .collect();

        MetricSnapshot {
            timestamp,
            gauges,
            counters,
            histograms,
        }
    }
}

// ---------------------------------------------------------------------------
// 便捷宏
// ---------------------------------------------------------------------------

/// 通过以 `.` 连接的 key 段注册或获取计数器。
///
/// ```ignore
/// let c = counter!("dt.embed.requests", "status", "ok");
/// c.inc();
/// ```
#[macro_export]
macro_rules! counter {
    ($($segment:expr),+ $(,)?) => {{
        let key = vec![$($segment.to_string()),+].join(".");
        $crate::metrics::MetricsCollector::global().counter(&key)
    }};
    ($key:literal) => {{
        $crate::metrics::MetricsCollector::global().counter($key)
    }};
}

/// 通过以 `.` 连接的 key 段注册或获取仪表。
///
/// ```ignore
/// gauge!("dt.embed.queue_depth").set(12.0);
/// ```
#[macro_export]
macro_rules! mx_gauge {
    ($($segment:expr),+ $(,)?) => {{
        let key = vec![$($segment.to_string()),+].join(".");
        $crate::metrics::MetricsCollector::global().gauge(&key)
    }};
    ($key:literal) => {{
        $crate::metrics::MetricsCollector::global().gauge($key)
    }};
}

/// 通过以 `.` 连接的 key 段注册或获取直方图。
///
/// ```ignore
/// histogram!("dt.build.duration", "project", "my-app").observe(4.2);
/// ```
#[macro_export]
macro_rules! mx_histogram {
    ($($segment:expr),+ $(,)?) => {{
        let key = vec![$($segment.to_string()),+].join(".");
        $crate::metrics::MetricsCollector::global().histogram(&key)
    }};
    ($key:literal) => {{
        $crate::metrics::MetricsCollector::global().histogram($key)
    }};
}
