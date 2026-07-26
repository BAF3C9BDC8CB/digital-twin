# 阶段 2：合并命令 — SourceRouter + KnowledgeExtractor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `dt build` 加 `--source` 参数，实现 `KnowledgeExtractor` 吸收 `KgBridge::sync_incremental` 逻辑，让 `dt build --source knowledge` 替代 `dt kg-sync`，`dt kg-sync` 标记 deprecated 并内部转发。

**Architecture:** 在 `Build` 命令加 `--source` clap 参数。当 `--source knowledge` 时，直接调用已有的 `KgBridge::sync_incremental`（不重写逻辑，直接复用）。`dt kg-sync` 命令保留但打印 deprecation 提示，内部转发到相同逻辑。这是最小改动路径——不引入 SourceRouter trait 抽象（YAGNI），因为当前只有 knowledge 一种新源类型需要合并，code/doc 已在 build 中。

**Tech Stack:** Rust, clap CLI, tokio async, Memgraph, Qdrant

**Spec:** `docs/superpowers/specs/2026-07-26-unified-build-kg-first-design.md` 第 3 节（统一命令架构）、第 11 节阶段 2

## Global Constraints

- Rust edition 2021, workspace single crate
- 测试命令：`cargo test --lib`（当前 689 passed, 1 pre-existing failure in backup_sqlite）
- 增量默认：`dt build --source knowledge` 默认增量（只处理 `_kg_synced_at IS NULL`），`--full` 才全量
- 复用现有 `KgBridge::sync_incremental` / `sync_all`，不重写逻辑
- `dt kg-sync` 保留向后兼容，标记 deprecated 但不删除
- 不引入 SourceRouter trait 抽象（YAGNI）——当前只有 knowledge 一种源类型需要合并

## File Structure

| 文件 | 职责 | 改动类型 |
|------|------|---------|
| `src/main.rs` | CLI 定义，Build 命令加 `--source` 参数，KgSync 加 deprecation 提示 | 修改 |
| `src/interfaces/cli/build.rs` | `handle_build` 加 `source` 参数，分发到 knowledge sync 逻辑 | 修改 |
| `src/interfaces/cli/sync.rs` | `handle_kg_sync` 加 deprecation 日志 | 修改 |

---

### Task 1: Build 命令加 `--source` 参数

**Files:**
- Modify: `src/main.rs` L202-230（Build 命令定义）
- Modify: `src/interfaces/cli/build.rs` L28-39（handle_build 签名）
- Test: 手动验证 `dt build --help` 显示新参数

**Interfaces:**
- Consumes: 无
- Produces: `Build` 命令新增 `source: Option<String>` 字段，`handle_build` 新增 `source` 参数

- [ ] **Step 1: 在 Build 命令定义中加 `--source` 参数**

在 `src/main.rs` 的 `Build` 命令定义中（约 L202-230），在 `test` 字段后新增：

```rust
    Build {
        // ... 现有字段 ...

        /// Run the self-contained pipeline integration test.
        #[arg(long = "test")]
        test: bool,

        /// Source type to build: code (default), knowledge (sync KG nodes to vectors).
        /// Use "knowledge" as a replacement for `dt kg-sync`.
        #[arg(long = "source")]
        source: Option<String>,
    },
```

- [ ] **Step 2: 更新 main.rs 中 Build 命令的处理逻辑**

在 `src/main.rs` 的 `Some(Commands::Build { ... })` 处理分支（约 L1324），更新解构以包含 `source`：

```rust
        Some(Commands::Build { path, name, file, full, no_pipeline, test, source }) => {
```

然后在 `--test` 分支之后、`// No args at all → build all projects` 之前，新增 `--source knowledge` 分支：

```rust
            // ── dt build --source knowledge: replace dt kg-sync ──
            if let Some(ref src) = source {
                if src == "knowledge" {
                    tracing::info!("dt build --source knowledge: syncing KG nodes to vectors");
                    let graph = connect_graph().await;
                    let embed = connect_embed().await;
                    let vector = connect_vector().await;

                    if graph.is_none() || embed.is_none() || vector.is_none() {
                        eprintln!("error: build --source knowledge requires Memgraph + Qdrant + embed backends");
                        std::process::exit(1);
                    }

                    let graph = graph.unwrap();
                    let embed = embed.unwrap();
                    let vector = vector.unwrap();

                    let queue = Arc::new(
                        dt_daemon::application::sync::queue::VectorQueue::spawn(embed.clone()),
                    );

                    let incremental = !full;
                    dt_daemon::interfaces::cli::sync::handle_kg_sync(
                        incremental, None, false, Some(graph), Some(queue),
                    ).await?;
                    return Ok(());
                } else {
                    eprintln!("error: unknown source type '{src}'. Supported: knowledge");
                    std::process::exit(1);
                }
            }
```

- [ ] **Step 3: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过（可能有 unused variable 警告，后续步骤会用到）

- [ ] **Step 4: 验证 --help 显示新参数**

Run: `cargo run -- build --help 2>&1 | grep source`
Expected: 输出包含 `--source <SOURCE>` 和描述

- [ ] **Step 5: 提交**

```bash
git add src/main.rs
git commit -m "feat: Build 命令加 --source 参数 — 支持 knowledge 源类型

dt build --source knowledge 替代 dt kg-sync，统一命令入口。
当前只实现 knowledge 源类型，code/doc 走现有 build 路径。"
```

---

### Task 2: KgSync 命令加 deprecation 提示

**Files:**
- Modify: `src/main.rs` L1602-1610（KgSync 处理逻辑）
- Modify: `src/interfaces/cli/sync.rs` L149-155（handle_kg_sync 开头）

**Interfaces:**
- Consumes: 无
- Produces: `dt kg-sync` 执行时打印 deprecation 提示

- [ ] **Step 1: 在 KgSync 处理逻辑中加 deprecation 提示**

在 `src/main.rs` 的 `Some(Commands::KgSync { ... })` 处理分支（约 L1602），在调用 `handle_kg_sync` 之前新增：

```rust
        Some(Commands::KgSync { full, labels, config_chunks }) => {
            eprintln!("⚠️  Deprecated: `dt kg-sync` is deprecated. Use `dt build --source knowledge` instead.");
            eprintln!("   The command still works but will be removed in a future release.");
            let graph = connect_graph().await;
            // ... 现有逻辑不变 ...
```

- [ ] **Step 2: 在 handle_kg_sync 函数开头加 deprecation 日志**

在 `src/interfaces/cli/sync.rs` 的 `handle_kg_sync` 函数（约 L149）开头，在 `tracing::info!` 之后新增：

```rust
pub async fn handle_kg_sync(
    incremental: bool,
    labels: Option<String>,
    config_chunks: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    queue: Option<Arc<crate::application::sync::queue::VectorQueue>>,
) -> anyhow::Result<()> {
    tracing::warn!("dt kg-sync is deprecated — use `dt build --source knowledge` instead");
    tracing::info!(
```

- [ ] **Step 3: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 4: 验证 deprecation 提示**

Run: `cargo run -- kg-sync 2>&1 | head -5`
Expected: 输出包含 "Deprecated" 提示

- [ ] **Step 5: 提交**

```bash
git add src/main.rs src/interfaces/cli/sync.rs
git commit -m "feat: dt kg-sync 标记 deprecated — 提示用 dt build --source knowledge

保留向后兼容，命令仍可执行但打印 deprecation 警告。"
```

---

### Task 3: 端到端验证

**Files:**
- 无代码改动，纯验证任务

- [ ] **Step 1: 验证 dt build --source knowledge 正常工作**

Run: `cargo run --release -- build --source knowledge 2>&1 | tail -10`
Expected: 输出 "KG sync complete: N nodes synced, Mms"（与原 dt kg-sync 相同）

- [ ] **Step 2: 验证 dt build --source knowledge --full 全量同步**

Run: `cargo run --release -- build --source knowledge --full 2>&1 | tail -10`
Expected: 全量同步所有业务节点

- [ ] **Step 3: 验证 dt kg-sync 仍可工作（向后兼容）**

Run: `cargo run --release -- kg-sync 2>&1 | head -5`
Expected: 打印 deprecation 提示，然后正常执行同步

- [ ] **Step 4: 验证 dt build --help 显示 --source**

Run: `cargo run -- build --help 2>&1 | grep -A2 source`
Expected: 显示 `--source <SOURCE>` 和描述

- [ ] **Step 5: 运行全部测试确保无回归**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: 689 passed, 1 pre-existing failure

- [ ] **Step 6: 提交验证记录**

```bash
git commit --allow-empty -m "test: 阶段 2 端到端验证通过 — dt build --source knowledge 替代 kg-sync

验证结果：
- dt build --source knowledge 正常同步 KG 节点
- dt build --source knowledge --full 全量同步
- dt kg-sync 仍可工作（向后兼容 + deprecation 提示）
- dt build --help 显示 --source 参数
- 全部测试通过（689 passed, 1 pre-existing failure）"
```

---

## Self-Review

**1. Spec coverage:**
- 第 3 节（统一命令架构）：`--source` 参数 + knowledge 源类型 ✅
- 第 11 节阶段 2：合并命令 ✅
- `dt kg-sync` deprecated 转发 ✅

**2. Placeholder scan:** 无 TBD/TODO，所有步骤有完整代码 ✅

**3. Type consistency:**
- `source: Option<String>` 在 clap 定义和 main.rs 解构中一致 ✅
- `handle_kg_sync` 签名不变，复用现有逻辑 ✅

**4. 设计决策:**
- 不引入 SourceRouter trait（YAGNI）——当前只有 knowledge 一种源类型，直接 if 分支更简单
- 复用 `handle_kg_sync` 而非重写——避免代码重复
- `dt kg-sync` 保留向后兼容——避免破坏现有用户习惯