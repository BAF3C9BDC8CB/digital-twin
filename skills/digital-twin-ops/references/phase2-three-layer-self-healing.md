# Phase 2 三层自愈架构（2026-08-11 用户拍板，已实施验证）

## 背景：为什么之前的方案被否决

团队最初方案（缺口补偿：以 Qdrant `llm_analysis is_empty` 为事实来源，构建末尾扫描补齐）被用户否决。
用户原话要点：「不能以 llm_analysis 缺失的点为事实来源」「应该将 LLM 和其他的逻辑分开」——
前期的向量、知识图谱关系建立**不应该依赖 LLM 分析**；LLM 失败不能影响基础数据完整性。
更深层动机：旧系统的"降级"机制（llm_client.rs:157 空图占位、pipeline.rs 空响应假成功 mark）
导致**有效/无效/未处理三种数据无法区分，只能全删重建**。用户要的是状态可辨识。

## 用户拍板的三层架构（严格执行，勿跑偏）

```
基础层（确定性，不依赖 LLM，必须全部成功）
  AST 提取 → 图谱关系 → embed(signature+comment) = base 向量 → 方法点入库 → 快照
  事实来源：file_snapshots + Qdrant 点存在（"方法已索引"唯一判定依据）

LLM 增强层（渐进式，逐步补）
  成功 → llm_analysis 文本 + llm 向量（embed 分析文本）+ llm_status=success
  失败 → llm_status=failed + llm_retries++（重试 3 次，之后暂停但保留状态）
  🚫 无降级：绝不写空串/占位/空图伪装成功

状态位（Qdrant payload，用户拍板放这里：状态跟着数据走，查询零成本）
  llm_status: "success" | "failed" | (缺失=未处理)
  llm_analysis: 文本（仅 success 时有）
  llm_retries: 失败重试计数

检索（用户拍板：base 只召回，llm 做 rerank）
  base 向量召回 → llm 向量 rerank（Qdrant named vectors 双向量）
```

## 数据模型变更

- Qdrant `code_methods` 集合 → named vectors：`{"base": {size:1024, Cosine}, "llm": {...}}`
- base 必填（Phase 1 写，永不覆盖）；llm 可选（Phase 2 成功才写）
- ⚠️ **存量单向量集合 collection_exists=true 不重建** → 换新代码必须删集合重建（用户已授权全量删）
- payload 新增：`llm_status` / `llm_analysis` / `llm_retries`

## 关键代码变更（5 文件 + 3 文件基础设施）

### 基础设施层（src/infrastructure/qdrant/repo.rs、domain/traits.rs、shared/collections.rs）
- `VECTOR_NAME_BASE="base"` / `VECTOR_NAME_LLM="llm"` 常量（shared/collections.rs）
- `VectorRepository` trait 新增：`scroll_points`(返回 {id,payload})、`set_payload`(只更新 payload 保留向量)、
  `search_named`(指定向量名搜索)、`search_named_with_filter`、`fetch_vectors`(按 id 拉指定命名向量)
- `json_to_condition` 扩展 is_empty/is_null（与 match 互斥）

### 构建层（src/application/build/pipeline.rs + main.rs + builder.rs + cli/build.rs + service.rs）
- **Phase 1（步骤 7b）**：upsert 点改 `"vectors": {"base": vec}`，payload 不含 llm_analysis/llm_status
- **Phase 2**：
  - 成功（chat Ok 且非空）：一次 `embed_batch(&[base_text, llm_response])` 出双向量 → upsert 完整 payload + `llm_analysis` + `llm_status=success` → mark_llm_analyzed
  - 失败（chat Err **或空响应**）：set_payload `{llm_status:failed, llm_retries+1}`，**不 mark**（修复空响应假成功 bug）
  - 重试上限：chat 前 scroll 读 llm_retries，>=3 时跳过 LLM 调用（保持 failed 状态可辨识）
- **补偿自愈 backfill_llm_gaps**（Phase 2 await 之后、BuildReport 之前）：
  - 两次 scroll（failed match 过滤 + is_empty 过滤，各限 LLM_BACKFILL_BATCH=200，按 id 去重）
    —— json_to_qdrant_filter 不支持 must 内嵌套 should，拆两次是可行形状
  - 每点：读源文件按 start/end 行切片（<10 字符回退整文件）→ sha256 → prog_key=`method:{entity_id}` → is_llm_analyzed 守卫 → chat → 成功双向量 upsert + mark；空响应/Err → set_payload failed+retries+1；源文件缺失/nacos → warn 跳过
  - ⚠️ **必须并发（2026-08-11 用户报 warehouse-center 构建极慢的根因）**：初版是串行 `for point in ...` 循环——一次只发 1 个 LLM 请求，opencode.go 平均 12.8s/个 → 302 个缺口 ≈ 64 分钟，且构建日志表现为"卡在补偿阶段"（大量 `补偿成功` 但无 Phase 2 日志）。改为 `stream::iter(...).map(|point| async move {...}).buffer_unordered(self.phase2_concurrency)`（与 Phase 2 主循环同一并发值），闭包返回 `(u64, Option<bool>)`（Some(true)=成功/Some(false)=失败/None=跳过），末尾聚合 succeeded/failed。改造后 16 路并发 ≈ 4 分钟，16 倍加速。**新代码的补偿/批量 LLM 循环一律 buffer_unordered，禁止串行 await。**
  - 开关：`--no-llm-backfill`（clap `action=ArgAction::SetFalse, default_value_t=true` 实现"默认开、flag 关"）
  - ⚠️ main.rs 有 **3 处**调用点要加 llm_backfill 参数（--test 分支、普通 handle_build、handle_build_all）

### 搜索层（src/application/context/search_mcp.rs + interfaces/cli/search_render.rs）
- 精确通道/向量通道改用 `search_named(VECTOR_NAME_BASE)`（named vectors 集合搜索必须指定向量名，否则 400）
- rerank：`rerank_with_llm_vectors` — 对 top-50 候选 `fetch_vectors(VECTOR_NAME_LLM)`，cosine(query, llm_vec)，`final = 0.5*base_score + 0.5*sim`；无 llm 向量保持 base 分数
- `hit_from_payload` llm_analysis 空串 → None（与 config 世界对齐）
- 渲染：Method llm_analysis 空 → **"分析: 暂无 LLM 分析"**（不再伪装 file:Ls-e 位置串；位置单独一行保留）

## 端到端验证（doctor-center，63 方法，2026-08-11 实测）

1. `dt build --full` 正常构建 → 63/63 success，双向量集合确认
2. **失败注入**：pipeline.yaml model_llm 改成 `nonexistent-model-fail-test` + `--full` → **63/63 failed**（0 假成功 0 缺失，状态完全可辨识）
3. **恢复自愈**：恢复 deepseek-v4-flash + 增量构建 → 日志 `缺口补偿: 63 缺口, 53 成功, 10 失败`（自动补 53 个 failed 点！）
4. 再跑一次 → 剩余 10 个 `llm_retries=3` 达上限跳过（保持 failed，不再烧 LLM）
5. 搜索验证：failed 点显示"分析: 暂无 LLM 分析"+ 独立位置行；success 点显示完整"用途：/逻辑："
6. 幂等：第二次构建 `缺口补偿: 0 缺口`（补过的已 mark，不重复消耗 LLM）

## 验证脚本

`scripts/verify-phase2-self-healing.sh`（如已落盘）或手动步骤：
```bash
# 状态位分布（核心可观测性）
curl -s -X POST http://127.0.0.1:6333/collections/code_methods/points/count -H 'Content-Type: application/json' \
  -d '{"exact":true,"filter":{"must":[{"key":"llm_status","match":{"value":"success"}}]}}'
# 失败注入前备份 pipeline.yaml，改 model_llm 为坏值，--full 构建，观察全 failed
# 恢复后增量构建，观察"缺口补偿: N 缺口, M 成功, K 失败"
```

## 遗留事项

- 剩余 10 个失败方法 llm_retries=3 达上限：接口方法/getter 类反复失败，可能是 prompt 或空体问题，状态保留可排查
- 存量数据全量删除重建（用户已授权）：重建途中的 LLM 失败会被补偿机制自动补齐
- `--full` 不清 code_methods（full_rebuild.rs 只删 kg_nodes/doc_chunks）——正交缺陷，建议后续把 CODE_METHODS 加入清理列表
- 源文件删除不留痕（孤儿点永久泄漏）——补偿靠哈希守卫跳过，清理属可选增强

## ⚠️ ensure_collection 双向量副作用坑（2026-08-11 二次修复）

**症状**：warehouse-center 构建日志 `GPU 处理器执行失败 processor="store" path=.gitlab-ci.yml error=... Not existing vector name`。
**根因**：`ensure_collection`（qdrant/repo.rs）初版对**所有集合**无条件创建 named vectors 双向量 {base,llm}，但 kg_nodes/doc_chunks 的写入方（kg_bridge.rs:339/905、consolidate.rs:353/478）和读取方（consolidate.rs:277 `search_with_filter` 不带 vector_name）都是**单向量**——双向量集合上单向量 upsert/搜索报 "Not existing vector name"。
**修复**：ensure_collection 分支——**仅 `collection == CODE_METHODS` 建双向量，其余集合保持单向量**（`VectorParamsBuilder` 单向量创建）。修复后需**删除 kg_nodes/doc_chunks 集合重建**（存量已是双向量结构，collection_exists=true 不会自动重建；数据由后续构建补）。
**验证**：`GET /collections/{c}` 的 `config.params.vectors`——code_methods 应为 dict {base,llm}；kg_nodes/doc_chunks 应为单向量配置（**无 base 键**，注意单向量配置本身也是 dict，判据是 `'base' in vectors_config` 而非 isinstance dict）。

## Qdrant named vectors API 实测（1.18.2 / qdrant-client 1.18.0，2026-08-11）

- named vectors 集合 search 必须指定 name：`{"vector": {"name":"base","vector":[...]}}`（不带 → 400）；REST upsert 用 `PUT /collections/{c}/points`（POST 要求 `ids` 字段）。
- `is_empty` 匹配缺失键；`is_null` **只匹配显式 null 不匹配缺失键**——补偿扫描必须用 is_empty。
- set_payload 只更新 payload 保留向量（REST `POST /collections/{c}/points/payload`；gRPC `SetPayloadPointsBuilder`，构造要 Qdrant Value map 不能直接传 serde_json::Value）。
- 取命名向量：`GetPointsBuilder` + `.with_vectors(VectorsSelector{names})`；`VectorsOutput` 结构是 `vectors_options: Option<VectorsOptions>`（**不是** vectors/named_vectors 字段）；`VectorOutput.data` 是 flat f32 数组（deprecated 但可用）。
- 现有 `scroll_payloads` 不含 point id 且 max 处静默截断——勿改它，新增独立 `scroll_points` 返回 {id, payload}。

## 团队实施分工（2026-08-11，3 worker 并行）

按**文件隔离**分工避免冲突：Worker A（qdrant/repo.rs + traits.rs + collections.rs 基础设施）→ 完成后并行 B（build/pipeline.rs 构建层）+ C（search_mcp.rs + search_render.rs 搜索层）。
⚠️ **delegate_task 子 agent 工具预算耗尽可能留下未编译/未完成代码**（本次 B 改了 5 文件未编译、C 只侦察未改码）。派发后必须亲自 `cargo check` + 验证实际落地，不能信"完成"报告；修子 agent 遗留编译错误（调用点参数、签名不同步）是父 agent 的职责。
