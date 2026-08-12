# 导出分析第三轮 — Builder/构造器噪声优化（2026-08-12）

**分析对象**：用户第三次导出的 default profile（87 会话，与上次导出差异 = 3 个 digital-twin-ops 新引用，均为子代理沉淀）
**核心发现**：导出中"优化方案综合设计员"任务书第 5 项（数据层）—— **Builder/构造器噪声** —— 是唯一未落地的优化方向。

## 噪声量化（im-center 实测）

| 指标 | 值 |
|------|-----|
| im-center 方法总数 | 2287 |
| builder 方法（Lombok @Builder 样板） | **93 (4.1%)** |
| 构造器方法 | 1 |
| llm_status=success | 2287 (100%) |

93 个 builder 方法由 Lombok `@Builder` 注解生成，Phase 2 LLM 对其生成的分析是无意义样板文本
（"用途：暂无"类），污染语义检索——搜"群组消息"等业务语义时这些 builder 方法的低质量
描述向量参与打分，挤占高质量方法/类结果。

## 优化方案

### P0：builder 方法跳过 LLM 分析 + 低分降权
- **方案 A（推荐）**：Phase 2 检测 `name == "builder"`（且 class_name 非空）→ 跳过 LLM 调用，
  直接写 `llm_status = 'skipped_builder'`（不浪费 LLM token），检索时对 skipped 方法降权。
- **方案 B（兜底）**：检索后处理过滤 builder 方法（name == builder 且 llm_analysis 为空/低质）。

### P1：检索侧对样板方法降权
- search_code 对 `name == "builder"` 或 `llm_status == "skipped_builder"` 的命中降权（score * 0.5），
  或默认不参与向量检索（仅精确检索可达）。

### P2：构造器同样处理（数量少，优先级低）

## 收益
- LLM token 节省：93 方法 × ~200 token/次 ≈ 18.6K token/构建
- 检索质量：清除 4.1% 噪声方法对语义检索的干扰

## 相关文件
- src/application/build/pipeline.rs（Phase 2 方法分析）
- src/application/context/search_mcp.rs（检索后处理）
- 验证：im-center 全量构建后统计 skipped_builder 数量 + 检索结果中 builder 占比
