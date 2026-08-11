# Phase 2 LLM 分析失败自愈 — 执行 Spec（2026-08-11 用户拍板版）

> 用户最终拍板的架构方向，团队必须严格执行，不得跑偏。
> 状态：实施中。Worker A（基础设施层）→ Worker B（构建层）→ Worker C（搜索/展示层）。

## 0. 背景与病根

- Phase 2 LLM 方法分析失败后只打 warn，不写任何持久化状态（pipeline.rs L537-543）
- 增量构建只处理 hash 变化的文件 → 失败方法永不重试 → llm_analysis 永久缺失
- "降级"机制（llm_client.rs:157 空图占位、pipeline.rs:452-456 空响应假成功 mark）导致有效/无效数据无法区分 → 只能全删重建
- 现状：code_methods 4695 点中 2282 缺 llm_analysis（48.6%），用户决定全量删除重建

## 1. 三层架构（用户拍板，不可偏离）

```
基础层（确定性，不依赖 LLM，必须全部成功）
  AST 提取 → 图谱关系 → embed(signature+comment) = base 向量 → 方法点入库 → 快照
  事实来源：file_snapshots + Qdrant 点存在

LLM 增强层（渐进式，逐步补）
  成功 → llm_analysis 文本 + llm 向量（embed 分析文本）+ llm_status=success
  失败 → llm_status=failed + llm_retries++（重试 3 次，之后暂停但保留状态）
  🚫 无降级：绝不写空串/占位/空图伪装成功

状态位（Qdrant payload，用户拍板）
  llm_status: "success" | "failed" | (缺失=未处理)
  llm_analysis: 文本（仅 success 时有）
  llm_retries: 失败重试计数

检索（用户拍板）
  base 向量只做召回；llm 向量做 rerank（Qdrant named vectors 双向量）
```

## 2. 数据模型变更

### 2.1 Qdrant code_methods 集合 → named vectors

```
vectors:
  base: {size: VECTOR_DIM, distance: Cosine}   # embed(signature+comment)，Phase 1 写，永不覆盖
  llm:  {size: VECTOR_DIM, distance: Cosine}   # embed(llm_analysis)，Phase 2 成功才写，可选
```

- VECTOR_DIM 不变（1024）
- 存量数据全量删除重建（用户已拍板），无需迁移
- upsert 时 base 必填、llm 可选（NamedVectors）

### 2.2 payload 状态位

```json
{
  "name": "...", "signature": "...", "class_name": "...", "file_path": "...",
  "package_or_module": "...", "language": "...", "project": "...",
  "start_line": N, "end_line": N, "params": "...", "return_type": "...",
  "calls": [...], "comment": "...", "entity_id": "dt://entity/...",
  "llm_status": "success" | "failed",      // 缺失 = 未处理
  "llm_analysis": "用途：...\n逻辑：...",   // 仅 success 时存在
  "llm_retries": 0                          // failed 时计数
}
```

## 3. 构建层改造（Worker B：build/pipeline.rs）

### 3.1 Phase 1（步骤 7b）——基础向量 + 状态初始化

- 向量：embed(signature + comment) → named vector "base"（现有 L215 逻辑不变，只是包一层 named vectors）
- payload：现有字段 + 无 llm_status（=未处理态），**不写 llm_analysis**
- 方法点 upsert 成功 → 这就是"已索引"事实来源

### 3.2 Phase 2 —— 无降级 + 状态位 + llm 向量

对每个方法（现有 jobs 循环 L361-400 不变）：
- chat 成功且响应非空：
  - embed(llm_response) → named vector "llm"
  - set_payload（或带 llm 向量的 upsert）：llm_analysis=响应, llm_status=success
  - mark_llm_analyzed(prog_key, hash)（沿用）
- chat 失败 / 响应为空串（**空响应 = 失败，不再假成功**）：
  - set_payload：llm_status=failed, llm_retries=原值+1（读取已有 llm_retries，无则 1）
  - **不 mark_llm_analyzed**（保持可重试）
  - 重试策略：llm_retries >= 3 时本轮跳过（状态保留 failed，数据可辨识）
- 删除现有"空响应也 mark"逻辑（L452-456 → 516 假成功路径）

### 3.3 补偿自愈（构建末尾，Phase 2 await 之后、BuildReport 之前）

```
1. scroll_points(code_methods, filter={project=X AND llm_status is_empty} OR {llm_status=failed AND llm_retries<3}, limit=200)
   —— 注意 is_empty 只匹配缺失键；failed 需用 match 值过滤。两个条件 OR。
2. 对每个缺口点：读源文件（file_path + start/end 行号切方法体，<10 字符回退整文件，同 Phase 2 策略）
3. chat → 成功：set_payload(llm_analysis, llm_status=success) + mark_llm_analyzed
        失败：set_payload(llm_status=failed, llm_retries+1)（不 mark）
4. 源文件不存在 → 跳过 + warn（不阻塞构建）
5. 汇总日志：补偿 N 缺口, M 成功, K 失败
```

- 限批 LLM_BACKFILL_BATCH=200/轮；多轮收敛
- 开关：`--no-llm-backfill`（CLI 透传）；默认开
- 只读不写 Memgraph；Qdrant 只 set_payload + 可能带 llm 向量的 upsert

### 3.4 其他

- 空响应 bug 修复后，build_progress 中旧 `llm_analysis=""` 假完成记录会在全量重建时自然清除（clear_llm_progress）
- `--full` 语义不变（clear_llm_progress 后全量重跑 Phase 2）

## 4. 搜索/展示层改造（Worker C：search_mcp.rs + search_render.rs）

### 4.1 召回（search_mcp.rs search_code ~L442-517）

- 向量通道：base 向量召回（search_with_filter / search 用 base）
- 保持：精确通道（name match）、关键词兜底通道不变

### 4.2 rerank（新增）

- 召回候选后，若点有 llm 向量：用 llm 向量对 query 向量二次打分（rerank），融合进最终 score
- 无 llm 向量的点：保持 base 分数
- 实现位置：search_mcp.rs 向量通道结果处理后；或独立 rerank 函数
- 注意不要破坏 RRF 融合（fusion.rs 按 world:id 去重）

### 4.3 渲染（search_render.rs render_hit）

- Method 命中：
  - llm_analysis 非空 → `分析: 用途：...\n逻辑：...`（现状）
  - llm_status=failed 或缺失 → `分析: 暂无 LLM 分析`（**不再显示 file:Ls-e 位置串**）
  - 位置行单独显示：`位置: file:Ls-e [signature]`（现有逻辑保留）
- 保留 score/类型/摘要/来源 契约元素（用户 contract-level）

## 5. 测试计划

- Worker A 单测：filter is_empty/is_null 翻译、noop scroll_points/set_payload
- Worker B 单测：backfill_disabled_skips_scroll、backfill_success_marks_analyzed、backfill_failure_leaves_gap（chat Err → 不 mark）、empty_response_marks_failed（空串 → failed 不 mark）、retry_limit_skips_after_3
- Worker C 测试：rerank 分数融合、渲染"暂无 LLM 分析"分支
- 回归：cargo test 全量（现有 667 个须绿）+ golden 12 条（基线当前 3/12=25%，因数据被清；重建后应回到 11-12/12）
- e2e：全量删除 → 全量重建 → 缺口率 0% → 模拟 LLM 失败（断网/坏 key）→ 下次构建自动补齐 → 状态位可查

## 6. 数据处置（用户已拍板）

- 实施完成并验证后：删除全部存量数据（Qdrant 3 集合 + Memgraph + SQLite）→ 全量重建 65 项目
- 重建途中的 LLM 失败会被补偿机制自动补齐（实测自愈闭环）
- 验证：重建后 llm_analysis 缺口率 → 0；golden 12/12

## 7. 文件改动清单（按 worker 隔离，避免冲突）

| Worker | 文件 | 内容 |
|--------|------|------|
| A | src/infrastructure/qdrant/repo.rs | named vectors、set_payload、scroll_points、is_empty 过滤 |
| A | src/domain/traits.rs | VectorRepository + scroll_points/set_payload 默认实现 |
| A | src/shared/collections.rs | VECTOR_DIM 常量确认（如需 named vectors 配置） |
| B | src/application/build/pipeline.rs | Phase 1 base 向量、Phase 2 无降级+状态位、补偿自愈 |
| B | src/main.rs + src/interfaces/cli/build.rs | --no-llm-backfill 透传 |
| C | src/application/context/search_mcp.rs | base 召回 + llm rerank |
| C | src/interfaces/cli/search_render.rs | 状态位渲染"暂无 LLM 分析" |

## 8. 验证命令

```bash
cd /data/myProject/digital-twin-v2
cargo check --release
cargo test --release --lib          # 单测
# e2e（数据重建后）
curl -s -X POST http://127.0.0.1:6333/collections/code_methods/points/count -H 'Content-Type: application/json' \
  -d '{"exact":true,"filter":{"must":[{"key":"llm_analysis","is_empty":true}]}}'   # 应为 0
dt search "sendSmsCode" --world code --project message-center   # 应显示"用途：/逻辑："或"暂无 LLM 分析"
```
