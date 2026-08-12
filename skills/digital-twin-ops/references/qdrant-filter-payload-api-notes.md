# Qdrant 过滤条件与 payload 更新实测笔记（2026-08-11，Qdrant 1.18.2 实测）

场景：digital-twin-v2「Phase 2 LLM 分析失败自愈（缺口补偿）」设计时的可行性验证。
结论全部来自本机运行中的 Qdrant（127.0.0.1:6333, v1.18.2）REST 实测 + qdrant-client 1.18.0 crate 源码确认。
复验脚本：`scripts/verify-qdrant-gap-filter.sh`。

## 1. 过滤条件语义（关键，勿搞反）

- `{"key":"llm_analysis","is_empty":true}` ✅ 匹配**键缺失**和**值为空**的点。
  实测：code_methods 集合 message-center 项目命中 1473 个真实缺口点（llm_analysis 键不存在）。
- `{"key":"llm_analysis","is_null":true}` ⚠️ 只匹配显式 JSON null，**不匹配键缺失**。实测同条件 0 命中。
- 结论：找"从未写入 llm_analysis"的缺口点必须用 `is_empty`，不能用 `is_null`。

REST 示例：
```bash
# 缺口计数（exact=true 走索引）
curl -s -X POST http://127.0.0.1:6333/collections/code_methods/points/count \
  -H 'Content-Type: application/json' \
  -d '{"exact":true,"filter":{"must":[{"key":"project","match":{"value":"message-center"}},{"key":"llm_analysis","is_empty":true}]}}'
# 缺口分页（scroll，limit≤256/页，next_page_offset 续页）
curl -s -X POST http://127.0.0.1:6333/collections/code_methods/points/scroll \
  -H 'Content-Type: application/json' \
  -d '{"limit":5,"filter":{"must":[{"key":"project","match":{"value":"message-center"}},{"key":"llm_analysis","is_empty":true}]},"with_payload":true,"with_vector":false}'
```

## 2. set_payload 只更新 payload、保留向量（实测）

- REST `POST /collections/{c}/points/payload`，body `{"payload":{...},"points":[<数值id>]}`。
- 实测：写入后原向量完整保留（1024 维），无需重嵌 → 缺口补偿可只补 llm_analysis 字段。
- 删除字段：`POST /collections/{c}/points/payload/delete`，body `{"keys":[...],"points":[...]}`。
- ⚠️ 只对**数值点 id** 生效；本仓库方法 id 是字符串哈希成的 u64（repo.rs upsert L261-279 的 DefaultHasher）。

## 3. qdrant-client 1.18.0 crate 支持情况

- `Condition::is_empty(key)` / `Condition::is_null(key)`：crate src/filters.rs L165/L175。
- `SetPayloadPointsBuilder::new(collection, payload).points_selector([PointId])`：crate src/builder_ext.rs L87。
- REST 与 gRPC 语义一致，gRPC 侧直接可用，无需手搓 proto。

## 4. 代码库落点与陷阱（src/infrastructure/qdrant/repo.rs）

- `scroll_payloads` 只 push payload、**不含 point id**（无法驱动 set_payload）——✅ 已新增独立方法
  `scroll_points`（返回 {id, payload}，见 §8），`scroll_payloads` 未动，调用方
  sense/mod.rs、search_mcp.rs 零波及。
- `json_to_qdrant_filter` / `json_to_condition` 原只支持 match string/bool/int——✅ 已扩展
  is_empty/is_null 分支（`Condition::is_empty(key)`/`is_null(key)`），与 match 互斥（同时出现报错），见 §8。
- 集合缺失已降级：`collection_missing` 判定后返回空结果而非报错，补偿逻辑可复用。

## 5. Phase 2 缺口补偿机制设计（2026-08-11 已出计划，尚未实施）

- 缺陷根因：Phase 2 逐方法 LLM 分析失败只 warn 不写持久化标记（pipeline.rs L537-543），
  增量构建只处理 hash 变化的文件 → 失败方法永不重试，llm_analysis 永久缺失。
- 方案：构建末尾追加补偿步骤——scroll 过滤 `project=<当前> AND llm_analysis is_empty`
  （限批 200/轮）→ 按 payload.file_path + start_line/end_line 从磁盘读源码
  （切片 <10 字符回退整文件，与 Phase 2 pipeline.rs L364-372 同策略）→
  同一 code_analysis.yaml prompt 调 `chat(model, sp, source, 0.1, 100, false)` →
  成功则 set_payload 只补 llm_analysis → `mark_llm_analyzed(project, "method:{entity_id}", sha256(source))`；
  失败不 mark，下轮构建自动重试（幂等）。
- SQLite 零迁移：build_progress 主键 (file_path, project, stage) 为自由文本，
  prog_key=`method:{entity_id}` 直接复用 mark_llm_analyzed/is_llm_analyzed；
  file_sha1 与 Phase 2 的 sha256(source_text) 算法一致 → 增量跳过闭环成立。
- ChatClient trait（src/application/pipeline/infer_client.rs L354）：
  `chat(&self, model, system_prompt, user_prompt, temperature: f32, max_tokens: u32, json_mode: bool) -> Result<ChatResponse, String>`；
  Semaphore 限并发（max_concurrent），**无自动重试**，失败即 Err。
- 开关设计：`PipelineTemplate::with_llm_backfill(bool)` 默认 true；CLI `--no-llm-backfill`（Phase B）；
  并发复用 `PHASE2_CONCURRENCY = 4`（pipeline.rs L32）。

## 6. 二次设计复核补充（2026-08-11 晚；完整方案已落盘仓库）

完整架构方案（时序/失败策略/幂等并发/风险边界/7 项落地清单）见
**`/data/myProject/digital-twin-v2/docs/phase2-self-healing-design.md`** —— 实施前先读它，本节只记复核新增事实。

- **全量构建不清 code_methods**：`full_rebuild.rs::prepare()` L52-89 只按 project filter 删
  `KG_NODES`/`DOC_CHUNKS`，**CODE_METHODS 不在列** → `--full` 后旧缺口点与陈旧点仍残留。
  落地时建议把 CODE_METHODS 加入清理列表（与缺口补偿正交，勿混为一谈）。
- **源文件删除不留痕**：`delete_files_from_graph`（pipeline.rs L930+）只写 Memgraph；
  sqlite `delete_file_progress`（repo.rs L372）只清 `file_snapshots`+`pipeline_progress`，
  **不碰 build_progress 与 Qdrant** → 孤儿点永久泄漏。补偿必须加**哈希守卫**：
  磁盘 hash vs `list_snapshots` 不一致（文件已变/已删）→ 跳过该缺口；已删文件建议顺手清孤儿点。
- **method_id 确定性**：`make_method_id(project, file_path, class, name, start_line)`
  （domain/id.rs），且 id 内含 start_line（如 `isSectNoConvertIf@32`）→ 文件未变时重提取
  可精确按 `entity_id` 匹配方法。⚠️ 第五节"按 payload start_line/end_line 切片补源码"的方案
  在行号漂移时会取到错代码——**重提取 + entity_id 匹配更稳**（代价：多一次文件解析 + 重嵌入；
  若只想补字段可用 set_payload 免重嵌，但源码来源仍需可信）。两种路线是落地时的真实分叉。
- **code_methods 是跨项目共享单集合**（shared/collections.rs：所有项目映射到同一 CODE_METHODS）
  → 一切滚动/计数过滤必须带 `project` 条件，否则串项目。
- **Phase 2 "后台"任务实际被同步 await**（pipeline.rs L562，注释误导）→ 补偿步骤插在
  L565 与 BuildReport（L570）之间即可，无需改并发模型。
- **方案取舍结论（防重复论证）**：retry queue 覆盖不了"进程中途被杀的在途方法"与历史遗留缺口，
  且需新表迁移、与 Qdrant 现实漂移；改 select_files 需 file→method 映射表，等价于查 Qdrant
  且把未变文件重新送进 Phase 1 浪费 embed —— **缺口补偿（Qdrant 为真值）是推荐主机制**。
- **Nacos 虚拟文件边界**：prog_key=`nacos:{data_id}`、entity_id=`dt://nacos/...`；补偿的重提取
  依赖磁盘物化文件，不存在则跳过该类缺口（由 Phase 2 变更驱动兜底）。prog_key/路径分支务必与
  Phase 2（pipeline.rs L378-389）抽成共享函数，防两处漂移。

## 7. 全库消费方审计补充（2026-08-11，只读审计会话；改代码前先看此节）

### 7.1 双写/覆盖路径——跨构建会擦除已分析内容（比"从未分析"更糟）
- 本仓库 Qdrant upsert 是**整点替换**（`qdrant/repo.rs` L296-305 `PointStruct::new(id, vector, payload)`，
  `upsert_points(wait=true)`），不是 merge。code_methods 只有两个写入者，都在 pipeline.rs：
  Phase 1（L280-292，payload **无 llm_analysis 键**，向量=embed("signature comment") L215）与
  Phase 2（L494，payload 带 llm_analysis，向量=embed(llm_response) L470）。
- 跨构建时序（真实回归路径）：构建 N 文件变更 → Phase 1 覆盖写擦掉旧分析 → Phase 2 成功补回 ✓；
  构建 N+1 同文件再变 → Phase 1 再次擦掉 → **Phase 2 chat/embed/upsert 任一失败（L506-512 仅 warn）→
  点处于无 llm_analysis 态且无 mark → 增量永不重试 → 缺口固化**。
- 结论：llm_analysis 缺失缺口有两类——"从未分析"（Phase 2 首建失败）与"被 Phase 1 反向擦除"
  （历史分析丢失）。补偿都能治；但更稳的根因选项是让 Phase 1 保留旧 llm_analysis（set_payload 语义，
  见 §2），与补偿正交可并行。

### 7.2 空响应也是缺口（Phase 2 的第二个 mark bug）
- `chat` 成功但响应为空串时（pipeline.rs L452-456），llm_response="" 仍会 embed+upsert 且
  `persisted=true` → **mark_llm_analyzed**（L516-533）→ 点带 `llm_analysis=""` 且永不再试。
- 补偿扫描定义"缺口"时必须同时覆盖：键缺失（is_empty）**和空串值**——Qdrant `is_empty` 正好两者都
  匹配（见 §1），所以过滤条件不用改，但**补偿里 `""` 必须视为缺口重跑**，不能沿用 Phase 2 的
  "空串也算成功"逻辑。

### 7.3 消费方契约不一致（渲染回退差异）
- code 世界 `hit_from_payload`（search_mcp.rs L173-176）直接 `as_str()`，**无空串归一化** →
  `Some("")` 透传；config 世界 `payload_llm_analysis`（search_config.rs L144-151）trim+空→None。
- 渲染回退：Method 空→snippet 位置串（search_render.rs L69），Config 空→"暂无摘要"（L77）。
  补偿保证非空后，Method 命中展示从"位置串"变为"分析文本"，属预期改善；动渲染层测试前查
  search_render.rs L189/L233/L293/L364 四组断言。

### 7.4 mark 契约红线
- `skill/SKILL.md` L208：不要在 LLM 失败时伪造 llm_analysis 或手工标记完成；L174：方法命中
  llm_analysis 为空时查 Phase 2 日志并允许后续增量补偿。补偿的 mark 必须：真实 upsert 成功后才
  `mark_llm_analyzed(project, "method:{entity_id}", sha256(source))`，hash 与 Phase 2 同源
  （方法体 hash，体<10 字符→整文件，pipeline.rs L364-375），否则下次变更会重复分析。

### 7.5 测试影响清单（补偿落地后需回归的断言）
- ⚠️ `pipeline/test/runner.rs` + `test/expected.json` 已随 2026-08-12 清理删除（test/ tests/ 目录与 build --test 一并移除），此处的 `has_llm_analysis_on_methods=true` 断言不再存在。
- `search_mcp.rs` / `search_render.rs` 的 llm_analysis 单测（stub 输入）——同样只会更稳。
- ~~`tests/t3_verify_config_llm_analysis.rs`~~（已删）config_chunks 契约测试已随清理移除。
- `search_mcp.rs` L1218-1260 / `search_render.rs` 各断言均为 stub 输入，不受影响。
- 需新增：stale 点跳过、空响应重试、向量保留（set_payload）、mark 幂等四类测试。

### 7.6 读码工具怪癖（审计本仓库时）
- `read_file` 会把本仓库部分 UTF-8 文本文件误判为 binary：`interfaces/cli/search_render.rs`、
  `application/build/strategy/incremental.rs`、
  `application/build/service.rs`、`infrastructure/qdrant/repo.rs`、`application/build/builder.rs`
  （实际无 NUL，`file` 报 Unicode text；`pipeline/test/runner.rs` 已随 2026-08-12 清理删除）。
  绕过：`python3 -c "lines=open(p).read().split('\n'); [print(f'{i+1}|{lines[i]}') for i in range(a,b)]"`。
  搜索类工具（search_files）不受影响。

## 8. 基础设施层已落地（2026-08-11 实施会话；只改 repo.rs / traits.rs / collections.rs 三文件）

§4 的"需新增 scroll_points / 扩展 json_to_condition"已实现并叠加在工作区 WIP 之上，编译测试全绿：
- `VectorRepository`（domain/traits.rs）新增 `scroll_points`（返回 `{"id":<u64>,"payload":{...}}`）与
  `set_payload(collection, Vec<{"id":u64,"payload":{...}}>)`，均有默认实现（空列表/Ok）→ NoopVectorRepo 零改动。
- `shared/collections.rs` 新增常量 `VECTOR_NAME_BASE="base"` / `VECTOR_NAME_LLM="llm"` ——
  **pipeline/search 层必须用这两个向量名**，这是双向量契约。
- `ensure_collection` 创建 named vectors 双向量（base 必填 + llm 可选，均 Cosine+on_disk）；
  **旧单向量集合 collection_exists=true 不重建 → 存量数据不受影响**。
- `upsert` 双路径：point 带 `"vectors":{"base":[...],"llm":[...]}` 对象 → named；
  只有 `"vector"` 字段 → 单向量（既有调用方 pipeline.rs/kg_bridge.rs 零破坏）。
  Qdrant named collection 允许点缺部分向量（llm 可选）。
- `collection_info` 已支持 ParamsMap 配置（取 base 的 size），单向量路径不变。

### qdrant-client 1.18.0 编译级坑（本会话实测修过，直接可用）
- **没有 `VectorParamsMapBuilder`**，也无 `From<VectorParamsMap> for VectorsConfig`；必须手工构造：
  `VectorsConfig{config:Some(vectors_config::Config::ParamsMap(VectorParamsMap{map}))}`，
  每项用 `VectorParamsBuilder::new(size, Distance::Cosine).on_disk(true).build()`。
- **`PointStruct::new(id, vectors: impl Into<Vectors>, payload)`**：crate 有
  `From<HashMap<String,Vec<f32>>> for Vectors` 与 `From<Vec<f32>> for Vectors` → named upsert 直接传 HashMap。
- **`SetPayloadPointsBuilder::new(c, payload)` 的 payload 是 `impl Into<HashMap<String, qdrant::Value>>`**
  —— 传 serde_json::Value 报 E0277，必须先 `serde_json::from_value::<HashMap<String,qdrant Value>>` 转回。
- **`.points_selector(ids)` 接受 `Vec<PointId>`**（`From<Vec<PointId>> for PointsSelectorOneOf` 存在）。
- **`.map_err(|e| if collection_missing {Ok(())} else {Err(..)})?` 编译失败**（`?` 解出嵌套 Result）；
  改用显式 `match qdrant.set_payload(...).await { Err(e) => if !collection_missing { return Err(..) } }`。
- 测试断言变体名：`condition::ConditionOneOf::IsEmpty(IsEmptyCondition{key})` / `IsNull(IsNullCondition{key})`。
- 批量 set_payload 按 payload 内容分组（serde_json::Value 实现 Hash+Eq，可直接做 HashMap key），
  同组多点合并一次 gRPC 请求；分页 scroll 用 `ScrollPointsBuilder.limit(256).with_vectors(false)`，
  `resp.next_page_offset` 为 None 即集合耗尽。

### 验证状态
- `cargo check --release` 通过（仅 WIP 既有警告）；`cargo test --release --lib qdrant` 19/19
  （含 filter_translates_is_empty_and_is_null / filter_rejects_is_empty_with_match /
  noop_scroll_points_returns_empty / noop_set_payload_is_noop 四个新测试）。
- curl 只读复验：`project=message-center AND llm_analysis is_empty` → 1473 缺口点（next_page_offset=null），
  `is_null` 同条件 0 命中（与 §1 语义一致）。注意 `is_empty` 同时匹配缺失键和空串值（§7.2 的缺口定义成立）。
- ⚠️ 未对真实 named collection 做过 upsert/搜索运行时验证（任务禁止创建 collection）——
  落地 Phase 2 双向量写入时，首次真实运行前先确认存量单向量集合的兼容路径。

## 9. 构建层落地要点（2026-08-11 Worker B 会话；✅ 已完成并验证）

⚠️ 本节的 pipeline.rs / main.rs / builder.rs / cli-build.rs / service.rs 改动最初因工具预算中断未验证，
**后续会话已补完并全绿**（674 tests + doctor-center e2e）。接续时的收尾要点：main.rs 三处调用点补
`llm_backfill` 参数（见 §9.5）；集成测试 t3_verify_config_llm_analysis.rs 的 mock 需同步 chat 7 参签名
（+`_json_mode: bool`）与 `LlmClientProcessor::new` 5 参（+provider 字符串）。以下为读码确认的事实。

### 9.1 过滤 OR 的可行形状（json_to_qdrant_filter 不支持 must 内嵌套 should）
- 任务书里的 `{"must":[{"key":"project","match":{...}},{"should":[...]}]}` **会报错**：
  `json_to_condition` 对无 `key` 的 should 子句抛"过滤条件缺少 'key'"；且 Qdrant 语义里
  must 存在时顶层 should 只是打分加权、不参与过滤，也不能表达 OR。
- 可行做法：**拆两次扁平 scroll 再按 id 去重**，语义等价 `project AND (failed OR is_empty)`：
  `{"must":[{"key":"project","match":{"value":P}},{"key":"llm_status","match":{"value":"failed"}}]}`
  + `{"must":[{"key":"project","match":{"value":P}},{"key":"llm_status","is_empty":true}]}`，
  各限 LLM_BACKFILL_BATCH=200，HashSet<u64 id> 去重后取 min(200) 处理。

### 9.2 named vectors upsert 是整点替换 —— base 必须带上
- Phase 2/补偿成功路径若只 upsert `"vectors":{"llm":...}`，该点 Phase 1 的 base 向量会被抹掉
  （§7.1 整点替换语义在 named vectors 下同样成立）。
- 稳的写法：**一次 `embed_batch(&[base_text, llm_response])` 出双向量**，
  `"vectors":{"base":&e[0],"llm":&e[1]}`；base_text = `format!("{} {}", signature, comment)`
  （与 Phase 1 同源、确定性，重嵌开销=1 次调用）。失败路径用 set_payload（合并语义、保留向量）
  写 `{"llm_status":"failed","llm_retries":n+1}`。

### 9.3 set_payload 需要数值点 id（DefaultHasher 不可外部复算）
- upsert 收字符串 method_id，repo.rs 内部 DefaultHasher 成 u64，外部拿不到同值。
- 失败路径写状态位前先 `scroll_points(filter={"must":[{"key":"entity_id","match":{"value":method_id}}]}, max=1)`
  拿真实数值 id；同一 scroll 顺带读 `payload.llm_retries` 做重试上限（**>=3 本轮跳过**，保持 failed）。

### 9.4 空响应 = 失败（修复 §7.2 的假成功 mark bug）
- chat Ok 但响应 `trim().is_empty()` → 按失败处理：set_payload failed + retries+1，**不 mark_llm_analyzed**。
- 补偿与 Phase 2 同规则；`is_llm_analyzed(prog_key, sha256(source))` 幂等守卫（成功才 mark）。

### 9.5 开关透传链 + clap "默认开、flag 关"模式
- CLI：`#[arg(long = "no-llm-backfill", action = clap::ArgAction::SetFalse, default_value_t = true)]`
  → 不带 flag 时 true，带 flag 时 false（bool 字段直接 #[arg(long)] 只能默认 false，语义反了）。
- 透传链：main.rs Build 变体 → `handle_build`/`handle_build_all`（interfaces/cli/build.rs）→
  `BuildCommand`（application/build/builder.rs）→ `BuildServiceImpl`（**用 `with_llm_backfill()` setter
  而非改 `new()` 签名**，避免波及 grpc/wiring.rs + grpc/services/build_service.rs 两个调用方）→
  `PipelineTemplate.with_llm_backfill()`（execute 内守卫：flag && client/snapshot/embed/vector 齐 && !skip_embed）。
- ⚠️ main.rs 共 **3 处**调用点要加 `llm_backfill` 参数：--test 分支 handle_build、普通 handle_build、
  handle_build_all（本次中断时这三处尚未补，编译必失败）。

## 10. 搜索层落地（2026-08-11 主会话完成；named vectors 双向量检索）

### 10.1 基础设施补充（qdrant repo.rs，主会话加的 3 个 trait 方法）
- `search_named(collection, vector_name, vector, limit)`：`SearchPointsBuilder.vector_name(name)` ——
  **named vectors 集合搜索必须指定向量名，不指定 Qdrant 返回 400**（实测）。默认实现退化为 search()。
- `search_named_with_filter`：同上 + `.filter(json_to_qdrant_filter)`。
- `fetch_vectors(collection, ids, vector_name)`：`GetPointsBuilder.with_vectors(VectorsSelector{names})`
  按数值 id 批量取指定命名向量（rerank 数据源）。点缺失该向量时不在结果中。
- ⚠️ qdrant-client 1.18.0 编译坑：`with_vectors` 需要 `VectorsSelector`（`From<VectorsSelector> for
  SelectorOptions` 存在，`Vec<String>` 不行）；`VectorsOutput.vectors_options` 是 prost oneof
  （`vectors_output::VectorsOptions::Vectors(NamedVectorsOutput)`），不是 `named_vectors` 字段；
  `VectorOutput` 是扁平 struct（稠密数据在 `v.data`），不是枚举。
- `extracted_named_vector(point, name)` 辅助函数：从 RetrievedPoint 提取命名向量（repo.rs 末尾）。

### 10.2 search_mcp.rs（search_code 向量通道）
- 精确通道改用 `search_named_with_filter(VECTOR_NAME_BASE, ...)`；语义通道改用 `search_named(VECTOR_NAME_BASE, ...)`。
- 新增 `rerank_with_llm_vectors()`：基础过滤（阈值/name/project/exact_ids 去重）→ 对 top-50 候选
  `fetch_vectors(VECTOR_NAME_LLM)` → `cosine_similarity(query_vec, llm_vec)` → `final = 0.5*base + 0.5*sim`
  （无 llm 向量保持 base 分数）→ 按融合分降序重排。
- `hit_from_payload` llm_analysis 改为 trim + 空串→None（与 config 世界对齐）。

### 10.3 search_render.rs（Method 渲染）
- llm_analysis 非空 → "分析: 用途：/逻辑："；空 → **"分析: 暂无 LLM 分析"**（不再回退 snippet 位置串）；
  位置信息由独立"位置:"行展示。新增测试 `human_render_method_without_llm_shows_placeholder`。

### 10.4 验证（2026-08-11 实测）
- 单测：674 passed（lib）；新增 filter is_empty/is_null、noop scroll_points/set_payload、渲染占位测试全绿。
- 真实 Qdrant 临时集合（用完即删）实测：is_empty 命中缺失键点、base 搜索返回全部点、llm 搜索只返回
  有 llm 向量的点、set_payload 后 GET 确认向量完整保留。
- doctor-center e2e：63 方法全量构建 → 63/63 success；坏 model 注入 + --full → 63/63 failed（0 假成功）；
  恢复 model 增量构建 → 补偿自动补 53/63；剩余 10 个 llm_retries=3 达上限保持 failed 可辨识。
