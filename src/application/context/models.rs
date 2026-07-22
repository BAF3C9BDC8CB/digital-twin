//! Domain models for the Context Builder pipeline.
//!
//! Defines `AggregatedContext`, `WorldSlice`, `Alert`, `ContextOptions`,
//! and the internal `ContextState` that flows through pipeline stages.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// AggregatedContext — output of the pipeline
// ---------------------------------------------------------------------------

/// The fully aggregated context produced by the ContextPipeline.
///
/// Contains six world slices (one per knowledge world) plus any alerts
/// generated during resolution and summarisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedContext {
    /// Optional Digital Thread summary linking this context to a long-running
    /// conversation or investigation thread.
    pub thread: Option<ThreadSummary>,

    /// **Reality World** — code, services, configurations (from Qdrant).
    pub reality: WorldSlice,
    /// **Knowledge World** — domain concepts, patterns, playbooks (from Memgraph).
    pub knowledge: WorldSlice,
    /// **Memory World** — historical events, experiences, lessons learned (from Memgraph).
    pub memory: WorldSlice,
    /// **Semantic World** — document and code vectors (from Qdrant).
    pub semantic: WorldSlice,
    /// **Runtime World** — live Pod / Service / metrics (from K8s API).
    pub runtime: WorldSlice,
    /// **Reasoning World** — past analyses and decision chains (from Memgraph).
    pub reasoning: WorldSlice,

    /// Alerts raised during conflict detection and experience injection.
    pub alerts: Vec<Alert>,

    /// Estimated token count of the full context (for budget tracking).
    pub estimated_tokens: usize,
}

impl Default for AggregatedContext {
    fn default() -> Self {
        Self {
            thread: None,
            reality: WorldSlice::new("reality"),
            knowledge: WorldSlice::new("knowledge"),
            memory: WorldSlice::new("memory"),
            semantic: WorldSlice::new("semantic"),
            runtime: WorldSlice::new("runtime"),
            reasoning: WorldSlice::new("reasoning"),
            alerts: Vec::new(),
            estimated_tokens: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// ThreadSummary
// ---------------------------------------------------------------------------

/// A lightweight summary of a Digital Thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// Thread identifier (e.g. graph element ID).
    pub thread_id: String,
    /// Thread title or summary line.
    pub title: String,
    /// Human-readable description.
    pub description: String,
    /// Number of events / entries in this thread.
    pub event_count: usize,
    /// ISO 8601 timestamp of the most recent activity.
    pub last_updated: String,
}

// ---------------------------------------------------------------------------
// WorldSlice — a single world's aggregated content
// ---------------------------------------------------------------------------

/// A slice of aggregated content from one knowledge world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSlice {
    /// The world name (e.g. "reality", "knowledge").
    pub world: String,
    /// Retrieved items from this world.
    pub items: Vec<WorldItem>,
    /// Total number of items before ranking/dedup (may exceed `items.len()`).
    pub count: usize,
}

impl WorldSlice {
    pub fn new(world: impl Into<String>) -> Self {
        Self {
            world: world.into(),
            items: Vec::new(),
            count: 0,
        }
    }

    /// Returns `true` when this slice contains at least one item.
    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }
}

// ---------------------------------------------------------------------------
// WorldItem — a single piece of evidence from a world
// ---------------------------------------------------------------------------

/// A single item retrieved from a knowledge world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldItem {
    /// Unique identifier (e.g. graph element ID, Qdrant point id).
    pub id: String,
    /// Short label / title.
    pub label: String,
    /// Primary content or summary text.
    pub content: String,
    /// Relevance score [0.0, 1.0].
    pub score: f64,
    /// Source references (entity type, file path, URL, etc.).
    pub sources: Vec<String>,
    /// Additional metadata as key-value pairs.
    pub metadata: std::collections::HashMap<String, String>,
    /// Entity type / node label (e.g. "Method", "Experience", "NacosConfig").
    pub entity_type: String,
}

impl WorldItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            content: content.into(),
            score: 1.0,
            sources: Vec::new(),
            metadata: std::collections::HashMap::new(),
            entity_type: String::new(),
        }
    }

    /// Set the relevance score.
    pub fn with_score(mut self, score: f64) -> Self {
        self.score = score;
        self
    }

    /// Add a source reference.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.sources.push(source.into());
        self
    }

    /// Set the entity type.
    pub fn with_type(mut self, ty: impl Into<String>) -> Self {
        self.entity_type = ty.into();
        self
    }

    /// Add metadata.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Rough character-count estimate for token budgeting.
    pub fn char_count(&self) -> usize {
        self.label.len()
            + self.content.len()
            + self.entity_type.len()
            + self.sources.iter().map(|s| s.len()).sum::<usize>()
    }
}

// ---------------------------------------------------------------------------
// Alert
// ---------------------------------------------------------------------------

/// An alert raised during pipeline processing (conflict, experience, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alert {
    /// Severity level.
    pub severity: AlertSeverity,
    /// Source that triggered this alert (e.g. "resolver", "summarizer").
    pub source: String,
    /// Human-readable alert message.
    pub message: String,
    /// Optional entity ID this alert relates to.
    pub related_id: Option<String>,
}

// ---------------------------------------------------------------------------
// AlertSeverity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    /// Informational — no action required.
    Info = 0,
    /// Warning — attention recommended.
    Warning = 1,
    /// Critical — immediate action needed.
    Critical = 2,
}

impl AlertSeverity {
    /// Returns `true` if this severity is at least `Warning`.
    pub fn at_least_warning(&self) -> bool {
        *self >= AlertSeverity::Warning
    }
}

// ---------------------------------------------------------------------------
// ContextOptions — pipeline configuration
// ---------------------------------------------------------------------------

/// Options for building a context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOptions {
    /// Which worlds to include.  If `None`, all six worlds are queried.
    pub worlds: Option<Vec<String>>,
    /// Maximum estimated tokens for the aggregated context.  When the total
    /// exceeds this budget the summarizer will compress lower-priority items.
    pub max_tokens: Option<usize>,
    /// Optional Digital Thread ID to link this context to a thread.
    pub thread_id: Option<String>,
    /// Minimum relevance score for an item to be retained [0.0, 1.0].
    /// Items below this threshold are dropped by the ranker.
    pub min_score: Option<f64>,
    /// Maximum items to retain per world after ranking (0 = unlimited).
    pub max_items_per_world: Option<usize>,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            worlds: None,
            max_tokens: Some(4096),
            thread_id: None,
            min_score: Some(0.5),
            max_items_per_world: Some(20),
        }
    }
}

impl ContextOptions {
    /// Returns `true` when the given world should be included given the
    /// configured world filter.
    ///
    /// Supports user-friendly aliases:
    /// - "code" → "reality"
    /// - "doc"  → "semantic"
    pub fn includes_world(&self, world: &str) -> bool {
        includes_world_inner(&self.worlds, world)
    }
}

/// Resolve user-friendly world name aliases (case-insensitive).
fn resolve_world_alias(world: &str) -> &str {
    if world.eq_ignore_ascii_case("code") {
        "reality"
    } else if world.eq_ignore_ascii_case("doc") {
        "semantic"
    } else {
        world
    }
}

/// Check whether a world name (possibly aliased) is in the filter list.
fn includes_world_inner(filter: &Option<Vec<String>>, world: &str) -> bool {
    let normalized = resolve_world_alias(world);
    match filter {
        Some(worlds) if !worlds.is_empty() => {
            worlds.iter().any(|w| {
                resolve_world_alias(w).eq_ignore_ascii_case(normalized)
            })
        }
        _ => true, // No filter → include all worlds
    }
}

// ---------------------------------------------------------------------------
// ContextState — internal pipeline state (flows through stages)
// ---------------------------------------------------------------------------

/// Mutable state that flows through each pipeline stage.
/// Each stage reads from and writes to this struct.
#[derive(Debug, Clone)]
pub struct ContextState {
    /// The original task or question text.
    pub task: String,

    /// Pipeline configuration.
    pub options: ContextOptions,

    // -- Raw retrieval results (before ranking) --
    pub reality_raw: Vec<WorldItem>,
    pub knowledge_raw: Vec<WorldItem>,
    pub memory_raw: Vec<WorldItem>,
    pub semantic_raw: Vec<WorldItem>,
    pub runtime_raw: Vec<WorldItem>,
    pub reasoning_raw: Vec<WorldItem>,

    // -- Ranked items (after ranking) --
    pub reality_ranked: Vec<WorldItem>,
    pub knowledge_ranked: Vec<WorldItem>,
    pub memory_ranked: Vec<WorldItem>,
    pub semantic_ranked: Vec<WorldItem>,
    pub runtime_ranked: Vec<WorldItem>,
    pub reasoning_ranked: Vec<WorldItem>,

    // -- Deduped items --
    pub reality_deduped: Vec<WorldItem>,
    pub knowledge_deduped: Vec<WorldItem>,
    pub memory_deduped: Vec<WorldItem>,
    pub semantic_deduped: Vec<WorldItem>,
    pub runtime_deduped: Vec<WorldItem>,
    pub reasoning_deduped: Vec<WorldItem>,

    /// Thread summary, if a thread ID was provided and resolved.
    pub thread_summary: Option<ThreadSummary>,

    /// Alerts collected during processing.
    pub alerts: Vec<Alert>,
}

impl ContextState {
    /// Create a fresh pipeline state for a given task and options.
    pub fn new(task: impl Into<String>, options: &ContextOptions) -> Self {
        Self {
            task: task.into(),
            options: options.clone(),
            reality_raw: Vec::new(),
            knowledge_raw: Vec::new(),
            memory_raw: Vec::new(),
            semantic_raw: Vec::new(),
            runtime_raw: Vec::new(),
            reasoning_raw: Vec::new(),
            reality_ranked: Vec::new(),
            knowledge_ranked: Vec::new(),
            memory_ranked: Vec::new(),
            semantic_ranked: Vec::new(),
            runtime_ranked: Vec::new(),
            reasoning_ranked: Vec::new(),
            reality_deduped: Vec::new(),
            knowledge_deduped: Vec::new(),
            memory_deduped: Vec::new(),
            semantic_deduped: Vec::new(),
            runtime_deduped: Vec::new(),
            reasoning_deduped: Vec::new(),
            thread_summary: None,
            alerts: Vec::new(),
        }
    }

    /// Consume the state and produce an `AggregatedContext`.
    pub fn into_aggregated(mut self) -> AggregatedContext {
        let mut ctx = AggregatedContext {
            thread: self.thread_summary.take(),
            reality: self.take_slice("reality"),
            knowledge: self.take_slice("knowledge"),
            memory: self.take_slice("memory"),
            semantic: self.take_slice("semantic"),
            runtime: self.take_slice("runtime"),
            reasoning: self.take_slice("reasoning"),
            alerts: std::mem::take(&mut self.alerts),
            estimated_tokens: 0,
        };

        // Compute total estimated tokens
        ctx.estimated_tokens = Self::estimate_tokens_for(&ctx);
        ctx
    }

    fn take_slice(&mut self, world: &str) -> WorldSlice {
        let items = match world {
            "reality" => std::mem::take(&mut self.reality_deduped),
            "knowledge" => std::mem::take(&mut self.knowledge_deduped),
            "memory" => std::mem::take(&mut self.memory_deduped),
            "semantic" => std::mem::take(&mut self.semantic_deduped),
            "runtime" => std::mem::take(&mut self.runtime_deduped),
            "reasoning" => std::mem::take(&mut self.reasoning_deduped),
            _ => Vec::new(),
        };
        let count = items.len();
        WorldSlice {
            world: world.to_string(),
            items,
            count,
        }
    }

    /// Estimate token count from an AggregatedContext (rough: chars / 4).
    pub fn estimate_tokens_for(ctx: &AggregatedContext) -> usize {
        let mut chars = 0usize;

        for slice in &[
            &ctx.reality,
            &ctx.knowledge,
            &ctx.memory,
            &ctx.semantic,
            &ctx.runtime,
            &ctx.reasoning,
        ] {
            for item in &slice.items {
                chars += item.char_count();
            }
        }

        for alert in &ctx.alerts {
            chars += alert.message.len() + alert.source.len() + 32;
        }

        if let Some(ref t) = ctx.thread {
            chars += t.title.len() + t.description.len() + 64;
        }

        // Rough: 1 token ≈ 4 characters
        chars / 4
    }

    /// Get a mutable reference to the correct ranked vec for the given world.
    pub fn ranked_mut(&mut self, world: &str) -> &mut Vec<WorldItem> {
        match world {
            "reality" => &mut self.reality_ranked,
            "knowledge" => &mut self.knowledge_ranked,
            "memory" => &mut self.memory_ranked,
            "semantic" => &mut self.semantic_ranked,
            "runtime" => &mut self.runtime_ranked,
            "reasoning" => &mut self.reasoning_ranked,
            _ => panic!("unknown world: {world}"),
        }
    }

    /// Get a reference to the raw vec for a world.
    pub fn raw(&self, world: &str) -> &[WorldItem] {
        match world {
            "reality" => &self.reality_raw,
            "knowledge" => &self.knowledge_raw,
            "memory" => &self.memory_raw,
            "semantic" => &self.semantic_raw,
            "runtime" => &self.runtime_raw,
            "reasoning" => &self.reasoning_raw,
            _ => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_item_construction() {
        let item = WorldItem::new("id1", "MyLabel", "Some content here")
            .with_score(0.85)
            .with_source("memgraph://entity/123")
            .with_type("Method")
            .with_meta("language", "rust");

        assert_eq!(item.id, "id1");
        assert_eq!(item.label, "MyLabel");
        assert_eq!(item.content, "Some content here");
        assert!((item.score - 0.85).abs() < 0.001);
        assert_eq!(item.sources.len(), 1);
        assert_eq!(item.entity_type, "Method");
        assert_eq!(item.metadata.get("language"), Some(&"rust".to_string()));
    }

    #[test]
    fn world_item_char_count() {
        let item = WorldItem::new("id", "hello", "world");
        // "hello" (5) + "world" (5) + "" (0) + sources sum (0) = 10
        assert_eq!(item.char_count(), 10);
    }

    #[test]
    fn world_slice_new() {
        let s = WorldSlice::new("reality");
        assert_eq!(s.world, "reality");
        assert_eq!(s.count, 0);
        assert!(s.items.is_empty());
        assert!(!s.has_items());
    }

    #[test]
    fn world_slice_has_items() {
        let mut s = WorldSlice::new("test");
        s.items.push(WorldItem::new("x", "y", "z"));
        s.count = 1;
        assert!(s.has_items());
    }

    #[test]
    fn alert_severity_ordering() {
        assert!(AlertSeverity::Critical > AlertSeverity::Warning);
        assert!(AlertSeverity::Warning > AlertSeverity::Info);
        assert!(AlertSeverity::Critical.at_least_warning());
        assert!(AlertSeverity::Warning.at_least_warning());
        assert!(!AlertSeverity::Info.at_least_warning());
    }

    #[test]
    fn context_options_defaults() {
        let opts = ContextOptions::default();
        assert_eq!(opts.worlds, None);
        assert_eq!(opts.max_tokens, Some(4096));
        assert_eq!(opts.min_score, Some(0.5));
        assert_eq!(opts.max_items_per_world, Some(20));
    }

    #[test]
    fn context_options_includes_world_no_filter() {
        let opts = ContextOptions::default();
        assert!(opts.includes_world("reality"));
        assert!(opts.includes_world("any_world"));
    }

    #[test]
    fn context_options_includes_world_with_filter() {
        let opts = ContextOptions {
            worlds: Some(vec!["reality".into(), "knowledge".into()]),
            ..Default::default()
        };
        assert!(opts.includes_world("reality"));
        assert!(opts.includes_world("knowledge"));
        assert!(!opts.includes_world("memory"));
        assert!(!opts.includes_world("semantic"));
    }

    #[test]
    fn context_options_includes_world_case_insensitive() {
        let opts = ContextOptions {
            worlds: Some(vec!["Reality".into()]),
            ..Default::default()
        };
        assert!(opts.includes_world("reality"));
        assert!(opts.includes_world("REALITY"));
    }

    #[test]
    fn context_state_new_produces_empty_state() {
        let state = ContextState::new("test task", &ContextOptions::default());
        assert_eq!(state.task, "test task");
        assert!(state.reality_raw.is_empty());
        assert!(state.alerts.is_empty());
        assert!(state.thread_summary.is_none());
    }

    #[test]
    fn context_state_ranked_mut() {
        let mut state = ContextState::new("task", &ContextOptions::default());
        state.ranked_mut("reality").push(WorldItem::new("a", "b", "c"));
        assert_eq!(state.reality_ranked.len(), 1);
    }

    #[test]
    fn context_state_into_aggregated() {
        let mut state = ContextState::new("task", &ContextOptions::default());
        state.reality_deduped.push(WorldItem::new("r1", "Real", "Content"));
        state.knowledge_deduped.push(WorldItem::new("k1", "Know", "Stuff"));

        let agg = state.into_aggregated();
        assert_eq!(agg.reality.items.len(), 1);
        assert_eq!(agg.knowledge.items.len(), 1);
        assert_eq!(agg.memory.items.len(), 0);
        assert!(agg.estimated_tokens > 0);
    }

    #[test]
    fn aggregated_context_default() {
        let ctx = AggregatedContext::default();
        assert!(ctx.thread.is_none());
        assert!(ctx.reality.items.is_empty());
        assert!(ctx.alerts.is_empty());
    }

    #[test]
    fn estimate_tokens_empty() {
        let ctx = AggregatedContext::default();
        assert_eq!(ctx.estimated_tokens, 0);
        assert_eq!(ContextState::estimate_tokens_for(&ctx), 0);
    }

    #[test]
    fn estimate_tokens_with_content() {
        let mut ctx = AggregatedContext::default();
        ctx.reality.items.push(WorldItem::new("id", "label", "some content"));
        // "label"(5) + "some content"(12) + "" (0) = 17 / 4 = 4
        let tokens = ContextState::estimate_tokens_for(&ctx);
        assert_eq!(tokens, 4);
    }
}
