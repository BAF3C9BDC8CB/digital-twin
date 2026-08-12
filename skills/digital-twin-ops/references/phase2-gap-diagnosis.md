# Phase 2 缺口诊断与修复（"暂无 LLM 分析" 排查）

## 症状
`dt search "关键词"` 返回大量 `分析: 暂无 LLM 分析` 的方法。

## 根因（2026-08-11 实证，warehouse-center 场景）
**主因：并行启动两个 `dt build` 进程，第二个会静默杀死第一个的 Phase 2。**

时序证据（daemon 日志）：
1. `dt build --name warehouse-center`（20:41）→ Phase 1 写入 3715 向量 →
   `Phase 2: 3715 个待分析`（20:42:44）→ Phase 2 后台任务开始逐方法 LLM 分析。
2. 用户又跑一次 `dt build`（20:49:07）→ 新进程初始化，**第一个进程日志从此消失**（
   20:49:06 是最后一条 "Phase 2 完成"；无 panic、无 abort、无错误日志）。
3. 结果：只有 1458 个方法实际发起过 LLM 调用（1399 成功 + 27 空响应 + 32 传输失败），
   **1922 个方法连 llm_status 都没写**（纯 MISSING，不是 failed）。

**次因：补偿限批。** `LLM_BACKFILL_BATCH = 200`（pipeline.rs L33）——每次构建末尾的
补偿自愈只处理 200 个缺口点。1922 个缺口要 10 轮构建才能补完；用户跑 2 轮只补了 ~385。

**注意区分**：`MAX_PIPELINE_FILES = 500`（build.rs L150）是**流水线分析**（文档级）
的文件截断，与 Phase 2（方法级 LLM 分析）无关——排查时别被"已达到流水线文件限制 (500)"
日志带偏。

## 诊断路径（按序执行）
1. **Qdrant 状态分布**：scroll `code_methods` 集合，按 payload `llm_status` 计数
   （success / failed / 缺失=MISSING）。MISSING = 从未进入 LLM 分析（连失败标记都没有）。
   ```python
   # 127.0.0.1:6333/collections/code_methods/points/scroll
   # body={"limit":1000,"with_payload":true,"with_vector":false} + offset 翻页
   ```
2. **快照表对账**：`sqlite3 /var/lib/digital-twin/snapshots.db`
   - `build_progress` 行数 ≈ success 数（stage='llm_analysis'，file_path 列存 `method:{method_id}`）
   - `pipeline_progress` 只有 500 文件（流水线文档级，别混淆）
3. **日志时间线**：解析 `/var/log/digital-twin/dt-daemon.log`（49MB+，用 Python json 逐行，
   勿用 grep 单行匹配）——找 `Phase 2: N 个待分析` / `工作线程已启动` /
   `后台任务完成` / `缺口补偿`。**若"待分析 3715"之后没有"后台任务完成"日志，就是
   Phase 2 被中途杀死**。
4. **确认进程互斥问题**：找两次相邻 `开始构建` 日志，若第二次初始化时间戳紧跟在
   第一次最后一条 Phase 2 日志之后（秒级），即并行构建互杀。

## 修复
- **立即见效（推荐）**：`dt build --full --name <project>` 全量重建。
  Phase 2 完整跑完（warehouse-center 3715 方法 ≈ 29 分钟），期间**不要再启动任何
  其他 dt 命令**。结果：3713 success / 2 failed / 0 MISSING（failed=LLM 多次重试仍失败，
  retries<3 下次构建补偿会再试——符合"失败不造假"设计）。
- **根治（需改代码+重打包）**：a) 构建进程互斥锁（flock，第二个等待/报错而非互杀）；
  b) `LLM_BACKFILL_BATCH` 200→1000+ 减少补偿轮次。
- **纯数据层面**：连续跑 N 次增量构建（每轮补 200），慢且不推荐。

## 铁律
**构建期间不要并行启动多个 dt 命令**（`dt build` 是单进程模型，第二个进程静默取代
第一个，Phase 2 后台任务随之丢失，缺口只能靠补偿缓慢消化）。
