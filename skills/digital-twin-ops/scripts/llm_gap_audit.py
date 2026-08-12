#!/usr/bin/env python3
"""llm_analysis 缺口只读审计 — Qdrant code_methods 空分析点 × SQLite build_progress 交叉比对.

用法:
  python3 llm_gap_audit.py [--project <name>] [--out /tmp/dt_llm_gap_report.json]

只读:不写 Qdrant / SQLite / Memgraph。输出:
  - 全库(或指定项目)缺分析点统计:总点数、缺点数、缺口率
  - 交叉分析:有 llm_analysis 记录但仍缺(覆盖/失效) vs 无记录(从未被 Phase 2 处理)
  - 按 (project, file_path) 归组的缺口文件清单(含方法名),保存到报告 JSON

⚠️ 关键陷阱:SQLite build_progress 表 stage='llm_analysis' 行的 file_path 字段实际存的是
   `method:{entity_id}`(prog_key),不是源文件路径 —— 交叉比对必须按 entity_id 关联,
   不能按源文件路径 LIKE 匹配(永远空 → 误判「从未分析」)。
背景详见 digital-twin-ops 技能「llm_analysis 缺口盘点方法」节。
"""
import json
import sqlite3
import sys
import urllib.request
from collections import Counter, defaultdict

QDRANT = "http://localhost:6333"
SNAPSHOTS_DB = "/var/lib/digital-twin/snapshots.db"
COLLECTION = "code_methods"


def scroll_collection(collection, filt=None, limit=5000):
    """Qdrant 分页 scroll,返回全部点(payload + id,无 vector)。"""
    pts, offset = [], None
    while True:
        body = {"filter": filt, "limit": limit, "with_payload": True, "with_vector": False}
        if offset:
            body["offset"] = offset
        req = urllib.request.Request(
            f"{QDRANT}/collections/{collection}/points/scroll",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req) as r:
            d = json.load(r)
        pts.extend(d["result"]["points"])
        offset = d["result"].get("next_page_offset")
        if not offset:
            break
    return pts


def is_empty_llm(pl):
    v = pl.get("llm_analysis")
    return v is None or (isinstance(v, str) and not v.strip())


def main():
    args = sys.argv[1:]
    proj_filter = None
    out_path = "/tmp/dt_llm_gap_report.json"
    i = 0
    while i < len(args):
        if args[i] == "--project" and i + 1 < len(args):
            proj_filter = args[i + 1]
            i += 2
        elif args[i] == "--out" and i + 1 < len(args):
            out_path = args[i + 1]
            i += 2
        else:
            i += 1

    pts = scroll_collection(COLLECTION)
    if proj_filter:
        pts = [p for p in pts if p["payload"].get("project") == proj_filter]
    total = len(pts)
    missing = [p for p in pts if is_empty_llm(p["payload"])]
    pct = len(missing) * 100 // max(total, 1)
    label = f"({proj_filter})" if proj_filter else ""
    print(f"code_methods{label}: 总点 {total}, 缺 llm_analysis {len(missing)} ({pct}%)")

    # build_progress 记录 → entity_id 集合(去 method: 前缀)
    conn = sqlite3.connect(SNAPSHOTS_DB)
    rows = conn.execute(
        "SELECT file_path FROM build_progress WHERE stage='llm_analysis'"
    ).fetchall()
    conn.close()
    prog_keys = {
        fp[0][len("method:"):] if fp[0].startswith("method:") else fp[0] for fp in rows
    }

    missing_eids = {p["payload"].get("entity_id") for p in missing}
    overwritten = missing_eids & prog_keys
    never = missing_eids - prog_keys
    print(
        f"缺口交叉: 有 llm_analysis 记录但仍缺(覆盖/失效) {len(overwritten)}, "
        f"无记录(从未被 Phase 2 处理) {len(never)}"
    )

    # 按项目统计
    by_proj = Counter(p["payload"].get("project", "?") for p in missing)
    for proj, n in sorted(by_proj.items(), key=lambda x: -x[1]):
        print(f"  项目 {proj:22s} 缺 {n:5d}")

    # 按文件归组
    files = defaultdict(list)
    for p in missing:
        pl = p["payload"]
        files[(pl.get("project"), pl.get("file_path"))].append(pl.get("name"))
    print(f"缺分析涉及文件(去重): {len(files)}")
    for (proj, fp), ms in sorted(files.items(), key=lambda x: -len(x[1]))[:30]:
        print(f"  {len(ms):4d} methods | {proj} | {fp}")

    report = {
        "total_points": total,
        "missing_points": len(missing),
        "with_record_still_missing": len(overwritten),
        "never_processed": len(never),
        "by_project": dict(sorted(by_proj.items(), key=lambda x: -x[1])),
        "missing_by_file": {
            f"{proj}|{fp}": {"project": proj, "file_path": fp, "methods": ms}
            for (proj, fp), ms in sorted(files.items(), key=lambda x: -len(x[1]))
        },
    }
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=1)
    print(f"报告已保存: {out_path}")


if __name__ == "__main__":
    main()
