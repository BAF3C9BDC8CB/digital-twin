# llm_analysis 缺口盘点与增量修复（2026-08-11）

问题形态：搜索结果 `[代码/Method]` 的「分析:」行显示 `file: Ls-e` 位置串而非「用途：/逻辑：」= 该点 `llm_analysis` 为空（渲染回退 snippet）。用户要求"不清空所有数据、增量修复"——先盘点缺口。

## 盘点方法（全部只读）

1. Qdrant scroll `code_methods`（REST :6333，with_payload=true, with_vector=false，分页 next_page_offset）取全量点。
2. 空判定：`llm_analysis` 为 None 或空白字符串。
3. SQLite `/var/lib/digital-twin/snapshots.db`：`SELECT file_path FROM build_progress WHERE stage='llm_analysis'`。
4. **⚠️ 关键陷阱：`build_progress.file_path` 存的是 prog_key = `method:{entity_id}`，不是源文件路径！** 按源文件路径 LIKE 关联 → 恒 0 命中（2026-08-11 实测，第一版脚本因此误判"0 个被覆盖"）。必须 strip `method:` 前缀后用 entity_id 与 Qdrant payload 关联。
5. 分类：缺分析点 ∩ 有记录 = 被覆盖（双写竞争）；缺分析点 − 有记录 = 从未被 Phase 2 处理。

可直接跑 `scripts/find_llm_analysis_gaps.py`（--project 过滤 / --out 报告路径），输出按项目统计 + 分类 + 按文件聚合 JSON 报告。

## 2026-08-11 实测基线（全库）

- code_methods 4695 点，缺分析 2282（48.6%）；缺口文件 565 个。
- 按项目：message-center 1735/1573（90%，重灾区）、hospital-center 733/424（57%）、pay-center 1404/180（12%）、archive-api 644/78（12%）、copartner-h5 116/18（15%）、doctor-center 63/9（14%）。
- 缺分析 2282 = 从未处理 1658（73%）+ 有记录被覆盖 624（27%）。
- 全库 llm_analysis 记录 3037 条 vs 实际含分析点 2413 → 差 624 = 双写竞争实锤（见 SKILL.md「双写竞争缺陷」一节）。

## 根因机制（为什么增量构建永远修不好）

- Phase 2（build/pipeline.rs ~L356）只遍历 `extraction.methods` = **本次变更文件**提取的方法；未变更文件永不进 Phase 2 → 缺的分析永不补齐。
- `is_llm_analyzed` / `mark_llm_analyzed`（sqlite/repo.rs:254-294）用 prog_key（`method:{entity_id}`，Nacos 用 `nacos:{data_id}`）+ source_text SHA256 判定。
- 结论：**只有让文件"看起来变更"才能触发重分析**。

## 增量修复路径（2026-08-11 提出，待用户批准后实施）

0. **先修双写竞争**（StoreProcessor upsert 前合并已存在 llm_analysis，或调 Phase 2 顺序）——否则补完又被覆盖，白补。
1. 删缺口文件在 `file_snapshots` 表的行（`DELETE FROM file_snapshots WHERE project=? AND file_path IN (...)`）→ 增量构建把文件当"新文件" → 重新提取方法 → Phase 2 对无记录方法逐个 LLM 分析 → upsert 补齐。
2. 对 624 个被覆盖点：同时删 `build_progress` 中对应行（`DELETE FROM build_progress WHERE stage='llm_analysis' AND file_path LIKE 'method:dt://entity/<proj>/%'` 且 entity_id 在缺口清单）。
3. 重跑 `dt build --path <proj>`（普通增量，无需 --full）；验证：scroll 缺分析点数归零 + `dt search "..." --world code` 显示"用途：/逻辑："。

规模参考：2282 方法，opencode-go 64 并发下 Phase 2 约 10-20 分钟（参照 2026-08-11 全量 1607 次分析实测）。注意与「KG reset & rebuild」区分：这是**增量补缺**，Qdrant/Memgraph 其他数据全部不动。
