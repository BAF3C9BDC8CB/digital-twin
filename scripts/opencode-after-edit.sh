#!/usr/bin/env bash
# OpenCode after-edit hook: update only the edited file in the KG.
set -u

file="${1:-${OPENCODE_FILE:-}}"
repo="${DT_PROJECT_ROOT:-/data/myProject/digital-twin-v2}"
log="${DT_BUILD_LOG:-/var/log/digital-twin/opencode-build.log}"

[ -n "$file" ] || exit 0
[ -f "$file" ] || exit 0
case "$file" in
  "$repo"/*) ;;
  *) exit 0 ;;
esac

mkdir -p "$(dirname "$log")" 2>/dev/null || true
lock="/tmp/dt-build-$(printf '%s' "$file" | sha256sum | cut -c1-16).lock"
(
  flock -n 9 || exit 0
  {
    printf '%s file=%s\n' "$(date -Is)" "$file"
    HTTPS_PROXY="${HTTPS_PROXY:-http://127.0.0.1:7897}" \
    HTTP_PROXY="${HTTP_PROXY:-http://127.0.0.1:7897}" \
    ALL_PROXY="${ALL_PROXY:-http://127.0.0.1:7897}" \
    SILICONFLOW_PROXY="${SILICONFLOW_PROXY:-http://127.0.0.1:7897}" \
    cargo run --quiet --manifest-path "$repo/Cargo.toml" -- \
      build --path "$repo" --file "$file"
  } >>"$log" 2>&1 || true
) 9>"$lock"
exit 0
