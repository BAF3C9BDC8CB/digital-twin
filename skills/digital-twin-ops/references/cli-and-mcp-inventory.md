# dt CLI & MCP Tool Verified Inventory (2026-08-09)

Source of truth: `dt --help` / `dt <cmd> --help` / `src/main.rs` Clap definitions — **NOT the README** (README historically drifted: claimed "17 CLI 命令", non-existent `nacos-sync`/`k8s-sync`/`kub`, HanLP processor, Qwen3.5-9B model, MCP-via-gRPC).

## Verified top-level CLI commands (14)

```
clean backup schema health memorize event learn build search sense jcli jc-sync
```

Key signatures (from `--help`, 2026-08-09):

- `dt build [--path <p>] [-n/--name <n>] [--file <f>] [--full] [--no-pipeline] [--test] [--source code|knowledge]`
  - `--source knowledge` = KG→Qdrant sync (replaces deprecated `kg-sync`)
  - `--test` = built-in pipeline integration test (writes `test-`-prefixed data; needs real Memgraph+Qdrant)
- `dt search <QUERY> [--world all|code|knowledge|doc|config|memory] [--limit N] [--json] [-p/--project] [--file-type] [--content-type] [--show-content]`
- `dt event <HOOK_NAME> <CONTEXT_JSON>` — positional; old `--type/--entity-id/--details` interface replaced by hook-name + JSON context (hook templates in `config/event-hooks.yaml`)
- `dt memorize <KNOWLEDGE_TYPE> <ENTITY_ID> <DETAILS> [--entity-type] [--project]`
- `dt learn <TASK> [--entities --pattern --pitfalls --decisions --thread-id --success --project]`
- `dt backup create|list|restore <date>|verify <date>`
- `dt clean [--confirm] [--dry-run] [--targets]` — destructive; requires `--confirm`
- `dt schema init`
- ~~`dt kg-sync`~~ — 已移除(2026-08-12)；等价 `dt build --source knowledge`（config_chunks 用 `--config-chunks`）
- `dt jcli <action> [-j/--job] [--build] [--limit] [--params] [--env test|production]` (actions: list/params/history/log/build)
- `dt jc-sync [--job <name>]`
- ~~`dt daemon`~~ — 已移除(2026-08-12)，gRPC 层整体删除（CLI 为唯一入口）

## Non-existent commands (old docs still mention them — do not use)

- `dt nacos-sync` / `dt k8s-sync` / `dt kub` — NOT in the Clap enum. Remote-source ingestion now goes through the unified pipeline: `dt build --source nacos|jenkins`. MCP `svc_*`/`kublog_*`/`jcli_*` tools call external binaries (`svc`, `kublog`, `jcli`), not `dt` subcommands.

## Config load paths (verified in code)

- `~/.config/digital-twin/config.yaml` — main config (`main.rs` `load_config()`)
- `~/.config/digital-twin/pipeline.yaml` — pipeline providers (fixed user-level path since 2026-08-06)
- `~/.config/digital-twin/event-hooks.yaml` — runtime hook defs; repo `config/event-hooks.yaml` is only a template
- Repo `config/` files are templates/examples — not auto-loaded by the binary

## Ports

- Memgraph Bolt: code default `bolt://localhost:7687`; `config.yaml.example` uses `:7688` — deployment-dependent
- Qdrant: REST `:6333` answers; `:6334` is gRPC only (REST call → `HTTP/0.9 when not allowed`)
- ~~dt gRPC daemon: `127.0.0.1:50051`~~ — 已移除(2026-08-12)

## MCP tool removal procedure (mcp/mcp-server.py)

When a backing CLI command is removed, the MCP tool becomes a dead subprocess call. Remove it in ALL 3 places:

1. Module docstring tool list + count (e.g. `提供工具 (25个):` → 24)
2. `Tool(name="...", ...)` registration
3. Dispatcher `elif name == "...":` branch

Then update the header count. Example: `nacos_sync` removed 2026-08-09 (25 → 24).

Verify:

```bash
python3 -m py_compile mcp/mcp-server.py
```

Ad-hoc AST check (write temp script, run, delete): parse the file, collect `Tool(name=...)` keyword args, assert the removed name is absent and count matches; also grep source for the removed token (docstring/comments included). Repo-wide grep for the removed name afterward: `README.md`, `skill/guides/*.md`, `src/` — flag leftovers, but only edit what the user asked for.

## README audit findings (2026-08-09 rewrite)

README was rewritten from 327 lines of drifted claims to a verified-usage doc. Biggest false claims found and corrected:

- "CLI 命令 (17 个)" — actually 14
- `nacos-sync` / `k8s-sync` / `kub` listed as commands — don't exist
- HanLP processor chain — HanLP removed 2026-08-06 (see main SKILL.md)
- Model claims (Qwen3-14B / Qwen3.5-9B) — actual model config lives in `pipeline.yaml`, `llm_provider` is config-driven (glmcoding/siliconflow/xinference)
- "MCP via gRPC" — MCP is subprocess→CLI today
- Hook auto-trigger overclaims — several hooks are manual/reserved per `config/event-hooks.yaml` comments
- Hardcoded file/module counts in the directory tree (5 files / 11 modules / 8 services) — drift; don't hardcode counts in docs

Doc-review rule for this repo: verify every command/flag/file claim against `dt --help`, `dt <cmd> --help`, and `src/` greps; never trust README numbers or feature claims.
