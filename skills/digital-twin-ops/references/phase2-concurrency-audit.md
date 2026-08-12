# Phase 2 方法级并发审计 (2026-08-11) — PHASE2_CONCURRENCY=4 硬编码

## 核心发现

`src/application/build/pipeline.rs:32` — `const PHASE2_CONCURRENCY: usize = 4`（注释"Provider 请求并发由配置降至 4；文档 chunk 在 pipeline.yaml 中为 1"）。
**Java/代码文件的"方法级 LLM 分析"（Phase 2）并发被硬编码为 4**，2026-07-26「LLM 分析后台异步化」commit 6aea1fe 引入（SiliconFlow 429 时代产物）。
用户改 `providers.<llm_provider>.max_concurrent=32` 对 Phase 2 **无效**：Phase 2 用 `buffer_unordered(PHASE2_CONCURRENCY)`（pipeline.rs:652），与 max_concurrent 完全无关。

症状：方法分析有并发但吞吐上限=4；"一个文件 32 个方法应 32 请求并行"实际被卡 4。排查"方法分析慢"先查这个常量，别只看 max_concurrent。

## 并发四维度全景（dt build 的 LLM 并发模型）

| 维度 | 控制点 | 值来源 |
|---|---|---|
| 文件间（pipeline 引擎 GPU 阶段） | build.rs:489 `ProcessorEngine::new(registry, llm_provider_max_concurrent())` | providers.<llm_provider>.max_concurrent（2026-08-11 重构后） |
| 文件内 chunk（文档/配置块级提取） | llm_client.rs:188 `buffer_unordered(chunk_concurrency)` | pipeline.yaml `llm.chunk_concurrency` |
| **方法级（Phase 2，代码文件）** | pipeline.rs:652 `buffer_unordered(PHASE2_CONCURRENCY)` | **硬编码 4（待配置化）** |
| 全局在飞上限 | 客户端 semaphore = max_concurrent | providers.<llm_provider>.max_concurrent |

全局在飞 = min(文件并发 × chunk_concurrency, semaphore)。`chunk_concurrency>1` 不增加全局并发（semaphore 兜底），只减少单文件内多 chunk 的尾部延迟（.gitlab-ci.yml 12 chunks × chunk_concurrency=1 → 串行 4m22s，实测拖垮整个 doctor-center 构建）。

## Java 文件的三条 LLM 路径（诊断"哪个并发参数影响什么"必读）

1. **Phase 2 方法级**（build/pipeline.rs:365-434，主要）：tree-sitter 提取 MethodBlock（`source_text`=方法源码片段，java.rs:242），每方法一次 `code_analysis` 调用（输出"用途：/逻辑："，json_mode=false，max_tokens=100），接口/抽象方法 source_text<10 字符回退读整文件（:379-386）。日志：`LLM 方法分析开始/完成`。
2. **pipeline code_with_ast 整文件单次**（llm_client.rs:300-316 select_prompt：有 tree_sitter 输出→code_with_ast→execute_single_call）：整文件 file_text 一次给 AI，旧路径输出不解析，**无 file_start 日志**。不切块、不吃 chunk_concurrency。
3. **文档/配置块级提取**（document_with_nlp/nacos_config → execute_block_extraction，llm_client.rs:158-204）：ChunkProcessor 切块（yaml 按顶层 key / properties 按前缀 / 文本段落→句子→固定长度回退；chunk_size=256 token 近似、min 128），每 chunk 一次调用。日志：`LLM file_start ... chunks=N`。**chunk_concurrency 只影响这里**。

select_prompt 优先级：Nacos > tree_sitter(code_with_ast) > chunk(document_with_nlp) > raw_text。

## 待实施方案（用户 2026-08-11 认可方向，未实施）

1. **PHASE2_CONCURRENCY 配置化**：新增 `llm.phase2_concurrency`（或直接读 llm_provider_max_concurrent）。传递链：BuildDependencies 加字段（builder.rs:57）→ BuildServiceImpl::new（builder.rs:86，参数已多，注意 too_many_arguments）→ BuildPipeline::new（pipeline.rs:85）→ execute → buffer_unordered(值)。build.rs CLI 构造点填值。
   ⚠️ **默认 16 起步，勿直接 32**：历史 Phase 2 在 32 并发下触发过上游 429（2026-08-11 上午 1557 次/天，Phase 2 是 429 风暴源头——它吃满 semaphore 而上游限流）。
2. **chunk_concurrency 1→4**：文档文件提速，全局 semaphore 32 兜底安全。

## 硬链接事实（修正 SKILL.md 旧记载）

`~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` 是**硬链接**（同一 inode，实测 1482722）——**改一处两处同时变，无需手工同步**（技能旧文"keep it in sync manually"过时）。验证：`stat -c '%i' <两路径>`。`ln -sf` 拒绝 "same file" 正是硬链接的证据。

## api_key 打码覆盖事故（2026-08-11 实战）

patch 配置文件时若从 Hermes 工具输出复制 api_key 行（输出显示打码 `sk-NUb...vuGg`），**会把文件里真实 key 覆盖成打码串**（67 字符 key 被替换成 13 字符占位）。教训：
- key 行不手动写进 patch 的 new_string；old_string/new_string 都避开 key 行，或从文件读原文
- 改完立即验证 key 完整性：python 读文件对比前缀+长度（不打印全文）
- 事故已发生一次并恢复（diff 中可见完整 key 用于恢复）

## 验证命令

```bash
# Phase 2 并发实测：数方法分析完成节奏
sudo grep 'LLM 方法分析开始' /var/log/digital-twin/dt.log | grep '2026-08-11T14:3' | head
# 文件级并发（file_start 时间戳差 2-7ms = 并发生效）
sudo grep -E 'LLM file_start' /var/log/digital-twin/dt.log | grep '2026-08-11T14:3' | head -5
# 判断"并发 N 生效"：完成间隔 ≈ 耗时/N；间隔 ≈ 耗时 = 串行（并发 1）
```
