# 残留项目清理 + 索引对账（2026-08-12 实测）

## 场景

误用目录名构建（如 `dt build --path <dir>` 而非 `dt build --name <注册名>`）会产生**死项目数据**（Memgraph 节点 + Qdrant 向量），且持续污染检索（跨项目噪音）。本次案例：`uvp-im-center` 残留 411 个 Memgraph 节点 + 3 个集合的向量。

**判定残留**：`dt search` 结果里出现注册表外的项目名（如 uvp-im-center 而注册名是 im-center）。

## 清理脚本（Python，一次性）

```python
from neo4j import GraphDatabase
from qdrant_client import QdrantClient
from qdrant_client.http import models as qm

PROJECT = "uvp-im-center"
MG_URI = "bolt://127.0.0.1:7688"   # Memgraph bolt
QDRANT_URI = "http://127.0.0.1:6333"

driver = GraphDatabase.driver(MG_URI)
with driver.session() as s:
    s.run("MATCH (n) WHERE n.project = $p DETACH DELETE n", p=PROJECT)
driver.close()

qc = QdrantClient(url=QDRANT_URI)
for coll in [c.name for c in qc.get_collections().collections]:
    qc.delete(
        collection_name=coll,
        points_selector=qm.FilterSelector(
            filter=qm.Filter(
                must=[qm.FieldCondition(key="project", match=qm.MatchValue(value=PROJECT))]
            )
        ),
    )
qc.close()
```

**Qdrant 删除坑**：`points_selector={"filter": {...}}` 裸 dict 会报 `Unsupported points selector type: <class 'dict'>`——必须用 `qm.FilterSelector(filter=qm.Filter(must=[qm.FieldCondition(...)]))` 结构化对象。

## 索引对账（dt health 新功能）

`dt health` 增加索引对账行：`Memgraph N 方法 = Qdrant M 向量`，不一致标 ⚠ 并建议 --full 重建。

**对账不齐的三种原因判定**：
1. **残留数据**：Memgraph 多 → 有未清理的残留项目（用上述脚本清）
2. **并行构建瞬态**：另一个 `dt build` 进程正在跑（`ps aux | grep "dt build"` 确认），Memgraph 已写入但 Qdrant 未同步完 → **不是 bug，等构建完成再查**。案例：copartner-center 构建中，Memgraph 21069 = Qdrant 18652 + 2417（正好是 copartner-center 数）
3. **真漂移**：两者差 ≠ 任何在建项目数 → 需要 --full 重建

## 验证

清理后 `dt health` 对账应恢复一致（本次 16355 = 16355），且 `dt search` 不再出现残留项目名。可用 `dt search --project <项目名> --limit 5` 抽样复测（verify-kg.sh 已删）。
