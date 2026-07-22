# Configurable Batch Sizes Implementation Plan

> **For agentic workers:** Use subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract hardcoded batch/chunk/concurrency constants in the build pipeline and updater into `config.yaml`, with sensible defaults, so they can be tuned without recompilation.

**Architecture:** A new `batch:` section in `config.yaml` is parsed into a `BatchConfig` struct, threaded through `DaemonConfig` → `handle_build` → `BuildServiceImpl` → `PipelineTemplate` where it replaces inline `const` / `.chunks(N)` values. The defaults live in `BatchConfig::default()` so missing config is harmless.

**Tech Stack:** Rust, serde_yaml, tokio

## Global Constraints

- Backward-compatible: config.yaml without `batch:` section uses defaults
- Build pipeline and updater share the same config values
- Test: unit tests must compile; `cargo test` must pass

---

## Files Map

| File | Action | Purpose |
|------|--------|---------|
| `config.yaml` | Modify | Add `batch:` section |
| `config.yaml.example` | Modify | Add documented example |
| `src/domain/types.rs` | Modify | Add `BatchConfig` struct with `Default` |
| `src/main.rs` | Modify | Parse `batch:` into `DaemonConfig`, pass to build CLI |
| `src/interfaces/cli/build.rs` | Modify | Accept `BatchConfig` in `handle_build` and `handle_build_all` |
| `src/application/build/builder.rs` | Modify | Add `batch_config` to `BuildDependencies`, pass to `BuildServiceImpl` |
| `src/application/build/service.rs` | Modify | Accept `BatchConfig` in `BuildServiceImpl::new`, pass to `PipelineTemplate` |
| `src/application/build/pipeline.rs` | Modify | Replace hardcoded consts/chunks with `BatchConfig` fields |
| `src/application/build/updater.rs` | Modify | Replace hardcoded `.chunks()` with `BatchConfig` fields |

---

### Task 1: Define `BatchConfig` struct

**Files:**
- Modify: `src/domain/types.rs`

**Interfaces:**
- Produces: `BatchConfig` struct with `Default` impl, serde `Deserialize`

- [ ] Add `BatchConfig` struct after `AppConfig` (line ~80), before `Logger`:

```rust
/// Batch processing sizes for build pipeline and upsert operations.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BatchConfig {
    /// Number of items per UNWIND batch when writing nodes to Memgraph.
    /// Applies to Method, Class, and Module nodes uniformly.
    #[serde(default = "default_unwind_batch")]
    pub unwind: usize,
    /// Number of text items per embedding gRPC call.
    #[serde(default = "default_embed_batch")]
    pub embed: usize,
    /// Number of vector points per Qdrant upsert call.
    #[serde(default = "default_upsert_batch")]
    pub upsert: usize,
    /// Number of concurrent embedding gRPC streams.
    #[serde(default = "default_embed_concurrency")]
    pub embed_concurrency: usize,
}

fn default_unwind_batch() -> usize { 200 }
fn default_embed_batch() -> usize { 512 }
fn default_upsert_batch() -> usize { 1000 }
fn default_embed_concurrency() -> usize { 3 }

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            unwind: default_unwind_batch(),
            embed: default_embed_batch(),
            upsert: default_upsert_batch(),
            embed_concurrency: default_embed_concurrency(),
        }
    }
}
```

- [ ] Commit: `feat: add BatchConfig struct with defaults`

---

### Task 2: Parse `batch:` from config.yaml into `DaemonConfig`

**Files:**
- Modify: `src/main.rs:552-559` (DaemonConfig struct)

- [ ] Add `batch` field to `DaemonConfig`:

```rust
#[derive(Debug, Deserialize)]
struct DaemonConfig {
    #[serde(default)]
    projects: Vec<ProjectGroup>,
    #[serde(default)]
    services: ServiceConfig,
    #[serde(default)]
    batch: BatchConfig,  // <-- new
}
```

- [ ] Add import at top of main.rs:

```rust
use dt_daemon::domain::types::BatchConfig;
```

- [ ] In `handle_build_all` (main.rs:1300), extract `batch_config` and pass it:

```rust
let batch_config = cfg.batch.clone();
dt_daemon::interfaces::cli::build::handle_build_all(
    projects, full, graph, vector, embed, snapshot, batch_config,
)
.await?;
```

- [ ] In single `handle_build` (main.rs:1331), extract `batch_config` and pass it:

```rust
let batch_config = load_config()
    .map(|c| c.batch)
    .unwrap_or_default();
dt_daemon::interfaces::cli::build::handle_build(
    actual_path, name, file, full, graph, vector, embed, snapshot, batch_config,
)
.await?;
```

- [ ] Commit: `feat: parse batch config from config.yaml`

---

### Task 3: Thread `BatchConfig` through build CLI and builder

**Files:**
- Modify: `src/interfaces/cli/build.rs:17-26` (handle_build signature)
- Modify: `src/interfaces/cli/build.rs:74-81` (handle_build_all signature)
- Modify: `src/application/build/builder.rs:43-48` (BuildDependencies)

- [ ] Update `handle_build` signature to accept `batch_config`:

```rust
pub async fn handle_build(
    path: PathBuf,
    name: Option<String>,
    file: Option<PathBuf>,
    full: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    batch_config: BatchConfig,   // new
) -> anyhow::Result<()> {
```

- [ ] Add import at top of `build.rs`:

```rust
use crate::domain::types::BatchConfig;
```

- [ ] Pass `batch_config` into `BuildDependencies`:

```rust
let deps = crate::application::build::builder::BuildDependencies {
    graph,
    vector,
    snapshot,
    embed,
    batch_config: Some(batch_config),  // new
};
```

- [ ] Update `handle_build_all` signature similarly, and pass `batch_config.clone()` to each `handle_build` call.

- [ ] In `builder.rs`, add field to `BuildDependencies`:

```rust
pub struct BuildDependencies {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
    pub batch_config: Option<BatchConfig>,  // new
}
```

- [ ] Add import in `builder.rs`:

```rust
use crate::domain::types::BatchConfig;
```

- [ ] In `BuildCommand::run()`, pass `batch_config` to `BuildServiceImpl::new`:

```rust
let batch = deps.batch_config.unwrap_or_default();
let service = BuildServiceImpl::new(
    registry,
    deps.graph,
    deps.vector,
    deps.snapshot,
    deps.embed,
    self.full,
    batch,  // new
);
```

- [ ] Commit: `feat: thread BatchConfig through build CLI to BuildServiceImpl`

---

### Task 4: Plumb `BatchConfig` into `BuildServiceImpl` and `PipelineTemplate`

**Files:**
- Modify: `src/application/build/service.rs:28-57` (BuildServiceImpl struct + new)
- Modify: `src/application/build/pipeline.rs:54-62` (PipelineTemplate struct + new)

- [ ] Add `batch_config` field to `BuildServiceImpl`:

```rust
pub struct BuildServiceImpl {
    parser_registry: Arc<ParserRegistry>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    scan_config: ScanConfig,
    full: bool,
    batch_config: BatchConfig,  // new
}
```

- [ ] Add import in `service.rs`:

```rust
use crate::domain::types::BatchConfig;
```

- [ ] Update `new()` signature:

```rust
pub fn new(
    parser_registry: Arc<ParserRegistry>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    snapshot: Option<Arc<dyn SnapshotRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    full: bool,
    batch_config: BatchConfig,  // new
) -> Self {
    Self {
        parser_registry,
        graph,
        vector,
        snapshot,
        embed,
        scan_config: ScanConfig::default(),
        full,
        batch_config,  // new
    }
}
```

- [ ] Pass `batch_config` to `PipelineTemplate` in `build()`:

```rust
let pipeline = PipelineTemplate::new(self.parser_registry.clone(), self.batch_config.clone());
```

- [ ] In `pipeline.rs`, add field to `PipelineTemplate`:

```rust
pub struct PipelineTemplate {
    parser_registry: Arc<ParserRegistry>,
    batch_config: BatchConfig,  // new
}
```

- [ ] Add import in `pipeline.rs`:

```rust
use crate::domain::types::BatchConfig;
```

- [ ] Update `new()`:

```rust
pub fn new(parser_registry: Arc<ParserRegistry>, batch_config: BatchConfig) -> Self {
    Self { parser_registry, batch_config }
}
```

- [ ] Fix the test in `pipeline.rs` that creates `PipelineTemplate`:

```rust
#[test]
fn pipeline_can_be_created() {
    let registry = Arc::new(ParserRegistry::new());
    let _pipeline = PipelineTemplate::new(registry, BatchConfig::default());
}
```

- [ ] Commit: `feat: plumb BatchConfig into PipelineTemplate`

---

### Task 5: Replace hardcoded constants in `pipeline.rs`

**Files:**
- Modify: `src/application/build/pipeline.rs`

Replace each inline constant with `self.batch_config.*`:

- [ ] **Embed batching (line 173-174):**

```rust
// Before:
const EMBED_BATCH: usize = 512;
const CONCURRENT: usize = 3;

// After:
let embed_batch = self.batch_config.embed;
let concurrent = self.batch_config.embed_concurrency;
```

- [ ] **Qdrant upsert batch (line 224):**

```rust
// Before:
const UPSERT_BATCH: usize = 1000;

// After:
let upsert_batch = self.batch_config.upsert;
```

- [ ] **Doc embed/concurrent/upsert (lines 861-862, 902) — same values, no separate doc constants needed after simplification:**

Remove `DOC_EMBED_BATCH`, `DOC_CONCURRENT`, `DOC_UPSERT_BATCH` and use the same `embed`, `embed_concurrency`, `upsert` fields:

```rust
// Before (line 861):
const DOC_EMBED_BATCH: usize = 256;

// After:
let embed_batch = self.batch_config.embed;

// Before (line 862):
const DOC_CONCURRENT: usize = 2;

// After:
let concurrent = self.batch_config.embed_concurrency;

// Before (line 902):
const DOC_UPSERT_BATCH: usize = 500;

// After:
let upsert_batch = self.batch_config.upsert;
```

> Note: previously doc embed batch was 256 and doc concurrency was 2, which were different from code embed (512/3). After this change, docs and code share the same values from config. The defaults (512 embed, 3 concurrency, 1000 upsert) work fine for docs too.

- [ ] **Memgraph UNWIND batch (lines 422, 454, 480):**

Replace `methods.chunks(200)`, `classes.chunks(100)`, `modules.chunks(100)` with:

```rust
let unwind = self.batch_config.unwind;

// Before (line 422):
for chunk in methods.chunks(200) {

// After:
for chunk in methods.chunks(unwind) {

// Before (line 454):
for chunk in classes.chunks(100) {

// After:
for chunk in classes.chunks(unwind) {

// Before (line 480):
for chunk in modules.chunks(100) {

// After:
for chunk in modules.chunks(unwind) {
```

- [ ] Verify `cargo check` passes
- [ ] Commit: `feat: use BatchConfig in pipeline.rs`

---

### Task 6: Replace hardcoded chunks in `updater.rs`

**Files:**
- Modify: `src/application/build/updater.rs`

- [ ] First, make `updater.rs` accept a `BatchConfig`. Check how it's called — search for the call site.

The updater functions `write_methods`, `write_classes`, `write_modules` are top-level async functions. They need to accept `&BatchConfig`:

- [ ] Update signatures at lines ~283, ~339, ~443:

```rust
use crate::domain::types::BatchConfig;

async fn write_methods(graph: &Arc<dyn GraphRepository>, methods: &[MethodBlock], batch: &BatchConfig) {
    for chunk in methods.chunks(batch.unwind) {
        // ... rest unchanged
    }
}

async fn write_classes(graph: &Arc<dyn GraphRepository>, classes: &[ClassBlock], batch: &BatchConfig) {
    for chunk in classes.chunks(batch.unwind) {
        // ... rest unchanged
    }
}

async fn write_modules(graph: &Arc<dyn GraphRepository>, modules: &[ModuleBlock], batch: &BatchConfig) {
    for chunk in modules.chunks(batch.unwind) {
        // ... rest unchanged
    }
}
```

- [ ] Update the call sites in `updater.rs` to pass `batch`. Search for where these functions are called and add the parameter.

- [ ] Verify `cargo check` passes
- [ ] Commit: `feat: use BatchConfig in updater.rs`

---

### Task 7: Update config files

**Files:**
- Modify: `config.yaml`
- Modify: `config.yaml.example`

- [ ] In `config.yaml`, add after `snapshot_dir` (line 106):

```yaml
# ── 批量处理 ─────────────────────────────────────────────────────────────────
# 控制构建管道中各阶段的批次大小和并发数
batch:
  # UNWIND 批量写入 Memgraph 的节点数（方法/类/模块共用）
  unwind: 200
  # 每次 gRPC 嵌入调用的文本数
  embed: 512
  # 每次 Qdrant upsert 的向量点数
  upsert: 1000
  # 并发嵌入 gRPC 流数
  embed_concurrency: 3
```

- [ ] In `config.yaml.example`, add after `snapshot_dir` line:

```yaml
# Optional: batch processing sizes for build pipeline
# batch:
#   unwind: 200             # nodes per Memgraph UNWIND batch
#   embed: 512              # texts per embedding gRPC call
#   upsert: 1000            # vectors per Qdrant upsert call
#   embed_concurrency: 3    # concurrent embed gRPC streams
```

- [ ] Commit: `feat: add batch config to config.yaml and example`

---

### Task 8: Verify with cargo check / test

**Files:** (none — verification only)

- [ ] Run `cargo check` in project root:

```bash
cargo check 2>&1
```

Expected: no errors

- [ ] Run `cargo test`:

```bash
cargo test 2>&1
```

Expected: all tests pass

- [ ] If any compilation errors, fix them (e.g., missing imports in `updater.rs` call sites).

- [ ] Final commit: `chore: fix compilation after BatchConfig integration`
