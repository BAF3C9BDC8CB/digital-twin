# Phase 2 LLM 方法分析失败自愈闭环 — 架构设计方案

> 目标：以后任何构建（增量/全量）产生的 LLM 分析失败都不会留下永久性缺口 —— 允许失败，但失败必须能被后续增量构建自动补齐，不依赖 `--full`。
> 本方案基于对当前代码的实读验证（2026-08-11）：`pipeline.rs`、`incremental.rs`、`full_rebuild.rs`、`qdrant/repo.rs`、`sqlite/repo.rs`、`traits.rs`、`search_render.rs`，以及对本机运行中 Qdrant 1.18.2 的实测。

---

## 0. 现状机制验证结论（先读代码后的核实）

| 断言 | 验证结果 |
|---|---|
| Phase 2 只遍历本次构建变更文件的方法 | ✅ `pipeline.rs` L363 `for m in methods`（`methods` 来自 `extraction.methods`，仅含 `files_to_process`） |
| 失败只打 warn，无持久化标记 | ✅ L537-543：仅 `tracing::warn!("Phase 2 结果未持久化…")`，`persisted=false` 时不写任何行 |
| 增量构建只送 hash 变化文件进提取 | ✅ `incremental.rs` `select_files`：mtime 复用 + `detect_changes`，未变文件不进 Phase 1/2 |
| 未变文件的方法永远不会被重试 → 缺口固化 | ✅ 推论成立：无失败标记 + 未变文件不进提取 = 永久缺口 |
| 全量构建清进度后重跑 | ✅ `pipeline.rs` L184-189 `clear_step_progress` + `clear_llm_progress` |
| build_progress 只记成功 | ✅ `repo.rs` L254-294：`INSERT OR REPLACE`（仅成功路径调用）；key 为 `(file_path=prog_key, project, stage, sha1)`，无失败状态 |
| code_methods 写入点只有 2 处 | ✅ 全库搜索确认：pipeline.rs L290（Phase 1，无 llm_analysis）、L494（Phase 2，带 llm_analysis）；StoreProcessor 不碰 code_methods |
| **`--full` 不清 code_methods** | ✅ `full_rebuild.rs` `prepare()` L57-58 只删 `[KG_NODES, DOC_CHUNKS]`，**不含 CODE_METHODS** —— 全量重建同样不消除缺口 |
| Qdrant 支持 is_empty/is_null | ✅ 实测：服务端 1.18.2 的 count API 对 `{"must":[{"key":"llm_analysis","is_empty":true}]}` 返回 `count: 1658`；Rust 客户端 1.18.0 有 `Condition::is_empty/is_null`（filters.rs L165/175）。**但本项目 `json_to_qdrant_filter`（qdrant/repo.rs L452-499）只翻译 `match.value`，不支持 is_empty/is_null，需扩展** |
| 搜索展示缺口 | ✅ `search_render.rs` L68-76：Method 命中取 `llm_analysis`，缺失时回退 `snippet`（即 "分析: file:Ls-e" 位置串） |
| 源文件删除时 Qdrant 点不清理 | ✅ `delete_files_from_graph`（pipeline.rs L930+）只写 Memgraph；`delete_file_progress`（sqlite repo.rs L372）只清 `file_snapshots`+`pipeline_progress`，**不碰 build_progress 与 Qdrant** |
| method_id 确定性 | ✅ `make_method_id(project, file_path, class, name, start_line)` —— 文件未变时重提取可得相同 method_id（entity_id），可按 entity_id 精确匹配补偿 |

---

## 1. 推荐架构方案：缺口补偿（Gap Compensation，Qdrant 为准）

### 方案对比

| 方案 | 机制 | 优点 | 缺点 | 评价 |
|---|---|---|---|---|
| **A. 缺口补偿（推荐）** | 每次构建末尾，以 Qdrant `code_methods` 中 `llm_analysis` 为空/缺失的点为工作清单，重提取源文件、按 entity_id 匹配方法、走标准 Phase 2 流程补齐 | ① 以**用户可见的最终状态（搜索读的集合）为真值**，任何失败原因（LLM 500/超时、embed 失败、upsert 失败、进度写失败、**进程中途被杀**、历史遗留缺口）全部覆盖；② 无需 SQLite 迁移/新表；③ 与构建策略无关（增量/全量同样生效）；④ 天然幂等 | ① 需重提取文件拿 source_text（单文件解析成本低）；② 需扩展 `json_to_qdrant_filter` 支持 is_empty；③ 缺口点可能是陈旧点（文件已变），需哈希守卫 | **推荐**：一个机制同时满足「失败自愈」与「收敛保证」，是唯一能覆盖进程崩溃与历史缺口的方案 |
| B. 失败入队 retry queue | Phase 2 失败时 INSERT 队列行（含重试次数/下次时间），下次构建先排空队列 | 只重试真正失败的方法；可控退避 | ① 需新表+迁移+新 repo 方法；② **进程中途被杀时在途方法无失败行 → 依然漏**；③ 队列与 Qdrant 现实会漂移（如集合被删/重建）；④ 覆盖不了本特性上线前的历史缺口 | 不推荐作为主机制；可作为 A 的可选观测补充 |
| C. 修 select_files（把"有未分析方法"的文件视为变更） | 增量选择时检查文件内是否有方法缺分析 | 复用现有流程 | ① build_progress 的 key 是 `method:{id}` 不含文件路径，要反查文件必须另建映射表或查 Qdrant —— 绕回 A；② 会把未变文件重新送进 Phase 1 嵌入，浪费 embed 配额 | 不推荐：实现代价≈A 且更绕，还污染增量语义 |

**明确推荐：方案 A（Qdrant 缺口补偿），作为构建流水线内建步骤（默认开启、可配置），可选暴露独立 CLI。** 理由：搜索读的是 Qdrant，`llm_analysis` 缺不缺的判定只能在 Qdrant 上做才不可能漂移；补偿循环天然构成「失败 → 下次构建自动补齐 → 收敛」的闭环，且对失败原因完全不敏感。

### 关键设计点

- **缺口定义**：`code_methods` 中 `llm_analysis` 字段 `is_empty`（Qdrant 语义：缺失 或 null 均命中；实测 count=1658，字段非空即视为已分析）。
- **缺口清单获取**：`scroll_payloads(CODE_METHODS, Some(filter), max)`，filter 为
  `{"must": [{"key": "project", "match": {"value": <project>}}, {"key": "llm_analysis", "is_empty": true}]}`
  （code_methods 是跨项目共享单集合，必须带 project 过滤；scroll_payloads 在 QdrantRepo 已有实现，256/页分页）。
- **补缺口方法**：按 `file_path` 分组 → 哈希守卫（见 §5.1）→ 对每组文件调用 `self.extract_entities(project, root, &[path])` 重提取 → 以 `MethodBlock.method_id == payload.entity_id` 精确匹配 → 复用 Phase 2 的 job 构建逻辑（chat → embed → upsert(带 llm_analysis) → `mark_llm_analyzed`）。由于 method_id 由 `(project, file_path, class, name, start_line)` 确定性生成，文件未变时匹配必然精确。
- **代码复用防漂移**：把 Phase 2 中「方法 → (prog_key, source_hash, source_text)」的构建逻辑（pipeline.rs L363-400，含 nacos 分支）抽成共享辅助函数 `build_phase2_job(m, root)`，Phase 2 与补偿共用，避免两处逻辑漂移。
- **限批**：每次构建最多补偿 `phase2_compensation.max_per_build`（默认 300，可配）；超出部分留待后续构建继续 —— 当前 1658 个缺口按 300/构建约 6 次构建收敛（夜间 cron 即约 6 天），全程无人值守。
- **独立 CLI（可选增强）**：`dt build --repair-phase2 [--max N]` 或独立子命令，复用同一补偿模块，只跑补偿不跑 Phase 1/2，便于手动加速收敛。

---

## 2. 构建流程时序

补偿步骤插在 **Phase 2 同步等待完成之后、`BuildReport` 生成之前**（pipeline.rs L565 与 L570 之间），与 Phase 1/Phase 2 的关系：

```
execute():
  ① 扫描/选择文件（增量:仅变更文件；全量:全部）
  ② 删除已删文件图谱数据（Memgraph）
  ③ strategy.prepare()  —— 全量时清 kg_nodes/doc_chunks + 图谱（注意:不碰 code_methods，见 §5.4）
  ④ 全量时 clear_step/llm_progress
  ⑤ extract_entities(变更文件) → extraction
  ⑥ Phase 1: 写图谱 + embed 变更方法 → upsert code_methods（无 llm_analysis）
  ⑦ 重建调用图
  ⑧ 更新快照 file_snapshots（含新 file_sha1）     ← 补偿的哈希守卫依赖此处已更新的快照
  ⑨ Phase 2: 逐方法 LLM 分析（chat→embed→upsert 带 llm_analysis→mark_llm_analyzed）
      失败 → 仅 warn，不写进度（缺口自然留在 Qdrant）
  ⑩ NEW: Phase 2.5 缺口补偿
       a. 守卫: llm_client 可用 && !skip_embed && (embed, vector, snapshot_repo) 齐备
       b. scroll code_methods where project=X AND llm_analysis is_empty, limit max_per_build(+margin)
       c. 按 file_path 分组; 哈希守卫剔除"文件已变/已删"（见 §5.1）
       d. 逐文件 extract_entities → entity_id 匹配 → 构建 job（复用 Phase 2 逻辑）
       e. buffer_unordered(PHASE2_CONCURRENCY) 执行: chat→embed→upsert→mark_llm_analyzed
       f. 汇总日志: 本轮补偿 N 个/成功 M 个/仍缺口 K 个; 任何错误仅 warn，不使构建失败
  ⑪ BuildReport
```

要点：
- 补偿**放在 Phase 2 之后**：本构建新增/变更文件的分析有最高优先级，补偿只吃存量缺口；二者顺序执行（复用同一并发常量 `PHASE2_CONCURRENCY=4`），不叠加瞬时负载。
- 补偿跑在**快照更新之后**：`file_snapshots` 中已是本构建后的哈希，哈希守卫拿它与 Qdrant 缺口点所在文件的磁盘哈希比较，语义一致。
- 补偿与进度记录关系：成功路径写**同一张** `build_progress`（同 prog_key+hash）→ 补上的缺口下次 `is_llm_analyzed` 命中，不再进清单；失败路径照旧不写 → 下次构建继续补。闭环自洽。
- 全量构建时流程相同：`--full` 后 Phase 2 重分析全部方法，其失败缺口由同一次构建末尾的补偿再补一轮，双保险。

---

## 3. 失败处理策略（重试 / 限批 / 退避 / 降级）

- **客户端重试**：维持现状 —— glmcoding 客户端对 500/超时已有 3 次指数退避重试（task 上下文实测 0 限流）。
- **层内重试**：补偿对单个方法**不额外重试**（避免放大坏日子的负载）；一次失败即留给下次构建。**退避 = 构建节奏本身**（夜间增量构建即自然退避周期），无需维护每方法退避表。
- **限批（核心防雪崩）**：`max_per_build`（默认 300）+ 复用 `PHASE2_CONCURRENCY=4`。缺口再大，单次构建只吃固定额度，构建耗时可控；队列自然摊到后续构建。
- **慢/坏方法隔离（可选增强）**：若同一 prog_key 连续 N 次（如 5 次）补偿失败，可短期跳过（内存 HashSet 或轻量表记录 next_retry_at），防止一个永远失败的坏方法每次构建都白烧一次 chat。默认先不做，观察日志；出现高频失败再补。
- **降级**：
  - LLM 不可用 / Qdrant 不可用 / store 未启用（`processors.store=false` / `skip_embed`）→ 补偿整体跳过，构建照常成功（与 Phase 2 现状一致）；下个构建再试。
  - 搜索展示侧不变：缺口方法仍回退显示 snippet（"分析: file:Ls-e" 位置串），不报错、不阻塞 —— 补偿收敛后自动消失。
  - 补偿内部错误一律 `tracing::warn!`，绝不让补偿失败导致整个 build 返回 Err（`BuildReport` 照常产出）。
- **观测**：补偿步输出 `phase="phase2_compensation"` 的结构化日志（project / gaps_seen / skipped_stale / matched / success / still_missing），`dt build` 汇总行同步体现，便于 cron 与群监控核对收敛速度。

---

## 4. 幂等性与并发控制

| 关注点 | 设计 |
|---|---|
| 点写入幂等 | 点 ID = entity_id 的确定性 u64 哈希（与 Phase 1/2 同一 `upsert` 路径），重复 upsert 覆盖同一点，幂等 |
| 进度幂等 | `mark_llm_analyzed` 是 `INSERT OR REPLACE`；`is_llm_analyzed(prog_key, hash)` 前置守卫：已分析且 hash 未变 → 跳过（Phase 2 与补偿共用同一守卫，补偿对缺口点先查一次再开 chat） |
| 补偿与 Phase 2 同构建 | 顺序执行（Phase 2 await 完成才进补偿），无并发冲突 |
| 两个构建进程并发 | **at-least-once**：可能两个构建同时 scroll 到同一缺口 → 重复 chat（浪费）但无脏数据（upsert/进度均幂等）。SQLite `conn` 互斥锁串行化进度写；Qdrant 无事务，不做抢注/租约（复杂度不值） |
| 并发上限 | 补偿流与 Phase 2 相同：`buffer_unordered(PHASE2_CONCURRENCY)`，不另开并发 |
| 补偿不持锁跨 await | 沿用 repo 逐调用加锁模式，无死锁面 |
| 缺口消失条件 | 写入成功 + 进度成功，二者都满足才从清单消失（与 Phase 2 的 persisted 判定一致），不会出现"点有了但下次还重试"（最多一次无害重试） |

---

## 5. 主要风险与边界

### 5.1 源文件已删 / 已变（哈希守卫）
缺口点的 payload 不含 file_sha1，但 `file_snapshots` 有每文件最新哈希：
- **文件已删**：磁盘不存在或不在 `list_snapshots` 中 → **跳过补偿**；并建议顺手删除该孤儿 Qdrant 点（一次性 `delete_by_filter`，扩展 is_empty 后可精确按 `project+file_path` 删），避免孤儿点永续存在（现状 delete_files_from_graph 不碰 Qdrant，属既有泄漏，本次一并补上）。
- **文件已变**：磁盘哈希 ≠ snapshot 哈希 → 跳过（该文件的现状已由本次构建 Phase 1/2 处理；补偿只处理"快照与磁盘一致"的缺口，保证喂给 LLM 的是与 Qdrant 点对应的旧源码）。守卫实现：补偿开始时构建一次 `HashMap<file_path, file_sha1>`（`list_snapshots`），对每缺口文件 `scanner::compute_file_hash` 比对；不一致直接 skip。

### 5.2 Qdrant 过滤可行性（已实测）
- **服务端**：Qdrant 1.18.2 支持 `FieldCondition.is_empty/is_null`；实测 `llm_analysis is_empty` count=1658，命中缺口（字段缺失即命中，含历史 2282 缺口的最新子集）。
- **客户端**：qdrant-client 1.18.0 提供 `Condition::is_empty/is_null`（filters.rs L165/175）。
- **落地改动**：扩展 `json_to_qdrant_filter`/`json_to_condition`（qdrant/repo.rs L452-499）支持 `{"key": K, "is_empty": true}` 与 `{"key": K, "is_null": true}` 两种条件（目前只支持 match.value，遇到 is_empty 会报"缺少 match.value"错误），并补单元测试；`scroll_payloads` 与 `delete_by_filter` 立即受益。注意 is_empty 与 is_null 语义差异：is_empty=缺失或 null 都命中（用于缺口检测）；is_null=仅 null（一般不用）。
- 备选（零改动）：scroll 全量 payload 后在 Rust 侧过滤缺失字段 —— 4700 点规模完全可承受，但推荐还是扩展 translator（更干净、可复用）。

### 5.3 Nacos 虚拟文件
prog_key 为 `nacos:{data_id}`，entity_id 为 `dt://nacos/...`。补偿的重提取依赖磁盘真实文件：若 nacos 虚拟文件已物化到 root 下（Phase 2 L368-372 的整文件回退读取暗示存在），则走同一 `proj_root.join(file_path)` 路径即可；若不存在，则跳过该类缺口并 warn（该类文件的方法分析只靠 Phase 2 变更驱动）。实现时把 prog_key/路径分支抽进共享函数，两处行为天然一致。

### 5.4 全量重建不清 code_methods（既有缺陷，建议一并修）
`full_rebuild.rs::prepare()` 只删 kg_nodes/doc_chunks，**code_methods 旧点（含旧 start_line 的陈旧 entity_id）在全量重建后仍残留**；陈旧点 entity_id 与重提取结果不匹配，补偿也救不了它（匹配不到方法）→ 搜索可能命中陈旧点。建议：把 CODE_METHODS 加进 `prepare()` 的清理列表（与 kg_nodes/doc_chunks 同逻辑，按 project 过滤删除）。注意本特性与缺口补偿正交：清空后 Phase 1 重建点、Phase 2 重分析、失败缺口由补偿兜底，闭环依旧成立。

### 5.5 集合被删 / 手动重建（本方案覆盖不到的点缺失）
若有人删了整个 code_methods 集合：增量构建因文件未变不会重建点（无 gap 记录可查），补偿也无从补起（点都没了）。这是补偿的盲区，恢复手段仍是 `--full` 或集合级重建。与缺口自愈正交，如实说明边界即可（用户当前"全量删除重建"正是走这条恢复路径）。

### 5.6 进度行存在但点丢失 / 点存在但无进度行
- 点有 llm_analysis、进度行缺失：补偿看不到缺口（以 Qdrant 为准），下次变更驱动可能重分析一次 —— 无害浪费，不构成缺口。
- 进度行有、点缺失（如集合被重建）：is_llm_analyzed 会跳过，点永远不回来 —— 同 5.5 边界，需 `--full`。
两例均不影响"llm_analysis 不再缺失"的达成。

### 5.7 补偿负载与构建时长
最坏情况 = 缺口文件数 × 单文件解析 + max_per_build × (chat+embed)。单文件 tree-sitter 解析毫秒级；300 × ~2-5s chat ≈ 10-25 分钟上限（4 并发）。夜间 cron 场景可接受；若在意，`max_per_build` 调小即可。补偿只 read 源文件、不改图谱/快照，无副作用面。

### 5.8 并发构建（见 §4）与 SQLite 锁
`conn.lock()` 是互斥的，两个构建同时 mark_llm_analyzed 不会写坏；仅可能重复 chat。可接受。

---

## 6. 落地改动清单（建议实施顺序）

1. **`qdrant/repo.rs`**：`json_to_condition` 支持 `is_empty`/`is_null`（客户端 `Condition::is_empty/is_null`）+ 单测；顺带可给 `VectorRepository` 加 `count_points(filter)` 方便日志（可选）。
2. **`pipeline.rs`**：把 L363-400 的 job 构建（prog_key/hash/source 回退/nacos 分支）抽为共享函数 `build_phase2_job`；Phase 2 与补偿共用。
3. **新模块 `src/application/build/phase2_compensation.rs`**：`run_compensation(...)`，含哈希守卫、文件分组、entity_id 匹配、限批、结构化日志；`execute()` 在 Phase 2 await 之后调用。
4. **配置 `pipeline.yaml`**：新增 `phase2_compensation: {enabled: true, max_per_build: 300, concurrency: 4}`，构建时传入（`BatchConfig` 或独立结构）。
5. **`full_rebuild.rs`**：清理列表加 CODE_METHODS（修复 §5.4）。
6. **删除文件清理（可选，配合 §5.1）**：`delete_files_from_graph` 或新增逻辑删除孤儿 code_methods 点 + 清对应 build_progress 行。
7. **CLI（可选）**：`dt build --repair-phase2` 独立补偿入口。

---

## 附：关键代码锚点

- Phase 2 失败路径：`src/application/build/pipeline.rs` L537-543（warn，不写失败）
- 进度读写：`src/infrastructure/sqlite/repo.rs` L254-307；`domain/traits.rs` L186-204
- Qdrant scroll+filter：`src/infrastructure/qdrant/repo.rs` L190-245（scroll_payloads）、L452-499（json_to_qdrant_filter，待扩展）
- 增量选择：`src/application/build/strategy/incremental.rs` L24-84
- 全量清理：`src/application/build/strategy/full_rebuild.rs` L52-89（不含 CODE_METHODS）
- method_id：`src/domain/id.rs` `make_method_id(project, file_path, class, name, start_line)`
- 展示回退：`src/interfaces/cli/search_render.rs` L68-76
