# 团队 B 建议落地 + 复测闭环 (2026-08-12)

背景: im-center 团队测试第二轮。团队 B(KG 审计组) 5 条建议全部落地 + 复测团队验证通过。commit 45158fa / 99571d6 / 92f95d8。

## 5 条建议落地清单

1. **低分降级提示**(`src/interfaces/cli/search_render.rs`): rerank 分数 < 0.5 时输出
   `⚠️ 结果可能不相关: 最高分 X 低于阈值 0.5(常见原因: world 选错或跨项目噪音; 可尝试 --world code + --project <项目名>)`。
   实测依据: 跨项目污染结果普遍 <0.5, 有效命中 >0.66。
2. **索引对账巡检**(`src/interfaces/cli/cleanup.rs` dt health): Memgraph Method 节点数 vs
   Qdrant `code_methods` 向量数对账。`read_query("MATCH (m:Method) RETURN count(m) AS n")` +
   `collection_info(CODE_METHODS).points_count`; 不一致 → 提示索引漂移建议 `--full` 重建。
   ⚠️ **瞬态漂移 ≠ 真漂移**: 有并行 `dt build` 在跑时(别的项目), Memgraph 先写入、
   Qdrant 后同步, 对账会短暂不一致——先 `ps aux | grep "dt build"` 判断是否有构建进行中。
   清理 uvp-im-center 残留后实测 16355 = 16355。
3. **跨项目分组展示**(search_render.rs): 多项目命中按 project 分组输出 +
   `📦 命中项目分布: A / B / C(共 N 条)` 首行。None project 归 "（无项目）"。
4. **残留项目清理**(脚本模式): 误用目录名构建产生的死数据污染检索。
   用 Python neo4j 驱动(bolt://127.0.0.1:7688) DETACH DELETE + qdrant-client
   `FilterSelector(Filter(must=[FieldCondition(key="project", match=MatchValue(...))]))`
   逐集合删除。⚠️ qdrant delete 不支持裸 dict selector, 必须用 `qdrant_client.http.models` 的 FilterSelector。
   清理后索引对账恢复一致; dt 无按项目删除的 CLI。
5. **知识层补全**(im-center 代码在 knowledge 世界检索不到业务文档):
   `dt learn "<任务标题>" --project im-center --entities "..." --pattern "..." --pitfalls "..." --decisions "..." --success true`
   写入 Knowledge/Experience/Playbook 节点 → `dt build --source knowledge`(替代已弃用的 dt kg-sync)
   同步向量 → `dt search "im-center 消息发送链路" --world knowledge --project im-center` 0.92 分命中。

## 查询词使用建议(复测验证, 已写入 AGENTS.md)

- 中文查询尽量带具体动词: "添加群成员" 而非 "群成员管理"(后者 0 命中)。
- 功能名/标识符类(accountImport/addGroupMember/getUserSig)用英文或中英混搭: 100% 召回 vs 中文 80%。
- 失败判定顺序: 先看是不是查询词问题(换英文标识符重试) → 再判断索引缺失/数据污染, 勿直接归咎索引。

## 复测闭环模式(团队测试 → 修复 → 复测)

- 首轮: 3 角色并行(项目功能分析/功能测试/KG 审计) → 发现 3 类问题(dt_search_kg 硬编码 world、
  infra_search 虚构语法、注释错位 bug) → 修复 + 全量重建(`dt build --name im-center --full`)。
- 复测轮: 2 角色并行(检索正确率 8-10 查询逐条判定 / 知识层+回归+新功能 5 项验证)。
- 复测判定: 中文口径 80% + 中英文兜底 100% = 达标(失败项为查询词问题非索引问题)。
- 全流程记录: `reports/2026-08-12-imcenter-team-test-kg-improve.md`(五节: 团队A/B报告摘要、
  修复实施、建议落地、复测结果、使用建议)。

## 用户自测脚本(项目内 scripts/, 已提交)

- `scripts/verify-kg.sh [--full]`: 8 组检查(索引状态 sense / 中文检索 5 项 / 英文标识符 4 项 /
  注释回归 2 项 / 索引对账 dt health / 低分提示 / 分组展示 / knowledge 世界), 输出 PASS/FAIL 统计。
  14/14 通过为基线。--full 增加 cargo test(约 1 分钟)。
- `scripts/check-dt-usage.sh [小时数] [agent.log路径]`: **元测试**——统计 Hermes 会话日志中
  dt_sense / dt_search_kg / dt search / run_cypher_query 调用次数 + [DT-SENSE] 简报注入次数,
  判定 Hermes 是否真的使用 dt 搜索(验证插件+AGENTS.md 准则生效)。
  用法: 开新会话问需项目事实的任务(如 "im-center 的消息撤回流程"), 跑完查日志计数。
