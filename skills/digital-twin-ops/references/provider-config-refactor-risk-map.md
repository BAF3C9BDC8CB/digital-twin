# Provider 配置重构风险地图（glmcoding → openai_compatible / 删 inference_server / xinference 补 max_concurrent）

> 来源：2026-08-11 兼容性风险角色评估（全库 rg 证据，实施前必读）。计划改动：
> ① `providers.glmcoding`（GLMCodingProviderConfig / GLMCodingChatClient / build.rs "glmcoding" 分支 / infer_client.rs 日志 / config.rs 默认值）改名通用 OpenAI 兼容 provider（拟名 `openai_compatible`）；
> ② 删除顶层 `inference_server` 段（InferenceServerConfig + build.rs 三处使用点），引擎并发改从当前 llm_provider 的 max_concurrent 读取；
> ③ xinference provider 新增 max_concurrent 字段。

## 1) 代码引用点（文件:行号）

### glmcoding 改名涉及
- `src/application/pipeline/config.rs:233` `pub glmcoding: Option<GLMCodingProviderConfig>` — 字段改名，旧键用 `#[serde(alias = "glmcoding")]` 保留
- `src/application/pipeline/config.rs:318-351` GLMCodingProviderConfig 结构 + 5 个 default_glmcoding_* 函数 + Default（url=`https://glmcoding.cn`、model=`deepseek-v4-flash`、protocol=`openai`、max_concurrent=32）
- `src/interfaces/cli/build.rs:13` import；`:325-343` match `"glmcoding"` 分支（默认 url/model/concurrency 32、日志 "使用 GLM Coding LLM..."）
- `src/application/pipeline/infer_client.rs:420-447` GLMCodingChatClient 结构 + new()（L430 env `GLMCODING_API_KEY` 回退、L440 默认 url、L445 semaphore max(1)）；`:448-560` health_check/chat（**日志 L501/519/542 `provider = "glmcoding"` 硬编码**；"GLM Coding" 错误串 9 处）；`:564-588` ChatClient impl
- `src/application/pipeline/processors/llm_client.rs:54` provider 字段；L187/197/202/226 日志 `provider = %self.provider` = YAML 原始字符串

### inference_server 删除涉及
- `config.rs:26` PipelineConfig 字段；`:57-86` InferenceServerConfig（url 默认 SF、grpc_url、max_concurrent 默认 16）+ Default；`:400` default 构造；`:421-422` + `:434/:457` **测试断言（删字段必须同步改测试）**
- `build.rs:98-101` handle_build 普通构建传 `inference_server.max_concurrent`；`:423` run_pipeline_analysis 的 infer_max_concurrent；`:489` `ProcessorEngine::new(registry, inference_server.max_concurrent)`（引擎闸门）
- `engine.rs:54-59` ProcessorEngine.max_concurrent 字段

### xinference 补字段涉及
- `config.rs:287-305` XInferenceProviderConfig **当前无 max_concurrent** → 新增 `#[serde(default=...)]`
- `infer_client.rs:274-277` XInferenceChatClient::new(base_url, api_key, max_concurrent) 第 3 参现由 build.rs 传 `inference_server.max_concurrent` → 改读 xi_cfg.max_concurrent
- `build.rs:344-367` xinference 分支（L362-366 用函数参数）

### 间接相关
- `build.rs:459-469` LlmClientProcessor::new(..., p.llm_provider.clone(), ...) — provider 字符串进 processor 日志
- `main.rs:674`、`build.rs:260` llm_provider → embedder::ProviderConfig（仅路由字段）
- `src/infrastructure/provider_router.rs:156-168` llm_provider() 只认 siliconflow|xinference，其他 Err("未知的 llm provider") — **死路径**（全库无 LlmService::chat 调用），glmcoding 现在同样 Err，改名无回归
- 硬编码 "siliconflow"（不受影响）：grpc/wiring.rs:331、grpc/services/build_service.rs:112/135、cli/sync.rs:64
- 测试：tests/s5_knowledge_search.rs:44/54/225/227（ProviderConfig 构造 xinference，不受影响）
- mcp/mcp-server.py：仅 L67 注释提及 providers；L734-735 dt_health 调 `dt health` CLI → **Python 层零改动**

## 2) 配置文件引用
- 仓库 `config/pipeline.yaml:20` `llm_provider: glmcoding`；`:48-53` glmcoding 段（url=`https://opencode.ai/zen/go`、api_key、protocol=openai、model_llm=deepseek-v4-flash、max_concurrent=32）；`:56-58` inference_server 段（max_concurrent=1）；`:41-47` 注释残留明文 api_key 历史值（顺手清理）
- `config/pipeline.yaml.bak.20260809184926:20/42-43` 历史备份（git 历史，不改）
- `config/config.yaml.example:23-25` glmcoding 示例段 → **模板需同步改**
- 仓库 `config/config.yaml` 与 `~/.config/digital-twin/config.yaml`：无相关项
- **`~/.config/digital-twin/pipeline.yaml` 与仓库版 diff IDENTICAL（普通文件非 symlink，手工同步）** → 迁移必须双端同时改

## 3) 文档/技能/脚本
- 仓库 `skill/SKILL.md:122-124`（`glmcoding` → GLM Coding 映射表）、`:131-132`（max_concurrent: 4 示例）
- `docs/phase2-self-healing-design.md:88`（glmcoding 客户端重试）
- `docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md:857`（inference_server.url 旁注）
- 本 skill SKILL.md 需同步 7 处 glmcoding 记录：L172（max_concurrent 历史 + `python3 -c '...["providers"]["glmcoding"]["max_concurrent"]'` 检查命令——改名后失效）、L173（双并发旋钮陷阱）、L183（路由统一历史）、L188-198（glmcoding=通用 OpenAI 兼容客户端、protocol 字段"不要动"）、L494（重建指令）
- scripts/fixes/fix2.py、fix_pipeline.py 只改 xinference/siliconflow，不含 glmcoding
- logs/session-ses_0476.md 大量历史记录（L3157/3290 inference_server 证据链）——不改但会过时

## 4) 外部契约影响
- **daemon 日志 provider= 字段**：infer_client.rs L501/519/542 硬编码 "glmcoding" 改名后变新值；llm_client.rs provider=%self.provider 输出配置原串；其余 "pipeline"/"storage"/"sqlite"/"siliconflow" 不受影响 → 按 provider="glmcoding" 过滤的日志消费者会失效
- **健康检查**：cleanup.rs check_health! 宏输出固定标签（Memgraph/Qdrant/SQLite/Embed），无 provider 名，不受影响
- **SQLite/Qdrant 落库**：sqlite/repo.rs、qdrant/repo.rs、domain/types.rs 均无 provider 字段 → 数据零兼容问题
- **gRPC 契约**：不含 provider 名
- MCP dt_build 输出透传 daemon 日志 → 会带新 provider= 值

## 5) 兼容策略（关键结论）
1. **llm_provider 是 String 非枚举 → serde alias 对值无效**，必须 build.rs match 分支双匹配 `"glmcoding" | "openai_compatible" => {...}`（build.rs:326）。serde alias 只对 providers 段的**键**有效（`#[serde(alias = "glmcoding")]` 让旧 YAML `glmcoding:` 键反序列化进新字段）
2. **GLMCODING_API_KEY env 必须保留回退**（infer_client.rs:430）：建议 OPENAI_COMPATIBLE_API_KEY 优先、GLMCODING_API_KEY 兜底共存一个版本周期
3. **旧配置不迁移的后果**：YAML 显式写了 llm_provider: glmcoding → 不走默认值（默认 "siliconflow" 只在字段缺失时生效 config.rs:242），而是 match 落 `_` 兜底 → **静默走 SiliconFlow 分支（build.rs:369-397）**。这正是 L183 记录的历史事故（glmcoding 静默落 SF 分支，真实请求发往 SF）。SILICONFLOW_API_KEY 有效则真实打到 SF（错计费），否则 401 静默降级。**无 alias 兼容 = 最高风险**
4. **serde 未知字段**：全库无 deny_unknown_fields，删除 inference_server 后旧配置残留段静默忽略不报错；但 build.rs:100/423/489 三处字段引用必须同步改否则编译失败
5. **xinference max_concurrent 默认值**：现有效值 1（本地串行）；默认 16/20 会改变并发行为，建议保守（4-8）并文档注明

## 6) 风险分级与规避
- **H1** 旧配置不迁移 → 静默错路由 SF：match 加 "glmcoding" 别名 ≥1 版本 + 启动 warn + 迁移文档
- **H2** build.rs 三处 inference_server.max_concurrent 引用与结构删除不同步 → 编译失败/并发漂移：同 PR 原子修改，抽公共函数从当前 provider 读并发
- **H3** ~/.config 与仓库 pipeline.yaml 只改一端 → 新旧名混用：双端同步 + 提交前 diff 校验（当前 IDENTICAL 可作基线）
- **M1** provider= 日志值变化 + 本 skill L172 检查命令失效：skill 同步更新 7 处
- **M2** 删 GLMCODING_API_KEY 回退 → env 用户 key 静默变空：双 env 名共存
- **M3** xinference 并发默认值变化：保守默认 + 文档
- **M4** 文档/模板不同步（skill/SKILL.md、config.yaml.example、docs/phase2）：同 PR 一并更新
- **M5** config.rs 测试 L421-422/434/457 断言 inference_server：同步改断言否则 cargo test 红
- **L** provider_router 死路径（无回归）；logs 历史记录（不改）；MCP/健康检查/SQLite/Qdrant 零影响

## 迁移核对清单（实施时逐项打勾）
- [ ] build.rs:326 分支 `"glmcoding" | "openai_compatible"`
- [ ] config.rs providers 键 serde alias；GLMCodingProviderConfig → OpenAiCompatibleProviderConfig（默认值函数改名）
- [ ] build.rs:100/423/489 三处并发来源改读当前 llm_provider 的 max_concurrent
- [ ] infer_client.rs GLMCodingChatClient 改名 + L501/519/542 日志 provider= 值 + env 双名回退
- [ ] config.rs 测试 L421-422/434/457 同步改
- [ ] 仓库 config/pipeline.yaml + ~/.config/digital-twin/pipeline.yaml 双端同步（当前 IDENTICAL）
- [ ] config/config.yaml.example 模板更新
- [ ] 仓库 skill/SKILL.md:122-132、docs/phase2:88 更新
- [ ] 本 skill SKILL.md 7 处 glmcoding 记录 + L172 检查命令更新
- [ ] xinference 段补 max_concurrent（保守默认值）+ config.yaml.example 同步
