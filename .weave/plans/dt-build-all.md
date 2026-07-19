# Add `dt build --all` Command

## TL;DR
> **Summary**: Add a `--all` flag to the `dt build` CLI command that reads config.yaml, iterates over all configured projects, and runs build for each one sequentially with graceful error handling.
> **Estimated Effort**: Short

## Context
### Original Request
Add `dt build --all` that builds ALL projects defined in config.yaml. `--all` should be mutually exclusive with `--path`/`--name`, support `--full`, and handle errors gracefully (continue on failure, report summary).

### Key Findings

1. **CLI definition** is in `src/main.rs` lines 251-267 — the `Build` variant uses required `--path` and optional `--name`, `--file`, `--full`.
2. **CLI handler** is in `src/interfaces/cli/build.rs` — `handle_build()` takes individual params and creates a `BuildCommand` + `BuildDependencies`, then calls `cmd.run(deps)`.
3. **CLI dispatch** in main.rs lines 1334-1348 — connects backends, calls `handle_build()`.
4. **Config loading** is in main.rs — `DaemonConfig` with `projects: Vec<ProjectGroup>`, `resolve_project_paths()` flattens to `Vec<(String, PathBuf)>`, `load_config()` searches `./config.yaml` and `~/.config/...`.
5. **MCP tool `dt_build`** in `mcp-server.py` lines 870-885 — shell wrapper that calls `dt build --path <path> [--name <name>]`. Does NOT yet support `--all`.
6. **Old reference** at `/data/myProject/digital-twin/engine-rust/src/index/build_all.rs` — shows the expected pattern (iterate projects, collect success/fail, print summary).
7. **BuildReport** in `src/domain/types.rs` line 368 — has `project`, `files_scanned`, `files_changed`, `methods_total`, `methods_new`, `classes_total`, `elapsed_ms`.

### Project structure
```
src/
├── main.rs                           # Clap CLI definition + dispatch + config loading
├── interfaces/cli/build.rs           # handle_build(), handle_search(), handle_search_kg()
├── application/build/
│   ├── builder.rs                    # BuildCommand (clap Parser) + BuildDependencies + run()
│   ├── service.rs                    # BuildServiceImpl (implements BuildService trait)
│   └── pipeline.rs                   # PipelineTemplate
├── domain/types.rs                   # BuildReport
└── domain/traits.rs                  # BuildService trait
mcp-server.py                         # MCP dt_build handler (Python shell wrapper)
config.yaml                           # Project registry
```

## Objectives
### Core Objective
Add `dt build --all` flag that builds every project listed in config.yaml.

### Deliverables
- [ ] `--all` flag on `Build` CLI variant, mutually exclusive with `--path`/`--name`/`--file`
- [ ] `handle_build_all()` function in `src/interfaces/cli/build.rs` that iterates over projects
- [ ] Updated `--all` dispatch in `main.rs` 
- [ ] MCP `dt_build` tool updated to support `all` parameter
- [ ] Error handling: per-project failures don't abort the entire run; summary printed at end

### Definition of Done
- [ ] `dt build --all` runs without `--path`/`--name` and builds all config.yaml projects
- [ ] `dt build --all --full` does full rebuilds for all projects
- [ ] `dt build --all --path X` produces a conflict error
- [ ] A failing project does not stop the rest; summary reports success/failure counts
- [ ] MCP `dt_build` with `{"all": true}` triggers `dt build --all`

### Guardrails (Must NOT)
- Do NOT change existing `dt build --path` behavior
- Do NOT add `--filter` flag (keep it simple for this iteration)
- Do NOT change the `BuildCommand` struct in `builder.rs` (it's used programmatically elsewhere)
- Do NOT change the `BuildService` trait or `BuildServiceImpl`

## TODOs

- [x] 1. Add `--all` flag to `Commands::Build` variant in main.rs
  **What**: Add `#[arg(long = "all")] all: bool` to the `Build` variant. Change `path` from required to `Option<PathBuf>`. Add manual validation for mutual exclusivity.
  **Files**: `src/main.rs`
  **Acceptance**: Code compiles. `dt build --help` shows `--all` flag.

- [x] 2. Add `handle_build_all()` function in interfaces/cli/build.rs
  **What**: New async function that takes `projects: Vec<(String, PathBuf)>`, `full: bool`, and the 4 backend `Option<Arc<...>>` params. Iterates projects, calls `handle_build()` per project, catches errors, prints `[{i}/{total}]` progress, collects per-project results, and prints final summary (`N succeeded, M failed`).
  **Files**: `src/interfaces/cli/build.rs`
  **Acceptance**: Function compiles and demonstrates correct iteration/error-handling pattern.

- [x] 3. Add `--all` dispatch in main.rs match arm
  **What**: In the `Some(Commands::Build { path, name, file, full, all }) =>` arm, check if `all` is true. If so, validate `path.is_none() && name.is_none() && file.is_none()`, then load config via `load_config()`, resolve projects via `resolve_project_paths()`, and call the new `handle_build_all()`. If `all` is false, keep existing behavior (backward compat).
  **Files**: `src/main.rs`
  **Acceptance**: `dt build --all` runs without errors. `dt build --all --path X` prints "error: --all cannot be combined with --path/--name/--file" and exits.

- [x] 4. Add backwards-compatible connection of backends for `--all`
  **What**: The `--all` arm in main.rs needs the same `connect_neo4j()`, `connect_embed()`, `connect_vector()`, `connect_snapshot()` connections as the normal build arm. Reuse the same connection logic (don't duplicate).
  **Files**: `src/main.rs`
  **Acceptance**: Backends connected only once for the entire `--all` run (not re-connected per project).

- [x] 5. Update MCP `dt_build` tool in mcp-server.py
  **What**: Add `"all"` parameter (`{"type": "boolean", "default": false}`) to the `dt_build` tool's `inputSchema`. In the handler (line 870), add `if arguments.get("all")` branch that runs `[DT_BIN, "build", "--all"]` (with optional `--full`). Add `full` parameter to the schema as well for consistency.
  **Files**: `mcp-server.py`
  **Acceptance**: MCP `dt_build` with `{"all": true}` passes `--all` to the CLI binary.

- [x] 6. Verify existing tests pass and no regressions
  **What**: Run `cargo build` and `cargo test` in the project root. Verify all existing tests still pass. The builder test (`command_parses_args`) should still pass since `BuildCommand` is unchanged.
  **Files**: None (verification only)
  **Acceptance**: `cargo build` compiles without error. `cargo test` passes all tests.

## Verification
- [ ] `cargo build` compiles successfully
- [ ] `cargo test` passes all existing tests
- [ ] `cargo run -- build --help` shows `--all` option
- [ ] `cargo run -- build --all --path /tmp` exits with conflict error
- [ ] `cargo run -- build --path /tmp --name test` still works (backward compat)
- [ ] MCP tool response when `{"all": true}` calls the correct CLI command

## Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| `dt build --all` with no config.yaml | Print "config.yaml not found" and exit |
| `dt build --all` with empty projects list | Print "no projects configured" and exit |
| `dt build --all` — one project dir missing | Skip with warning, continue others |
| `dt build --all` — one project build fails | Collect error, continue, report at end |
| `dt build --all --full` | Pass `full=true` to each `handle_build()` call |
| `dt build --all --path X` | Print conflict error, exit |
| `dt build --all --name X` | Print conflict error, exit |
| `dt build --all --file X` | Print conflict error, exit |
| `dt build --path X` (no --all, existing behavior) | Unchanged |
| `dt build --path X --name N` (no --all) | Unchanged |
