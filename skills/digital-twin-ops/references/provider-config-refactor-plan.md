# Provider 配置模型重构方向 (2026-08-11 用户拍板, 团队分析中)

> 状态: 用户明确要求 + 3 角色团队(架构/风险/可行性)并行分析中, **方案未定稿前勿自行实施**。
> 本次"构建慢"排查的完整证据链见同目录 `engine-concurrency-gate-audit.md`(根因 =
> `inference_server.max_concurrent=1` 卡死 ProcessorEngine 引擎并发)。

## 用户拍板的需求(3 条, 持久决策)

1. **`providers.glmcoding` 改名通用化** — `GLMCodingChatClient` 是通用 OpenAI 兼容客户端
   (POST `{base_url}/v1/chat/completions` + Authorization Bearer, 任意 base_url),
   不应锁死 glmcoding.cn 品牌。拟名 `openai_compatible`(待团队定稿)。
2. **每个 provider 的 max_concurrent 各自独立** — siliconflow/xinference/glmcoding 各有各的并发。
   ⚠️ `XInferenceProviderConfig` 目前**缺 max_concurrent 字段**(config.rs:294-316 只有
   url/api_key/model_embed/model_reranker/model_llm), 需新增(默认值建议 2-4, 本地 GPU 推理慢)。
3. **删除顶层 `inference_server` 段** — 早期 dt-inference-server(Python)服务遗留,
   `url`/`grpc_url` 从未被代码读取, 仅 `max_concurrent` 被使用, 与 providers.xinference
   重复(localhost:9997)。用户质疑"为什么又出来一个 inference_server?和 xinference 不是一样的吗?"
   — 质疑成立。替代设计: 引擎并发改读当前 `llm_provider` 的 max_concurrent
   (新增 helper 如 `llm_provider_max_concurrent(&PipelineConfig) -> usize`,
   llm_provider 缺配置时按 provider 类型给默认值)。

## 临时缓解(不改代码, 已可用)

两端 pipeline.yaml(`~/.config/digital-twin/pipeline.yaml` + 仓库 `config/pipeline.yaml`)同步改:
`inference_server.max_concurrent: 1 → 32`(与 glmcoding.max_concurrent 对齐)。
改前 `pgrep -af 'dt build'` 清残留进程, 改后重跑验证吞吐(预期 20-30 倍提升)。

## 完整改动面(团队可行性角色已核实)

| 文件 | 改动点 |
|------|--------|
| `src/application/pipeline/config.rs` | GLMCodingProviderConfig 结构体(:318-351)+ 默认值函数 + Default + tests 断言(:400-457, 含 inference_server 默认 url/max_concurrent=16 断言); XInferenceProviderConfig 加 max_concurrent; 删 InferenceServerConfig(:57-95)与 PipelineConfig.inference_server 字段(:26) |
| `src/interfaces/cli/build.rs` | import(:13 GLMCodingChatClient); build_llm_client glmcoding 分支(:326-343)→ openai_compatible; 3 处 inference_server.max_concurrent 使用点(:100/:423/:489)改读 provider 配置; 注释(:308-314) |
| `src/application/pipeline/infer_client.rs` | 类名 GLMCodingChatClient(:420-588); 日志字符串 'GLM Coding'/provider="glmcoding" 硬编码(:457/:501/:506/:513/:519/:524/:530/:547/:554/:557); env 回退 GLMCODING_API_KEY(:429-433, 建议保留或换 OPENAI_API_KEY 行业名); health_check(:448-458) |
| `config/pipeline.yaml` + `~/.config/digital-twin/pipeline.yaml` | providers.glmcoding → providers.openai_compatible; llm_provider: glmcoding → openai_compatible; 删 inference_server 段 |
| 本技能 SKILL.md | 14 处 glmcoding/GLM Coding 引用需同步(改名前先记下引用位置) |

## 兼容策略要点

- serde 反序列化默认**忽略未知字段** → 旧 `glmcoding:` 段残留不报错, 但 llm_provider 字符串
  必须匹配新分支; 若保留向后兼容: build_llm_client 分支匹配 `"glmcoding" | "openai_compatible"`
  或给字段加 serde alias。
- `GLMCODING_API_KEY` env 回退名: 改名后旧 env 用户可能还在用, 建议保留为回退链。
- engine.rs CPU 阶段(run_cpu_stages)用 `available_parallelism`, 不受 max_concurrent 影响;
  max_concurrent 只作用于 GPU/LLM 阶段(引擎注释"同时运行的 GPU 密集型处理器调用数上限")。
- 工作区有 22 个未提交改动(分支 ahead 39), 改前先 `git status`; config/pipeline.yaml.bak.20260809184926 是旧备份(勿误删)。
- 验证: cargo fmt --check → cargo check --release → cargo test --release --lib(一次一个 filter)
  → dt health → 单项目小规模构建对比吞吐。
