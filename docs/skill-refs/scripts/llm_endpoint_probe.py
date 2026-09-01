#!/usr/bin/env python3
"""Probe an OpenAI-compatible LLM endpoint with the EXACT request shape dt's
GLMCodingChatClient uses (response_format json_object, stream false, max_tokens 4096).

Use BEFORE pointing dt's `providers.glmcoding.url` at any new endpoint
(opencode-go, glmcoding.cn, siliconflow, etc.) — verifies the endpoint is
reachable AND returns non-empty `content` (not reasoning-only), so a build
won't fail with 404 or empty-content EOF parsing.

Pitfall this guards against (2026-08-10): GLMCodingChatClient concatenates
`{url}/v1/chat/completions` — the url MUST be the ROOT (no `/v1` suffix).
`https://opencode.ai/zen/go/v1` + client => `/v1/v1/chat/completions` -> 404
while health check `{url}/v1/models` stays 200 (confusing green-health/red-build).

Usage:
    python3 llm_endpoint_probe.py --url https://opencode.ai/zen/go --model deepseek-v4-flash
    python3 llm_endpoint_probe.py --url http://localhost:9997/v1 --model qwen3.5
    # key: auto-read from ~/.config/digital-twin/pipeline.yaml providers.glmcoding.api_key
    #      fallback: ~/.hermes/.env OPENCODE_GO_API_KEY ; or --key directly

Never prints the key itself (prefix + length only).
"""
import argparse
import json
import pathlib
import re
import sys
import time
import urllib.error
import urllib.request

USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"


def load_key(explicit: str | None) -> str:
    if explicit:
        return explicit
    # 1) pipeline.yaml glmcoding.api_key
    p = pathlib.Path.home() / ".config/digital-twin/pipeline.yaml"
    if p.exists():
        try:
            import yaml  # type: ignore
            cfg = yaml.safe_load(p.read_text(encoding="utf-8"))
            k = cfg.get("providers", {}).get("glmcoding", {}).get("api_key", "")
            if k:
                return k
        except Exception:
            pass
    # 2) ~/.hermes/.env OPENCODE_GO_API_KEY
    env = pathlib.Path.home() / ".hermes/.env"
    if env.exists():
        m = re.search(r"OPENCODE_GO_API_KEY=(.*)", env.read_text(encoding="utf-8"))
        if m and m.group(1).strip():
            return m.group(1).strip()
    return ""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", required=True, help="Root base URL, WITHOUT /v1 (dt client appends /v1/chat/completions)")
    ap.add_argument("--model", default="deepseek-v4-flash")
    ap.add_argument("--key", default=None, help="override key; default auto-read from pipeline.yaml / .env")
    args = ap.parse_args()

    key = load_key(args.key)
    if not key:
        print("ERROR: no API key found (pipeline.yaml glmcoding.api_key / OPENCODE_GO_API_KEY / --key)")
        return 1
    print(f"key: {key[:6]}... (len {len(key)})  url-root: {args.url}  model: {args.model}")

    chat_url = f"{args.url.rstrip('/')}/v1/chat/completions"
    body = json.dumps({
        "model": args.model,
        "messages": [
            {"role": "system", "content": "你是代码分析助手,只输出 JSON。"},
            {"role": "user", "content": '分析以下 Java 方法的用途,返回 {"purpose": "...", "logic": "..."}:\npublic String encrypt(String data) { return data; }'},
        ],
        "temperature": 0.1,
        "max_tokens": 4096,
        "stream": False,
        "response_format": {"type": "json_object"},
    }).encode()

    req = urllib.request.Request(chat_url, data=body, method="POST")
    req.add_header("Authorization", f"Bearer {key}")
    req.add_header("Content-Type", "application/json")
    req.add_header("User-Agent", USER_AGENT)

    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            resp = json.loads(r.read().decode("utf-8"))
            msg = resp["choices"][0]["message"]
            content = msg.get("content") or ""
            print(f"OK   POST {chat_url} -> {r.status}  {time.time()-t0:.1f}s")
            print(f"     finish_reason: {resp['choices'][0]['finish_reason']}")
            print(f"     content len: {len(content)}  (non-empty + finish=stop => dt build will parse)")
            print(f"     content: {content[:200]!r}")
            if resp["choices"][0]["finish_reason"] != "stop" or not content.strip():
                print("WARN: empty content or finish != stop — dt reads only `content`; build would degrade/EOF.")
                return 2
            return 0
    except urllib.error.HTTPError as e:
        print(f"FAIL POST {chat_url} -> HTTP {e.code}  {time.time()-t0:.1f}s")
        print(e.read(500).decode("utf-8", "replace"))
        print("If 404: check url-root has NO /v1 suffix (client appends it).")
        return 1
    except Exception as e:
        print(f"ERR  {type(e).__name__}: {e}  {time.time()-t0:.1f}s")
        return 1


if __name__ == "__main__":
    sys.exit(main())
