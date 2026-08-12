# Class 向量化回填：增量构建即可，无需全量（2026-08-12 实测）

## 结论
GAP-A（类向量化进 code_classes）落地后，**旧项目跑普通增量构建 `dt build --name <项目>` 即可自动回填类向量，不需要 `--full`**。

实测：digital-twin-v2 自身 330 类，一次普通 `dt build` 后 0→330 全部 vectorized=true（330 类 llm_status=success）。

## 原理：两个构建入口，提前 return 只在非 CLI 路径

| 入口 | 调用链 | Phase 2.6 是否执行 |
|---|---|---|
| CLI `dt build` | BuildCommand::run → BuildServiceImpl::build → PipelineTemplate::execute()（pipeline.rs L133） | ✅ 无条件执行（llm_backfill 默认开） |
| analyze_batch（engine.rs） | src/interfaces/cli/build.rs:660 | 无关（处理器模型，build.rs:637 提前 return 在这里） |

**build.rs:637 的 `files_to_process.is_empty() → return Ok(())` 只在 analyze_batch 路径**，与 CLI 构建无关。CLI 构建走 BuildCommand（builder.rs）→ service.rs build → pipeline.rs execute()，全程无空文件提前返回。

## 为什么旧项目会被回填
Phase 2.6 扫描条件（pipeline.rs L1487）：
```cypher
WHERE c.project = $project
  AND ((c.description IS NULL OR c.description = '')
       OR c.vectorized IS NULL OR c.vectorized = false)
  AND (c.llm_status IS NULL OR c.llm_status <> 'failed')
```
- 旧项目类**有 description 但 vectorized IS NULL** → 被捞起 → 走"已有描述"分支（跳过 LLM）→ 直接 embed + upsert + 标记 vectorized（免费，无 LLM 成本）
- 无 description 的类 → LLM 生成描述 → 向量化（耗时，失败自动重试续跑）

## 批量回填注意
- 各项目类数差异大（130–2064），无描述的类需要 LLM 时间（约 5–20 分钟/项目，受 phase2_concurrency 限制）
- 并发 LLM 限流会出现 "LLM 响应为空，视为失败（将重试）" WARN——非错误，延迟 1 秒重试机制自动处理，续跑可完成
- 回填后验证：Memgraph `vectorized=true` 计数 == Qdrant `code_classes` 点数

## 排查提示
若某项目 vectorized 全 0：先确认该项目最后一次构建时间是否在 GAP-A 实施（2026-08-12）之后——旧构建产物没有经过新代码路径，重跑一次普通构建即可。
