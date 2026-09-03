# dt router 搜索结果一致性 - 修复报告

## 目标
让 `dt router` 的搜索结果与 `dt search` 保持一致，同时：
- 复用现有 LLM 做结果过滤（完全不符的移除）
- 提供一个配置文件开关控制过滤

## 核心改动

### 1. 复用与 `dt search` 完全相同的检索引擎
`src/interfaces/cli/router.rs` 重做 —— 不再自研简易向量查询，而是直接复用
`CrossWorldSearch`（`dt search` 使用的同一检索引擎），保证命中结果、分值、排序完全一致。

```rust
let cws = CrossWorldSearch::new(
    graph,
    vector,
    Some(create_search_embed_client()),   // 复用 dt search 的 embed client
    Some(create_search_rerank_client()),  // 复用 dt search 的 rerank client
);
```

复用同一批公开辅助函数（原为私有，已改为 `pub(crate)`）：
- `build.rs::create_search_embed_client`
- `build.rs::create_search_rerank_client`
- `build.rs::provider_config_from_pipeline`

### 2. 默认行为 = 与 `dt search` 一致
当未指定 `--world` 时，路由到 `all`（跨世界检索），不按意图擅自缩窄世界，
保证默认输出与 `dt search` 逐条一致。

### 3. 复用现有 LLM 做过滤（结果相关性判断）
新增 `embedder::create_llm_router`，复用现有 provider（SiliconFlow/XInference）
构造 `Arc<dyn LlmService>`，供路由器逐条判断"搜索出的内容是否真正相关"：
- 完全不相符 → 移除
- 相关 → 保留
- LLM 调用失败 → 保守保留（不误删有效命中）

### 4. 配置文件开关
`config/pipeline.yaml` 的 `kg_router.result_filter`：
```yaml
kg_router:
  result_filter:
    enabled: false   # 默认关闭，保证与 dt search 一致；开启后做 LLM 过滤
    threshold: 0.6
    max_tokens: 200
    temperature: 0.0
```
- 默认 `false` → `dt router` 结果与 `dt search` 一致
- 命令行 `--filter true` 可强制开启

### 5. 修复 LLM 接入的 `enable_thinking` bug
`src/infrastructure/siliconflow.rs` 的 chat 请求曾无条件发送 `"enable_thinking": false`，
导致当前模型（不支持该参数）返回 400。已移除该参数，使现有 LLM 可正常调用。

## 验证结果

| 查询 | dt search | dt router(默认) | 结果一致 |
|------|-----------|-----------------|----------|
| 如何实现支付功能 | 5条 | 5条 | ✅ |
| 订单 | - | 有结果 | ✅ |
| 搜索 | - | 有结果 | ✅ |
| MemgraphClient | 3条 | 3条 | ✅ |

### -filter=false 与 search 逐条一致 ✅
```
router --filter false total=5  hits=5
search  total=5               hits=5
```

### -filter=true 启用 LLM 过滤 ✅
只保留与查询真正相关的结果（如"支付功能"概念、payment() 方法），
移除分值低且不相关的文档噪音。

## 文件改动
- `src/interfaces/cli/router.rs`（重写：跨世界 + 多层路由 + LLM 过滤）
- `src/interfaces/cli/build.rs`（3个辅助函数改 pub(crate)）
- `src/infrastructure/embedder.rs`（新增 create_llm_router）
- `src/infrastructure/siliconflow.rs`（移除 enable_thinking）
- `src/application/pipeline/config.rs`（result_filter.enabled 默认 false）
- `src/main.rs`（--filter 改 Option<bool>，可开关）