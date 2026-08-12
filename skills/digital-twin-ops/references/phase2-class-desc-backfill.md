# Phase 2.6 类描述补偿 — 增量机制与踩坑（2026-08-12）

## 背景
用户要求所有实体类型（不只是 Method）都要有描述，且必须增量（不清数据重建）。
实现：`ClassBlock.description` 字段 + `ts_java.rs` 提取类级 javadoc + Phase 2.6 `backfill_class_descriptions`（pipeline.rs）。

## 状态机（与方法 Phase 2 完全对齐）
- `llm_status` 缺失 或 `= "failed"` → 缺口，本轮处理
- 成功 → `SET n.description = $d, n.llm_status = 'success'`
- 失败（chat Err / 空响应 / 类体 <10 字符）→ `SET n.llm_status = 'failed'`（下次构建自动重试）
- 完全增量：只处理缺口；源码有 javadoc 的类（description 非空）天然跳过

## 触发条件（关键坑！）
⚠️ **增量构建 `files_to_process.is_empty()` 时 build.rs 直接 `return Ok(())`**，Phase 2/2.5/2.6 全部不执行！
- 表现：`dt build --name im-center` 报"增量跳过 342 个文件, 0 个文件有步骤待执行"就结束，类描述永远不跑
- 绕过：用 `dt build --name im-center --file <任意文件>` 触发单文件处理 → pipeline 走完 → Phase 2.6 执行
- 或等有真实文件变更的构建

## 扫描查询必须 AS 重命名（2026-08-12 实测根因）
⚠️ Memgraph read_query（bolt 驱动 `row.to::<serde_json::Value>()`）返回的 key **带别名前缀**：
- `RETURN c.class_id` → JSON key 是 `"c.class_id"` 不是 `"class_id"`
- `row.get("class_id")` → None → jobs 空 → 误报"本项目无缺口"
- 修复：查询里显式 `RETURN c.class_id AS class_id, c.name AS name, ...`，解析用字段名（与 search_config.rs 同款）
- 现有代码全部用 AS 重命名模式（search_config.rs / search_memory.rs），新写查询必须跟随

## 限流语义
- `LIMIT 300` 每轮只处理 300 个类缺口 → 357 个类需 2 轮（增量续跑自动补完）
- 并发 = `self.phase2_concurrency`（= provider max_concurrent，当前 openai_compatible=24），buffer_unordered 流式
- LLM 调用失败重试：`LLM_CALL_MAX_ATTEMPTS=6` + 1s 延迟，全失败标 failed 可重试

## 验证
- im-center 357/357 类全部 description + llm_status=success（两轮增量）
- 描述质量实测：SourceHolder → "管理线程本地字符串变量。用可传递线程局部存储字符串。" ✓
- 插件 pytest 11/11；cargo test 678 passed（+2 类 javadoc 回归测试）

## 相关坑：项目内非源码目录污染
- 用户把 Hermes profile 导出（default/，40MB，含 skills/sessions）放进 uvp-im-center/ 项目目录 → scanner 当源码索引（50 个 Python 文件混入 Method 节点，Phase 2.5 疯狂补偿）
- 处理：ignore_dirs 加 `default`（~/.config/digital-twin/config.yaml）+ 清理 Memgraph（DETACH DELETE where file_path CONTAINS 'default/'）+ Qdrant（FilterSelector + MatchText，注意 MatchText 用 `text=` 字段不是 `value=`）
