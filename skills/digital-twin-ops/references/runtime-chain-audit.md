# Runtime-chain audit reference

Observed and reusable audit pattern for digital-twin-v2:

```text
user command: dt ...
  -> /home/luis/.local/bin/dt
  -> /data/myProject/digital-twin-v2/target/release/dt
```

The symlink target and the debug binary can diverge. In the investigated case:

- release: `target/release/dt`, version `dt-daemon 0.1.0`, older mtime;
- debug: `target/debug/dt`, same version, newer mtime and debug info;
- source had uncommitted search-result changes;
- `cargo check --bin dt` passed;
- debug JSON contained new `content`/`metadata` fields, while release JSON did not;
- both binaries could return similar search hits, so comparing only hit titles would miss the skew.

Minimal evidence collection:

```bash
R=/data/myProject/digital-twin-v2
printf 'PATH=%s\n' "$(command -v dt)"
readlink -f "$(command -v dt)"
for f in "$R/target/debug/dt" "$R/target/release/dt"; do
  stat -c '%n|%s|mtime=%y' "$f"
  sha256sum "$f"
  "$f" --version
 done
git -C "$R" status --short --branch
git -C "$R" diff --stat
cargo check --bin dt
```

Controlled comparison should use identical arguments and capture stdout/stderr independently. Compare JSON keys as well as values:

```bash
/path/to/debug/dt search Nacos --limit 3 --json > /tmp/debug.json 2>/tmp/debug.err
/path/to/release/dt search Nacos --limit 3 --json > /tmp/release.json 2>/tmp/release.err
python3 - <<'PY'
import json
for p in ['/tmp/debug.json', '/tmp/release.json']:
    x=json.load(open(p))
    print(p, sorted(x.get('hits',[{}])[0].keys()), x.get('per_world_counts'), x.get('degraded'))
PY
```

Config lookup for the core daemon is `~/.config/digital-twin/config.yaml`; verify from source and filesystem. `dt health` validates backend connectivity only, not binary freshness or index coverage.
