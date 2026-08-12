# LLM 分析失败重试机制（2026-08-11 实现）

用户需求：LLM 分析失败后不要立即标记 failed——延迟 1 秒重试，最多 5 次，全部失败才标记。

## 代码位置

`src/application/build/pipeline.rs`（dt-daemon crate）：

```rust
const LLM_CALL_MAX_ATTEMPTS: usize = 6;   // 1 次初始 + 最多 5 次重试
const LLM_RETRY_DELAY_SECS: u64 = 1;      // 失败后延迟 1 秒
const LLM_RETRIES_HARD_CAP: u32 = 5;      // 跨构建累计上限（原为 3）
```

## 语义

- **调用内重试**：`for attempt in 0..LLM_CALL_MAX_ATTEMPTS` 循环；attempt>0 时先
  `tokio::time::sleep(1s)` 再重试。失败（`Err`）或空响应都触发重试（空响应 = 失败，无降级，
  绝不写空串/占位伪装成功——用户铁律）。
- **最终失败**：6 次尝试全失败才 `set_payload(llm_status=failed, llm_retries+1)`，不 mark，
  保持可重试。日志 `"LLM 分析失败（6 次尝试均未成功），标记 failed"`。
- **跨构建上限**：`llm_retries >= LLM_RETRIES_HARD_CAP(5)` 时本轮跳过 LLM 调用（保持 failed）。
  旧值 3 → 5，与调用内重试对齐。Phase 2 主循环 + 补偿自愈两处判断都已改。
- 改动的两处调用点（结构相同）：
  1. Phase 2 主循环（`execute` 内，`phase = "phase2"`）
  2. 补偿自愈 `backfill_llm_gaps`（`phase = "phase2_compensation"`）

## 行为验证（实测）

- 全量重建后剩 2 个 failed 点（retries=2）：`DomainFactory.get`、`RegionRepository.getRegionByCode`。
- 增量构建补偿：`getRegionByCode` attempt=0 空响应 → WARN"响应为空，视为失败（将重试）" →
  attempt=1 延迟 1s 重试 → 成功。最终日志 `缺口补偿: 2 缺口, 2 成功, 0 失败`。
- Qdrant 终态：`code_methods` 3715/3715 全部 `llm_status=success`，0 缺口。
- 回归：`cargo test --release --lib` 674 passed / 0 failed。

## 排障注意（本次踩坑）

### dt build 并行互杀（根因级教训）

- **现象**：`dt search` 结果大量"暂无 LLM 分析"。
- **根因链**：两个 `dt build` 进程并行时，后启动的进程静默终止前一个的 Phase 2 后台任务
  （无 panic/abort 日志，日志最后一条停在旧进程最后事件）→ 方法点只有 base 向量、无
  `llm_status`（MISSING）→ 增量快照把未分析文件标"已是最新"跳过 → 补偿限批
  `LLM_BACKFILL_BATCH=200`/轮太慢。
- **修复**：`dt build --full --name <proj>` 全量重建，**期间禁跑其他 dt 命令**。
  对账法：Qdrant 状态计数（success+failed+MISSING）与 daemon 日志事件数核对，
  `build_progress` 行数 == success 数。

### pipeline.rs 大改的括号平衡技巧

把 `match cli.chat() { Ok(...) => { ... } Err(e) => {...} }` 重构为
`for attempt ... { match ... }` + 失败守卫时，极易残留旧 Err 分支/多余闭合括号，
cargo check 报 mismatched delimiter。不要靠肉眼数——用 Python 深度计数定位：

```python
depth = 0
for i, line in enumerate(open('src/application/build/pipeline.rs', encoding='utf-8')):
    s = line.split('//')[0]  # 粗去注释
    depth += s.count('{') - s.count('}')
    if depth != 0 or '{' in s or '}' in s:
        print(f"L{i+1} (depth={depth:+d}): {line.rstrip()[:90]}")
```

逐行看 depth 在哪里多跳/少跳，一次改对。改完先 `cargo check`（比猜括号快）再 `cargo test --release --lib`。
