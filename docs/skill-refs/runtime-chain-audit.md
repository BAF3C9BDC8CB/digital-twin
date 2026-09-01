# Runtime-chain audit

**用途**: 当 dt 行为可能来自过期或不同的可执行文件时, 先做运行时链审计, 再诊断搜索或改代码/配置。

## 1. 运行时链形态

```text
user command: dt ...
  -> /home/luis/.local/bin/dt
  -> /data/myProject/digital-twin-v2/target/release/dt
```

symlink 目标与 debug 二进制可能分叉。典型分歧:

- release: `target/release/dt`, 版本 `digital-twin 0.1.0`, 旧 mtime
- debug: `target/debug/dt`, 同版本, 新 mtime 且带 debug info
- 源码有未提交的 search-result 改动
- `cargo check --bin dt` 通过
- debug JSON 含新的 `content`/`metadata` 字段, release JSON 没有
- 两个二进制可能返回相似的 search hits, 只比 hit 标题会漏掉分叉

## 2. 最小证据收集

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
