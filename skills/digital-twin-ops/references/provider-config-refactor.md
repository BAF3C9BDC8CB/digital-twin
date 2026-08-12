# Provider 配置重构(glmcoding → openai_compatible)— 2026-08-11 实施计划存档

目标:① providers.glmcoding 改名 openai_compatible(含 Rust 结构体/客户端类/分支/日志字符串 + pipeline.yaml);② 删除 inference_server 段,引擎并发改从当前 llm_provider 的 max_concurrent 读取;③ xinference provider 补 max_concurrent 字段。

## 重构前代码地图(精确 file:line)

### src/application/pipeline/config.rs
- `PipelineConfig { enabled, inference_server(:24-26), processors, llm, ecosystem, providers(:44) }`
- `InferenceServerConfig` :55-91(url 默认 `https://api.siliconflow.cn/v1`、max_concurrent 默认 16)— **整块删除**
- `ProvidersConfig` :210-234(`glmcoding: Option<GLMCodingProviderConfig>` 在 :231-233;xinference 在 :228-229)
- `SiliconFlowProviderConfig` :246-283(max_concurrent 默认 20)
- `XInferenceProviderConfig` :285-314(**无 max_concurrent 字段** ← 要补,默认 16)
- `GLMCodingProviderConfig` :316-354(url 默认 `https://glmcoding.cn`、model_llm deepseek-v4-flash、protocol openai、max_concurrent 默认 32)
- `impl PipelineConfig { pub fn load() }` :360-392 ← **新增 helper `llm_provider_max_concurrent` 放这里**
- `impl Default for PipelineConfig` :395-403(:400 有 `inference_server: InferenceServerConfig::default()` ← 删)
- tests :405-470(`default_config_is_valid` :421-422 断言 inference_server.url/max_concurrent;`deserialize_full_config` :434-437 YAML 含 inference_server 段、:457 断言 ← 全删,补 helper 断言)

### src/interfaces/cli/build.rs(865 行,无测试模块)
- import :12-14 `ChatClient, GLMCodingChatClient, SiliconFlowChatClient, XInferenceChatClient`
- :97-101 第一处调用 `build_llm_client(&pipeline_config, pipeline_config.inference_server.max_concurrent)`
- `build_llm_client` :315-399 签名 `(&PipelineConfig, max_concurrent: usize)`;分支:"glmcoding" :326-343、"xinference" :344-368(用外部参数 max_concurrent ← 改自读 cfg)、`_` siliconflow :369-397
- :422-426 第二处调用(run_pipeline_analysis):`let infer_max_concurrent = pipeline_config.inference_server.max_concurrent;`(:423 ← 删)
- :489 `ProcessorEngine::new(registry, pipeline_config.inference_server.max_concurrent)` ← 改 helper

### src/application/pipeline/infer_client.rs
- `GLMCodingChatClient` :420-588;`new()` :427-447(env 回退 `GLMCODING_API_KEY` :429-433 ← 改先读 `OPENAI_COMPATIBLE_API_KEY` 再回退旧变量;url 默认 `https://glmcoding.cn` :439-443 **值保留**)
- health_check :448-458;chat :459-560;`provider = "glmcoding"` 日志标签 :501/:519/:542;日志前缀 "GLM Coding" :457/:495/:506/:513/:524/:530/:547/:554/:557
- `impl ChatClient` :564-588
- tests :594-630 与 GLM 无关(chat_response 序列化 + SiliconFlow 构造),可加 `openai_compatible_chat_client_can_be_constructed`

### 其他
- engine.rs: `ProcessorEngine::new(registry, max_concurrent)` :132 — max_concurrent 控制 run_gpu_stages(Semaphore :417 + buffer_unordered :482),当前=1 是构建慢根因
- config/pipeline.yaml: `llm_provider: glmcoding` :20、glmcoding 段 :48-54(url=`https://opencode.ai/zen/go`、max_concurrent: 32)、inference_server 段 :55-58(url `http://localhost:9997/v1`、max_concurrent: 1 ← 删)
- `~/.config/digital-twin/pipeline.yaml` 与仓库 config/pipeline.yaml 需同步(本会话 diff 确认 SAME)
- 文档引用(可选改): config/config.yaml.example :23、skill/SKILL.md :124/:131、docs/phase2-self-healing-design.md :88

## 关键设计决策(已定)
- helper 签名 `pub fn llm_provider_max_concurrent(&self) -> usize`,match llm_provider:openai_compatible→32、xinference→16(与旧 InferenceServerConfig 默认一致,行为零变化)、其他→20
- build_llm_client **去掉 max_concurrent 参数**,各分支自读 cfg(xinference 读 `xi_cfg.map(|c| c.max_concurrent).unwrap_or(16)`)
- env 回退:先 `OPENAI_COMPATIBLE_API_KEY` 再 `GLMCODING_API_KEY`(兼容既有部署)
- `https://glmcoding.cn` 是既有网关地址(值),不是键名,保留为 URL 默认

## 陷阱
- **pipeline.yaml 的 `llm_provider: glmcoding` 必须同步改名**,否则 match 落到 `_` 分支静默走 siliconflow(serde 默认忽略未知字段不报错,最隐蔽)
- 读含中文 rs 文件:read_file 误报 binary → 用 `scripts/dump_lines.py <path> <start> <end>`(或 python3 -c "print(open(p).read())")
- 检查 dt build 进程别用 `pgrep -af 'dt build'`(hermes 包装进程自带该字符串会自匹配)→ 用 `ps aux | grep -E '[d]t build'`
- cargo test --release --lib 一次只能一个 filter
- 验证顺序:pgrep 复查 → cargo fmt --check → cargo check --release → 逐个单测 → 同步 pipeline.yaml → dt health → `dt build --path <小目录>`（build --test 已随 2026-08-12 清理删除）；日志出现 "使用 OpenAI-Compatible LLM" 即通过
