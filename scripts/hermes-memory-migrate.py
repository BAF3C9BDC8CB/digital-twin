#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Hermes 记忆 → KG 存量迁移脚本(修正版)
将 ~/.hermes/memories/MEMORY.md 和 USER.md 的每条记录写入 digital-twin KG。
dt memorize 的 details 必须是 "key: value; ..." 格式, 核心内容放 content: 字段。
按 '§' 分条, 每条独立 entity_id。
"""
import hashlib
import os
import subprocess

MEM_DIR = os.path.expanduser("~/.hermes/memories")
DT = os.path.expanduser("~/.local/bin/dt")
PROJECT = "hermes-memory"


def read_sections(path):
    if not os.path.exists(path):
        return []
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()
    return [s.strip() for s in text.split("§") if s.strip()]


def memorize(eid, name, content, domain=""):
    details = f"name: {name}; summary: {name}; content: {content}; domain: {domain}"
    cmd = [DT, "memorize", "KnowledgeAdded", eid, details, "--project", PROJECT]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        print(f"  [FAIL] {eid}: {r.stderr.strip()[:150]}")
        return False
    return True


def classify(text):
    # 高频行为/偏好 → 短名; 运维/连接 → domain 归 network/db
    if any(k in text[:6] for k in ["中文", "交互", "模型", "措辞", "偏好"]):
        return "偏好", "preference"
    if "DB " in text or "Redis" in text or "mysql" in text:
        return "运维", "database"
    if "VPN" in text or "tun0" in text or "路由" in text:
        return "运维", "network"
    if "GitHub" in text or "DataDome" in text or "风控" in text:
        return "运维", "security"
    return "运维", "general"


def main():
    total_ok = 0
    for fname in ["MEMORY.md", "USER.md"]:
        sections = read_sections(os.path.join(MEM_DIR, fname))
        if not sections:
            print(f"{fname}: 空/不存在"); continue
        print(f"--- {fname}: {len(sections)} 条 ---")
        for i, sec in enumerate(sections):
            # 首行作为 name(截断)
            first_line = sec.split("\n")[0].strip()[:40]
            name, domain = classify(sec)
            if len(first_line) <= 6:
                name = first_line or name
            else:
                name = f"{name}-{first_line[:20]}"
            h = hashlib.sha1(sec.encode()).hexdigest()[:12]
            eid = f"hermes-{fname[:-3].lower()}-{i+1:02d}-{h}"
            if memorize(eid, name, sec, domain):
                total_ok += 1
    print(f"\n完成: {total_ok} 条写入 KG (project={PROJECT}, world=knowledge)")

if __name__ == "__main__":
    main()
