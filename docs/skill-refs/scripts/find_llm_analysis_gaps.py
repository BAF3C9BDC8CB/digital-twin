#!/usr/bin/env python3
"""盘点 code_methods 中缺 llm_analysis 的方法点（只读，不写任何数据）。

背景: 搜索结果「分析: file: Ls-e」= llm_analysis 缺失（渲染回退 snippet 位置串）。
      本脚本回答 "缺口有多大、缺在哪、是没分析过还是被覆盖" —— 为增量修复提供清单。

用法:
    python3 find_llm_analysis_gaps.py                        # 全库盘点
    python3 find_llm_analysis_gaps.py --project message-center
    python3 find_llm_analysis_gaps.py --out /tmp/gap.json

关键陷阱: SQLite build_progress.file_path 存的是 prog_key = "method:{entity_id}"
          （不是源文件路径！）。按源文件路径 LIKE 关联恒 0 命中（2026-08-11 实测）
          —— 必须 strip "method:" 前缀后用 entity_id 与 Qdrant payload 关联。

输出:
    - 按项目: 总点 / 缺分析点 / 缺口率
    - 缺分析分类: 从未被 Phase 2 处理（无 build_progress 记录）vs
                 有记录但被覆盖（双写竞争, StoreProcessor 覆盖 llm_analysis）
    - JSON 报告（按 (project, file_path) 聚合, 含方法名）→ --out 指定
"""
import argparse
import json
import sqlite3
import sys
import urllib.request
from collections import Counter, defaultdict

QDRANT = "http://localhost:6333"
SNAPSHOTS_DB = "/var/lib/digital-twin/snapshots.db"
COLLECTION = "code_methods"


def scroll_collection(filt=None, limit=5000):
    pts, offset = [], None
    while True:
        body = {"filter": filt, "limit": limit, "with_payload": True, "with_vector": False}
        if offset:
            body["offset"] = offset
        req = urllib.request.Request(
            f"{QDRANT}/collections/{COLLECTION}/points/scroll",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=60) as r:
            d = json.load(r)
        pts.extend(d["result"]["points"])
        offset = d["result"].get("next_page_offset")
        if not offset:
            break
    return pts


def is_empty_analysis(pl):
    v = pl.get("llm_analysis")
    return v is None or (isinstance(v, str) and not v.strip())


def main():
    ap = argparse.ArgumentParser(description="盘点 code_methods 缺 llm_analysis 的方法点（只读）")
    ap.add_argument("--project", help="只看某项目（payload.project 精确匹配）")
    ap.add_argument("--out", default="/tmp/dt_llm_gap_report.json", help="JSON 报告路径")
    args = ap.parse_args()

    filt = None
    if args.project:
        filt = {"must": [{"key": "project", "match": {"value": args.project}}]}
    pts = scroll_collection(filt)
    total = len(pts)
    missing = [p for p in pts if is_empty_analysis(p["payload"])]

    # build_progress: stage='llm_analysis' 的记录; file_path 列实际是 method:{entity_id}
    conn = sqlite3.connect(SNAPSHOTS_DB)
    rows = conn.execute(
        "SELECT file_path FROM build_progress WHERE stage='llm_analysis'"
    ).fetchall()
    conn.close()
    analyzed_eids = set()
    for (fp,) in rows:
        analyzed_eids.add(fp[len("method:"):] if fp.startswith("method:") else fp)

    missing_eids = {p["payload"].get("entity_id") for p in missing}
    overwritten = missing_eids & analyzed_eids   # 有记录但点里没分析 → 被覆盖
    never = missing_eids - analyzed_eids         # 无记录 → 从未被 Phase 2 处理

    print(f"{COLLECTION} 总点数: {total}, 缺 llm_analysis: {len(missing)} "
          f"({len(missing) * 100 // total}%)")
    print(f"  从未被 Phase 2 处理(无记录): {len(never)} ({len(never) * 100 // max(len(missing),1)}%)")
    print(f"  有记录但被覆盖(双写竞争):   {len(overwritten)} "
          f"({len(overwritten) * 100 // max(len(missing),1)}%)")

    proj_total = Counter(p["payload"].get("project", "?") for p in pts)
    proj_missing = Counter(p["payload"].get("project", "?") for p in missing)
    print("\n=== 按项目: 总点 / 缺分析 / 缺口率 ===")
    for proj in sorted(proj_total, key=lambda x: -proj_total[x]):
        m = proj_missing.get(proj, 0)
        print(f"  {proj:24s} {proj_total[proj]:6d}  {m:6d}  {m * 100 // proj_total[proj]:3d}%")

    files = defaultdict(list)
    for p in missing:
        pl = p["payload"]
        files[(pl.get("project"), pl.get("file_path"))].append({
            "id": p["id"],
            "name": pl.get("name"),
            "start_line": pl.get("start_line"),
            "end_line": pl.get("end_line"),
            "entity_id": pl.get("entity_id"),
        })
    print(f"\n缺分析涉及文件数(去重): {len(files)}")

    out = {
        "total_points": total,
        "missing_points": len(missing),
        "never_processed": len(never),
        "overwritten": len(overwritten),
        "by_project": {k: v for k, v in sorted(proj_missing.items(), key=lambda x: -x[1])},
        "missing_by_file": {
            f"{proj}|{fp}": {"project": proj, "file_path": fp,
                             "methods": [m["name"] for m in ms]}
            for (proj, fp), ms in sorted(files.items(), key=lambda x: -len(x[1]))
        },
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
    print(f"\n报告已保存: {args.out}")
    print("\n增量修复提示: 删缺口文件在 file_snapshots 表的行 → 增量构建当新文件重新提取")
    print("→ Phase 2 自动补齐; 被覆盖点还需删 build_progress 对应 method:{entity_id} 行。")


if __name__ == "__main__":
    sys.exit(main())
