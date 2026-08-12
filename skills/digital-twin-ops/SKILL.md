---
name: digital-twin-ops
description: Use when operating/troubleshooting digital-twin-v2.
version: 1.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [digital-twin, dt, memgraph, qdrant, xinference, search-testing, mcp]
---

# Digital-Twin v2 Operations & Search Testing

digital-twin-v2 操作与排障。铁律: 改动先提方案; 方案有疑问派 3 并行角色分析后综合再实施。⚠️ 方案必须一次给全,勿挤牙膏式分段等用户补问(2026-08-11 用户纠正)。

- **Hermes 注入 dt sense(2026-08-11)**: 插件首轮注入定案+踩坑, 详见 `references/hermes-hook-dt-sense-injection.md`/`references/dt-sense-plugin-implemented.md`/`docs/superpowers/specs/2026-08-11-pre-llm-call-injection-final.md`。⚠️ 用户修正: dt sense 感知 cwd 而非目标项目——须从 user_message 匹配注册表传路径, 勿用 payload cwd。
- **KG 查询策略(2026-08-11)**: 三层漏斗+实测 token 预算+降级语义, 见 `references/kg-query-strategy.md`。
- **KG 搜索决策规则(2026-08-11)**: 必搜/可不搜/禁止搜, 见 `references/kg-search-decision-rules.md`; 审计见 `references/kg-usage-audit.md`。
- **release 打包(2026-08-11)**: 见 `references/release-packaging.md`; 部署=软链接免安装见 `references/dt-deployment-symlink.md`。
- **opencode.go 403 + max_tokens(2026-08-11)**: 无 UA→403; max_tokens 太小→content 空。修复: UA + providers.<name>.max_tokens 配置化(默认 512)。见 `references/opencode-go-403-ua-maxtokens.md`。
- ⚠️ **read_file 脱敏陷阱 + pipeline.yaml hardlink(2026-08-11)**: read_file 显示 `«redacted:sk-…»` 是显示层脱敏, 文件里是真 key——照抄显示值 patch 会覆盖真 key! `~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` 是同一 inode hardlink(`ls -i` 确认)。恢复源: siliconflow key 在 `config/pipeline.yaml.bak.20260809184926`, openai_compatible key 在 `~/.hermes/.env` 的 `OPENCODE_GO_API_KEY`。**规则: ①含 key 配置一律 Python open() 读写, 不走 read_file/patch; ②改前 `ls -i` 查 hardlink + 备份 /tmp; ③发布包 config 模板脱敏 `«redacted:set-your-key»` 再打包, 打包后 grep "sk-" 验证**。
- **backfill 补偿必须并发(2026-08-11)**: 串行 for(1×12.8s→64min)改 buffer_unordered 后≈4min。批量 LLM 循环一律并发。
- **集合向量/clean/ignore(2026-08-11)**: ensure_collection 仅 CODE_METHODS 双向量(kg_nodes/doc_chunks 单向量否则报 vector name 错); dt clean 漏清 pipeline_progress+遗留库; ignore_files 需 3 处联动, 忽略后下次构建自动删。详见 `references/build-perf-collections-clean-ignore.md`。6-08-11)**: read_file 对含 key 文件显示 `«redacted:sk-…»` 但磁盘是真实值——**绝不能把脱敏显示值当 patch old/new_string 写回**, 否则真实 api_key 被覆盖成占位符; 且 ~/.config/digital-twin/pipeline.yaml 与仓库 config/pipeline.yaml 是同一 inode hardlink, 改一处两端都变。恢复路径与打包脱敏检查见 `references/readfile-redacted-patch-incident.md`。
- Qdrant 缺口过滤/Phase2 自愈: references/qdrant-filter-payload-api-notes.md；复验脚本 scripts/verify-qdrant-gap-filter.sh。**基础设施层已落地(2026-08-11): named vectors 双向量/scroll_points/set_payload/is_empty 过滤, 编译级 API 坑与向量名契约见参考文件 §8。**
- **Phase 2 三层自愈架构(2026-08-11 用户拍板, 已实施验证)**: 用户否决"以 llm_analysis 缺失为事实来源"的补偿方案, 拍板三层分离(基础层 AST/图谱/base 向量确定性 + LLM 增强层状态位无降级 + 补偿自愈) + base 召回/llm rerank 双向量。实施细节/状态位契约/失败注入 e2e 验证: `references/phase2-three-layer-self-healing.md`。核心变化: ① Phase 1 写 base 向量不含 llm_analysis; ② Phase 2 空响应=失败写 llm_status=failed 不 mark(修假成功 bug); ③ 构建末尾 backfill_llm_gaps 限批 200; ④ 渲染"分析: 暂无 LLM 分析"替代位置串; ⑤ main.rs 3 处调用点加 llm_backfill 参数。**诊断新规: 看到"分析: file:Ls-e"先查状态位分布(success/failed/缺失)再查 Phase 2 日志, 别按旧"双写竞争"思路(元凶实为 Phase 1 覆盖+Phase 2 失败无状态, 非 StoreProcessor)。**

## 工作群定位(Feishu「知识图谱优化群」)

该飞书群是 **digital-twin-v2 知识图谱优化的专用工作群**(用户 2026-08-08 明确确认)——本群内收到的任务默认按 dt 知识图谱/搜索/Nacos pipeline 上下文理解,不是 opencode 账号业务群。⚠️ 群用途识别陷阱:同一飞书网关入口曾长期承载 opencode 账号业务,历史会话会把本群误识别为「opencode账号」群(2026-08-08 凌晨的会话即如此)——**群可能被改名/改用途,识别群用途以当前会话上下文标注(`group: ...`)为准,不要照搬旧会话的识别结论**;不确定时用 session_search 查历史 + 直接向用户确认(用户回答一句话即可定案,无需深挖)。

## Architecture & Key Paths

- `dt` binary: `/data/myProject/digital-twin-v2/target/release/dt` (symlink `~/.local/bin/dt`); config via `~/.config/digital-twin/config.yaml`
- `digital-twin-mcp`: `~/.local/bin/digital-twin-mcp` (Python; symlink to project `mcp/mcp-server.py`) — **calls `dt` via subprocess**, no daemon needed
- `digital-twin-mcp` provides 24 MCP tools: `dt_search`, `dt_search_kg`, `dt_sense`, `dt_build`, `dt_health`, `dt_kg_sync`, `dt_backup`, plus svc/kub/jcli/kublog tools. **`nacos_sync` MCP tool removed 2026-08-09** — its backing CLI `dt nacos-sync` no longer exists, so the tool was a dead subprocess call (removal procedure: `references/cli-and-mcp-inventory.md`).
- ⚠️ **CLI reality (2026-08-09, verified via `dt --help`)**: top-level commands = `clean backup schema health memorize event learn build search sense`. **`dt nacos-sync` / `dt k8s-sync` / `dt kub` DO NOT exist** — old README/guides still list them; treat `dt --help` + `src/main.rs` Clap definitions as the ONLY truth (README in this repo drifts badly). `dt kg-sync` is **deprecated** → use `dt build --source knowledge`. `dt event <hook> '<json>'` is positional (old `--type/--entity-id` interface gone). `dt backup` has `create/list/restore/verify` subcommands; `dt clean` needs `--confirm`. Full verified inventory + MCP-tool-removal checklist: `references/cli-and-mcp-inventory.md`.
- Backends: Memgraph `bolt://localhost:7688` (KG), Qdrant REST `:6333` (vectors — **REST API answers on 6333, NOT 6334**; 6334 returns `HTTP/0.9 when not allowed`), SQLite snapshots. **HanLP was REMOVED from the codebase 2026-08-06** (service never deployed; health_check always failed → processor never ran → pure dead code). No hanlp module, processor, config block, or `:8765` service remains.
- Embed/LLM providers in `~/.config/digital-twin/pipeline.yaml` → `providers:` section

## ⚠️ PIPELINE CONFIG LOADING (fixed 2026-08-06)

`PipelineConfig::load()` now reads **fixed** `~/.config/digital-twin/pipeline.yaml` (same user-level location as `config.yaml`) — NOT relative to CWD anymore. Config file: `src/application/pipeline/config.rs` (`home_pipeline_config()` helper, no `dirs` crate). Keep `~/.config/digital-twin/pipeline.yaml` in sync with the project's `config/pipeline.yaml` (currently identical copy; a symlink also works). Symptom this fixed: running `dt search` from any directory other than the project root logged "pipeline.yaml 中无 providers 配置,使用默认配置" and fell back to default siliconflow → embed 401/0 hits. Historical context (superseded): the old CWD-relative load made the MCP server's spawn-CWD matter; the MCP fix forcing `cwd=_DT_PROJECT_ROOT` is still harmless to keep.

**User manual CLI runs hit this too** (2026-08): running `dt search ...` from ANY directory outside the project root (e.g. `/tmp`) logs `WARN pipeline.yaml 中无 providers 配置，使用默认配置` and silently falls back to default siliconflow (no API key → 401/0 hits). Only the project root works. Root cause of the convention split: `config.yaml` loads from fixed `~/.config/digital-twin/config.yaml` (main.rs `load_config()` via `dirs_like_home_config`), while `pipeline.yaml` is CWD-relative (`Path::new("config/pipeline.yaml")` in `PipelineConfig::load()`, src/application/pipeline/config.rs) — two different lookup conventions in one binary.

**Root fix (APPLIED 2026-08-06, user-approved simplified variant)**: user chose the minimal version — `PipelineConfig::load()` now reads ONLY the fixed `~/.config/digital-twin/pipeline.yaml` (via `home_pipeline_config()`: HOME + `.config/digital-twin/pipeline.yaml`, no `dirs` crate; import changed `std::path::Path` → `PathBuf`). No env-var/candidate chain. Missing file logs a WARN with a `ln -s` hint and falls back to default. Verified: `dt search` from `/tmp` → 0 WARN lines, results normal, golden-set regression still 12/12. Note: `~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` 是**硬链接（同 inode, 2026-08-11 stat 实测）**— 改一处自动同步; 旧说法「identical copy 手工同步」已过时, `ln -sf` 报 "same file" 即因硬链接。 Also note: `~/.local/bin/dt` is a symlink to `target/release/dt` and MCP's `DT_BIN` resolves to the same binary, so `cargo build --release` auto-deploys everywhere with no install step.

## Minimal approved config/client changes

For a user-approved, single-file preparation change, inspect the real repository state before editing: record `git status`, `git log -1`, and locate every `pipeline.yaml` (including symlinks such as `~/.config/digital-twin/pipeline.yaml`). Do not assume a request for “two pipeline files” means two tracked files; verify whether the second path is the same file or a symlink. Preserve API keys exactly and never run a real `dt build` when the request explicitly excludes it.

For SiliconFlow chat payload changes, inspect every chat implementation (the pipeline client and the infrastructure client), not only the first `ChatRequest` type. Add provider-specific fields only where supported: for DeepSeek-V3.2, serialize `enable_thinking: false`; for a shared request DTO, use an optional field with `skip_serializing_if` so unrelated models retain their prior payload shape. Keep the change minimal and verify the final diff for accidental secrets or unrelated edits.

Verification/commit gate for this class of change: run `cargo fmt --check`, `cargo check --release`, and focused unit/integration tests; do not combine multiple test filters in one `cargo test` invocation (Cargo accepts one filter). Run `git diff --check`, inspect `git diff --stat` and the full diff, then commit only the requested files with the exact user-specified message. Record warnings separately from failures and report the commit SHA, effective config values, tests, and whether a real build was intentionally skipped.

A session-specific checklist for this workflow is in `references/minimal-approved-change-checklist.md`.

## ⚠️ config.yaml `scanner:` 段是死配置 — ignore 列表从未生效 (2026-08-10 代码追踪确认)

用户配置的所有 ignore 规则(ignore_dirs/ignore_ext/ignore_files)**实际从未被加载**, 构建用的一直是 `ScanConfig::default()` 硬编码值:

- `src/application/build/service.rs:67` — `BuildService::new()` 写死 `scan_config: ScanConfig::default()`; `with_scan_config()`(service.rs:75)**全代码库零调用**。
- main.rs 的 `DaemonConfig` 只反序列化 `projects/services/batch` 等, **没有 scanner 字段**; `rg "scanner" src/main.rs` 无命中。
- 实际生效 = 默认值(types.rs:375): ignore_dirs 仅 13 项 `node_modules .git target build __pycache__ .venv dist .next vendor .idea .vscode coverage .nyc_output`; ignore_ext 仅 19 项(`.class .jar .war .so .dll .exe .bin .png .jpg .jpeg .gif .svg .ico .zip .tar .gz .bz2 .pdf .lock`); max_file_size 500KB。
- **匹配语义 = 目录名单段**(scanner.rs `collect_files`/`collect_document_files` 用 `entry.file_name()` 查 HashSet): 配置里 `node_modules/.cache`、`target/debug`、`test/fixtures`、`.mvn/wrapper` 这类**多段路径条目永不匹配**(没有目录会叫这个名字)。即"Duplicate search hits"一节建议的 `add test/fixtures to scanner.ignore_dirs` 按当前实现**无效**——必须先修匹配逻辑(路径前缀匹配)。
- **`ignore_files` 字段在 `ScanConfig` struct 中不存在**(types.rs:362 只有 ignore_dirs/ignore_ext/max_file_size/document_extensions/max_doc_file_size), scanner.rs 无任何按文件名忽略逻辑(仅硬编码 `.min.js`/`.bundle.js`/`.generated.` 三条)。

症状: 用户以为配了 ignore 但构建仍扫描 charts/public/target 等; 排查第一站是查 service.rs 构造点, 不是看 config.yaml。

**修复方案 (2026-08-10 提出, ✅ 同日已实施 — 见下方「scanner ignore 配置生效链路(2026-08-10 修复)」一节)**: ① `ScanConfig` 加 `ignore_files` 字段; ② `DaemonConfig` 解析 `scanner` 段 → 构造 `ScanConfig`; ③ `BuildService` 构造时接 `with_scan_config`; ④ scanner.rs 目录按「单段名 OR 相对路径前缀」匹配、文件按 `ignore_files` 匹配。配套配置: ignore_dirs 补 `charts docs logs .mvn .weave static assets runtime libs uploads img doc`, ignore_ext 补 `.pyc .pyo .log .iml .tsbuildinfo .sqlite .map`。**用户已拍板 (2026-08-10)**: docs/tests **不忽略**(保留索引), public/assets/static **全部忽略**; 改后重跑增量构建验证(`files_scanned` 应明显下降)。

**64 项目噪音普查 (2026-08-10 实测)**: 一级目录 charts×36、public×12、docs×10、.mvn×10、tests×8、logs×6、.github×5、.weave×3、static×2(均未被默认 ignore 覆盖); 二级 assets×18、static×10、runtime×10(.weave/runtime)、libs×3、build×3、logs×2、docs×2、uploads/img/doc 各 1; target×35 已被默认覆盖无需处理。完整普查数据与代码追踪: `references/scanner-ignore-audit.md`。

## Provider Routing (pipeline.yaml)

> 关联:OpenCode-Register 的 MCP 化工具清单与验收状态见 `references/opencode-register-mcp-tooling.md`(OCR 已 MCP 化,25 工具,用户决策"只要 MCP 不要 CLI,人工走网页")。

```yaml
providers:
  embed_provider: siliconflow   # "siliconflow" | "xinference"
  rerank_provider: siliconflow
  llm_provider: siliconflow     # 远程源(Nacos/Jenkins)同样生效, 见下方「远程源 LLM 路由」
  xinference:
    url: http://localhost:9997/v1
    api_key: ""
    model_embed: bge-m3          # local Xinference model IDs
    model_reranker: bge-reranker-v2-m3
    model_llm: qwen3.5
  siliconflow:
    url: https://api.siliconflow.cn/v1
    api_key: ""                  # pipeline.yaml 优先, 空则回退 env SILICONFLOW_API_KEY
    model_embed: BAAI/bge-m3
    model_reranker: BAAI/bge-reranker-v2-m3
    model_llm: Qwen/Qwen3-14B   # ⚠️ 勿用 Qwen/Qwen3.5-9B(推理模型, content 恒空, 见下方排障)
```

Local Xinference serves OpenAI-compatible API at `:9997` (verify: `curl -s :9997/v1/models`). If a capability's provider is unreachable, the router falls back to the other provider — which can silently route to siliconflow and 401.

### 远程源(Nacos/Jenkins)LLM 路由 —— 已统一(2026-08-08)

`dt build --source nacos/jenkins` 的 LLM 客户端原本**硬编码 XInferenceChatClient**(无视 `llm_provider` 配置),F5 自检也强制要求 xinference。2026-08-08 两 commit 修复:

- `c4d4251` — F5 自检放宽:`validate_xinference_for_remote_source()`(build.rs)非 xinference provider 直接 `Ok(())`,仅 xinference 才做 localhost:9997 TCP 自检。Nacos/Jenkins 远程源现在**支持 siliconflow 云 LLM**。
- `595c64e` — 提取公共 `build_llm_client(&PipelineConfig, max_concurrent)` 路由函数,普通文件管线 `run_pipeline_analysis` 与 `handle_nacos_build` 共用;`SiliconFlowChatClient::new` 签名改为 `(base_url, api_key, max_concurrent)`(api_key 来自 pipeline.yaml 或 env,空则回退 env)。
- 改这两处时要同步:单测 `validate_tests`(rejects→allows 语义翻转)、`infer_client::tests::silicon_flow_chat_client_can_be_constructed`(新签名)。已验证 8+2 pass。
- ⚠️ `dt build --help` 里「nacos 需 llm_provider=xinference」是**过时文案**(build.rs 帮助串未同步),实际已放行 siliconflow——别被 help 误导。

## ⚠️ JS 项目构建崩溃 `tcache_thread_shutdown(): unaligned tcache chunk detected`(2026-08-11 修复)——static mut UB

**症状**:`dt build` 构建前端项目(如 copartner-h5,大量 .js/.ts)时,SIGABRT 崩溃,`tcache_thread_shutdown(): unaligned tcache chunk detected`,**无 Rust backtrace**。gdb 定位:崩溃在 worker 线程的 `ts_parser_parse → ts_tree_new → ts_calloc_default → malloc: unaligned tcache chunk`——表面像 tree-sitter C 库 bug,实际是 dt 自己代码的 use-after-free。

**根因**:`src/infrastructure/parser/ts_javascript.rs` 有遗留 hack——4 个 `static mut` 全局 String(`CUR_PROJECT/CUR_FILE/CUR_MODULE/CUR_CLASS`,注释"临时占位函数——将在 parse() 中正确设置")。`parse()` 在每个 worker 线程里 `unsafe` 写它们(`CUR_FILE = file_path.clone()` 触发 drop+alloc),而多线程并发解析 JS 时其他线程可能正在读 → **use-after-free → 堆损坏 → glibc tcache 检测触发 SIGABRT**。只有 JS 解析器有(其他语言解析器用参数传递),所以只有前端项目崩;单文件 `--file` 不崩(单线程无竞争)。

**修复(已应用)**:删掉 4 个 `static mut` + `project_dummy()`/`file_path_dummy()` + parse() 里 3 处 unsafe 写块 + `CUR_CLASS` 写;`collect_methods` 改为接收 `project/file_path/module` 参数(顺带修复了 JS 方法 `file_path/project/package_or_module` 恒为空 String 的隐藏 bug)。检查过其他 parser 无同类 static mut。

**验证(2026-08-11)**:修复前 copartner-h5 构建立即 SIGABRT;修复后 exit 0,扫描 345 文件(忽略 node_modules 后真实源文件数)、提取 121 方法。注意:copartner-h5 有 638MB node_modules(27760 文件),合并后的 ScanConfig 忽略它后只剩 ~345-409 个真实源文件——排查"文件数异常多"先查 node_modules 是否被忽略。

## 搜索"来源/位置"统一显示磁盘全路径(2026-08-11 扩展,展示层)— code 世界补 project 字段

**背景**:doc/config/knowledge 世界用 `source_ref`(dt://doc URI)经 `ProjectPathResolver` 显示磁盘全路径;code 世界(方法)原本只有 `file_path`(相对路径)显示"位置: src/..."——用户反馈不统一。

**修复(已应用,纯展示层)**:
1. `SearchHit` 加 `project: Option<String>` 字段(`#[serde(default)]` 保持 JSON 契约向后兼容)
2. `hit_from_payload`(search_mcp.rs)从 Qdrant payload 读 `project`(数据早已存在,无需重建索引)
3. 其余 12 处 SearchHit 构造点补 `project: None`
4. `ProjectPathResolver` 加 `root_for(project)` 方法;`render_hit` 的 code 分支:有 project 且 resolver 命中 → `位置: {磁盘根}/{file_path}:L{s}-{e}`;否则回退相对路径

**验证**:667 tests 全绿(新增 3 个:解析成功/未知项目回退/无 project 回退);真实搜索 code 方法显示 `/data/aflmProjects/.../X.java:L132-134`;JSON 契约不变(file_path 仍相对路径、source_ref 仍 dt:// URI、project 为新增可选字段)。

**注意**:改 `SearchHit` 字段时,全库 SearchHit 字面量构造点约 13 处(含测试),漏一处会报 E0063;编译错误会精确指出位置。

## 搜索"来源"显示磁盘全路径(2026-08-11 新增,展示层)— 团队评审通过后实施

用户需求:搜索结果"来源"字段从 `dt://doc/pay-center/xxx.md` 显示为磁盘全路径(用户原话"替换来源字段,通过项目名配置的路径+dt的路径,生成磁盘全路径,不破坏原本设计")。**3 角色团队评审结论:必须做展示层,不能改 `SearchHit.source_ref` 数据值**(兼容性风险:kg_bridge 里 config 世界 `id: source_ref` 当 point id;fusion 去重 key=world:id 不依赖 source_ref;purge 用 doc_id;dt://nacos 前缀的 file_type 推断依赖 URI)。

**实施(2 文件)**:
1. `search_render.rs` 新增 `pub struct ProjectPathResolver`(项目名→磁盘根路径表,`resolve_doc_source()` 把 `dt://doc/{project}/{rel}` 解析为磁盘全路径;仅 `dt://doc/` 前缀、项目在表、rel 不含 `..` 才解析,其余返回 None 保留原值);`render_human`/`render_hit` 加 `&ProjectPathResolver` 参数
2. `build.rs` 新增 `pub fn project_roots_from_config()`(读 `~/.config/digital-twin/config.yaml` projects 段,base+别名目录→绝对根路径表,serde_json::Value 宽松解析,失败返回空表);`handle_search` 人类渲染分支构造 resolver 传入;JSON 分支不动

**关键约定**:只改 CLI 人类渲染输出;**JSON(`--json`/MCP)的 `source_ref` 保持原始 URI 不变**(已验证)。虚拟来源(dt://nacos/、dt://entity/、未知项目)保留 URI。新增测试 `human_render_resolves_doc_source_to_disk_path`(含 nacos 保留原值断言)。

**验证(2026-08-11)**:`dt search "bootstrap" --world knowledge` → `来源: /data/aflmProjects/unimportant/uvp-pay-center/src/main/resources/bootstrap.yml`;`--json` 里 source_ref 仍是 `dt://doc/pay-center/...`。664 lib 测试绿。

## ⚠️ Qdrant 集合不存在时搜索报 WARN 堆栈(2026-08-10 修复)——集合缺失=空结果

**历史缺陷(已修复 2026-08-10)**:`dt clean` 删掉 Qdrant 集合后(或从未构建某世界),`dt search "商品" --world code` 打印两行 WARN 错误堆栈(`对 code_methods 的 Qdrant 搜索失败: ... Collection 'code_methods' doesn't exist!` + `关键词兜底 scroll 失败`),用户要求"集合不存在应返回空结果而非失败内容"。

**根因**:Qdrant gRPC 对不存在集合返回 `Not found: Collection 'xxx' doesn't exist!`,但 `QdrantRepo` 的 `search`/`search_with_filter`/`scroll_payloads`(src/infrastructure/qdrant/repo.rs)把它当普通 `Err` 冒泡到 search_mcp.rs 的 `tracing::warn!`。

**修复**:repo 层新增 `fn collection_missing(err: &str) -> bool`(检测 `doesn't exist` / `does not exist` / `not found`,小写匹配),三个读方法统一 `Err(e) if collection_missing(&e.to_string()) => Ok(vec![])`——集合不存在=该世界无数据,语义正确返回空,所有上层调用(search_mcp code 世界三通道 + knowledge 世界 `recall`)自动优雅降级,无需逐个改调用点。scroll_payloads 循环内对首页集合缺失同样返回空。新增单测 `collection_missing_detects_qdrant_errors` 锁行为(含非误判用例)。

**验证闭环(2026-08-10)**:clean 后 `dt search "商品" --world code` 输出只有 `搜索: ...` + `(无结果)`,零 WARN;knowledge/doc 世界同样干净;`--json` 纯 JSON 可解析 total=0。663 lib 测试绿。

## ⚠️ `dt clean --confirm` 曾只清 Memgraph,Qdrant/SQLite 是 noop(2026-08-10 修复)

**历史缺陷(已修复 2026-08-10)**:`run_clean`(src/interfaces/cli/cleanup.rs)的 Qdrant 部分写死 `NoopVectorRepo` + `qdrant_removed = 0`,SQLite 部分 `snapshots_cleared = true` 是硬编码假值——**从未真正删除**。main.rs 调用 `run_clean` 也只传 `connect_memgraph()`,没传 vector/snapshot。结果:`dt clean --confirm` 只清空 Memgraph,决定搜索结果的 Qdrant 向量原封不动(实测 11095+16+274 点残留),搜索依然有结果,用户以为 clean 失灵。

**修复内容**:
1. `traits.rs` `SnapshotRepository` 新增带默认实现的 `clear_all()`(默认 noop),`sqlite/repo.rs` 覆盖为 `DELETE FROM file_snapshots` + `DELETE FROM build_progress`
2. `cleanup.rs` `run_clean` 签名扩为 `(confirm, graph, vector, snapshot)`,内部对 Qdrant `list_collections()` + 逐个 `delete_collection()`,SQLite 调 `clear_all()`,后端未连接时打印 ⚠️ 而非静默假成功
3. `main.rs` 调用点补传 `connect_vector().await` 与 `connect_snapshot().await`(注意 `Arc<dyn>` → `&dyn` 要用 `.as_deref()` 不是 `.as_ref()`,否则 E0277)
4. 测试同步:cleanup.rs 两处 `run_clean(x, None)` → `run_clean(x, None, None, None)`(E0061)

**验证闭环(2026-08-10)**:clean 前 Qdrant 3 集合 11095/16/274 点 → `dt clean --confirm` 输出"移除的集合: 3 (code_methods/doc_chunks/kg_nodes) + 已删除快照/进度行 5517" → clean 后 Qdrant 0 集合、Memgraph 0 nodes/0 rels、`dt search "queryGoods"` total=0。662 lib 测试绿。

## ⚠️ scanner ignore 配置生效链路(2026-08-10 修复)——配置曾完全无效

**历史缺陷(已修复 2026-08-10)**:`config.yaml` 的 `scanner:` 段(ignore_dirs/ignore_ext/ignore_files)此前**完全没有被代码读取**——`BuildServiceImpl::new()` 写死 `ScanConfig::default()`(service.rs:67),`with_scan_config()` 全代码库零调用,main.rs 的 `DaemonConfig` 无 scanner 字段。用户精心配置的忽略规则全是死配置,实际构建只用硬编码默认(13 目录+19 扩展名)。症状:charts/public/assets/static 等噪音目录全部进入构建,LLM 分析被 charts 文件拖死(Chart.yaml 一个文件重试 4 次×20-30s,Java 文件轮不到,构建被用户中断)。

**修复内容(2026-08-10, 已 build+662 测试绿)**:
1. `types.rs` `ScanConfig` 新增 `ignore_files: HashSet<String>` 字段 + 默认值(composer.lock/Gemfile.lock/Cargo.lock 等)
2. `scanner.rs` 新增 `pub fn dir_is_ignored(rel, name, ignore_dirs)` — 支持**单段目录名 OR 相对路径前缀**(`node_modules/.cache`、`target/debug` 这类多段条目现在生效;旧逻辑只按 `file_name()` 单段匹配,多段条目永不命中);`collect_files`/`collect_document_files` 同步升级
3. `main.rs` `DaemonConfig` 加 `scanner: ScannerFileConfig` 段;新增 `scan_config_from(&cfg)` 合并函数(用户配置**与内置默认合并**而非覆盖——否则用户列表缺 node_modules/target 会丢失基础保护);`handle_build`/`handle_build_all` 加 `scan_config` 参数
4. `builder.rs` `BuildDependencies` 加 `scan_config` 字段(注意:不能放 clap `BuildCommand` 结构体里,clap derive 会要求 FromStr);`run()` 里 `.with_scan_config(deps.scan_config)`
5. `build.rs(CLI)` `collect_project_files` 接受 `&ScanConfig` 并应用 ignore_dirs/ignore_files/ignore_ext —— **pipeline 分析(LLM 文件分析)此前完全无视 ignore 规则**,只跳隐藏文件+固定二进制扩展,这是 charts 拖死构建的直接原因
6. 配置同步:两处 `config.yaml`(用户级+仓库级)ignore_dirs 补 charts/charts-dev/charts-test/logs/.mvn/.weave/static/assets/runtime/libs/uploads/img/unpackage/miniprogram/.hbuilderx/.umi 等;ignore_files 补 package-lock.json/pnpm-lock.yaml/yarn.lock 等。⚠️ 用户明确 **docs/tests 目录不忽略**(不要加进 ignore_dirs)

**验证方法**:`cargo test --release --lib scanner`(9 pass,含新增 path-prefix+ignore_files 用例)+ 构建对比日志:`扫描 N 个文件` 从 430 → 406(pay-center,charts 被滤),`grep charts/ daemon.log` 处理记录归零。新增测试 `collect_files_respects_path_prefix_and_ignore_files` 锁行为。

## GLM Coding LLM ops — 并发纪律 / 路由 / JSON (2026-08-10 实测)

> 文件类型→LLM 分析路径三通道(Java 方法级 vs 文档块级) / chunk_concurrency 与 max_concurrent 维度语义 / pipeline.yaml 硬链接 / api_key 打码陷阱: references/pipeline-llm-analysis-paths.md

- **`openai_compatible.max_concurrent`(旧名 `glmcoding`)— 并发纪律(2026-08-11 改名)**: 旧底座时代 32 曾致 429/502 风暴(当时结论「≤4」); **2026-08-11 opencode-go 底座 + max_concurrent=64 全量 65 项目实测: 0×429 / 0×4xx / 0×502, 1607 次成功** — **≤4 已过时**。2026-08-11 重构: provider 段改名 `openai_compatible`(通用网关), 旧键/旧值经 alias+双匹配兼容。检查生效配置: `python3 -c 'import yaml,pathlib;print(yaml.safe_load(pathlib.Path.home()/".config/digital-twin/pipeline.yaml")["providers"]["openai_compatible"]["max_concurrent"])'`。
- **✅ 并发模型已统一为「单参数块级并发」(2026-08-11 重构)**: 旧版多旋钮互相打架: `glmcoding.max_concurrent`(客户端 semaphore) / `inference_server.max_concurrent`(引擎文件级闸门, 曾=1 卡死构建, 实测 41 分钟仅 215 请求并发=1) / **`PHASE2_CONCURRENCY=4` 硬编码**(方法级并发锁死 4, 用户改 32 无效) / `llm.chunk_concurrency`(单文件 chunk 限流)。**现已统一单一参数** `providers.<llm_provider>.max_concurrent`: ① `inference_server` 段删除, 引擎文件级读 `llm_provider_max_concurrent()`; ② `PHASE2_CONCURRENCY` 常量删除, Phase 2 方法级经 BuildDependencies.llm_concurrency 读同一值(grpc 占位 16); ③ `chunk_concurrency` 字段删除, 单文件内 chunk 全并发发起, 全局在飞由客户端 semaphore 统一限流——**块为并发粒度, 全局并发未满就继续取块**。实测: doctor-center 63 方法 32 并发 5s 全启动(旧 4 并发 16 波), .gitlab-ci.yml 12 chunks 16s(旧串行 4m22s)。证据链: references/engine-concurrency-gate-audit.md + provider-config-refactor-risk-map.md。
- **实际并发测量法(2026-08-11)**: ① 数 `OpenAI-Compatible 响应` 日志条数 ÷ 运行秒数; ② 完成间隔 ≈ 单请求耗时=串行(并发1), ≈ 耗时/N=并发 N 生效。实测例: 41 分钟 215 请求, 11.4s/请求 ≈ p50 10.7s → 并发 1(旧配置)。
- **429 计数必须提取 status 字段 + 时间窗过滤(2026-08-11 再确认)**: 正则提取日志行 `"status":"429 ..."` 完整字符串才是真 429; 且按时间戳过滤本次构建窗口(`>= '2026-08-11T11:38'`)— 上午旧构建的 429(09:00 时段 581 + 11:00 时段 976)会混入误导。**慢≠429**: 本次构建 41 分钟只完成 215 请求(并发 1)但 429 仅开局 22 次, 先测并发再归因。
- **Phase 2 与 pipeline 文件分析的并发路径不同(2026-08-11 修正)**: Phase 2 方法分析(build/pipeline.rs:652)并发 = **硬编码 `PHASE2_CONCURRENCY=4`**(pipeline.rs:32, 与 max_concurrent 无关!); pipeline 文件分析(ProcessorEngine)旧版被 inference_server.max_concurrent 卡死 → 同一构建里可能"Phase 2 4 并发 + 文件分析串行爬行"。2026-08-11 重构后文件分析读 `llm_provider_max_concurrent()`(单旋钮), 但 **Phase 2 仍是硬编码 4**(待配置化)。并发四维度全景 + Java 文件三条 LLM 路径 + 待实施方案: `references/phase2-concurrency-audit.md`。分阶段看日志: `LLM 方法分析开始/完成`(Phase 2) vs `OpenAI-Compatible 响应`+`StoreProcessor start`(pipeline)。
- **代理不是慢的主因(2026-08-11 实测)**: 构建进程环境带 `http_proxy=127.0.0.1:7897`(Clash verge-mihomo)时, curl 走代理 vs `--noproxy '*'` 直连: 2.52s vs 2.34s, 差异可忽略 — 不用先怀疑代理。
- **deepseek-v4-flash 真实负载耗时基准(2026-08-11)**: 长 prompt 代码分析(pipeline 文件分析 + Phase 2)单请求 p50 10-12s / p95 27-29s / max 51s, **全天一致**(凌晨 p50=12.2s 与中午 10.7s 相同) — 是模型/上游真实水平不是异常; 技能里 2.8s 是 kimi-k3 小请求实测, 别当基准。慢的主因是并发, 不是单请求延迟。
- **验证「是否被限流」的正确方法(2026-08-11 实测, 三个陷阱)**: ① **`grep "429"` 是误报陷阱** — daemon 日志 JSON 里的数字(elapsed_ms 等含 42/429 的值)会假命中, 实测 2743 次"429"全为误报; ② **真实 HTTP 状态码不进 daemon 日志** — `status=500` 等 WARN 行在进程 stdout/终端, daemon 日志只有 `暂态错误`/`传输失败`/`返回 HTTP 502` 这类消息文本; ③ **统计必须加完整日期前缀过滤**(`grep 2026-08-11T`)— 昨天/前天的 502(如 08-09 有 1407 条)混入会让结论错得离谱。正确做法: 按消息类型分类 WARN + 日期过滤 + 对比成功数(`LLM 方法分析响应成功`)与构建汇总(`流水线分析完成: N 个成功, 0 个有错误`)。
- **失败文件靠增量构建自动补偿**: 只有 LLM→Embedding→Qdrant upsert→SQLite progress **全链路成功**才标记完成; 失败文件下次增量构建自动重试(本次 135/135 补齐)。核对 daemon 日志 `流水线分析完成: 分析了 N 个文件, N 个成功, 0 个有错误 (跳过 M 个未变更)`。
- **构建进程残留陷阱**: 用户以为构建已结束但 `pgrep -af 'dt build'` 仍有进程在跑(继续以 32 并发产生失败)。先 kill 再改配置重跑, 不要直接重跑。
- **日志时间过滤陷阱**: grep 模式 `00:4x` 会误匹配其他日期的 `13:00:5x`(昨天 502 混入本次统计)。用完整前缀 `2026-08-10T00:` 过滤。
- **provider 路由统一 (2026-08-09, 2026-08-11 改名)**: build.rs `handle_build` 原先只分支 `xinference`/`_`(默认 siliconflow), `llm_provider: glmcoding` 会**静默落进 SiliconFlow 分支** → 日志 `SiliconFlow request_start` 是真实请求。修复: 统一走 `build_llm_client()`。2026-08-11 改名后路由分支为 `"openai_compatible" | "glmcoding"`(双匹配兼容旧值)。诊断对照: 配置 openai_compatible + 日志 siliconflow = 先查路由分支。
- **LlmClientProcessor 日志动态化 (2026-08-09)**: 原先 4 处日志硬编码 `provider="siliconflow"`(file_start/chunk_start/chunk_done/file_done), 现构造时注入 provider 名(`LlmClientProcessor::new(client, model, provider, registry, config)`), 日志 `provider=` 字段可信。
- **JSON 严格输出 — 已改为 json_mode 条件附加 (2026-08-09 引入, 2026-08-10 修复误伤)**: 原实现 GLM Coding chat 请求体**无条件**加 `response_format: {"type":"json_object"}`; 2026-08-10 发现这**误伤 Phase 2 方法分析**——见下方「opencode-go 400: json_object 要求 prompt 含 'json'」。现 `ChatClient::chat()` 增加 `json_mode: bool` 参数: `true` 才附加 response_format(pipeline chunk 分析 `processors/llm_client.rs` 传 true), `false` 不加(Phase 2 方法分析 `build/pipeline.rs` 传 false)。解析层 `extract_json_object()` 平衡括号扫描替换 `find('{')+rfind('}')` — 容忍 markdown 围栏、前后赘述、字符串内 `}`(单测 `extracts_balanced_json_with_trailing_text`)。错误对照: `trailing characters at line N column 1` = JSON 后有额外文本; `key must be a string at line 1 column 2` = 单引号/markdown 包裹。首次失败带 JSON_CORRECTION 提示重试一次, 仍失败降级, 增量构建补偿。改 trait 签名时同步改测试 MockChatClient 补 `_json_mode` 参数(E0050)。
- **单文件增量构建**: `dt build --path <root> --file <file>`(target_file 贯穿 BuildDependencies→PipelineTemplate→scanner, 只扫描该文件)。OpenCode after_edit hook 已注册(`/home/luis/opencode.json`)→ `scripts/opencode-after-edit.sh`(flock 防并发, 日志 `/var/log/digital-twin/opencode-build.log`); 脚本实际执行 `cargo run --manifest-path <root>/Cargo.toml -- build --path <root> --file <file>`, 文档若写 `dt build` 是简化写法。

### openai_compatible provider = 通用 OpenAI 兼容客户端 — 可指向 opencode-go 等任意端点 (2026-08-10 实测, 2026-08-11 从 glmcoding 改名)

`OpenAICompatibleChatClient`(infer_client.rs, 旧名 `GLMCodingChatClient`)与 `build_llm_client()`(build.rs)只依赖 `base_url + api_key + model`: 请求 `{url}/v1/chat/completions` + `Authorization: Bearer`, **没有锁死 glmcoding.cn**——`providers.openai_compatible.url` 可指向任意 OpenAI 兼容底座(glmcoding / opencode-go / 任意厂商网关)。这正是用户「通用化」设计的含义。

- **接入 opencode-go 套餐 (2026-08-10 实测通过)**: `url: https://opencode.ai/zen/go`(**不带 `/v1`**) + `model_llm: deepseek-v4-flash|kimi-k3|deepseek-v4-pro`, 无需改代码。key 从 `~/.hermes/.env` 的 `OPENCODE_GO_API_KEY` 取(auth.json 只存指纹, 不是真 key)。实测: POST `{url}/v1/chat/completions` 200、2.8s、`finish_reason=stop`、content 非空纯 JSON。
- ⚠️ **url 必须是不带 `/v1` 的根地址 (2026-08-10 实测踩坑, Phase 2 全 404 根因)**: 客户端拼接约定为 `{url}/v1/chat/completions`(chat) 与 `{url}/v1/models`(健康检查)。若配 `https://opencode.ai/zen/go/v1`, 实际请求变成 `/v1/v1/chat/completions` → **Phase 2 全部 404 Not Found**, 而健康检查 `/v1/models` 恰好 200——「dt health 绿、构建全挂」的迷惑组合。与 SiliconFlow 约定相反(siliconflow.url 带 `/v1`), 别混用。改配置两端同步后, 重跑增量构建自动补偿失败文件; 先 `pgrep -af 'dt build'` 确认无残留进程。
- ⚠️ **opencode-go 400 Bad Request 根因 (2026-08-10 实测)**: Console Go 上游对 `response_format: {"type":"json_object"}` 严格校验, **prompt 必须含 "json" 字样**否则返回 `400 invalid_request_error: Prompt must contain the word 'json'`。Phase 2 的 `code_analysis.yaml` system prompt 不含 json → 全量 400 风暴(日志 `Phase 2 失败 <method>: OpenAI-Compatible 返回 HTTP 400`)。**Phase 2 方法分析输出纯文本两行(用途：/逻辑：), 本就不该声明 json_object**——加了不仅 400, 即便通过也会让推理模型把 100 token 思考吃光(content 空)。修复 = 上面 json_mode 条件附加; 验证: 全量构建 GLM 响应 100% 200 OK。
- ⚠️ **「重启后 Phase 2 不跑」排查陷阱 (2026-08-10)**: 修复 json_mode 后重跑增量构建, 日志只见 pipeline chunk 分析(如 `.gitlab-ci.yml`)、`Phase 2: N 个待分析` 不出现——这是**增量快照判定方法已分析过**(或 `--no-pipeline` 只跑 builder), 不是修复没生效。验证 json_mode 修复必须 `dt build --path <proj> --full` 强制全量触发 Phase 2, 或用后台任务跑完整构建看状态码分布。`构建完成: 扫描 N 个文件, 变更 M 个, 共 0 个方法` = 增量跳过了方法提取, 别误判为回归。
- ⚠️ **opencode-go 夜间不稳定(2026-08-10/11 实测)**:00:00-03:00 时段常 500/超时 30-40s(`OpenAI-Compatible 暂态错误，准备重试` / `请求传输失败 ... error sending request` WARN),GLM 客户端 3 次退避重试后通常成功。**别被中途 WARN 吓到**——判据是最终汇总(`流水线分析完成: N 个成功, 0 个有错误`),不是中途 500/超时日志。大构建(全量几千方法)尾段必然出现这类 WARN,只要汇总 0 错误就是成功。
- **端点探测脚本**: 配置任何新 OpenAI 兼容端点前先跑 `scripts/llm_endpoint_probe.py --url <根地址> --model <模型>`(请求体与 dt GLM 客户端完全一致), 确认 200 + content 非空再改配置, 别直接改配置重构建。
- `protocol` 字段(config.rs `OpenAICompatibleProviderConfig`)当前实现不分支, 固定按 openai 处理——不要动它, base_url 才是决定打哪里的字段。
- ⚠️ **并发纪律随底座变化(2026-08-11 修正旧结论)**: 旧实测「max_concurrent ≤4 对 opencode-go 同样适用, 32 会 429/502 风暴」是 SiliconFlow/XInference 底座时代的结论 — **2026-08-11 已实测 opencode-go + 64 全量构建零限流**(见上)。换底座/换并发后先小批量验证再全量, 别沿用旧结论; 但 429/502 风暴后仍有恢复惯例: 先 `pgrep -af 'dt build'` 清残留进程再改配置重跑。
- ⚠️ **改配置同步(2026-08-11 修正)**: `~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` 是**硬链接**(同一 inode, `ln -sf` refused "same file" 即证据; 验证 `stat -c '%i'` 两路径) — **改一处两处同时变**, 无需手工同步。但仍需确认两边是同一 inode(曾有过独立拷贝时期), 若 inode 不同则要双端同步。
- 验证顺序: 先探测端点可用(真实 nacos_config prompt + 样例 + max_tokens 4096, 确认 `choices[0].message.content` 非空), 再改配置。此属配置改动, 按用户规矩先方案后实施。

### ⚠️ SiliconFlow 推理模型陷阱 — content 恒空, dt 解析 EOF(2026-08-08 实测)

`Qwen/Qwen3.5-9B`(及 Qwen3.5 全系)是**推理模型**: 输出全部进 `reasoning_content`, `content` 为空; `max_tokens` 被思考过程吃光时 `finish_reason: length` 且正文 0 字。dt 的 `llm_client` 只读 `content` → 每条配置持续 `块 0 JSON 解析失败... EOF while parsing a value at line 1 column 0` WARN + 全部降级,**config_chunks 一个点都不进**(构建看起来在跑, 实际零产出)。

- 症状识别: daemon 日志大量 `EOF while parsing a value at line 1 column 0` + `重试后仍无法解析, 降级`, 但 config_chunks 计数不变、无 HTTP 报错(不是 402, 不是网络)。
- **解决: 换非推理模型 `Qwen/Qwen3-14B`**(实测 `finish_reason: stop`、content 纯 JSON、reasoning=0, 完全满足 dt 解析)。已验证备选: `deepseek-ai/DeepSeek-V3.2`(content 正常, 无 reasoning)。
- ⚠️ **单请求探测通过 ≠ 真实构建负载稳定(2026-08-08 实测)**: Qwen3-14B 单请求 2-3s 返回纯 JSON, 但 `dt build --source nacos` 真实构建**首条请求即挂 ~13 分钟**(120s 超时×多重重试后 `502 Bad Gateway`), 进程 CPU 0%、daemon 日志静默、config_chunks 零入库——构建像在跑实则卡死。识别: 日志里 `creating new connection...` 之后 >2-3 分钟无任何后续, 或出现 502。此时**直接杀进程换 `deepseek-ai/DeepSeek-V3.2` 重跑**, 别干等。教训: 模型选型不能只看单请求探测, 连续/并发负载下表现差异大。
- **换模型前先模拟 dt 完整请求验证 content 非空** — 别只发 "ping" 小请求(小请求 max_tokens 小, content 也空, 会误判): 用真实 nacos_config prompt(`config/prompts/nacos_config.yaml` 的 system+prompt) + 样例 yaml + `max_tokens: 4096`, 检查响应 `choices[0].message.content` 长度 > 0。脚本: `scripts/sf_model_probe.py`。
- ⚠️ 另一个小坑: 用 curl 测试含引号的 JSON body 时 `20015 parameter invalid` 常是 shell 引号转义破坏 body——用 Python `json.dumps` 构造, 别用 shell 字符串拼接。

### SiliconFlow 云 API 排障(402 余额不足)

- **402 `code 30001 "account balance is insufficient"` = key 有效但账户余额为 0**(区别于 401 key 无效)。chat 和 embed 都会 402。
- **构建前先查余额**: `curl https://api.siliconflow.cn/v1/user/info -H "Authorization: Bearer $KEY"` → `data.balance / chargeBalance / totalBalance`。余额 0 时大构建全 402,别浪费 token 重试。
- 领取额度后**未必立刻到账**: 用户「刚领取」但 user/info 仍 balance=0 时,如实报告账户状态,让用户确认领取到了哪个账号/key(可能是另一个账号)。
- ⚠️ **key 打码陷阱**: Hermes 工具输出会显示打码 key(如 `sk-hwm...osso`),**不要**用打码串去 curl(必然 `Token is invalid`)。须在脚本内从 pipeline.yaml 正则读出真实 key 再测,且不打印 key 本体(打印前缀+长度即可)。**也不要从工具输出复制 key 写 patch/config 文件**(2026-08-11 事故: 打码串覆盖了 pipeline.yaml 真实 key, 靠 diff 恢复; 改完用 python 读文件对比前缀+长度验证)。
- 排障脚本参考 `scripts/sf_probe.sh`(读 pipeline.yaml 真实 key → 余额 + chat + embed 三连测)。

### SiliconFlow 模型广场选型(价格/性价比对比)

用户常让在 `siliconflow.cn/models` 筛选高性价比对话模型(如"比 qwen3.5 14B 性价比高")。
- ⚠️ **Qwen3.5 系列没有 14B 型号**(只有 4B/9B/27B/35B-A3B/122B-A10B/397B-A17B);"qwen3.5 14B" 实际指 **Qwen/Qwen3-14B**(￥0.5/￥2,128K,稠密)。
- 列表页公开免登录;详情页/cloud 域要登录,benchmark 拿不到,性能只能按参数规模+代际+上下文推断。
- 分页用 JS 点 `li.ant-pagination-item`(browser_click 常不生效);卡片提取正则、2026-08-08 价格快照、选型结论见 `references/siliconflow-model-plaza.md`。

## Worlds & Qdrant Collections (search filtering)

`world` filtering happens at RETRIEVAL, not display — Qdrant stores per-world collections:
`code_methods` / `doc_chunks` / `kg_nodes` / `config_chunks` (`src/shared/collections.rs`). **Do not assume `world=all` includes every exposed world:** in the current `CrossWorldSearch::search` implementation it fuses `code + knowledge + doc`; `config` and `memory` are explicit-world paths and are not included in `all`. Verify the actual dispatch in `src/application/context/search_mcp.rs` when auditing behavior.

The shared result contract is `CrossWorldResult` + `SearchHit`. `SearchHit` is intentionally wide: code contributes `file_path/start_line/end_line/signature/calls/llm_analysis`, knowledge contributes `score_breakdown/hop/relations/evidence`, while config/doc/memory commonly leave code-specific fields null/empty. `score` is not globally comparable: single-world scores are retrieval/rerank scores, while `all` uses RRF scores. The CLI human renderer is a projection of this contract; `--json` is the machine-facing form.

**MCP path caveat:** `mcp/mcp-server.py::dt_search` currently invokes `dt search ... --json` as a subprocess rather than calling the Rust search service directly. Its `run_cmd()` concatenates stdout and stderr, so child-process warnings can pollute what should be JSON. When auditing MCP compatibility, inspect stream separation before changing the Rust renderer.

**LLM boundary:** search-time retrieval uses embedding and possibly rerank; `llm_analysis` is read from indexed payloads. Search does not normally invoke chat LLM online to explain each hit. LLM calls mainly occur during build/index pipelines (and remote-source ingestion), so report `indexed` analysis separately from any future online mode.

A detailed field/renderer/LLM audit and compatibility proposal is in `references/search-result-contract-audit.md`. Verify with the same query across worlds — returned entity types (`Method` vs `Doc`) prove the filter.

For config-world precision audits, use `references/config-world-precision-search.md`: separate service/dataId identity from section text, parse recognized service/resource queries as AND constraints, distinguish true datasources from incidental values such as `pagehelper.mysql`, and hard-filter before ranking.

## Nacos data-model and build-progress review

When reviewing Nacos modeling or build progress, separate three questions: **graph coverage**, **schema consistency**, and **runtime progress observability**. Do not infer live build completion from search hits or static code symbols.

1. Establish a read-only baseline from Memgraph using label/count queries: count `NacosNamespace`, `NacosConfig`, `NacosService`, then count key relations such as `IN_NAMESPACE`, `HAS_SECTION`, `CONTAINS`, and `DETECTED_IN`. Inspect namespace properties (`namespace`, `namespace_id`, `description`, `updated_at`) and config properties (`config_id`, `namespace`, `data_id`, `group`, `content_hash`, `config_type`, `updated_at`).
2. Check identity invariants: every config/service should resolve to one canonical `NacosNamespace`; namespace must not be modeled inconsistently as both `Server` and `NacosNamespace`; business entities must not be conflated with documentation concepts or AST `Class` nodes. Treat `NacosConfig`/`NacosService` as domain labels and `Concept` as a separate semantic category.
3. Treat inferred section/key relationships as lower-confidence than deterministic identity links. Review duplicate or contradictory relation directions (`CONTAINS` vs `BELONGS_TO`, repeated `HAS_SECTION`) before calling the model healthy.
4. For progress, inspect the actual progress repository/report path (`mark_step_done`, `is_step_done`, `mark_llm_analyzed`, `is_llm_analyzed`, `handle_build`, `sense`) and demand a run-level snapshot: run id, project/source, discovered, skipped, completed, failed, LLM-analyzed, vector-written, duration, and last update. File/hash step records alone are not a live progress dashboard.
5. Report static facts and live verification separately. If Jenkins, service registry, or sync invocation cannot be queried, say that current runtime progress is unverified; do not turn missing integration setup into a zero-progress conclusion. Record adapter/API invocation mismatches as compatibility defects to fix, not as data-model facts.

A reusable schema/count/progress audit template and the observed Nacos baseline are in `references/nacos-model-progress-audit.md`.

**Two data paths** — file builds (`dt build`, pipeline with hash-incremental + LLM extraction) vs Nacos sync (`dt nacos-sync`, SyncSource → `config_chunks`). ~~Jenkins sync (`dt jc-sync` / `jcli build` auto-sync) 已移除 2026-08-12~~。Full code-level walkthrough (entry points, node ids, relations, diff table): `references/sync-vs-build-architecture.md`.

**⚠️ USER ARCHITECTURE PREFERENCE (2026-08-07): unify the two processing paths.** User explicitly rejected having two parallel core-processing logics in one system (pipeline vs SyncSource): "一个系统中出现了两套不同的核心处理逻辑，这是一个不太好的处理方式". Approved direction: Nacos/Jenkins should be treated like ordinary files — introduce a `VirtualFile` abstraction (`(virtual_path, content, project)` — `PipelineContext` already only needs these three, so remote sources can feed the same pipeline: Chunk→LLM→Store) and delete the SyncSource system. User decisions: 不脱敏 (private KB, no secret masking), Jenkins included in the same change, CLI design left to agent (`dt build --source all|fs|nacos|jenkins`). **Team review COMPLETED 2026-08-07: kimi-k3 终审 有条件通过 (6 forced conditions F1-F6, see final doc); final doc `/data/doc/设计方案/digital-twin-统一处理架构方案-2026-08-07-FINAL.md`, reviewer outputs in `/tmp/team-review/{architect,risk,compat,feasibility}.md`. Key corrections vs draft: CLI default stays `--source fs` (NOT all); remote-source increment compares content SHA256 directly (no mtime fast path); `Processor::matches()` extends to take `&PipelineContext`; LLM-failure fallback `--llm-fallback=regex|skip|block` kept until Phase 3; K8s deferred. **Phase 0 IMPLEMENTATION DONE 2026-08-07 via kanban team (3 commits: 7076d3b VirtualFile+context, 82c482a 增量hash+Nacos源, e9d6e48 prompt+自检+Store); regression 4/4 green (694 pass, golden 12/12, no WARN, dual-store 2769/2769). BUT e2e verification found 6 gaps → 门禁未全绿, per plan rule \"门禁不通过→回到看板重新评审\", Phase 1 is BLOCKED until gaps fixed. See `references/phase0-unification-gaps.md` for G1-G6 detail, verdicts, and the re-verification harness (`tests/phase0_verify_nacos.rs`, run `cargo test --test phase0_verify_nacos -- --ignored --nocapture --test-threads=1`).** When implementing, note the pipeline input contract (`PipelineContext::new(file_path, file_text, project_name)` in `src/application/pipeline/context.rs:26`) is the seam — virtual files need only a plausible path + content; the `ProcessorRegistry::matching(path)` extension-based dispatch still applies.

**⚠️ USER REQUIREMENT — Nacos 配置收敛进统一 pipeline (2026-08-07 用户批准, 团队实施中)**: Nacos **不走独立管线** (否决 `docs/plans/nacos-llm-first-implementation-plan.md` 的独立 LLM-first 定位), 全部特殊性收敛为前置适配, 核心链路与普通文件一致 (VirtualFile→Chunk→LLM→Store→统一渲染)。用户原话: "和普通的项目、目录构建一样的处理方式, 只不过可能多一些前置处理或者特定节点的处理" + "整体就只有一个核心的处理逻辑"。实施方案: 仓库内 `docs/plans/unified-pipeline-search-plan-2026-08-07.md`; 已验证数据/代码定位细节 `references/unified-search-rendering.md` (替代旧 nacos-config-search-ux-gap 视角)。四条批准决策:

- **标签**: `FileCategory` 新增 `NacosConfig` (slug `nacos_config`, label `nacos配置`); `infer_file_type_pub` (search_mcp.rs:878) 对 `dt://nacos/` 前缀**来源优先**于后缀映射; `--file-type nacos` 专属过滤, 不与本地 config 混 (用户对澄清问题未响应, 按推荐默认执行)。
- **分析统一走 LLM**: `config_purpose_summary()` (search_config.rs:123, 关键字匹配 6 类兜底文案, 系统唯一非 LLM 摘要) 批准删除; 配置 chunk 与代码方法同一 `llm_analysis` 契约 (形态 `用途：...\n逻辑：...`), 空则渲染层回退 "暂无摘要"。不新增 EntityType 变体 (避开 G4 枚举三处同步问题)。
- **正文字段通用化**: `SearchHit.content` 本就全类型填充; 默认渲染一律不显示正文 (紧凑三行制), `dt search --show-content` 显式展开, Config/Method/Doc 同一渲染分支, 删除 Config* 的无条件正文特例。
- **来源格式**: `dt://nacos/{namespace}/{group}/{dataId}#{key路径}` (裸 key 锚点, 不用 `#section=` 前缀)。**删除 environment 段** — search_config.rs ~L240 旧代码在 payload environment 为空时兜底假数据 `"test"` (实测 config_chunks 1607 点全部 `environment=""`), ~L296 硬编码串同步修。
- 渲染目标形态: `[nacos配置/ConfigKey] spring.cloud.nacos.discovery` + `分析:` + `来源: dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud` (+ 可选正文块, 原文缩进/注释逐字符保留 — 已验证 text 字段本就保留 `#`/`##` 注释)。
- 执行: kanban team 板 T0 基线→{T1 渲染, T2 来源}→T3 LLM 分析→T4 验收, **worker 全部 deepseek-v4-flash (用户指定), 主会话(k3)终审**; 验收报告 `/data/doc/unified-pipeline-search-acceptance.md`。qwen3.5 仅 CPU (~40s/条): T3 LLM 验证限 3 条样本, 禁止全量回填。全量构建执行+T4 验收清单(含 lifecycle_guard embedded null byte 绕过=write_file 脚本+bash、旧 SyncSource 数据辨别、验证标记): `references/nacos-full-build-acceptance.md`;T4 被 flash worker 超时 blocked 时编排者直接执行验收, 勿重派 worker。

## Runtime-chain audits

When `dt` behavior may come from a stale or different executable, follow `references/runtime-chain-audit.md` before diagnosing search or changing code/config. It covers PATH/symlink resolution, debug-vs-release comparison, config lookup, source freshness, and controlled JSON A/B searches.

## Release deployment and verification audits

For a release/deployment review, first establish the **actual runtime chain** rather than trusting stale systemd units or old plans: resolve the project root, `DT_BIN`, symlinks, running processes, listeners, config paths, and service `ExecStart` targets. Distinguish the per-invocation CLI path from MCP subprocess callers and any true long-running daemon. Read-only audit details and the safe replacement/rollback checklist are in `references/release-deployment-audit.md`.

Rules:
- Do not infer a service needs restarting just because a command exists; verify a running process and its executable first. (gRPC daemon removed 2026-08-12; CLI is the only entry.)
- Treat stale service definitions with nonexistent `ExecStart` or config paths as **not current deployment evidence**.
- For a CLI binary, prefer build → smoke-test the staged artifact → hash/metadata capture → preserve previous version → atomic `mv` replacement → post-swap smoke test. Never truncate-write the live executable.
- Restart only the consumers that cache or hold the executable (normally MCP sessions or a verified daemon); do not restart Memgraph/Qdrant/embed services unless the release changes their compatibility, schema, model, or service configuration.
- Before approving release, inspect repository status and record whether local uncommitted changes are included in the build.

## Build/Test Failure Classification (read-only audits)

When evaluating a reported test failure, separate **test execution failures** from **test-target compilation failures**. Run `cargo test --lib` first to establish the source-unit baseline, then run full `cargo test` and preserve the first compile errors. A green `--lib` result does not imply the integration-test targets compile.

For each failure, classify it before calling it expected:

1. **Historical baseline failure** — verify the exact test name against current repository documentation and prior evidence; do not reuse an old “N failures” claim blindly.
2. **API drift** — an integration test initializes a struct or calls a constructor whose signature changed. Search all call sites and compare the current definition with the test.
3. **Removed-module drift** — a refactor/decommission commit removed a production module but left integration tests importing it. Inspect the deleting commit and current module exports.
4. **Runtime/environment failure** — only classify as environmental when the test target compiles and the failure occurs during execution (for example, unavailable Memgraph/Qdrant/Xinference).

Report these separately: compile blockers, executed-test failures, warnings, ignored/live tests, and dirty-worktree context. If a feature was intentionally removed, its old integration test is not a valid “pre-existing failure”; it is stale test coverage that must be retired, migrated, or explicitly quarantined. Do not modify files during a read-only assessment.

A compact evidence pattern from the latest audit is in `references/test-failure-classification.md`.

## Useful Commands

```bash
dt health / dt sense --json / dt search "<q>" --world code --project <p> --limit 5 --json
dt build --path <project> --name <name>   # incremental; `--full` rebuild
dt health                                 # backend health (gRPC daemon removed 2026-08-12)
hermes kanban create ...                  # multi-agent testing: see hermes-kanban-orchestration
```

## Read-only project verification (no build)

When validating an already-indexed repository without changing state, follow this order:

1. Read the repository's `skill/SKILL.md` and every referenced guide before acting. Check that referenced files are non-empty; record missing/empty guides as documentation defects rather than silently treating them as guidance.
2. Resolve project identity from `~/.config/digital-twin/config.yaml`, then run `dt sense --json` from the requested repository. Do not infer the project name from the directory name. Record both the top-level index stats and per-directory stats; if they disagree (for example, `methods` versus directory method totals), report the metric mismatch and do not call it a data-loss finding without tracing the field semantics.
3. Run `dt health` before searches. Then execute one positive, project-scoped JSON search in each required world (`code`, `knowledge`, `doc`). Check world-specific fields: code location/signature and `llm_analysis`; knowledge entity/score/relations; doc `source_ref`/path. Search success does not imply indexed LLM analysis is populated.
4. For hook audits, inspect both the configured command and the script body. Compare repository root, argument expansion, lock/log behavior, and the actual child command. Documentation may say `dt build` while a wrapper uses `cargo run ... build`; report the effective command, and do not execute it when the task is read-only.
5. Summarize provider identity from the effective pipeline configuration without exposing keys. Distinguish configured `providers.llm_provider` from the backend names printed by `dt health`; they may differ because health checks multiple configured services rather than only the selected LLM provider.

A concise command/result template and the observed documentation mismatches are in `references/read-only-project-verification.md`.

## Nacos `config_chunks` provenance contract and safe repair

When changing Nacos chunk indexing, trace and reconcile **both** writers: the unified `VirtualFile → Chunk → LLM → Store` path and the legacy `KgBridge::sync_config_chunks` path. Do not fix only the search projection. Centralize the identity contract in one helper and make both writers emit, for every new point:

- `source: "nacos"`
- `doc_id: dt://nacos/{namespace}/{group}/{dataId}`
- `namespace`, `group`, `data_id`
- `source_ref: dt://nacos/{namespace}/{group}/{dataId}#{section-or-key-path}`

Use `doc_id` as the config-level purge/repair identity and `source_ref`/point id as the section/key-level address. Search must prefer persisted `source_ref`, but retain a deterministic fallback for historical payloads missing it; otherwise old `config_chunks` points become unsearchable or change displayed provenance.

For an already-successful build, design an **offline/incremental repair plan**, not an implicit backfill: identify affected points by payload coverage, re-read source content, re-chunk only affected namespace/group/dataId records, and stage the intended upserts/deletes or a dry-run report. Never call Qdrant/Memgraph writes unless the user explicitly authorizes online repair. Do not launch a full build for a metadata-only contract change.

Verification gates: add a unit test for the canonical `(doc_id, source_ref)` helper and payload assertions for each writer; run focused Cargo tests one filter at a time (Cargo accepts one test filter, not several positional names), then `git diff --check`. Before committing, inspect `git status` and restore unrelated pre-existing edits; commit only the requested files. A session-specific contract/repair checklist is in `references/nacos-config-chunk-provenance-repair.md`.

## Read-only Nacos/config search acceptance

For the chain **Nacos test → Qdrant `config_chunks` → CLI JSON**, begin with read-only probes and treat synchronization as a separately approved write test.

### Field-completeness audit pattern

When the user asks for a read-only consistency audit, inspect the Qdrant collection directly in addition to exercising `dt search`. First `GET /collections/config_chunks` for `points_count` and health, then paginate `POST /collections/config_chunks/points/scroll` with `with_payload=true, with_vector=false`. Count non-null/non-empty values for `source`, `doc_id`, `data_id`, `namespace`, `group`, and `llm_analysis`; also report payload-key coverage and source distributions. This catches legacy points whose search projection can look valid while provenance fields are absent. Treat `data_id` as the identity field only when it is actually 100% populated; do not infer `source`/`doc_id` from `source_ref` or `dt://` strings.

For verification, use one known-positive config query and one high-entropy random negative query. Record hit count, entity type, source metadata, score, and whether `llm_analysis` is null. A positive result with very low score is retrieval evidence, not proof of strong semantic relevance; report it as a quality observation separately from metadata consistency. The audit is read-only: collection GET/scroll and `dt search --world config` are allowed; sync/build/upsert/delete and any Memgraph/SQLite writes are out of scope. A reusable session-specific example is in `references/config-chunks-readonly-audit.md`.
 See `references/nacos-config-readonly-acceptance.md` for the complete checklist. For the reusable adapter-contract checklist and the observed MCP/CLI mismatch, see `references/nacos-sync-vector-write-review.md`.

- Safe to automate: binary/path and service checks; Nacos GET/list/detail calls; Qdrant collection/scroll/count/payload audits; `dt search ... --world config --json`; JSON/schema assertions; duplicate, empty-field, and hash calculations.
- Not read-only: `dt nacos-sync test`, `dt nacos-sync test --config-chunks`, `dt build --source nacos`, Qdrant upsert/delete/payload updates, Memgraph `CREATE/SET/MERGE/DELETE`, and tests that seed then clean probe nodes. Require explicit approval.
- JSON gate: parse stdout as one JSON document; logs must not pollute stdout. Validate `hits`, numeric `total`/`score`, config-world/source provenance, known-key positive queries, and a random negative query.
- `config_chunks` acceptance must inspect provenance payloads (`doc_id`, `data_id`, namespace/group, source reference, chunk index/text, section/key count), not merely search hits. `world=config` does not prove anything about `kg_nodes`.

## Xinference Ops (restart procedure)

- Service: `sudo systemctl start xinference` (systemd unit, port 9997). After a restart, **models are NOT auto-loaded** — launch them via API (model files in `/data/inference/cache/v2/`):
  - `curl -X POST :9997/v1/models -d '{"model_uid":"bge-m3","model_name":"bge-m3","model_format":"ggufv2","quantization":"Q4_K_M","model_engine":"llama.cpp","model_type":"embedding","n_gpu":1}'`
  - reranker: same but `model_name/model_uid: bge-reranker-v2-m3`, `model_format: pytorch`, `model_type: rerank` — **do NOT pass model_engine** ("cannot be run on engine transformers")
  - qwen3.5 LLM: `xinference launch --model-name qwen3.5 --model-type LLM --model-format ggufv2 --quantization Q4_K_M --size-in-billions 4 --model-engine llama.cpp --n-gpu 1 --context-length 4096 --model-path /data/inference/cache/v2/qwen3_5-ggufv2-4b-Q4_K_M/Qwen3.5-4B-Q4_K_M.gguf` — **model name is `qwen3.5` (dot, not underscore)**. GPU mode **works when the other models are already loaded in the right order** (verified 2026-08-06: bge-m3 + bge-reranker + qwen3.5 all resident, ~6.2GB used / 1.6GB free, stable). Earlier crash root cause (journalctl 2026-08-06): **GPU OOM** — RTX 4060 Laptop 8GB total, ~3.6GB already used, llama.cpp fails `ggml_backend_cuda_buffer_type_alloc_buffer: allocating 8192.00 MiB ... cudaMalloc failed: out of memory` → `xoscar ServerClosed` → model vanishes. **`--context-length 4096` does NOT by itself fix OOM** (default n_ctx 262144 is huge; even small ctx still wants the big buffer). What makes GPU mode work: **`--n-gpu 1` (NOT `none`) + explicit `--model-path` + sequential loading (embed→rerank→LLM)**; if GPU still OOMs, CPU fallback (`--n-gpu none`) works but ~34s/query. **Trap: `xinference launch` exits 0 and prints `Model uid: qwen3.5` even when the worker crashes seconds later** — exit code 0 is NOT success. Always verify `curl -s :9997/v1/models` after ~25-30s; a missing model = crashed. Another trap: worker crash messages often do NOT reach journald (Xinference subprocess); check `curl /v1/models` first, and full error only appears in journalctl when the main service logged it.
- Verify: `curl -s :9997/v1/models`
- **⚠️ 2026-08-07 regression: qwen3.5 无法加载 — xllamacpp 版本不兼容**. `xinference launch` 报 `Model uid: qwen3.5` 但 `/v1/models` 无此模型;`/data/inference/logs/xinference.log` 有 `Failed to set the param context_length = 4096, error: 'xllamacpp.xllamacpp.CommonParams' object has no attribute 'context_length'` → n_ctx 落回 262144 → `MemoryEstimate(layers=0, vram_size=0, total_size=37GB)` → `Failed to load model qwen3.5-rep0`。根因:xinference 3.1.0rc1(/data/myProject/inference,git checkout)用 `setattr(params, k, v)` 配置 `CommonParams`,而 pip 装的是日期版本 **xllamacpp 2026.7.10068**(slots 冻结对象,拒绝 setattr;pyproject 只要求 `>=0.2.0`)。bge-m3 不传 context_length/size_in_billions 所以正常。修复方向(需用户确认,属 inference 项目): ① `pip install xllamacpp==<兼容旧版>`;② patch `xinference/model/llm/llama_cpp/core.py` 把 context_length/size_in_billions 映射到 n_ctx 等合法字段;③ 升级 xinference。**PROVEN WORKAROUND (2026-08-07, verified chat "ok")**: `xinference launch ... --n-gpu none` (CPU 模式) 直接可用——跳过 GPU kv-cache 分配与 context_length 参数问题;~30-60s/query,够数字孪生检索/验证用。症状区分:模型"曾经能加载后来不能"= 依赖升级回归,查 pip 变更,别反复重试 GPU launch。**Long-uptime silent death (2026-08-07)**: 服务跑 ~16h 后 qwen3.5 从 `/v1/models` 静默消失,无 journald 错误(embed/rerank 仍在);此时 relaunch 出现矛盾报错(`POST /v1/models` 说 `already in the model list` 而 `DELETE` 说 `not found, uid: qwen3.5-rep0`) = supervisor 注册表残留。处理:`sudo systemctl restart xinference` 清注册表,再按 embed→rerank→LLM 顺序重载。llama.cpp 的 `context_length` 经 API JSON body 传入**不会**缩小 n_ctx(kv cache 仍要 8192 MiB → 8GB GPU 在 embed+rerank 占 3.3GB 后必然 `cudaMalloc failed: out of memory`);要 GPU 跑只能卸载其余模型或接受 CPU 模式。

## ⚠️ dt-embed.service — BGE-M3 embedding gRPC 服务(2026-08-11 实况)

dt 的 embedding 服务,systemd 单元 **系统级 + 用户级各一份**(同名双单元):
- 系统级 `/etc/systemd/system/dt-embed.service`(ExecStart=/home/luis/.local/miniconda3/bin/dt-embed-grpc, User=luis, 日志 append:/var/log/dt-embed.log, RestartSec=5)
- 用户级 `~/.config/systemd/user/dt-embed.service`(env EMBED_MODEL=bge-m3 离线, 日志 append:/tmp/dt-embed-grpc.log)

**2026-08-11 状态:二进制 `/home/luis/.local/miniconda3/bin/dt-embed-grpc` 不存在 → 两个单元都 203/EXEC 崩溃循环,restart counter 17000+,每 ~5s 刷 3 条 journal,是 journal 洪水的头号来源**。症状识别:`journalctl` 满屏 `dt-embed.service: Failed with result 'exit-code'` + `status=203/EXEC`。处置:装回/修正二进制路径,或 `sudo systemctl disable --now dt-embed` + `systemctl --user disable --now dt-embed` 止住刷屏(改配置类操作先给用户方案)。

**✅ 已处置(2026-08-11, 用户批准停用+清理)**:根因是 v2 单 crate 重构(2026-07-11 commit 1c6dfcc)用 Rust 内置 embedder(SiliconFlow/XInference)取代 Python 版 embedding 服务(仓库 `services/embed-server`, pip editable 包 `dt_embed`),但两个 systemd 单元没删 → 7月21日 20:22 起 `/var/log/dt-embed.log` 全是 `ModuleNotFoundError: No module named 'dt_embed'`(二进制还在但 editable 源码目录已删)→ 8月7日 21:47 某次 pip 操作(装 camoufox/quart/rich-click, any-auto-register 依赖)连 console script 本身也清掉 → 8月10日 08:47 系统崩溃重启后 203/EXEC 无限循环(17793+169 次)。**此服务无恢复价值**(二进制/源码/模型缓存全无, 主程序零依赖——embedder.rs 只走 siliconflow/xinference, EmbedServerConfig 是反序列化残留), 正确处置=停用删除而非找二进制。已删: 两个 .service 单元+两个日志(/var/log/dt-embed.log 6.4MB, /tmp/dt-embed-grpc.log)。后续本地 embedding 走 Xinference bge-m3, 不需要此服务。

⚠️ 排障时先 `grep -vE 'dt-embed|bed-grpc'` 过滤这两个循环服务,否则 journal 全是它们,真正的崩溃签名(gnome-shell GLX 等)被淹没。桌面崩溃诊断完整流程见 `linux-system-crash-diagnostics` 技能。

## Search Accuracy Upgrades (2026-08, all applied — 50% → 100% on golden set)

Applied in `src/application/context/search_mcp.rs` + `src/application/knowledge/extract/retrieve.rs` + config index script. Full report: `/data/myProject/digital-twin-tests-upgraded/evaluation_report.md`, regression: `/data/myProject/digital-twin-tests/run_regression.py` (12-query golden set, `search_plan.json`).

**⚠️ golden Q10 已知 miss(2026-08-08 实测, 数据漂移非代码回归)**: 12 条 golden 中 Q10「数据库连接地址」会 miss(11/12)当 config_chunks 被 ~1606 个 nacos chunk 占满——本地 `dt://config/digital-twin-v2/services#0` 点(含 bolt://localhost:7688)被挤出 top-10 候选, 任何代码路径都召不回。且 Q10 预期机制(中英扩写召回 config.yaml)只在 Cypher 回退路径生效, 向量路径命中非空就不走回退——**设计缺口 pre-existing**。检索代码 Phase0→HEAD 逐行未变即证明非回归。判定口诀: Q10 miss + config_chunks 体量大 + 检索代码无 diff = 数据漂移, 别重跑全量/别改检索代码。T4 验收 worker 曾为此深挖超时 blocked, 编排者直接用此结论放行。

- **1A exact-identifier channel**: identifier queries (camelCase/snake_case detected by `is_identifier_query`) first run Qdrant filtered search `name == query`; exact hits get **fixed score 0.95** so the `search()` main body's re-sort (`hits.sort_by(score)` then `truncate`) keeps them on top. Without the high score, vector-similar methods (0.6x) outrank exact matches → target sinks to rank 5.
- **4.1 long-query keyword channel** (doc): queries >20 chars or with ≥4-char keywords scroll `doc_chunks` payloads (up to 5000) and CONTAINS-match each keyword; keyword hits get **fixed score 0.90** (same re-sort rationale). Chinese keywords work; `extract_keywords` splits on non-alphanumeric/non-ASCII.
- **3.3 keyword recall** (knowledge): `keyword_recall()` runs Memgraph Cypher `WHERE toLower(toString(coalesce(e.name,''))) CONTAINS toLower('{kw}')`, seeds get `semantic: 0.80`; after `bucket_truncate` keyword seeds are force-appended back into candidates (otherwise the 0.6×seed_cap truncation evicts them).
- **Config world**: `build_config_index.py` chunks `config/pipeline.yaml` + `~/.config/digital-twin/config.yaml`, embeds via Xinference bge-m3 (chunks ≤600 chars — bge-m3 batch limit), upserts to `config_chunks`. Payload MUST include `section_name`/`data_id`/`key_count` + unique `doc_id` (`#{idx}` suffix) — search_config dedups by title, so identical titles collapse to 1 hit.

## ⚠️ Memgraph bolt-client pitfalls (dt's neo4rs/bolt_driver)

- **`elementId(e)` in RETURN silently drops the row**: dt's `read_query` skips rows whose `row.to::<serde_json::Value>()` fails; elementId's special type fails → 0 rows. Never RETURN raw `elementId()`; use scalar fields only.
- **`toLower($kw)` parameterized CONTAINS returns 0 rows** in dt (works via other clients). Use string-literal interpolation with `'` escaped (`kw.replace('\'', "''")`).
- **`toString(coalesce(e.summary,''))` in WHERE with OR branches**: non-string props (List/Map) make `toString` → null → whole WHERE false. Keep WHERE to `name` only, or guard each branch.
- Debug pattern: a probe query (no WHERE) returning rows while the filtered query returns 0 proves the WHERE clause is the problem, not connectivity.

## KG Entity Type Classification (why md concepts show as [Config])

Search results tagged `[Config]` whose source is an `.md` doc (e.g. 提交规范, CommitMessageFormat, conventional_commits) are NOT a display bug — the KG nodes really are `type=Config` (`dt://entity/<proj>/Config/...` in Memgraph). Root cause: `config/prompts/document_with_nlp.yaml` defines the extraction entity vocabulary as `Service|Channel|Config|Table|Api|Concept|Person|Org|Product|Other` — there is **no Standard/Convention/Rule/Process type**, so the extraction LLM buckets 规范/标准/约定 concepts (commit conventions, naming standards) into the closest slot: Config. The node's `source_ref` then points back to the original md, which is why output looks like "Config type + md source".

- Diagnosis: `MATCH (e:Entity) WHERE e.name = 'x' RETURN e.entity_id, e.type` confirms the node type; check the prompt vocabulary BEFORE assuming a retrieval/display bug.
- Fix path (APPLIED 2026-08-06): `Standard` added to `config/prompts/document_with_nlp.yaml` vocabulary (`Service|Channel|Config|Table|Api|Concept|Standard|Person|Org|Product|Other`) + type definition in 规则区; `raw_text.yaml` checked too. Full record: `/data/doc/设计方案/digital-twin搜索优化记录.md`.
- ⚠️ **CRITICAL companion change: the closed `EntityType` enum must be updated in the SAME edit as the prompt vocabulary.** `src/application/knowledge/extract/model.rs` has a sealed enum (`Service|Channel|Config|Table|Api|Concept|Person|Org|Product|Other` + `#[default] Other`) with a custom `Deserialize` that normalizes out-of-vocabulary strings to `Other` and WARNs `LLM 返回词表外实体类型 'X'，归一为 Other`. Adding `Standard` to the YAML without adding the enum variant silently converts every Standard extraction to `Other` — discovered during the KG rebuild when qwen3.5 started returning `Standard` (k3's earlier edit only touched the YAML). Fix = 3 sites in `model.rs`: enum variant + `as_str()` match arm + `Deserialize` match arm. **Also hardened 2026-08-06: deserialization is now case-insensitive** (`to_ascii_lowercase()` before matching) with alias mapping — `tech|database|technology|module → Concept`, `people|human → Person`, `organization|organisation|company → Org`, `unknown|misc → Other` — because raw_text/code paths emit lowercase and loose variants (`Tech`, `module`) that otherwise WARN-spam every build. This enum is the ONLY vocabulary gate; there is no other normalization layer.
- **Data-layer correction of existing nodes (when LLM re-extraction is unavailable/slow)** — must update BOTH stores, in order:
  1. **Memgraph** via bolt: `MATCH (e:Entity) WHERE e.type='Config' AND e.name IN $names SET e.type='Standard', e.labels=['Entity','Standard']`. MCP `run_cypher_query` is READ-ONLY — use a direct bolt client (miniconda python has `neo4j` 6.2.0; `GraphDatabase.driver('bolt://localhost:7688', auth=('memgraph',''))`).
  2. **Qdrant payload** via REST `POST :6333/collections/kg_nodes/points/payload` `{"payload":{"type":"Standard"},"points":[ids]}` — find ids with a scroll filter on `name`. **`dt kg-sync` (incremental) does NOT update existing points' payloads** — after sync, `dt search` still shows old `[Config]` because search reads `type` from the Qdrant payload, not Memgraph. Symptom: Memgraph says Standard, search shows Config → payload stale.
  3. Verify with `dt search "<query>" --world knowledge` showing `[Standard]`.
- Batch classification heuristics: summary containing 规范/标准/格式约定 → Standard; 标准库类型 (VecDeque/Mutex) → Concept; git 命令/代码文件引用 → Other; 真配置项 (world=..., max_hops, business_id) keep Config. Same name can appear as multiple nodes with different types (`Concept/commit` vs `Other/Commit` vs `Config/commit`) — check each.
- **code_with_ast vocabulary drift (FIXED 2026-08-07)** — `code_with_ast.yaml` entity type vocabulary was `class|function|method` (AST-style), which the closed `EntityType` enum has NO variants for → every LLM `function`/`class`/`method`/`interface` output WARN-spammed and normalized to `Other`. The 2026-08-07 `--full` rebuild baked in 186×`function` + 169×`command` + 127×`file` + 54×`task` + 46×`tool` + 20×`interface` WARNs → ~146 nodes wrongly `Other`. Two-sided fix (do BOTH, they cover different gaps): (1) prompt: `code_with_ast.yaml` vocab changed to the full EntityType vocab (`Service|Channel|Config|Table|Api|Concept|Standard|Person|Org|Product|Other`) + rule "类/函数/方法/接口/结构体/枚举/宏 → Concept; 路径/文件引用 → Other"; (2) enum fallback arms in `model.rs` Deserialize: `function|class|method|interface|trait|struct|enum|macro|command|task|tool|step|job|type|namespace|library|model → Concept` so legacy LLM output never WARNs again. **Lesson: EVERY prompt template vocabulary (not just document_with_nlp) must stay in sync with the EntityType enum — audit all 4 templates when adding/renaming a type.**
- **Fast batch reclassify (avoid a 4h+ rebuild when only type labels are wrong)** — user-approved alternative 2026-08-07 for the function misclassification: fix prompt+enum, `cargo build --release`, then reclassify EXISTING `Other` nodes in-place (no LLM re-extraction): (1) Memgraph bolt `MATCH (e:Entity) WHERE e.type='Other' AND (e.name CONTAINS '::' OR e.name ENDS WITH '()' OR e.name =~ '^[a-z_][a-z0-9_]*$') AND NOT e.name CONTAINS '.' RETURN DISTINCT e.name` (:: / () / bare snake_case = function-like; dotted names like `retrieve.rs` stay Other); (2) `SET e.type='Concept', e.labels=['Entity','Concept']` per name; (3) Qdrant scroll `kg_nodes` `with_payload:true, with_vector:false` paginated, collect ids where `payload.type=='Other' AND payload.name in names`, `POST :6333/collections/kg_nodes/points/payload {"payload":{"type":"Concept"},"points":[...]}`; (4) verify counts + `dt search` no WARN. Result 2026-08-07: Concept 688→834, Other 432→286, 134 Qdrant points updated, search clean. This only fixes labels — summaries/relations stay as extracted, acceptable when the LLM content itself is fine. **Automated: run `scripts/fix_other_to_concept.py` (`--dry-run` first) — same logic, both stores, one command.**
- ⚠️ **neo4j Python driver UnicodeDecodeError on Chinese summaries** (neo4j 6.2.0 + Memgraph): selecting `summary` (or `left(summary,N)` with N splitting a multibyte char) crashes with `UnicodeDecodeError: 'utf-8' codec can't decode byte ... unexpected end of data`. Workaround: RETURN `name`/`entity_id` only (ASCII-safe), or fetch one row at a time and re-drive per name. Don't let the crash abort a batch — query names first, decide classification, then write.

## HanLP removal (2026-08-06, applied)

HanLP was dead code: `services.hanlp.url: http://localhost:8765` pointed at a service that was never deployed (no process, no container, port closed). `build.rs` gates registration on `hanlp_client.health_check()` → always failed → `hanlp_available=false` → **the HanLP processor never ran in production**; entity extraction was 100% LLM-vocabulary-based all along. User approved removal. What was deleted:

- Files: `src/infrastructure/hanlp.rs`, `src/application/pipeline/processors/hanlp_client.rs`, `scripts/fixes/cleanup_config.py` (stale one-off fixer)
- Code: `main.rs` `HanlpConfig` struct + `connect_hanlp()` fn + 2 call sites (10th arg of `handle_build`); `build.rs` hanlp_available block + registration; `config.rs` `processors.hanlp` field + defaults + test asserts; `mod.rs` exports (both infra & processors); `llm_client.rs` `hanlp_map` injection + `format_hanlp_candidates` + 5 unit tests → replaced with empty placeholder `("（无）", "（无）")`
- Config: `config/config.yaml` + `config/pipeline.yaml` (`hanlp: true` line). `~/.config/digital-twin/config.yaml` had no hanlp block by then.
- Comments referencing hanlp cleaned across engine.rs/context.rs/processor.rs/traits.rs/runner.rs/build-pipeline.rs.

Verification: `cargo build --release` OK; `cargo test --release --lib pipeline` 100 passed; regression `run_regression.py` 12/12; `grep -rn -i hanlp src/` clean.

### ⚠️ Pitfall: k3 worker deletion over-reached and broke the build

The Kanban worker (kimi-k3) deleted `DaemonConfig.services` field in `main.rs` along with the hanlp field (it removed the whole `services:` block instead of just `hanlp:` inside it) → `error[E0609]: no field services on type DaemonConfig` at ~12 call sites. Always run `cargo check` after ANY worker-assisted deletion and diff `git status` before accepting the result; a deletion task that "mostly compiled" can still have collateral damage in adjacent struct fields. This is also why the kanban-multi-agent-orchestration skill says: crashed worker ≠ no work, but verify compile state, don't assume the diff is correct.

## User preference: 关键修改用 kimi-k3 (delegate to k3, but verify credential health first)

For a task that explicitly names `kimi-k3`, model selection is a hard gate for substantive edits: perform the read-only baseline (AGENTS/instructions, git status, and scope) first, then verify the actual chat endpoint before dispatching. Do not silently substitute another model or make the requested code changes yourself if the named model cannot be used; report the block and preserve the workspace. Existing user edits must remain untouched, and prohibited build/test commands must not be run while blocked.

This user's standing preference for digital-twin code changes: **关键修改 (substantive changes) go through kimi-k3** — either `hermes kanban create ... --model kimi-k3` (worker model override) or as the review/implementation model. Practical lessons from 2026-08-06 (two separate k3 tasks):

- **Check credential health BEFORE dispatching**: `~/.hermes/auth.json` credential_pool may show `opencode-go → last_status: "exhausted"`. When exhausted, k3 workers crash early with `HTTP 503: Endpoint is unavailable` + `protocol_violation {exit_code: 0}` and the task lands `blocked` after max-retries. Re-dispatching with a dead key just burns cycles. **BUT `exhausted` can be a STALE status (2026-08-07): auth.json showed `exhausted` while the real key still served kimi-k3/flash/pro fine.** The actual key lives in `~/.hermes/.env` (`OPENCODE_GO_API_KEY=sk-...`), NOT in auth.json (which stores only a fingerprint). Before giving up, probe the real chat endpoint with the real key:
  ```bash
  KEY=$(grep -oE 'OPENCODE_GO_API_KEY=.*' ~/.hermes/.env | cut -d= -f2-)
  curl -s -X POST https://opencode.ai/zen/go/v1/chat/completions \
    -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
    -d '{"model":"kimi-k3","messages":[{"role":"user","content":"ok"}],"max_tokens":10}'
  # 有 choices → 可用,可以派发;只有这才是真正的 503/死 key
  ```
  Only a failed chat probe (503/401 on this exact call) means the key is truly dead and the user must refresh it.
- **Trap: `curl /v1/models` returning 200 is NOT a chat availability check** — the list endpoint stays up while `/chat/completions` 503s. Probe the chat endpoint (e.g. `curl -X POST <base>/v1/chat/completions` with a tiny message) or check `hermes kanban log <id>` for the actual 503s.
- **Crashed worker ≠ no work**: k3 workers crash BETWEEN tool calls; edits already made are already on disk (both HanLP files were deleted + most references cleaned before the crash). Before re-dispatching, `git status`/`ls` the workspace, see what's actually done, and either finish the small remainder yourself (mechanical cleanup of a crash-interrupted k3 task is fine to do directly — the user's k3 preference covers the substantive change, not the tidying) or create a follow-up task describing ONLY what's left.
- **`--board team` goes BEFORE `kanban create`** (global arg position: `hermes kanban --board team create ...`); the board the dispatcher serves is the one in the current `hermes kanban boards list` output (● marker).

## Pitfall: verify debug-probe removal after cleanup

Temporary `tracing::info!("PROBE ...")` blocks added while debugging can survive a cleanup script: rustfmt re-wraps long lines, so exact-string replacement fails silently and the probe keeps printing on every search. After running any cleanup, verify with `grep -rn "<probe-string>" src/` (plus one live search run) before declaring it removed — this session's PROBE query block in `retrieve.rs` survived the first cleanup and printed on every knowledge search until caught this way. Keeping the `keyword_recall: kw=... rows=N` info log is intentional (cheap observability for the keyword channel).

## ⚠️ mcp-server.py run_cmd() merges stdout+stderr (log pollution in MCP tool results)

`mcp/mcp-server.py` `run_cmd()` does `output = (result.stdout + result.stderr).strip()` — stderr is CONCATENATED onto stdout. Any `tracing::warn!` fired during `dt search --json` (e.g. build.rs:239/247 "pipeline.yaml … 使用默认配置", search-path warnings) therefore appears at the TOP of MCP tool results (`dt_search`/`dt_search_kg`), looking like "配置加载日志混进搜索结果". This merge is the root cause of the user-visible symptom — NOT the dt binary (CLI stdout is already pure per U-D4; verified `dt search` from /tmp prints no log lines, INFO→file, WARN→stderr). Diagnose "MCP search result polluted with logs" by inspecting run_cmd BEFORE touching the Rust side. Fix (proposed 2026-08-06, awaiting user confirm): separate streams — `--json` commands return stdout only, stderr routed to log file.

## Prompt 配置化状态 (2026-08-06, full-configurization plan)

- `config/prompts/` holds 4 templates: `document_with_nlp` / `raw_text` / `code_with_ast` / `code_analysis` (mirrored at `~/.config/digital-twin/prompts/` — keep in sync; load order: `DT_PROMPTS_DIR` → CWD `config/prompts` → `~/.config/digital-twin/prompts` → exe-relative).
- Extraction prompt selection (`select_prompt()` in `src/application/pipeline/processors/llm_client.rs`): `tree_sitter` output → `code_with_ast`; `chunk` output → `document_with_nlp`; else → `raw_text`. Entity types are LLM-chosen from each template's YAML vocabulary (no rule layer); per-type vocabularies deliberately differ — doc: `Service|Channel|Config|Table|Api|Concept|Standard|Person|Org|Product|Other`; raw_text: `person|org|tech|standard|product|other`; code: **was `class|function|method` — FIXED 2026-08-07** (see below).
- STILL HARDCODED (the configurization gap): `engine.rs` `summarize_via_llm` project-summary (~L539) and ecosystem-summary (~L721) prompts are string literals, not YAML-backed; `load_code_analysis_prompt()` (build/pipeline.rs ~L952) loads `code_analysis.yaml` ad-hoc via its own path list (NOT through PromptRegistry) with `PHASE2_DEFAULT_PROMPT` fallback. Plan: new `project_summary.yaml` / `ecosystem_summary.yaml` + unify code_analysis through the Registry, keep fallbacks. Plan doc: `/data/doc/设计方案/digital-twin-搜索与提示词优化方案-2026-08-06.md`.

## ⚠️ `dt://` source_ref URI 语义 + 反查本地文件 (2026-08-10 用户提问)

`dt://doc/pay-center/朱啸天_git提交记录分析报告.md` 里的 **`pay-center` 是项目别名, 不是目录名**: config.yaml `projects[].items` 定义映射(如 `{pay-center: uvp-pay-center}`), 磁盘全路径 = `base + 别名对应目录 + URI 相对路径` = `/data/aflmProjects/unimportant/uvp-pay-center/朱啸天_git提交记录分析报告.md`。URI 的后半段就是项目内真实相对路径, 与 code 世界的 `file_path` 同语义。

**为什么设计成 URI 而非绝对路径**: ① 项目别名稳定, 绝对路径随目录迁移/改名失效——存别名+相对路径, 换目录重构建后来源仍有效; ② 支持虚拟来源(Nacos `dt://nacos/{ns}/{group}/{dataId}#{key}` 无磁盘路径), 统一寻址格式; ③ 跨机器/环境可移植。

**反查本地文件 4 法**: ① config.yaml 找别名映射(`grep -A3 "pay-center" ~/.config/digital-twin/config.yaml`); ② `dt sense --json` 看项目注册路径; ③ `find /data -name "<文件名>"`(URI 相对路径在磁盘真实存在); ④ 搜索 `--json` 看 payload 字段。排查"搜索来源指向的文件在磁盘哪里"时先解析项目别名, 别直接拿 URI 当路径用。

**用户提案「搜索显示磁盘全路径」— 团队分析结论 (2026-08-10, 未实施, 结论已定)**: 用户要求把"来源"字段显示为磁盘全路径(项目名配置路径 + dt 路径拼接), 且"替换来源字段...不破坏原本的设计"。3 角色并行分析(架构/风险/可行性)后结论:

- **必须展示层替换, 禁止数据层改 `SearchHit.source_ref` 值** — 关键证据: `kg_bridge.rs:338` config 世界写入时 **`"id": source_ref`**(source_ref 就是 Qdrant point id), `doc_id = format!("{source_ref}#0")`; 改 source_ref 值会破坏 point id 一致性与 doc_id 关联 → 去重/清理/purge 全乱。
- **fusion 去重 key 是 `world:id`**(fusion.rs:37/65 `format!("{}:{}", source_world, id)`), **不是 source_ref**——展示层替换不影响去重, 这给展示层方案背书。
- **code 世界难点**: SearchHit 只有 `file_path`(相对)没有 `project` 字段, 要拼全路径需知道项目名——需给 SearchHit 加 project 字段(影响 JSON/MCP 契约)或从请求侧传入项目映射表。`dt://nacos/`、`dt://entity/` 无磁盘路径需跳过。
- **推荐做法**: 在 CLI 渲染层(handle_search → render_human)加载 config.yaml 项目映射表, 对 knowledge/doc 的 source_ref 和 code 的 file_path 做「项目别名→base+目录」拼接生成展示值; 映射查不到或虚拟来源保留原值。改 SearchHit 契约前先评估 JSON/MCP 消费方(render_json 输出、mcp-server.py dt_search 子进程)。

**2026-08-11 架构评审(正式结论, 待实施)**: 逐文件验证了上述结论, 证据与实施清单见 `references/source-ref-display-disk-path-review.md`。新增硬事实: ① source_ref 的消费点不止 point-id/doc_id — 还有 `infer_file_type_pub` 的 `dt://nacos/` 前缀判定(postprocess_hits search_mcp.rs:901-905 + retrieve.rs:1141-1145), 换值会打掉 NacosConfig 分类与 `--file-type nacos` 过滤; ② **handle_search 收不到 config**(main.rs Search 分支只传 graph/vector, 对比 handle_sense 传 resolve_project_paths) — 实施必须先接线; ③ code 世界 Qdrant payload 有 `project`(build/pipeline.rs:253/760 写入, search_code/sense 已按 payload project 过滤), SearchHit 缺字段 — 加 `project: Option<String>`(serde default)即可, 无需改数据; ④ config 实测 7 base/65 别名无跨 base 重复, mapping 值可含多段相对路径(`cashier: pay/offenpay-ui/offenpay-ui-cashier`), `test-pipeline` 不在 config 需回退原 URI; ⑤ 可翻译范围仅 `dt://doc/{alias}/{rel}`, dt://nacos/、dt://config/、dt://entity/、dt://method/ 均无磁盘路径保持原样。

**2026-08-11 兼容性角色全库审计补充(消费方清单 + 风险分级, 验证并细化上述结论)**: 逐行 grep 了 src/ 全部 60 处 source_ref, 确认:

- **去重/图扩展/purge 零依赖 source_ref**: 去重键 = `{world}:{id}`(fusion.rs:37,65)、title(search_config.rs:372)、name(:456)、business_id(retrieve.rs merge_candidates); 图扩展用 elementId; `purge_document`(consolidate.rs:557-588)按存储侧 doc_id, kg_bridge 写前清理按 namespace+data_id。**改 source_ref 值不会破坏这三类功能。**
- **doc 世界强不变量**: `source_ref == payload doc_id`, hit.id = `{doc_id}:{block}`(search_mcp.rs:663,672,745)——值级替换会撕裂 id↔source_ref 关联。
- **`dt://nacos/` 前缀是 file_type 判定的来源优先分支**(search_mcp.rs:883 `infer_file_type_pub`)——值级替换 nacos 来源会丢 [nacos配置] 标签与 `--file-type nacos` 过滤。
- **kg_bridge.rs:334-350 存储写侧**: config_chunks 的 Qdrant point **`"id": source_ref`**、`doc_id = source_ref#0`——此处是点身份, 存储写侧改=高风险。
- **值级替换必挂的测试断言 7 处**: search_mcp.rs:1340-1342(doc 世界 source_ref==dt://doc/...)、search_config.rs:601-602/636-639/677-711(nacos)、retrieve.rs:1732/1842-1850(边 doc_id 回填)、search_render.rs:164-168/193-198/232(渲染断言, 改渲染即挂)。值无关: search_mcp.rs:998(`"source_ref":null`)、tests/s5_knowledge_search.rs:364(is_some)、build_service.rs:340、fusion.rs:100。
- **config.yaml 映射坑**: `projects=[{base, items:[name | {display: 物理目录}]}]`, display 名≠物理目录名(copartner-h5→copartner/copartner-h5); 同名项目可跨 base; knowledge 世界 source_ref 的 project 段可能不在 config.yaml(test-pipeline/fixtures)→ 映射失败。
- **MCP 影响面**: mcp-server.py 纯透传零解析, schema 不变, 但值语义从"稳定 URI"变"本机路径", 跨机器不可移植。
- 完整消费方清单(生产者/消费者/身份键/测试断言逐行): `references/source-ref-consumer-map.md`。

**2026-08-11 实现可行性角色补充(实施计划定稿, 待用户批准)**: 展示层方案落地细节已全部核实——① main.rs 的 `load_config()/resolve_project_paths()` 是 **bin 私有**, lib 侧 render_human 拿不到, 必须由 main.rs:1207 算好 `HashMap<String,PathBuf>` 作为新参数传 handle_search(签名加 `Option<HashMap<...>>`), config 缺失→None→渲染零变化; ② code 世界唯一 SearchHit 构造器 `hit_from_payload`(search_mcp.rs:108, 调用点 L469/L505/L551)可读 payload `project` 标签, 给 SearchHit 加 `#[serde(default, skip_serializing_if = "Option::is_none")] pub project: Option<String>` 即可(grpc hit_to_proto 只读字段不破坏; 全库 11 处 `SearchHit {` 字面量构造点清单在 ref); ③ `dt://doc/` 解析 = `strip_prefix("dt://doc/")` + `split_once('/')` 取 (project, rel), `#section-` 锚点先剥离再 join; `dt://nacos|entity|config|jenkins|event` 跳过; 项目查不到/旧格式无项目段→**保留原值绝不置空**; ④ 完整改动点(文件:行号)+ 伪代码 + 测试计划 + 风险点: `references/source-ref-disk-path-display-plan.md`(本角色输出, 与架构评审/消费方清单互补)。

## Search result display elements (search_render.rs) — keep complete

Human output per hit: `[score] [entity_type] title` + `分析|原文|摘要: body` + `位置: file:Ls-e [signature]` or `来源: source_ref [hop=N]`. 准确率(score)/类型/摘要/来源 are contract-level elements for this user — when changing search output, extend the regression tests in `src/interfaces/cli/search_render.rs` (tests module asserts score/type/summary/source across Method/Entity/Doc hit shapes), not just search_mcp tests.

### ⚠️ 「分析: file: L98-98」位置串 = llm_analysis 缺失, 不是渲染 bug (2026-08-10)

症状: `[代码/Method]` 结果的 `分析:` 行显示 `src/.../Foo.java: L98-98` 这种位置串, 而不是「用途:.../逻辑:...」的 LLM 分析。**这是数据缺失, 不是搜索/渲染故障**:

- 渲染链: search_render.rs `render_hit()` 对 Method 取 `llm_analysis`, 为空回退 `snippet`; 而 code 世界的 snippet 是搜索端**构造的位置串** `format!("{}: L{}-{}", file_path, start_line, end_line)`(search_mcp.rs:142)——所以「分析」行出现 `file: Ls-e` 就等价于该点 `llm_analysis` 为空。
- **三站式确认: ① Qdrant scroll `code_methods`(filter `name`), 看 payload `llm_analysis` 是否 null; ② daemon 日志 grep 该文件名——0 次 = Phase 2 LLM 从未对它执行过; ③ `grep 'OpenAI-Compatible 响应'` 按小时分布状态码, 定位是哪波风暴(429=并发过高 / 404=url 带 `/v1`)。
- 已见根因 (2026-08-10, warehouse-center + pay-center): 429 风暴(并发 32)与 404 风暴期间 Phase 2 全失败, 方法节点入库但 `llm_analysis` 空; 事后重建若被中断(无「流水线分析完成」汇总行, 日志停在最后时间戳)则缺口保留。
- **修复必须 `dt build --path <proj> --full`**: 增量构建只处理变更文件, 缺的分析不会自己补齐。重建前先 `pgrep -af 'dt build'` 清残留, 确认 `openai_compatible.max_concurrent` 合理(≤4 旧规则对 opencode-go 已过时)。
- **⚠️ 先盘缺口再决定修法 (2026-08-11)**: 单点排查前先跑 `scripts/find_llm_analysis_gaps.py`(只读)看缺口规模与分类——缺分析可能是系统性的(全库实测 48.6% 方法点缺分析, message-center 高达 90%), 也可能是双写覆盖(见下下节)。系统性的缺口有比 --full 更省的增量修法(删 `file_snapshots` 行触发重分析), 详见下方「系统性 llm_analysis 缺口盘点 + 增量修复方案」一节与 `references/llm-analysis-gap-audit.md`。
- 排查顺序陷阱: 先看 Qdrant payload + 日志, 别先怀疑渲染层; 渲染回退逻辑本身是设计好的(空分析显示位置比「暂无摘要」更有定位价值)。

### ⚠️ llm_analysis 缺失的另两个根因 (2026-08-10 实测)——接口方法空 source_text + knowledge hit 硬编码 None

上文 429/404 风暴不是 llm_analysis 缺失的唯一来源, 另有两个独立根因:

**① Phase 2 接口方法空 source_text → 空 hash → 永不重分析 (build/pipeline.rs:363)**
- 症状: `--full` 重建后某些文件(尤其**接口/抽象类**)的方法 llm_analysis 仍空; SQLite `build_progress` 里这些方法 `file_sha1 = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`(= **sha256("") 空串 hash**), completed_at 是本次构建时间——看似已分析实为假完成。
- 根因: 接口方法无方法体, `m.source_text` 为空 → 触发回退 `fs::read_to_string(&m.file_path)`, 但 `m.file_path` 是**相对路径**而 tokio::spawn 任务无项目根上下文 → 读文件失败 → source_text 保持空 → hash=sha256("") 恒定 → LLM 收到空代码输出空内容 → `mark_llm_analyzed(prog_key, 空hash)` 写入**假完成记录** → 后续构建 `is_llm_analyzed` 命中 → 永久跳过。
- 修复: 回退读文件改为 `let fp = proj_root.join(&m.file_path)`(proj_root = `root.to_path_buf()` 在 execute() 内取, 克隆进 spawn 闭包)。
- 排查法: `sqlite3 /var/lib/digital-twin/snapshots.db "SELECT file_path, file_sha1 FROM build_progress WHERE project='X' AND file_sha1='e3b0c44...'"` 一条非空 hash 之外的记录即中招; Qdrant scroll `code_methods` 同名方法多个点(接口空 / 实现完整)是典型形态。
- 清假进度重跑: `DELETE FROM build_progress WHERE project='X' AND stage='llm_analysis'` 后 `dt build --path <proj> --full`(先 pgrep 清残留进程)。

**② knowledge hit `llm_analysis: None` 硬编码 → Config 实体永远「暂无摘要」 (retrieve.rs:1162)**
- 症状: `[配置/Config] bootstrap.yml 分析: 暂无摘要`, 而 Memgraph 里该 Entity 的 `summary` **有完整内容**; 其他类型(Service/Api)却显示摘要——因为渲染层 Config 分支读 `llm_analysis`(空→"暂无摘要"), 其他类型读 `snippet`(有值)。
- 根因: retrieve.rs `search_knowledge()` 构造 SearchHit 时 **`llm_analysis: None` 写死**, 知识实体的 summary 只进了 snippet 没进 llm_analysis。
- 修复: `llm_analysis: Some(c.summary.clone())`(知识实体无「方法分析」, 摘要即 summary; 同时 `snippet: c.summary` 需改 `.clone()` 避免 move 后 borrow, 否则 E0382)。搜索端字段映射见 search_mcp.rs:169-172(payload `llm_analysis`)。
- 验证: `dt search "bootstrap" --world knowledge` 应显示完整摘要而非「暂无摘要」; 663 lib 测试绿。

### ⚠️ 系统性 llm_analysis 缺口盘点 + 增量修复方案 (2026-08-11)

**症状**: 搜索 `[代码/Method]` 结果「分析:」行显示 `file: Ls-e` 位置串(如 `SmsFeignService.java: L27-28`)而非「用途：/逻辑：」→ 该点 payload `llm_analysis` 为空, 渲染回退 snippet(设计行为, 非 bug)。**单点排查不够——缺口常是系统性的**: 实测 message-center 项目 Qdrant `code_methods` 有 **1735 个方法点**, 而 build_progress 仅 **262 条** llm_analysis 记录 → **约 85% 的方法点从未获得 LLM 分析**(历史 429/404 风暴 + 构建中断批量造成)。

**缺口盘点三步法(只读)**: ① 点数 vs 分析数对比(`points/count` filter project vs `build_progress` count) → 差距=缺口规模; ② 排除空 hash 假完成根因(`file_sha1='e3b0c44...'` 为 0 → 非接口空 source_text 根因); ③ 日志时间线定根因(`grep '<文件>' dt-daemon.log`: 只有 `ProcessorEngine CPU start/done` 无 `StoreProcessor start` 也无 `Phase 2 ... 已写入` = 从未走到 Phase 2)。交叉比对注意 **build_progress.file_path 存 `method:{entity_id}` 而非源文件路径**。一键审计: `scripts/llm_gap_audit.py`(只读, 输出 /tmp/dt_llm_gap_report.json)。

**全库实测(2026-08-11)**: code_methods 4695 点缺分析 2282 (48.6%); message-center 1573/1735 (90%) 重灾区; 缺分析中 1658 从未处理 + 624 被覆盖。**已根治**: 三层自愈架构落地后 Phase 2 空响应=失败写状态位, 构建末尾补偿自愈自动补齐(见 references/phase2-three-layer-self-healing.md)。

### ⚠️⚠️ llm_analysis 缺口机制全解(2026-08-11 修正早前"双写竞争"结论)

**症状**:反复增量构建后,已分析的方法 llm_analysis 又变空(实测 message-center 1735 点中 1573 缺,全库 4695 点中 2282 缺=48.6%)。日志显示方法曾「LLM 分析已写入 Qdrant」成功,但 Qdrant 点后来变空。

**根因(二次核查修正)**:原诊断「StoreProcessor 覆盖 code_methods」**不成立**——代码级验证:StoreProcessor(store.rs)只调 `Consolidator::consolidate_document` 写知识实体(kg_nodes/doc_chunks),全库 `CODE_METHODS` 写入点 grep 仅 2 处(pipeline.rs:290 Phase 1 无分析 payload、pipeline.rs:494 Phase 2 带分析)。真实缺口机制 = **文件变更时 Phase 1 先以无分析 payload 覆盖点 → Phase 2 对该文件分析失败(超时/500/中断)→ 缺口固化**(且因增量只处理 hash 变更文件,永不重试,见「Phase 2 重试承诺断链」节)。日志里「StoreProcessor start + 流水线文件处理完成」与点变空的先后顺序是**时间巧合,不是因果**。

**修复(用户拍板,✅ 已实施验证)**:三层分离架构——基础层(AST/图谱/base 向量确定性)+ LLM 增强层(llm_analysis+llm 向量+llm_status 状态位,无降级)+ 构建末尾补偿自愈;检索 = base 召回 + llm rerank(named vectors)。674 测试绿,doctor-center e2e 通过。详见「Phase 2 三层自愈架构」节。

### ⚠️⚠️ Phase 2 "失败自动重试"是断链承诺 — 缺口永久固化根因(2026-08-11 代码级确认)

**核心缺陷**:pipeline.rs:537 失败时打日志「Phase 2 结果未持久化，下次增量构建将重试」——**这是假承诺**:
1. Phase 2 遍历范围 = `extraction.methods` = 仅本次 files_to_process(**文件 hash 变化**)提取出的方法(pipeline.rs:356)
2. 增量构建 select_files(pipeline.rs:128-132)只把 hash 变化的文件送进提取 → 未变更文件的方法**永远进不了 Phase 2**
3. 失败不写任何持久化标记(不 mark_llm_analyzed)→ 下次增量构建根本看不到该文件 → **永不重试**
4. 全量构建 --full 同样中招:1607 个方法哪怕 22 个超时/500,这 22 个永久缺分析,除非文件再被改动或再 --full

**后果**:任何一次构建中的 LLM 失败都会固化缺口。实测(2026-08-11):code_methods 4695 点中 **2282 缺 llm_analysis(48.6%)**;message-center 90%(1573/1735)、hospital-center 57%、pay-center 12%、archive-api 12%、copartner-h5 15%。

**判别口诀**:「增量构建补不上分析」= 机制缺陷(重试断链),不是数据污染;不要重跑 --full 期望增量自愈,先修机制。

### ⚠️ llm_analysis 缺口盘点方法(2026-08-11 实测)

**关键陷阱**:SQLite build_progress 表 stage='llm_analysis' 行的 file_path 字段**实际存 `method:{entity_id}`**(如 `method:dt://entity/message-center/class/SmsFeignService/method/sendSmsCode@27`),**不是源文件路径**!`WHERE file_path LIKE '%Foo.java'` 永远空 → 会误判「从未分析」。

**正确交叉比对**(Qdrant 点 payload.entity_id ↔ build_progress.file_path 去 `method:` 前缀):
1. Qdrant scroll code_methods 全量(分页 5000/页)筛 llm_analysis 空/缺失点
2. SQLite 取全部 stage='llm_analysis' 记录转 entity_id 集合
3. 缺口点 ∩ 记录集 = 有记录但仍缺(2026-08-11:624 个,系文件变更后 Phase 1 覆盖+Phase 2 失败);缺口点 − 记录集 = 从未被 Phase 2 处理(1658 个)

**一键审计脚本**:`scripts/llm_gap_audit.py`(只读,Qdrant scroll + SQLite 交叉,输出 /tmp/dt_llm_gap_report.json 含按文件聚合的缺口清单,可直接生成 DELETE 清单喂给修复步骤)。

### Display-type semantics (doc vs entity hits) & IMPLEMENTED two-dimension design

`[Doc]` + `原文:` = doc_chunks hit; `[Type]` + `摘要:/分析:` = KG entity node. **IMPLEMENTED 2026-08-06 (user-confirmed design, built & verified)**: `entity_type` split into TWO orthogonal dimensions:

- **`file_type`** — what the file is, by extension. Static map in `src/domain/file_type.rs`: `FileCategory::{Document, Code, Config, Other}`; `categorize_path()` / `categorize_ext()` / `resolve_file_types()`. Category slugs: `document` (NOT `doc` — avoids clashing with `world=doc`), `code`, `config`. Display labels: 文档/代码/配置/其他. Suffix map: md/doc/docs/txt/rtf/pdf/rst → 文档; java/go/rs/php/py/js/ts/c/cpp/cs/rb/kt/swift/sh/sql/proto/... → 代码; yaml/yml/properties/json/toml/ini/conf/xml/env/... → 配置.
- **`content_type`** (existing `entity_type`) — what the content means: LLM vocabulary (Service/Config/Standard...) for knowledge world; AST types (Method/Class/Function) for code world; **doc-world heuristic**: extension-based (yaml/properties → `Config`, md/txt → `Doc`) since doc chunks have no LLM classification.
- **`SearchHit`** carries `file_type: Option<String>` (slug) + `file_type_label: Option<String>` (中文显示名) + existing `entity_type`. `SearchRequest` carries `file_type` + `entity_type_filter` (both Option<String>).
- **Unified post-processing** in `search_mcp.rs`: `postprocess_hits()` (1) fills `file_type`/label from `file_path` → `source_ref` → `id` via `infer_file_type_pub()`; (2) filters by file_type (resolve spec to category set, match slug); (3) filters by entity_type (case-insensitive equality). Called ONCE before returning from `search()` — no need to touch every per-world SearchHit constructor.
- **CLI**: `dt search <q> --file-type document|code|config|md|yaml|java... --content-type Config|Method...` (alias `--type`). Both optional; neither = all. grpc: `proto/dt_core.proto` SearchRequest added `file_type=11`, `entity_type_filter=12` (mapped in `build_service.rs`).
- **Rendering** (`search_render.rs`): `[score] [类别/内容类型] title` e.g. `[0.0164] [配置/Config] config.yaml`, `[文档/Service] 支付服务`, `[代码/Method] doPay`. JSON includes `file_type` + `file_type_label` for MCP consumption.
- **Verified 2026-08-06**: build OK; `--file-type yaml` → only config.yaml; `--file-type java` → only doPay; `--file-type code --content-type Method` → only doPay; `--content-type Config` → only config.yaml; `--file-type document` → all md-sourced hits; no filters = all. Regression 12/12. Both original complaints resolved: `config.yaml` no longer shows bare `[Doc]`; md-sourced entities honestly show `[文档/Service]`.
- ⚠️ **Pitfall — batch regex/sed edits on Rust source are dangerous**: a Python script that inserts `file_type: None, file_type_label: None,` after every `entity_type:` line matched WRONG locations — inside struct DEFINITIONS (`RankedItem`, `SearchRequest`), inside function SIGNATURES (`blank_hit` params), and inside `if` expressions (grpc `doc_id: if ... {` block). Result: `E0573 expected type found variant None`, `E0124 field already declared`, `struct literal body without path`. Recovery was manual per-site patches (removing stray lines, changing `None` → `String::new()` for proto String fields). **Lesson: for Rust struct-literal field additions, patch each constructor site precisely (or add `#[serde(default)]`-friendly Option fields and fix compile errors one location at a time via `cargo check`), never blanket-insert by pattern.** Also: after ANY worker/bulk edit, `cargo check` + `git diff` inspection before accepting.

Full root-cause analysis + change surface: `references/search-display-types.md`.

Nacos/Memgraph/Qdrant/SQLite 只读审计、ConfigKey 身份与 `dt://nacos` 来源设计：`references/nacos-sync-data-model-audit.md`。其中包含 namespace/dataId/group/content 追踪、`config_chunks` 分块、environment/public 配置和本地目录隔离的复用审计矩阵。

## Duplicate search hits (same file indexed under two projects)

Symptom: `dt search "支付"` shows `doPay`/`createPay` twice (identical file+lines), each with a different Qdrant point id. RRF (`fusion.rs`) keys on `world:id`, so two points = two hits — NOT a fusion bug.

Diagnosis (scroll `code_methods` filtered on `name`): two points, same `file_path`, **different `project`** — e.g. `test-pipeline` AND `digital-twin-v2` both index `/data/myProject/digital-twin-v2/test/fixtures/java/PayChannelService.java`. Root cause is a **project-boundary gap**: `digital-twin-v2`'s own `config/config.yaml` `scanner.ignore_dirs` lacks `test/fixtures`, so its own build scans the test fixtures as real code, while `dt build --test` (test-pipeline, `main.rs` builds `/data/myProject/digital-twin-v2/test` with name `test-pipeline`) indexes the same physical files. Two projects → two points → duplicates in every query touching that file.

Fix (two layers, do both):
1. **Data (immediate)**: delete the Qdrant `code_methods` points for the project that shouldn't own the file (e.g. keep `test-pipeline`, delete `digital-twin-v2` points whose `file_path` contains `/test/fixtures/`). REST: scroll with filter → collect ids → `POST :6333/collections/code_methods/points/delete`.
2. **Config (root, prevents rebuild recurrence)**: add `test/fixtures` to `scanner.ignore_dirs` in `config/config.yaml`. ⚠️ **2026-08-10 修正: 此条按当前实现无效** — scanner 段从未被加载(死配置)且 ignore_dirs 只按单段目录名匹配, `test/fixtures` 永不命中。必须先修代码(见「config.yaml `scanner:` 段是死配置」一节)让 scanner 段生效并支持路径前缀匹配, 该配置条目才有意义。

Related: **same-doc multi-block repeats (`decision-ifcode-waycode.md:0/1/2`) are NOT duplicates** — different chunk indexes of one doc all matched; that's expected retrieval behavior, not a bug (merge-in-display is an optional enhancement, not a fix).

## ⚠️ elementId missing on ALL Memgraph Entity nodes (historical)

Every knowledge search logs `种子 <business_id> 缺少 elementId，跳过图扩展` (WARN, `retrieve.rs expand_business` ~L450). Diagnosis 2026-08-06: `MATCH (e:Entity) RETURN count(e), sum(elementId IS NULL)` → **2995/2995 nodes lack `elementId`** — the attribute was never written historically (0 have it). Meanwhile Qdrant `kg_nodes` payloads DO carry `elementId` as a string (verified `'69117'`), so the two stores are inconsistent: payload has it, Memgraph node doesn't. The Rust read is `.get("elementId").and_then(|v| v.as_str())` on the **Qdrant payload** — that path works; the missing link is the **Memgraph-side property**. Consequence: graph expansion silently no-ops for ALL seeds (warned, not fatal; vector+keyword retrieval still returns hits).

Options: (a) backfill Memgraph `SET e.elementId = <qid>` from Qdrant payloads (one-time bolt write, restores graph expansion); (b) code-only: downgrade the WARN to DEBUG in `expand_business` so terminal stays clean (user asked for the WARN to go away); (c) delete the specific node if genuinely orphaned. User's direction (2026-08-06): remove the warning log; if the source data is genuinely missing, the data source may be deleted.

**Applied 2026-08-06**: (b) done — `retrieve.rs expand_business` WARN → `tracing::debug!` (silent skip, terminal clean). User then chose (c) escalated to a **full KG wipe + rebuild** (the historical data was wrong at many levels: no elementId, old-vocabulary misclassification, Memgraph/Qdrant inconsistency) — see "KG reset & rebuild procedure" below.

## ⚠️ 删除源文件后 KG/向量库清理机制 (2026-08-10 代码确认) — purge_document 与实体保留策略

用户问「删除源文件(dt://doc/... 的 .md),对应的知识图谱和向量库是否会删除」——**会,但有保留策略**。机制:

- **删除检测**: 增量构建 `select_files`(build/pipeline.rs:129-177)通过快照对比产出 `deleted`; 文档删除走 `is_document_path` 过滤(避免代码路径误报), 然后对每个删除的文档调 `purge_document(graph, vector, doc_id)`(consolidate.rs:557-583, 幂等可重跑)。
- **purge_document 清理 4 样东西**: ① `RELATES` 边(`MATCH ()-[r:RELATES {doc_id}]->() DELETE r` — 实体到该文档的关联); ② `MENTIONED_IN` 溯源边; ③ `Document` 节点本身(`MATCH (d:Document {doc_id}) DELETE d`); ④ 该文档全部 `doc_chunks` 向量点(`delete_by_filter` 按 doc_id)。
- **保留策略**(consolidate.rs:553 注释明确): **Entity 节点不随文档删除** — 只要被别处引用就保留; 孤儿实体由 §6.5.4 定期清理流程处理, 不是删除文档时立即清。实测: 删 `朱啸天_git提交记录分析报告.md` 后, Document 节点/边/向量会清, 但「通联支付相关接口」等实体仍出现在搜索(仅失去 RELATES 溯源)。
- **若用户要求实体随文档删除**: 需另加「删源文档 → 删仅由它产生的孤儿实体」逻辑(设计决策, 需用户批准, 不能默认做)。
- 相关: 全量 clean 见「`dt clean --confirm`」一节; 实体分类/重分类见「KG Entity Type Classification」。

## KG reset & rebuild procedure (2026-08-06, user-approved)

When the knowledge layer is so stale that surgical fixes aren't worth it (all Entity nodes missing elementId, old prompt-vocabulary misclassifications baked in, Memgraph/Qdrant payload drift), the user-approved path is: **clear all KG data, rebuild with the current (fixed) prompts** — LLM re-extracts with the new vocabulary so types come out right automatically. Do NOT touch code/doc/config indices (they're extension/AST-based, no LLM classification needed).

1. **Backup first** (destructive): scroll `kg_nodes` with `with_vector: false` paginated (offset loop, 1000/页) → `/tmp/backup_kg_nodes.json`. 3042 points ≈ 1.4MB.
2. **Clear Qdrant**: `POST :6333/collections/kg_nodes/points/delete` with `{"filter": {}}` (delete-all). `operation_id` returned, `status: acknowledged`.
3. **Clear Memgraph Entity only**: `MATCH (e:Entity) DETACH DELETE e RETURN count(e)` via bolt. Preserves Method/Class/Module/Document/Project nodes (AST-derived, ~3120 nodes + 4168 edges stayed after deleting 2995 Entity). Node-label census query before deleting: `MATCH (n) UNWIND labels(n) AS lbl RETURN lbl, count(*)` — confirms what's Entity vs structural.
4. **Rebuild**: `dt build --path <project> --full` (LLM re-extraction via qwen3.5; slow — minutes per doc set). **Pitfall: `dt build --help` can hang >300s** when the binary does backend/Xinference init on startup and the LLM is slow — don't probe help first, go straight to the rebuild command with a generous timeout (background it).
5. **Watch the rebuild log for vocabulary WARNs** (`LLM 返回词表外实体类型 'X'，归一为 Other` in daemon log / dt stdout) — they reveal prompt-vocabulary vs `EntityType` enum drift (see the enum sync pitfall above). If they appear, KILL the rebuild, fix the enum (`model.rs`), `cargo build --release`, and restart the rebuild — otherwise you bake thousands of `Other`-misclassified nodes into the fresh KG. A rebuild killed mid-run leaves no partial KG (Memgraph/Qdrant writes are transactional per doc), so restarting from clean state is safe. Monitor progress via `curl :6333/collections/kg_nodes/points/count` + `sudo tail /var/log/digital-twin/dt-daemon.log` (INFO lines `Phase 2 完成 <method>` = per-method LLM analysis; **realistic budget: Phase 2 alone ≥1h, full rebuild 4-5h** — measured 2026-08-06: started 19:33, still in Phase 2 at 21:47 with 1797/~3000 entities = 60%; measured 2026-08-07: full rebuild 22:46 → 03:30 = **4h43m** for 2769 entities; the old "10-20 min" estimate was wrong, always plan for 4+ hours and run in background).

### ⚠️ Rebuild pitfalls (2026-08-06, user-corrected)

- **Mid-rebuild code change: swap to the new binary IMMEDIATELY, don't defer.** If you edit extraction code (enum mappings, prompt vocabulary) while a rebuild runs, the running process holds the STALE binary — `cargo build --release` does NOT affect the running process. The trap: choosing to "let the old binary finish and fix misclassified nodes later with an incremental rebuild". The user rejected this (`为什么不直接使用新二进制跑呢`). Deferring means (a) thousands of `Other`-misclassified nodes bake into the fresh KG, (b) the "incremental fix" never re-classifies existing nodes cleanly, (c) you end up doing a second full rebuild anyway. Correct: kill the process, apply the mapping fixes, recompile, clear the partial data (`MATCH (e:Entity) DETACH DELETE e` + Qdrant kg_nodes delete-all, steps 2-3 above), restart `--full`. One extra restart costs less than a KG full of wrong types. (This session: three waves of vocabulary WARNs — `module`, then `project`/`component`/`process` — each found while the old binary was mid-flight; fixes were `model.rs` `Deserialize` arms: `component|process|procedure → Concept`, `project → Other`.)
- **Verify the background rebuild actually STARTED before walking away.** A rebuild launched as a terminal background process from a session that then died can silently never run: `/tmp/dt_rebuild<N>.log` never created, no build records in daemon log, KG stays at 0. Symptom when the user searches: daemon log shows `搜索: query=... keyword_recall: kw=... 查询返回 rows=Some(0)` — empty results because the KG is EMPTY, not because the binary is old. Check after launch: (1) `ls -la /tmp/dt_rebuild<N>.log` exists (even 0 bytes), (2) `ps aux | grep 'dt build'` shows the process, (3) `sudo tail /var/log/digital-twin/dt-daemon.log` shows pipeline processors loading (`处理器: Chunk/LlmClient/Store`). Only then is the rebuild actually running.
- **"Search shows nothing" triage order**: (1) binary freshness — `md5sum ~/.local/bin/dt target/release/dt` equal + recent mtime (symlink `~/.local/bin/dt → target/release/dt`, so `cargo build --release` auto-deploys, no install step); (2) data counts — `MATCH (e:Entity) RETURN count(e)` via bolt + Qdrant `kg_nodes` count; both 0 = rebuild never ran or data was wiped, NOT a search bug. Don't debug the search path before checking these.
- **Content missing because its project is OUT of index scope**: before assuming a search/rebuild bug, check WHERE the expected content physically lives vs what `~/.config/digital-twin/config.yaml` `projects:` actually indexes. Example 2026-08-07: `支付二维码` exists only in `/data/myProject/miaosha-GLM-dev`, `Trae-Account-Manager-yuan`, `OpenCode-Register` — but the `base: /data/myProject` items list only includes `digital-twin-v2, svc, kub, jenkins-cli-rs, neatReader`. Search returns only test/fixtures + digital-twin-v2 doc hits for 支付 because the owning projects are simply not indexed — a config/scope matter, not a search bug. Triage: `grep -rl "<term>" <indexed-bases>` (e.g. `/data/myProject /data/aflmProjects`) to locate the real owner, then compare against the projects list. Indexing a new project = adding it to `projects:` (or a doc path) + `dt build --full` — a config change, so propose a plan to the user first.

After rebuild, verify: `dt search "<q>" --world knowledge` shows correct types (Standard etc.), no WARN spam, no duplicate hits.

## dt_search_kg world/project 参数（2026-08-12 新增）

`dt_search_kg` MCP 工具现支持 `world` 与 `project` 参数（mcp-server.py 2026-08-12 改动）：

- 默认 `world=knowledge`（向后兼容；knowledge 世界只含配置/服务实体，约 55 个节点）
- 检索**代码实体**（Class/Method）必须传 `world="code"`——否则命中率为 0，结果被其他项目污染
- `project="im-center"` 等限定项目，消除跨项目噪音
- 例：`dt_search_kg(query="发送单聊消息 sendMessage", world="code", project="im-center", limit=5)`
- CLI 等价：`dt search "发送单聊消息 sendMessage" --world code --project im-center --limit 5`

**历史教训**：2026-08-12 团队测试发现 dt_search_kg 硬编码 knowledge world 导致 im-center 检索 0% 命中（结果全被 message-center 污染）。修复=加 world/project 参数。排查"KG 检索不到代码实体"时先检查是否用了 world=code。

## --file 单文件构建误删 bug（2026-08-12 修复）

`dt build --file X` 曾误删 Memgraph 中该项目其他文件的方法节点（pipeline.rs 步骤 2/3）：
- 根因：`--file X` 时 all_files=[X]，IncrementalStrategy 把快照中其余文件全部判为 deleted → delete_files_from_graph 全删
- 症状：构建后 Memgraph 中项目方法数崩到个位数（Qdrant 向量仍在，sense 的 methods 从 Memgraph 读 → 崩）
- 修复：pipeline.rs 步骤 2 单文件模式直接 `(all_files.clone(), Vec::new())`，跳过 deleted 检测
- 教训：用 `--file` 单文件验证时，若随后 Memgraph 计数异常减少，立即怀疑此路径；全量 `--full` 无此问题

## TsJavaParser 注释错位 bug（2026-08-12 修复）

`extract_comment`（tree_sitter_utils.rs）曾把前一个方法的 javadoc 错误关联到无注释的后续方法：
- 根因：comment_lines 为空时遇到非注释节点（上一个方法节点）不 break，继续向前跨过它偷取其 javadoc
- 症状：KG 中多个无注释方法显示相同错位注释（如 groupMsgGetSimple/sendGroupSystemNotification/sendGroupMsg 全显示"删除群成员消息"）
- 修复：遇到非注释节点无条件 break（tree-sitter 中空白不产生节点，方法的前兄弟要么是紧邻注释要么是前成员）
- 注意：Java 文件实际由 **TsJavaParser（tree-sitter）解析**（ParserRegistry 中优先），JavaParser 是正则回退——调试 Java 解析问题必须看 ts_java.rs + tree_sitter_utils.rs，不是 java.rs！
- 测试：tree_sitter_utils::tests::comment_not_stolen_from_prev_method + adjacent_comment_still_extracted（676 测试全过）
