# Qdrant 集合拓扑 & GAP-A/B 类向量化审查备忘

来源: 2026-08-12 代码审查（commit 8c6a683, GAP-A/B 落地, 3 files）。审查方法见文末 checklist, 可复用到后续对 search_code/backfill 类函数的审查。

## 集合拓扑（GAP-A 之后）
| 集合 | 向量结构 | 写入方 | 查询方 |
|---|---|---|---|
| code_methods | named 双向量 base/llm (size=1024) | Phase 1 (base), Phase 2/backfill_llm_gaps (llm) | search_named / search_named_with_filter (base), rerank 用 fetch_vectors (llm) |
| code_classes | 单向量 size=1024（ensure_collection 默认分支） | Phase 2.6 backfill_class_descriptions | search() 单向量 search（is_classes 分支） |
| doc_chunks / kg_nodes | 单向量 | kg_bridge/consolidate | search_with_filter（单向量版） |

铁律:
- **单向量集合不能用 named 查询/写入**（报 "Not existing vector name"）; named 集合不能走单向量 search。
- ensure_collection 仅 code_methods 走 named 分支（repo.rs L133 的 if collection == CODE_METHODS）, 其余集合默认单向量。新建 code 类集合必须走单向量分支 → 写入用 `"vector": [...]`, 查询用 `vector.search()` 而非 search_named。
- upsert 自动识别 `"vectors": {...}`(named) vs `"vector": [...]`(单向量); 字符串 id（class_id/method_id）会哈希成 u64（DefaultHasher, 非加密）。

## GAP-A/B 审查结论（bug 模式, 可复用到后续审查）

1. **状态标记时机**: `vectorized=true` 必须在 upsert Ok 之后设置, 不能只看 embed 成功。
   - 实际代码: embed Ok → upsert match（Err 仅 warn）→ 无条件 SET vectorized=true → upsert 失败也标记 → 类永久不可检索且永不再重扫。
2. **`let _ =` 吞掉 Memgraph 写回错误**: LLM 分支 `SET description, llm_status='success'` 失败仍继续 embed+upsert+vectorized → 图/向量永久不一致。写回失败应 return false 不标记。
3. **多通道搜索合并新集合时逐通道核对**（本次最大坑）:
   - 1A 精确通道（search_named_with_filter）对 classes 跳过是合理的（单向量无 named）; 但注释声称"向量通道 + 关键词兜底覆盖"是**假的** —— 兜底只 `scroll_payloads(&method_cols[0])`, classes 不在内。
   - 向量通道 classes 分支直接 push results, 少了 rerank_with_llm_vectors 内部的 min_score / project / name 合法性过滤 → **指定 project 时跨项目类泄漏** + 低分噪声挤占 limit。
4. **补偿循环放大**: failed 类 desc 仍空 → 每轮查询命中 → LLM_CALL_MAX_ATTEMPTS(6) × 20 轮 = 每类最多 120 次调用/构建。方法侧 backfill_llm_gaps 有 llm_retries>=5 保护, 类侧没有。修复: WHERE 排除 `llm_status='failed'`（留给下次构建）或加 retries 计数。
5. **循环收敛条件**: 查询 WHERE 把"缺描述"(description 空) 和 "缺向量"(vectorized false) 混在 OR 里; 当 embed/vector 为 None 或 skip_embed=true 时 vectorized 永不设置 → 每轮 20 次空转（300 条查询 + 文件读）。且 Phase 2.6 未检查 skip_embed（Phase 2.5 有检查）。
6. **源文件不可读的类不写任何状态标记** → 每轮重扫（desc 空时每轮还重试 LLM）; 且已有描述类（desc 非空）也被源码读取挡在前面 —— 已有描述根本不需要读源码, 应先判 desc 再读文件。
7. 次要: existing_desc 分支不截断 500 字符（与 LLM 分支不一致）; ensure_collection 每类并发调用（首次创建有竞态但无害, 应提升到循环外）; code_classes 无删除清理路径（delete_files_from_graph / full_rebuild prepare 都不含）; is_global_collection / entity_type_from_collection 未覆盖 CODE_CLASSES（返回 "?"）。

## search_code 类函数审查 checklist
- [ ] 每个集合在 1A 精确 / 向量 / 关键词兜底通道的覆盖情况（兜底是否只查 method_cols[0]）
- [ ] project 过滤是否所有通道都有（新增集合分支最易漏）
- [ ] min_score / name 合法性过滤是否一致（跳过 rerank 的分支要手动补过滤）
- [ ] entity_type 解析: payload.get → as_str → filter(非空) → unwrap_or("Method")（方法点无该字段, 类点显式 "Class"）
- [ ] 状态标记（vectorized/llm_status）是否只在真正成功后设置, 失败路径是否留下可重试状态
