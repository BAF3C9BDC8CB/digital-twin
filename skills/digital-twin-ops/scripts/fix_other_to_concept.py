#!/usr/bin/env python3
"""将 Memgraph+Qdrant 中 type=Other 的函数/代码结构类节点批量改为 Concept(双存储)。

背景:code_with_ast 提示词词表曾为 class|function|method(AST 风格),而封闭
EntityType 枚举无这些变体 → 重建时大量 function/class/command 等实体被归一为
Other。提示词+枚举修好后,存量脏节点用本脚本就地重分类,无需 4h+ 全量重建。
详见 SKILL.md「KG Entity Type Classification」小节。

用法(依赖 miniconda python 的 neo4j 驱动 + Qdrant REST :6333):
  python3 fix_other_to_concept.py          # 实际执行
  python3 fix_other_to_concept.py --dry-run  # 只统计,不写入

识别规则:type='Other' 且 (name 含 '::' 或 以 '()' 结尾 或 纯 snake_case)
且不含 '.'(排除 retrieve.rs 这类文件引用)。
实测(2026-08-07):146 个 Memgraph 节点 / 134 个 Qdrant 点被修正。
"""
import json
import sys
import urllib.request

from neo4j import GraphDatabase

DRY = "--dry-run" in sys.argv
MEMGRAPH = "bolt://localhost:7688"
QDRANT = "http://localhost:6333"


def qdrant_post(path, body):
    req = urllib.request.Request(
        f"{QDRANT}/{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    return json.loads(urllib.request.urlopen(req).read())


def main():
    d = GraphDatabase.driver(MEMGRAPH, auth=("memgraph", ""))
    with d.session() as s:
        rows = s.run(
            """
            MATCH (e:Entity) WHERE e.type='Other'
            AND (e.name CONTAINS '::' OR e.name ENDS WITH '()'
                 OR e.name =~ '^[a-z_][a-z0-9_]*$')
            AND NOT e.name CONTAINS '.'
            RETURN DISTINCT e.name AS n
            """
        ).data()
    names = [r["n"] for r in rows]
    print(f"待修正 name 数: {len(names)}")
    if DRY:
        print("dry-run 模式,不写入。前 20 个:")
        for n in names[:20]:
            print("  ", n)
        sys.exit(0)

    # 1) Memgraph
    with d.session() as s:
        for n in names:
            s.run(
                "MATCH (e:Entity) WHERE e.type='Other' AND e.name=$n "
                "SET e.type='Concept', e.labels=['Entity','Concept']",
                n=n,
            )
    print("Memgraph 已更新")

    # 2) Qdrant payload(滚动全量 kg_nodes,按 payload.type + name 匹配)
    ids, offset, nameset = [], None, set(names)
    while True:
        body = {"limit": 1000, "with_payload": True, "with_vector": False}
        if offset:
            body["offset"] = offset
        r = qdrant_post("collections/kg_nodes/points/scroll", body)
        for p in r["result"]["points"]:
            pl = p.get("payload", {})
            if pl.get("type") == "Other" and pl.get("name") in nameset:
                ids.append(p["id"])
        nxt = r["result"].get("next_page_offset")
        if not nxt:
            break
        offset = nxt
    print("Qdrant 匹配点:", len(ids))

    if ids:
        r = qdrant_post(
            "collections/kg_nodes/points/payload",
            {"payload": {"type": "Concept"}, "points": ids},
        )
        print("Qdrant 更新:", r.get("status"), r.get("result", {}).get("status"))
    d.close()


if __name__ == "__main__":
    main()
