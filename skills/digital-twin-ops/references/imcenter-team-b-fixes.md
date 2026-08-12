# im-center 团队测试 → 团队 B 建议落地（2026-08-12）

背景：im-center（腾讯云 IM REST API 封装网关, 2287 方法/357 类）团队测试两轮后,
团队 B 的 5 条建议全部落地。本文件记录**可复用的实现方式与踩坑**, 与 SKILL.md
正文的 `TsJavaParser 注释错位 bug` 段配合使用。

## 1. 团队 B 建议 → 代码落地点（commit 7b91b96 + 45158fa）

| 建议 | 落地点 | 要点 |
|------|--------|------|
| dt_search_kg 加 world/project 参数 | `mcp/mcp-server.py` | schema + call_tool 各加 `world`(默认 knowledge 向后兼容) + `project` |
| 低分降级提示 | `src/interfaces/cli/search_render.rs` | `LOW_SCORE_THRESHOLD: f64 = 0.5`, hits 非空且 max_score < 0.5 时输出"⚠️ 结果可能不相关"提示（含 world 错配/跨项目噪音排查指引）。注意 `SearchHit.score` 是 **f64**（fold 初值用 `f64::NEG_INFINITY`） |
| 跨项目分组展示 | 同上 `render_human` | 按 `h.project` 分组输出 + "📦 命中项目分布: A / B（共 N 条）" 行；`HashMap<String, Vec<&SearchHit>>` 保持原顺序 |
| 索引对账巡检 | `src/interfaces/cli/cleanup.rs` `run_health` | `MATCH (m:Method) RETURN count(m)`（read_query 返回 serde_json, 用 pointer("/0/n") 取值）vs `collection_info(CODE_METHODS).points_count`；不等则标不健康 + 提示 --full 重建 |
| im-center 知识层补全 | `dt learn` + `dt build --source knowledge` | 见 §3 |

## 2. 残留项目清理（可复用脚本模式）

误用目录名构建（如 `dt build --path .../uvp-im-center --full` 而非注册名）会在
Memgraph+Qdrant 留下死数据, 污染跨项目检索。**无按项目删除的 CLI**, 用 Python 直连:

```python
# 依赖: neo4j + qdrant-client（本机已装）
from neo4j import GraphDatabase
from qdrant_client import QdrantClient
from qdrant_client.http import models as qm

PROJECT = "uvp-im-center"  # 要清理的项目名

driver = GraphDatabase.driver("bolt://127.0.0.1:7688")  # Memgraph 是 7688 不是 7687
with driver.session() as s:
    row = s.run("MATCH (n) WHERE n.project = $p RETURN count(n) AS c", p=PROJECT).single()
    print("nodes:", row["c"] if row else 0)
    s.run("MATCH (n) WHERE n.project = $p DETACH DELETE n", p=PROJECT)  # DETACH 连关系
driver.close()

qc = QdrantClient(url="http://127.0.0.1:6333")
for coll in [c.name for c in qc.get_collections().collections]:
    qc.delete(
        collection_name=coll,
        points_selector=qm.FilterSelector(
            filter=qm.Filter(must=[qm.FieldCondition(key="project", match=qm.MatchValue(value=PROJECT))])
        ),
    )
qc.close()
```

⚠️ Qdrant 删除 selector 必须是 `qm.FilterSelector` 对象, 传裸 dict
（`{"filter": {...}}`）会报 "Unsupported points selector type"。

清理后 `dt health` 的索引对账应恢复一致（本案例 16366≠17920 → 16355=16355,
对账功能自证有效）。

## 3. dt learn 补知识层 + 向量同步

团队 B 指出 im-center 在 knowledge 世界无业务知识（dt_search_kg 搜不到）。
补法（两主题, 各生成 Knowledge/Experience/Playbook 节点）:

```bash
dt learn "im-center 腾讯云IM消息发送链路" --project im-center \
  --entities "MessageController,MessageService,Message,ImClient" \
  --pattern "controller→service→core(ImClient)→腾讯IM REST→MongoDB落库" \
  --pitfalls "发送后需异步回查腾讯IM按msgKey匹配落库;撤回置MsgFlagBits=8" \
  --decisions "多租户按domainId路由sdkAppId/UserSig;UserSig本地HMAC-SHA256生成" \
  --success true
```

⚠️ 写节点后**必须** `dt build --source knowledge`（即旧 `dt kg-sync`, 已弃用）
把节点同步成向量, 否则 dt search knowledge 世界仍命中旧缓存/搜不到。
同步后检索分数 0.9+。

## 4. dt build 行为踩坑（调试/运维必读）

- **daemon 化执行**: `dt build` 会把任务交给 dt-daemon 执行, CLI 进程 stdout/stderr
  几乎为空; 真正日志在 `/var/log/digital-twin/dt-daemon.log`（JSON 行）。
  **eprintln! 调试日志看不到**——要看 daemon.log, 且代码级调试信息建议直接
  `tracing::info!` 而不是 eprintln。
- **CLI 参数陷阱**: `dt build <path>` 位置参数**不接受**（要 `--path`/`--name`）;
  `--json` **不接受**（报 unexpected argument）。
- **增量跳过**: 文件未变更时增量构建会"跳过 342 个文件, 0 个待执行", JavaParser
  完全不跑——验证解析器改动必须 `--full`。
- 并行 `dt build` 会互杀 Phase2（既有铁律, 详见 SKILL.md 正文）。

## 5. 全量构建后验证清单（快速判定构建是否正确）

1. `dt sense <path> --json` → stats.methods/classes/vectors 齐全
2. Memgraph: `MATCH (m:Method {project:'im-center'}) RETURN count(m)` = 2287
3. `dt health` → "✅ 索引对账: Memgraph X = Qdrant Y"
4. 注释抽查: `dt search "groupMsgGetSimple" --world code --project im-center --limit 1 --json`
   → comment 应为空（回归：不得含上方法 javadoc）
5. knowledge: `dt search "im-center 消息发送链路" --world knowledge --project im-center`
   → 命中 ≥1（dt learn 补的节点）
