# Provider 配置重构实施后记（2026-08-11，与 risk-map 配套）

> 本文件记录 `references/provider-config-refactor-risk-map.md` 所述重构**实施完成**后的
> 新事实、操作事故与后续注意点。重构本身（glmcoding→openai_compatible / 删 inference_server /
> xinference 补 max_concurrent）已实施并验证：668 lib 测试全绿 + doctor-center 全量构建 25/25 成功。

## 1. ⚠️ api_key 打码陷阱 — patch 配置时真实 key 被覆盖成打码串（真实事故）

**事故经过**：用 `patch` 改 `config/pipeline.yaml` 的 provider 段时，把 `glmcoding:` 键改名
`openai_compatible:`。替换文本里包含了 api_key 行，而我看到的值是 Hermes 工具输出的**打码串**
（`sk-NUb...vuGg`，真实值 67 字符），结果把文件里的真实 key 覆盖成了打码串。

**恢复**：patch 工具的 diff 输出显示了 old_string 的完整原始值（含完整 key），照抄恢复。
验证：`yaml.safe_load` 读回 key 长度 67、前缀/后缀与用户版一致。

**规则（避免重犯）**：
- **patch 含密钥的 YAML 时，old_string/new_string 绝不包含 key 行**——只改键名/注释/结构行，
  让 key 行原样保留在文件中。
- 若必须动 key 行，用 Python 读-改-写（读原文 → 只替换目标键 → 写回），key 值从内存变量走，
  不经过手写。
- 改完立即用 `yaml.safe_load` 验证 key 长度/前后缀（不打印全文）。

## 2. ✅ 事实修正 — pipeline.yaml 双端是硬链接，不是"手工同步"

`~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` **同一 inode（硬链接）**，
改一端自动同步另一端（`stat -c '%i'` 两者相同；这也解释了历史上 `ln -sf` 报 "same file"）。
risk-map 里"普通文件非 symlink，手工同步"的表述**不准确**。

**操作含义**：改 pipeline.yaml 只需改一端；验证双端一致用 `diff`（应恒空）或 `stat` inode 对比。
⚠️ 但注意：硬链接关系可能被用户/脚本打破（如某次 `cp` 覆盖），操作前仍建议 `stat` 确认。

## 3. SKILL.md 接近 100K 字符上限（Hermes 技能管理限制）

skill_manage patch 在 SKILL.md 超过 **100,000 字符**时直接拒绝（报
"SKILL.md content is 100,013 characters (limit: 100,000)"）。digital-twin-ops 的 SKILL.md
已逼近该上限（中文 UTF-8 3 字节/字符，wc -c 字节数≠字符数）。

**操作规则**：
- 给 digital-twin-ops 加内容前先估字符预算；超限时**压缩旧段落**（历史教训细节可移入
  references/，SKILL.md 留指针）再 patch。
- 长段落优先写 references/ 文件（references 不受 100K 限制），SKILL.md 只放一行指针。

## 4. 重构后配置模型速览（新常态）

```yaml
providers:
  llm_provider: openai_compatible   # 旧值 glmcoding 仍兼容（分支双匹配）
  openai_compatible:                 # 通用 OpenAI 兼容网关（glmcoding/opencode-go/任意厂商）
    url: https://opencode.ai/zen/go  # 不带 /v1（客户端自拼）
    api_key: "..."                   # 空则回退 env OPENAI_COMPATIBLE_API_KEY → GLMCODING_API_KEY
    protocol: openai
    model_llm: deepseek-v4-flash
    max_concurrent: 32               # 唯一并发旋钮：引擎(ProcessorEngine)+客户端 semaphore 都读它
  xinference:
    max_concurrent: 16               # 新增字段，默认 16（与历史 inference_server 默认一致）
# inference_server 段已删除；engine 并发 = PipelineConfig::llm_provider_max_concurrent()
```

- 引擎并发与客户端并发统一读 `llm_provider_max_concurrent()`：openai_compatible=32 /
  xinference=16 / siliconflow=20（缺失回退）。
- 日志 provider 标签从 `"glmcoding"` 变为 `"openai_compatible"`；日志消息前缀
  `GLM Coding` → `OpenAI-Compatible`（grep 旧名会失效）。

## 5. 验证结果（实施闭环）

- `cargo fmt` + `cargo check --release`：0 error
- `cargo test --release --lib`：668 passed（含新增 `llm_provider_max_concurrent_reads_current_provider`，
  覆盖 openai_compatible/xinference/siliconflow/旧 glmcoding alias 四路由）
- 真实构建：`dt build --path doctor-center --full` 25/25 文件成功，日志确认
  `provider="openai_compatible"` + "使用 OpenAI-Compatible LLM: deepseek-v4-flash @ https://opencode.ai/zen/go"
- 耗时对比：旧（并发 1 串行）41 分钟 215 请求；新（并发 32）25 文件全流程 6 分钟
