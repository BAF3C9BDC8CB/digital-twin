# Release deployment and verification audit

Use this read-only-first checklist for digital-twin-v2 release reviews.

## 1. Establish the real chain

Record:

```bash
ROOT=/data/myProject/digital-twin-v2
BIN="$ROOT/target/release/dt"
readlink -f ~/.local/bin/dt 2>/dev/null || true
readlink -f ~/.local/bin/digital-twin-mcp 2>/dev/null || true
stat "$BIN" 2>/dev/null || true
sha256sum "$BIN" 2>/dev/null || true
```

Inspect callers and current execution:

```bash
ps -eo pid,ppid,user,lstart,args | grep -E '[d]t( |$)|digital-twin|mcp' || true
ss -ltnp 2>/dev/null | grep -E ':5005|:7688|:6333|:6334|:9997' || true
```

The MCP server calls `dt` via `subprocess` only (gRPC daemon removed 2026-08-12); binary swap takes effect on the next CLI invocation. Restart is only needed for verified long-running dt processes using the old executable.

## 2. Validate service definitions, not their names

For candidate units, read `ExecStart`, `Environment`, `User`, and restart policy. Check every referenced executable/config path exists and resolve symlinks. A unit pointing at paths such as `/usr/local/bin/digital-twin-engine` or `/data/myProject/digital-twin/config.yaml` is stale evidence if those paths do not exist; do not deploy or restart it without explicit reconciliation.

Also inspect repository state:

```bash
git status --short --branch
git log -1 --format='%h %ad %s' --date=iso
```

A release build can include local modifications, so record this before approval.

## 3. Separate restart boundaries

- CLI: no restart; each invocation loads a new executable.
- MCP subprocess caller: restart/recreate the MCP session when you need a clean path resolution or process state, even though future calls spawn the CLI.
- Verified long-running dt process: restart after an atomic binary swap, using its actual unit/process—not a stale unit.
- Memgraph, Qdrant, and embed/Xinference: leave running unless the release changes their protocol, schema, model, port, or service configuration.

## 4. Safe binary replacement

Stage the artifact under the same filesystem, validate it before touching the live path, preserve rollback, and replace atomically:

```bash
set -euo pipefail
ROOT=/data/myProject/digital-twin-v2
BIN="$ROOT/target/release/dt"
NEW="$ROOT/target/release/dt.new"
OLD="$ROOT/target/release/dt.previous"

test -x "$NEW"
"$NEW" --help >/dev/null
sha256sum "$NEW"
cp -p "$BIN" "$OLD"
mv -f "$NEW" "$BIN"
"$BIN" --help >/dev/null
"$BIN" sense --json
```

For higher assurance, use versioned directories and atomically switch a symlink. Keep the previous artifact and its hash until post-release verification and rollback are complete. Never overwrite a live executable with a truncating write, and do not delete the only known-good copy.

## 5. Post-release gates

Run smoke tests against the exact deployed path, not merely `dt` from `PATH`:

```bash
"$BIN" --help >/dev/null
"$BIN" sense --json | python3 -m json.tool >/dev/null
"$BIN" health
```

If MCP is in scope, invoke one read-only MCP operation after recreating its session and confirm it resolves the intended binary. Compare pre/post SHA-256 and executable metadata. For rollback, atomically restore the previous artifact, then restart only the verified daemon/consumer that holds the old process.
