# Class 描述补偿（Phase 2.6）+ Cypher 写查询的坑（2026-08-12）

## 背景
im-center 357 个 Class 节点 description 全空（llm_analysis 只对 Method 做 Phase2）。
新增 Phase 2.6 类描述补偿：构建末尾扫描无描述的 Class → LLM 生成 → 写回。

## 机制（src/application/build/pipeline.rs backfill_class_descriptions）
- **状态机（与方法 Phase 2 对齐）**：`llm_status` 缺失 或 = `failed` → 缺口，本轮处理；
  成功 → `SET n.description=$d, n.llm_status='success'`；失败（chat Err/空响应）→ `SET n.llm_status='failed'`（下次构建自动重试）。
- **完全增量**：只处理缺口；源码有 javadoc 的类（description 非空）天然跳过；成功节点幂等。
- **分批**：扫描 `LIMIT 300` 一轮，剩余缺口下轮自动续跑（实测第一轮 300，第二轮补完 57）。
- **并发**：与 Phase 2 同用 `phase2_concurrency`（= provider max_concurrent，当前 24），buffer_unordered 流式。
- **LLM 重试**：LLM_CALL_MAX_ATTEMPTS=6、1s 延迟，与方法 Phase 2 同策略。
- 类体切片用 `slice_method_body`（<10 字符回退整文件），空类体标 failed 避免每轮重扫。
- 触发开关：`--no-llm-backfill` 可关（与 Phase 2.5 补偿共用）。

## ⚠️ 坑 1：增量构建提前返回，Phase 2.6 不执行！
`src/interfaces/cli/build.rs:637`：`if files_to_process.is_empty() { return Ok(()); }`
——增量构建"0 个文件有步骤待执行"时**直接返回，pipeline.execute 的 Phase 2/2.5/2.6 全都不跑**。
- 现象：`dt build --name X` 报"所有文件均为最新"，类描述永远不生成。
- 解法：用 `--file <某文件>` 触发单文件处理（会走到 pipeline 末尾，Phase 2.6 对全项目 Class 扫描），
  或 `--full` 全量重建。验证 Phase 2.6 用 `dt build --name X --file <路径>` 最快。

## ⚠️ 坑 2：read_query 返回格式 — 必须用 AS 重命名！
bolt 驱动 `row.to::<serde_json::Value>()` 返回的 key **带别名前缀**：
`RETURN c.class_id` → key 是 `"c.class_id"`（不是 `"class_id"`）！
- `row.get("class_id")` 和 `row.get(0)`（位置索引）都取不到 → jobs 空 → 报"无缺口（全部已有描述）"假象。
- **正确姿势**（与 search_config.rs/search_memory.rs 同款）：
  `RETURN c.class_id AS class_id, c.name AS name, ...` 然后 `row.get("class_id")`。
- 教训：写任何 read_query 都要 AS 重命名，字段名访问；不要用位置索引。

## ⚠️ 坑 3：default/ 目录污染（Hermes profile 导出被当源码索引）
用户把 `~/.hermes` profile 导出（sessions/skills/SOUL.md，40MB）放进项目目录
（如 `uvp-im-center/default/`），scanner 会当源码索引：
- 现象：daemon 日志出现 `default/skills/apple/...` 等 Python 文件被 LLM 分析，
  Phase 2 拖慢 10+ 分钟；Memgraph 混入 `_undeclared_axes`/`test_*` 等 Python 方法。
- 排查：`ps aux | grep "dt build"` 看是否在跑；daemon 日志 `tail -f /var/log/digital-twin/dt.log` 看处理文件。
- 修复：`~/.config/digital-twin/config.yaml` scanner.ignore_dirs 加 `default`；
  已污染的用 python neo4j 直连删除（`n.file_path CONTAINS 'default/'`）+ Qdrant
  FilterSelector(MatchText(text='default/')) 删除（⚠️ MatchText 参数名是 `text` 不是 `value`）。

## MCP run_cypher_query 只读模式限制
- 对 `NOT STARTS WITH`/`NOT CONTAINS` 误判为写操作（报 "Write operations are not allowed in read-only mode"）。
- 解法：过滤用 `NOT b.name IN [...]` 白名单排除（实测可用），或 python neo4j 驱动直连
  `bolt://127.0.0.1:7688` 执行复杂查询。

## 实测验证（im-center）
- 357/357 Class 全部生成描述（llm_status=success，0 空描述），两轮增量续跑验证通过。
- 描述质量样例：SourceHolder → "管理线程本地字符串变量。用可传递线程局部存储字符串" ✓
- Qdrant MatchText 过滤：`qm.FieldCondition(key="file_path", match=qm.MatchText(text="default/"))`
