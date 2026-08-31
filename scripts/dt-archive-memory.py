#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Hermes 记忆 → KG 自动归档 hook
拦截 memory 工具的写操作(post_tool_call), 将写入/替换/删除的记忆条目同步到 digital-twin KG。
只做镜像备份, 不影响 Hermes 本地记忆(USER.md/MEMORY.md)本身的读写语义。

stdin:  {"hook_event_name","tool_name","tool_input":{...},"session_id","cwd","extra":{...}}
stdout: 必须尽快输出 {} 并退出(阻塞 agent 循环即失败)
"""
import json
import os
import subprocess
import sys

DT = os.path.expanduser("~/.local/bin/dt")
PROJECT = "hermes-memory"

def _log(msg):
    try:
        with open(os.path.expanduser("~/.hermes/logs/dt-archive-memory.log"), "a", encoding="utf-8") as f:
            f.write(msg + "\n")
    except Exception:
        pass

def _memorize(kind, eid, details):
    """调 dt memorize 写 KG, 失败不致命。"""
    try:
        r = subprocess.run(
            [DT, "memorize", kind, eid, details, "--project", PROJECT],
            capture_output=True, text=True, timeout=20,
        )
        return r.returncode == 0
    except Exception as e:
        _log(f"memorize 异常 {eid}: {e}")
        return False

def main():
    raw = sys.stdin.read()
    try:
        data = json.loads(raw) if raw.strip() else {}
    except Exception:
        print("{}")
        sys.exit(0)

    tool = data.get("tool_name", "")
    if tool != "memory":
        print("{}")
        sys.exit(0)

    inp = data.get("tool_input") or {}
    action = str(inp.get("action") or "")   # add/replace/remove
    target = str(inp.get("target") or "")   # memory/user
    content = str(inp.get("content") or "")  # 新增/替换后的内容
    old_text = str(inp.get("old_text") or "")

    if not action:
        print("{}")
        sys.exit(0)

    # 构造归档内容
    details = content or old_text
    prefix = "MEMORY" if target == "memory" else "USER"
    if action == "remove":
        details = f"[REMOVED] {old_text}"
    elif action == "replace":
        details = f"[REPLACED] {old_text} -> {content}"
    elif action == "add":
        details = f"[ADDED] {content}"

    if not details.strip():
        print("{}")
        sys.exit(0)

    import hashlib
    h = hashlib.sha1((action + target + old_text + content).encode()).hexdigest()[:12]
    eid = f"hermes-{prefix.lower()}-{action}-{h}"

    # 分类: 行为/偏好 → name 短名, 运维/配置 → domain 归 network/db/security
    name = "偏好" if any(k in content[:8] for k in ["中文", "交互", "模型", "措辞", "偏好"]) else "运维"
    domain = "general"
    if "DB " in content or "Redis" in content or "mysql" in content:
        domain = "database"
    elif "VPN" in content or "tun0" in content or "路由" in content:
        domain = "network"
    elif "GitHub" in content or "DataDome" in content or "风控" in content:
        domain = "security"
    first_line = content.split("\n")[0].strip()[:20]
    if first_line and first_line != content.strip()[:20]:
        name = f"{name}-{first_line}"
    details = f"name: {name}; summary: {name}; content: {content}; domain: {domain}"
    kind = "KnowledgeAdded"

    ok = _memorize(kind, eid, details)
    _log(f"[{action}:{target}] {eid} {'OK' if ok else 'FAIL'} :: {details[:80]}")
    print("{}")
    sys.exit(0)

if __name__ == "__main__":
    main()
