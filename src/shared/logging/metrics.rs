//! Built-in metrics collection with counter, gauge, and histogram primitives.
//!
//! All metrics are stored in a global `MetricsCollector` singleton.
//! The collector uses separate typed `HashMap`s per metric kind, avoiding
//! trait-object downcasting issues.
//!
//! # Macros
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
// Metric value types (atomic-based, clonable handles)
// ---------------------------------------------------------------------------

/// A monotonically increasing counter. Each `Counter` handle points to the
/// same underlying `AtomicU64`, so cloning a `Counter` shares the value.
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

    /// Increment by `n`.
    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment by 1.
    pub fn inc(&self) {
        self.inc_by(1);
    }

    /// Current value (point-in-time read).
    pub fn value(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time gauge. `Clone` shares the underlying `AtomicU64`
/// (stores `f64` bits for portability on stable Rust).
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

/// A histogram with a fixed set of bucket upper bounds. `Clone` shares the
/// underlying bucket storage (guarded by a `RwLock`).
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

    /// Record an observation.
    pub fn observe(&self, value: f64) {
        self.inner.total.fetch_add(1, Ordering::Relaxed);
        // Atomic CAS for sum
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
// Snapshot types
// ---------------------------------------------------------------------------

/// Snapshot of a histogram at a point in time.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistogramSnapshot {
    pub count: u64,
    pub sum: f64,
    pub bounds: Vec<f64>,
    pub counts: Vec<u64>,
}

/// A complete point-in-time snapshot of all registered metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricSnapshot {
    pub timestamp: String,
    pub gauges: HashMap<String, f64>,
    pub counters: HashMap<String, u64>,
    pub histograms: HashMap<String, HistogramSnapshot>,
}

// ---------------------------------------------------------------------------
// Global collector (singleton)
// ---------------------------------------------------------------------------

/// Thread-safe global metrics registry.
pub struct MetricsCollector {
    counters: RwLock<HashMap<String, Counter>>,
    gauges: RwLock<HashMap<String, Gauge>>,
    histograms: RwLock<HashMap<String, Histogram>>,
}

impl MetricsCollector {
    /// Get (or initialise) the global singleton.
    pub fn global() -> &'static Self {
        static INSTANCE: once_cell::sync::OnceCell<MetricsCollector> =
            once_cell::sync::OnceCell::new();
        INSTANCE.get_or_init(|| MetricsCollector {
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        })
    }

    /// Register or retrieve a counter. If the key already exists, returns
    /// the existing counter (shared handle).
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

    /// Register or retrieve a gauge. Default value = 0.0.
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

    /// Get an existing gauge by key. Returns `None` if not registered.
    #[allow(dead_code)]
    pub fn get_gauge(&self, key: &str) -> Option<Gauge> {
        self.gauges.read().get(key).cloned()
    }

    /// Register or retrieve a histogram with linear buckets.
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

    /// Register or retrieve a histogram with exponential buckets.
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

    /// Default histogram (linear buckets 0..10, width 1, 10 buckets).
    pub fn histogram(&self, key: &str) -> Histogram {
        self.histogram_linear(key, 0.0, 1.0, 10)
    }

    /// Produce a point-in-time snapshot of all registered metrics.
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
// Convenience macros
// ---------------------------------------------------------------------------

/// Register or retrieve a counter by key segments joined with `.`.
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

/// Register or retrieve a gauge by key segments joined with `.`.
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

/// Register or retrieve a histogram by key segments joined with `.`.
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
