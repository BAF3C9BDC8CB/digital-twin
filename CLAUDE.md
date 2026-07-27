# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Build the project
cargo build
cargo build --release

# Run all unit tests (inline #[cfg(test)] modules)
cargo test

# Run a single test
cargo test test_name
cargo test <module>::<test_name>  # e.g., cargo test domain::id::tests::method_id_structure

# Run tests in a specific module
cargo test domain::id
cargo test application::build

# Integration tests (require Memgraph + Qdrant running)
# These run via the `dt` CLI binary, not cargo test:
dt build --test        # BuildCommand integration test — builds test-pipeline project, verifies KG+Qdrant
dt clean --test        # Remove all test- prefixed data

# Lint
cargo clippy --all-targets

# Format
cargo fmt
```

## Key Architecture

**Single-crate DDD layered architecture** (`src/lib.rs` is the crate root):

```
src/
  domain/          # Domain layer: types, traits, error, config, id (zero internal deps)
  infrastructure/  # Infrastructure: Memgraph, Qdrant, SQLite, tree-sitter parsers, scanner, embedder
  application/     # Application layer: build, sync, context, knowledge, plugins (orchestration)
  interfaces/      # Interface layer: gRPC server, CLI command handlers
  shared/          # Cross-cutting: logging, coordinator, chunker, vectorizer
```

**Six World Model** — the system classifies data into six worlds:

| World | Data | Storage |
|-------|------|---------|
| Reality | Code, config, K8s resources | Memgraph + Qdrant |
| Knowledge | Concepts, patterns, playbooks, experience | Memgraph |
| Memory | Events, sessions, timeline | Memgraph |
| Semantic | Documents, API, log pattern vectors | Qdrant |
| Runtime | Pod status, service runtime | K8s API (live) |
| Reasoning | Observation → Analysis → Decision chain | Memgraph (with TTL) |

**CLI binary** (`src/main.rs`): `dt` with 26+ commands. Dual-mode: server (gRPC daemon) or CLI subcommand.

## Pipeline Engine

Processor orchestration framework for converting unstructured files into structured knowledge:

```
File → TreeSitterProcessor → ChunkProcessor → {HanlpClientProcessor → LlmClientProcessor} → StoreProcessor → KG+Qdrant
```

- **CPU stages** (priority ≥ 85): tree_sitter (100), chunk (90) — run in full parallel
- **GPU stages** (priority < 85): hanlp (80), llm (60) — semaphore-capped concurrency
- Config: `config/pipeline.yaml`
- Processors: `src/application/pipeline/processors/`

## Qdrant Collections

Two collections with strict separation:
- `{project}_methods` — code search (method-level, from `dt build`)
- `kg_nodes` — knowledge graph entity vectors (from `dt kg-sync`)

## CrossWorldSearch (`src/application/context/search_mcp.rs`)

Unified search entry point — dispatches by `world` parameter:
- `world=code` → Qdrant `{project}_methods`
- `world=knowledge` → Memgraph (Concept/Decision/...)
- `world=doc` → Qdrant `kg_nodes`

## External Dependencies

- **Memgraph 5.x** (Bolt :7687) — knowledge graph
- **Qdrant** (gRPC :6334) — vector storage
- **SiliconFlow API** — embed (BGE-M3), rerank, chat (Qwen2.5-14B)
- **tree-sitter** — multi-language AST parsing (Java, Python, JS, TS, Go, Rust, PHP)

## Code Style

- `rust-toolchain.toml`: stable channel
- `rustfmt`: max_width=100, tab_spaces=4, edition=2021
- `clippy.toml`: cognitive-complexity-threshold=30, too-many-arguments-threshold=8
- Error handling: `anyhow` for application, `thiserror` for domain errors (`DtError`)
- Async: `tokio` + `async-trait` for async trait methods
- Entity IDs: `dt://entity/{project}/...` URI scheme (see `src/domain/id.rs`)

## Build Strategies (`src/application/build/strategy/`)

- **Incremental** (default): SHA1 diff against SQLite snapshots — only processes changed files
- **FullRebuild**: wipe all data and rebuild from scratch

## Test Infrastructure

- **Unit tests**: inline `#[cfg(test)]` modules in source files, run via `cargo test`
- **Integration tests**: `dt build --test` — runs against real test-pipeline project, verifies Memgraph + Qdrant output matches `test/expected.json`
- **Test runner**: `src/application/pipeline/test/runner.rs` — standalone verify function
- **Test fixtures**: `test/fixtures/` (Java, Python, Markdown, YAML)
- **Test project**: `test/project/` — real project used for integration testing

## Multi-Agent Team System

The project uses a formal multi-agent team pipeline for code changes. Every change goes through:

```
Change Request → Architect Guard → [Implementer + Tester] → Reviewer → Integrator → Done
```

### Agent Roles

| Agent | File | Role |
|-------|------|------|
| **Architect** | `.claude/agents/architect.md` | DDD layer boundary guardian — checks `use crate::*` imports against layer rules |
| **Implementer** | `.claude/agents/implementer.md` | Code implementation — TDD, cargo fmt, cargo clippy |
| **Tester** | `.claude/agents/tester.md` | Test writing — unit tests, edge cases, error paths |
| **Reviewer** | `.claude/agents/reviewer.md` | Code review — quality, security, performance |
| **Integrator** | `.claude/agents/integrator.md` | Integration — full build, test suite, clippy, fmt check |

### DDD Layer Rules (enforced by Architect)

| Layer | May import from | Must NOT import from |
|-------|----------------|---------------------|
| `src/domain/` | `crate::domain::*` | `infrastructure/`, `application/`, `interfaces/` |
| `src/infrastructure/` | `domain/`, `shared/` | `application/`, `interfaces/` |
| `src/application/` | `domain/`, `infrastructure/`, `shared/` | `interfaces/` |
| `src/interfaces/` | All layers | None |
| `src/shared/` | `domain/` | `infrastructure/`, `application/`, `interfaces/` |

**Exception**: `src/main.rs` (composition root) may reference all layers.

### Workflows

- **`change-workflow`**: Full change pipeline — architect guard → implement + test → review → integrate
- **`arch-guard-workflow`**: Standalone architecture check (read-only)