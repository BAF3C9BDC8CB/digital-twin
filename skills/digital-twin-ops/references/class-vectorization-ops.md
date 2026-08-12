# Class 向量化回填：增量构建语义 + 团队审查修复汇总（2026-08-12）

## 增量构建自动回填（无需全量构建）

用户问题："GAP-A 后旧项目是否需要全量重建？" → **不需要，普通 `dt build` 即可**。

CLI 构建路径：`dt build` → BuildCommand::run (builder.rs) → BuildServiceImpl::build (service.rs) →
`PipelineTemplate::execute()` (pipeline.rs:133) → Phase 2.6 `backfill_class_descriptions` 无条件执行
（llm_backfill 默认开，`--no-llm-backfill` 可关）。

⚠️ **build.rs:637 的提前 return 是误报源**：它属于 `analyze_batch` 入口（engine.rs 处理器模型），
**不是 CLI 构建路径**。排查"为什么旧项目不向量化"时先确认走的是哪条入口，别被该 return 误导。

Phase 2.6 扫描条件（pipeline.rs:1491）：
```cypher
WHERE c.project = $project
  AND ((c.description IS NULL OR c.description = '')
       OR c.vectorized IS NULL OR c.vectorized = false)
  AND (c.llm_status IS NULL OR c.llm_status <> 'failed')
```
- 旧项目类：有 description 但 `vectorized IS NULL` → 被捞起 → **跳过 LLM 直接 embed+upsert**（免费向量化）
- 无 description 的类（如 digital-twin-v2 自身 330 类）→ LLM 生成描述再向量化（耗时，受并发限流）
- 实测：digital-twin-v2 一次普通增量构建 0→330 全部向量化，330/330 llm success

## 团队审查修复的 7 项（P0/P1/P2/P3）

| 级别 | 问题 | 修复 |
|------|------|------|
| P0-1 | vectorized=true 与 upsert 脱钩（upsert 失败也标记） | 标记移入 upsert Ok(()) 分支 |
| P0-2 | LLM 分支 Memgraph 写回失败被 `let _ =` 忽略 | 写回失败 → return（不继续 embed+标记） |
| P0-3 | classes 向量通道缺 project/min_score/name/exact 过滤 | is_classes 分支补与 rerank 相同的基础过滤 |
| P1-4 | 精确类名检索失效（1A 跳过 code_classes） | is_classes 用单向量版 `search_with_filter` + 关键词兜底循环两集合 |
| P1-5 | failed 类同构建内无限重试（最多 120 次 LLM/类） | 扫描加 `AND c.llm_status <> 'failed'` |
| P2-7 | 已有描述类也读源码 | 源码读取移入 else 分支（已有描述直接走向量化） |
| P3-10 | full_rebuild 清理漏 code_classes | 清理循环加 CODE_CLASSES |

验证：`dt search "GroupController" --world code --project im-center` → [Class] 0.950 首条置顶（P1-4 修复后）。

## Builder 噪声（待优化，2026-08-12 发现）

im-center 2287 方法中 **93 个 builder（4.1%）** 是 Lombok `@Builder` 样板，Phase 2 LLM 对其生成
无意义描述（"用途：暂无"类），污染语义检索 + 浪费 ~18.6K token/构建。
优化方案（未实施）：Phase 2 检测 `name=="builder"` → 跳过 LLM 写 `llm_status='skipped_builder'`；
检索侧对样板方法降权。详见 reports/2026-08-12-export-analysis-round3-builder-noise.md。

## 大集合滚动注意

Qdrant scroll 单次 limit 默认截断（5000 点内），跨项目统计要用 `scroll_filter`（FieldCondition
project match）而非全量 scroll 后过滤——否则深处项目的点会被漏掉（im-center 实测首轮 0 命中）。
