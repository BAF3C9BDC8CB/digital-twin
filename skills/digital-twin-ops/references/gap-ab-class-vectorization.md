# GAP-A/B 落地记录（2026-08-12）

commit: `feat: GAP-A/B 落地 — 类向量化 + 补偿循环`（3 files: pipeline.rs / search_mcp.rs / collections.rs）

## GAP-A: Class 向量化

类实体（Class）此前只有 Memgraph 节点无 Qdrant 向量，`dt search --world code` 搜不到类。
Phase 2.6 类描述补偿生成 description 后，类描述文本白生成、检索价值未兑现。

**实现**：
1. `src/shared/collections.rs` 新增 `CODE_CLASSES = "code_classes"`
2. Phase 2.6 `backfill_class_descriptions` 成功后 upsert 类向量：
   - payload: `entity_type=Class` + `llm_analysis`(描述) + `file_path` + `project` + `name`
   - upsert 成功后 `SET c.vectorized = true` 防重扫
   - 已有描述的类（existing_desc 非空）走回填路径：跳过 LLM 直接 embed+upsert
3. `search_code` 合并查询 `code_methods` + `code_classes`

**集合配置契约（关键）**：
- code_classes = **单向量** size=1024（ensure_collection 非 CODE_METHODS 默认单向量）
- code_methods = **named 双向量**（base + llm）
- 按集合区分查询：单向量用 `vector.search`，named 用 `vector.search_named`；1A 精确通道同理：classes 用单向量版 `search_with_filter`，methods 用 `search_named_with_filter`
- upsert 单向量集合用 `"vector": [...]`，**不能用** `"vectors": {"base": [...]}`

**⚠️ 踩坑**：
- Qdrant 单向量集合用 named 查询/写入 → "Not existing vector name" 400
- **vectorized 标记时机（团队审查 P0，2026-08-12）**：`SET c.vectorized=true` 必须在 upsert **成功后**执行。曾实现为 embed_batch 成功后即标记（在 upsert match 外部），导致 upsert 失败也标记 → 假成功（下轮不重试，类向量永久缺失但对账看似一致）。修复：标记移入 upsert `Ok(())` 分支内。审查要点：状态位标记必须与写入结果绑定，先检查错误处理顺序再验收。
- 测试污染集合（python recreate_collection 创建 named size=4）需 `delete_collection` 后由 ensure_collection 重建，否则 upsert 全失败但被 `let _ =` 吞掉（无报错，vectorized 照常标记 → 假成功）
- Qdrant point id 必须是 u64 或 UUID；repo.upsert 层会自动把字符串 id hash 成 u64（DefaultHasher），但 python 直连 Qdrant 传字符串会 400
- `let _ =` 吞掉 upsert 错误 → 静默失败难排查。关键写入路径先加显式错误日志再验证

**entity_type 解析**：
`hit_from_payload` 从 payload 读 entity_type，方法点无该字段→默认 "Method"，类点=Class。

**检索实测**：
- "群组控制器 群组消息 管理入口" → GroupController [Class] 0.662 首条命中（最终版精确类名 'GroupController' → 0.950 置顶）
- "线程本地字符串管理" → SourceHolder [Class] 0.691
- 混合检索：类 + 方法按分数融合排序，正常工作

⚠️ **SKILL.md 超限维护说明（2026-08-12）**：digital-twin-ops 的 SKILL.md 已达 101K+（限制 100K），新知识点无法写入主体，只能放 references/。未来会话若需给 SKILL.md 加内容，先做一次瘦身（压缩 L22 read_file 段、L378 embed-server 段等历史记录为指针式，可净减 ~700+ 字符）。

## GAP-B: 补偿循环

此前 backfill_class_descriptions 每轮 LIMIT 300 只处理一批，大项目需手动多次构建。

**实现**：`loop` 包裹，每轮 LIMIT 300，直到：
- 无缺口（jobs.is_empty()）→ break
- 或 20 轮上限 → warn 停止
- 或本轮 succeeded==0（无可进展）→ break

`rounds` 累计，结尾日志 "Class 描述补偿总览: N 轮, X 成功, Y 失败"。

扫描条件：`(description IS NULL OR description='') OR vectorized IS NULL OR vectorized=false`
RETURN 加 `coalesce(c.description,'') AS desc_text`（回填分支用）。

**实测**：357 类三轮 300→57→0 自动补全，无需手动多次构建。

## 团队审查修复（deleg_83ef019e, 2026-08-12）

3 角色并行（代码审查/功能测试/KG 一致性）。功能测试+一致性 5/5 PASS；审查发现 7 项问题全部修复：

- **P0-1 vectorized 脱钩**：已在上文"踩坑"节。标记必须与 upsert 结果绑定。
- **P0-2 LLM 分支写回失败被忽略**：`SET c.description...` 用 `let _ =` 吞错 → 图缺 description 但被标记完成，图与向量永久不一致。修复：写回失败 `return (class_id, false)`（下轮重试），不继续 embed/upsert/vectorized。
- **P0-3 classes 向量通道缺过滤**：`is_classes` 分支曾直接 `reranked = results`，跳过 rerank 内部全部基础过滤 → 指定 project 时其他项目类泄漏 + 低分噪声挤占 limit。修复：classes 分支手工补 min_score/project/name/exact_ids 过滤（与 rerank 同款）。
- **P1-4 精确类名检索失效**：1A 精确通道曾跳过 classes（单向量无 named）→ "GroupController" 向量分不足时搜不到类（关键词兜底只在 all_hits < limit 时触发，方法命中填满后不兜底）。修复：1A 通道覆盖 classes（单向量版 `search_with_filter`，filter=name 精确匹配，命中 0.95 置顶）+ 关键词兜底循环两个集合。验证：'GroupController' → [Class] 0.950 首条。
- **P1-5 failed 类同构建内无限重试**：desc 空 + llm_status=failed 每轮命中 → 6 次 LLM 尝试 × 20 轮 = 120 次调用/类。修复：扫描条件加 `AND (c.llm_status IS NULL OR c.llm_status <> 'failed')`（failed 留给下次构建重试）。
- **P2-7 已有描述类也读源码**：desc 非空的类根本不需要源码。修复：`existing_desc` 非空直接走向量化分支（源码读取+LLM 移入 else）。
- **P3-10 full_rebuild 不清理 code_classes**：delete_by_filter 列表 `[KG_NODES, DOC_CHUNKS]` 未含 code_classes → 全量重建后已删类的旧向量点残留可被召回。修复：加 CODE_CLASSES。

**审查方法论教训**：委派任务书明确列出 5 个重点审查项（循环错误处理顺序/分支变量引用/调用点遗漏/解析边界/幂等性），子代理据此发现 P0 脱钩——任务书里预设"vectorized 标记在 upsert 成功后设置（即使 upsert 失败也标记？检查错误处理顺序）"直接指向了真实 bug。写任务书时把已知风险点作为问题抛给审查者，比泛泛"找 bug"有效得多。

## 验证命令

```bash
# 类可检索
dt search "群组控制器 群组消息" --world code --project im-center --limit 8 --json
# 点数对账
python3 -c "from qdrant_client import QdrantClient; qc=QdrantClient(url='http://127.0.0.1:6333'); print(qc.get_collection('code_classes').points_count)"
# Memgraph 对账
python3 -c "
from neo4j import GraphDatabase
drv = GraphDatabase.driver('bolt://127.0.0.1:7688')
with drv.session() as s:
    r = s.run(\"MATCH (c:Class) WHERE c.project='im-center' AND c.vectorized=true RETURN count(c) AS n\").single()
    print(r['n'])
drv.close()"
```
