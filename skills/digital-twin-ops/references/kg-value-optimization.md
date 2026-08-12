# KG 价值优化 + 残留清理 + 索引对账（2026-08-12）

审计结论（`reports/2026-08-12-hermes-kg-usage-audit.md` 综合报告）: Hermes 做代码问答时 KG 已能用（修复后 3/3 会话主动 dt_search_kg），但**能力远超使用**。本文档记录三类可复用操作。

## 1. KG 价值缺口（审计发现，行动前先读）

- **CALLS 关系可替代大量 read_file**: im-center 有 8903 条 CALLS 边。一条 2-hop 遍历即可复现 agent 花 8-10 次 read_file 梳理的调用链:
  ```cypher
  MATCH p=(a:Method)-[:CALLS*1..2]->(b) WHERE a.class_name='GroupController' AND a.name='groupMsgRecall' RETURN p
  ```
  ⚠️ **必须过滤噪音**: 未过滤时结果被 toString/success/fail/getUrl/getter 淹没（实测 LIMIT 60 里 toString 占 30+）。过滤规则: 排除 getter/setter/`toString|success|fail|error|info|printStackTrace|valueOf|equalsIgnoreCase`，或加 `b.class_name <> a.class_name` 只看跨类调用。
- **calls 列表随 L1 返回但被浪费**: dt_search_kg 的 hits 里 `calls` 字段已含跨层调用（如 GroupService.groupMsgRecall 的 calls 含 groupMsgRecallUpdate）。agent 只当"位置确认"，未用于建链。L2 准则应写明"L1 命中后可选 L2 CALLS 遍历"。
- **沉淀闭环是最大缺口**: 业务分析结论（回调未接入、标记体系、错误兜底不对称）零沉淀。knowledge 世界节点全靠发送链路+群组管理。**修法**:
  ```bash
  dt learn "im-center 消息撤回链路" --project im-center \
    --entities "MessageController,GroupController,MessageService,GroupService" \
    --pattern "远端撤回成功→异步延迟→本地标记更新; 单聊MsgFlagBits=8/群聊IsPlaceMsg=2" \
    --pitfalls "AfterMsgWithdrawCallback 无消费方,撤回回调未接入; 群撤回失败无错误集合兜底" \
    --success true
  dt build --source knowledge   # 必做: 把新节点同步到 Qdrant 向量, 否则 knowledge 检索搜不到
  ```
  ⚠️ dt learn 写入后**必须** `dt build --source knowledge`（旧命令 `dt kg-sync` 已弃用，会打 WARN 但仍工作），否则检索仍命中旧缓存。标题/pattern 里带英文关键词（recall/withdraw）提高向量命中。
- **跨会话复用**: 同问题重复询问时 agent 会全量重查（memory 世界 0 命中）。可先 `dt learn` 沉淀，后续会话 L1 直接命中 pattern。

## 1b. 第三轮优化点（2026-08-12 导出分析 round3）

- **GAP-A（最有价值）: Class 无向量 → code 世界检索不到类**。Phase 2.6 已给 357 个类生成 description，但类**只写 Memgraph、没进 Qdrant 向量**——`dt search "SourceHolder" --world code --project im-center` 搜不到类（只命中同名方法 getAddSource/setAddSource）。类描述白生成，检索价值未兑现。**修法（未实施）**: Phase 2.6 成功后把类也 upsert 进向量（新建 `code_classes` 集合或复用 code_methods，payload: name/class_id/file_path/project/description，向量=description 文本）。验证: `dt search "SourceHolder" --world code --project im-center` 应命中 Class 实体。
- **dt learn 中英文关键词（2026-08-12 已实施，commit 86f5500）**: `learn.rs` 新增 `with_keywords`——summary 尾部自动追加"（keywords: recall, withdraw, ...）"（内置中英映射表: 撤回→recall/withdraw/revoke、消息→message/msg、群组→group、单聊→c2c/single 等 18 组）。中文任务写入的 knowledge 节点用英文查询词（recall/withdraw）也能向量命中。⚠️ 已写入的旧节点不受影响（summary 是写入时生成的），需重新 dt learn 或手动补。
- **插件 pytest（2026-08-12 已实施）**: `plugins/dt-sense/tests/test_dt_sense.py` 11 个测试（简报渲染 6 + 项目匹配 5），运行 `python -m pytest plugins/dt-sense/tests/test_dt_sense.py -v`（用 conda python，pytest 已装）。改插件 `__init__.py` 后必须跑。

## 2. 残留项目清理（误用目录名构建产生的死数据）

现象: `dt build --path <目录>` 用了未注册名 → Memgraph + Qdrant 都写了 project=<错误名> 的节点/向量，污染检索（`dt search` 命中项目分布里出现残留名）。

清理脚本（python3，neo4j 驱动 + qdrant-client）:
```python
from neo4j import GraphDatabase
from qdrant_client import QdrantClient
from qdrant_client.http import models as qm

PROJECT = "uvp-im-center"  # 残留项目名
driver = GraphDatabase.driver("bolt://127.0.0.1:7688")
with driver.session() as s:
    n = s.run("MATCH (n) WHERE n.project = $p RETURN count(n) AS c", p=PROJECT).single()
    print(f"Memgraph {PROJECT} 节点数: {n['c'] if n else 0}")
    s.run("MATCH (n) WHERE n.project = $p DETACH DELETE n", p=PROJECT)
driver.close()

qc = QdrantClient(url="http://127.0.0.1:6333")
for coll in [c.name for c in qc.get_collections().collections]:
    qc.delete(collection_name=coll,
              points_selector=qm.FilterSelector(filter=qm.Filter(
                  must=[qm.FieldCondition(key="project", match=qm.MatchValue(value=PROJECT))])))
qc.close()
```
⚠️ **Qdrant 删除必须用 `qm.FilterSelector`**（filter 包在 FilterSelector 里）。裸 dict `{"filter": {...}}` 会报 `Unsupported points selector type`。三个集合都要清: code_methods / doc_chunks / kg_nodes。

## 3. dt health 索引对账（2026-08-12 新增）

`dt health` 现含"索引对账"行: Memgraph Method 节点数 vs Qdrant code_methods 向量数。
- 一致 → `✅ 索引对账 : Memgraph N 方法 = Qdrant N 向量`
- 不一致 → `⚠️ ...（索引漂移，建议 --full 重建）` 且 all_healthy=false

⚠️ **漂移 ≠ 故障**: 另一终端并行 `dt build`（构建中，Memgraph 已写节点、Qdrant 向量未同步完）会产生瞬态漂移，数量差恰为构建中项目的方法数。判定前先 `ps aux | grep "dt build"` 排除并发构建。残留清理（第 2 节）后对账会恢复一致（实测 16366≠17920 → 清理后 16355=16355）。

## 4. 自测脚本（已入库，勿重建）

- `scripts/verify-kg.sh`: 用户自测 KG 全链路（索引状态/检索正确率/注释回归/索引对账/低分提示/分组展示/knowledge 世界），`bash scripts/verify-kg.sh [--full]`。
- `scripts/check-dt-usage.sh`: 审计 Hermes 会话是否用了 dt 工具（grep agent.log 统计 dt_sense/dt_search/cypher 调用次数 + 判定标准），`bash scripts/check-dt-usage.sh [小时数] [日志路径]`。
