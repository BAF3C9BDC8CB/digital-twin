# Pipeline LLM 分析路径与并发模型

**用途**: 排查"某个文件为什么这样被分析 / 并发为什么不生效"时先读本文, 再进代码。

## 1. 三通道分流(select_prompt 优先级, llm_client.rs:300-316)

| 条件 | prompt | 处理方式 |
|------|--------|---------|
| Nacos 来源(source_kind==Nacos 或 dt://nacos/ 前缀) | nacos_config | 块级提取 |
| 上下文有 `tree_sitter` 输出(Java/Py/Rs/Go/TS/JS 等代码文件) | code_with_ast | **整文件单次调用**(execute_single_call, 不切块, 输出不解析) |
| 上下文有 `chunk` 输出(md/txt/yaml/properties 等无 AST 文件) | document_with_nlp | **块级提取**(每 chunk 一次 LLM) |
| 其他 | raw_text | 单次调用 |

LlmClientProcessor.matches 覆盖扩展名(llm_client.rs:88-108): java/py/rs/go/ts/tsx/js/jsx/php/md/txt/yaml/yml/properties。

## 2. Java 文件 = 方法级分析(builder Phase 2, build/pipeline.rs:365-434)

- tree-sitter 提取方法列表(MethodBlock, source_text = **方法源码片段**, java.rs:242)
- **每个方法一次 LLM 调用**(code_analysis.yaml prompt → 输出「用途:/逻辑:」两行)
- 接口/抽象方法(source_text <10 字符)回退读整文件(:379-386, join 项目根)
- 并发: tokio::spawn 任务池 + 客户端 semaphore(max_concurrent)
- 日志特征: `LLM 方法分析开始 <method>` / `LLM 方法分析响应成功`

## 3. 文档/配置文件 = 块级提取(execute_block_extraction, llm_client.rs:158-204)

- 切块规则(chunker.rs): yaml 按顶层 key、properties 按前缀、文本按 段落→句子→固定长度 逐级回退; chunk_size 默认 256 / min 128
- **每 chunk 一次 LLM 调用**, 单文件内并发 = `llm.chunk_concurrency`(buffer_unordered(limit))
- 日志特征: `LLM file_start ... chunks=N`(N>1 即多 chunk 文件)
- 性能: `.gitlab-ci.yml` 12 chunks × chunk_concurrency=1 → **串行 4m22s**, 独占 1 个引擎 slot, 成构建尾部瓶颈

## 4. 并发模型

### 4.1 两个并发维度

| 参数 | 粒度 | 说明 |
|------|------|------|
| `providers.strategy` | 端点选择 | `round_robin`（多端点多 key 并行，默认）= 并发请求轮流分发到不同端点，**同时用满全部 key**；`failover` = 固定顺序主备，同一时刻只用一个 key |
| `max_concurrent`(`llm_provider_max_concurrent()` = 池内各端点之和) | **文件间**并发 | 引擎 GPU 阶段 buffer_unordered(N) + 客户端 semaphore 全局在飞上限 |
| `chunk_concurrency` | **单文件内** chunk 并发 | 只影响多 chunk 文档/配置文件 |

### 4.2 全局在飞请求

`min(文件并发 × 每文件 chunk 并发, 客户端 semaphore)`。例: C=1 → 32; C=4 → min(128,32) = **32(semaphore 兜底, 不压垮上游)**。

多 key 并行后（2026-09-06）：semaphore 上限 = **各端点 max_concurrent 之和**（例如 4 个端点 × 48 = 192），
round_robin 把请求轮流发到各端点 → 每端点独立 48 在飞，整池最高 192，跨 key 横向扩展。
（旧版 semaphore 只取池内首端点并发，多 key 并未并行。）

- `chunk_concurrency=1` **不影响 Java 方法级并发**(方法并发吃 max_concurrent), 只拖慢多 chunk 文档/配置文件
- 并发生效验证(日志): `LLM file_start` 时间戳毫秒级间隔 = 文件间并行; 旧串行配置下完成间隔 ≈ 单请求耗时(11.4s)

## 5. 配置同步

`~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml` **同 inode(硬链接)**——改一处自动同步另一处, 无需手工双端。验证: `stat -c '%i'` 两个路径。

## 6. 操作安全: patch 配置文件含 api_key 的行

工具输出会对 key 打码(如 `sk-NUb...vuGg`)。若用「手动看到的打码值」写 patch 的 new_string, 会**把真实 key 覆盖成打码串**。规则:

1. 替换含 key 的行时, old_string 尽量不含 key 值(用相邻行锚定), 或整行不动只改键名
2. 改完用 python yaml 读回验证 key 长度/前后缀与改前一致
3. 事故恢复: 从 patch 输出的 diff 里取原始完整 key 再 patch 回去

## 7. 验证方法(provider/并发改动后)

1. `cargo check --release` → 关键单测逐个 → python yaml 真实配置断言 → 二进制 mtime vs config.rs → `dt health`
2. 并发生效验证: grep daemon 日志 `LLM file_start` 看时间戳毫秒级间隔; 对比 `OpenAI-Compatible 响应` 的 elapsed_ms p50 与完成间隔
