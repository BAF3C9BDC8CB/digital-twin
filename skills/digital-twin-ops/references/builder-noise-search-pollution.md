# Builder/构造器噪声：Lombok 样板方法污染语义检索（2026-08-12 实测）

## 现象
im-center 2287 个方法中 **93 个 `builder` 方法（4.1%）** 由 Lombok `@Builder` 注解生成。
Phase 2 LLM 对它们生成无意义分析（"用途：暂无"类样板文本），这些低质量描述向量
参与语义检索打分，挤占高质量方法/类结果（实测低分 0.28 噪声）。

## 量化方法（可复用的检测脚本思路）
```python
# qdrant scroll code_methods，按 project 过滤，统计:
# - name == "builder"（Lombok @Builder 样板）
# - signature 以 class_name 开头且含 '('（构造器）
```

## 优化方案（尚未实施，2026-08-12 报告：reports/2026-08-12-export-analysis-round3-builder-noise.md）
- **P0**：Phase 2 检测 `name == "builder"`（且 class_name 非空）→ 跳过 LLM 调用，
  写 `llm_status = 'skipped_builder'`（省 token：93 方法 × ~200 token ≈ 18.6K/构建）
- **P1**：search_code 检索侧对 `name == "builder"` / `llm_status == "skipped_builder"` 命中降权（score × 0.5）或默认不参与向量检索
- **P2**：构造器同样处理（数量少，优先级低）

## 为什么值得做
- 清理 4.1% 噪声方法对语义检索的干扰（搜"群组消息"等业务语义时 Builder 描述向量不参与挤占）
- 每次全量构建省 ~18.6K token

## 相关文件
- src/application/build/pipeline.rs（Phase 2 方法分析，检测点在此）
- src/application/context/search_mcp.rs（检索后处理/降权）
- 实施后验证：im-center 全量构建统计 skipped_builder 数量 + 检索结果中 builder 占比下降
