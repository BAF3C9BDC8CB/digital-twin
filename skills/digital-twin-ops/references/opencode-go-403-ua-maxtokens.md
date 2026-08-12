# opencode.go 并发 403 根因 + max_tokens 配置化（2026-08-11 实测）

## 症状

warehouse-center 构建大量 `LLM 响应为空，视为失败（写 failed 状态位，不 mark）`（数百条），
daemon 日志另有 `OpenAI-Compatible 请求传输失败 ... error sending request`（30s 超时，157 条）。
单请求 curl 探测（无 UA）却正常返回 content —— 单发 OK、并发全挂的迷惑组合。

## 根因（两个独立问题叠加）

### 1. 无浏览器 UA → 并发 403 Forbidden（主因）

- dt 的 OpenAI 兼容客户端（`src/application/pipeline/infer_client.rs` `OpenAICompatibleChatClient::chat`）
  只设 `Authorization` 头，**没有设 User-Agent** → reqwest 默认 UA 是 `reqwest/x.y.z`
- opencode.go（Console Go 网关）对无浏览器 UA 的**并发**请求返回 `403 Forbidden`；单请求可能侥幸通过
- 实测（curl 并发 8）：无 UA → **8/8 全 403**；带浏览器 UA → **8/8 全 200**（仅 2 个 content 空，是 max_tokens 问题）
- 403 不在 daemon 日志出现：客户端重试逻辑只对 429/5xx 重试（`retryable = 429 || is_server_error()`），
  403 直接返回 Err → Phase 2 走失败分支（写 failed 状态位），所以日志里看不到 403 字样

**修复（已应用）**：infer_client.rs chat() 加浏览器 UA 头：
```rust
req = req.header(
    "User-Agent",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
);
```

### 2. max_tokens=100 对推理模型太紧 → 200 但 content 空

- deepseek-v4-flash 是**推理模型**：输出分 `reasoning_content`（思考）和 `content`（正文）
- dt 硬编码 `max_tokens=100`：单请求实测 completion=87 中 reasoning 占 31，content 只剩 ~56 token；
  并发/代码复杂时 reasoning 一涨，content 被吃光 → status 200 但 `content=""` → Phase 2 判空响应失败
- 日志特征：`OpenAI-Compatible 响应 status=200 OK elapsed_ms=31035`（200 但 31s 慢）+ 紧跟 `LLM 响应为空`

**修复（已应用，用户要求配置化）**：
- 三个 LLM provider 配置（SiliconFlow/XInference/OpenAICompatible）新增 `max_tokens: u32` 字段（serde default=512）
- `build_llm_client()` 返回三元组 `(client, model, max_tokens)`，贯穿 BuildDependencies → BuildServiceImpl::new
  （13 参）→ PipelineTemplate::new（7 参）→ Phase 2 chat / backfill chat
- ⚠️ **该签名变更波及全库调用点**：main.rs 3 处（--test 分支/普通/批量）、grpc build_service.rs、
  grpc wiring.rs、lib 测试 2 处（pipeline.rs test、service.rs test）——全部要补参，编译错误会精确指出
- pipeline.yaml 每个 provider 段加 `max_tokens: 512`（仓库 config/pipeline.yaml 与用户级
  ~/.config/digital-twin/pipeline.yaml **是同一 inode hardlink**，改一处两端生效，但要两端都确认）

## 排查方法论（可复用）

1. **单请求探测通过 ≠ 并发可靠**：先单发（curl 无并发）确认模型/端点本身 OK，再并发 16 复现
2. **区分两类空响应**：① chat Err（403/超时/5xx）→ daemon 日志有"请求传输失败/返回 HTTP"；
   ② 200 但 content 空 → 日志是"OpenAI-Compatible 响应 status=200 OK" + "LLM 响应为空"。
   两类都表现为 Phase 2 failed 状态位，但根因不同
3. **排查顺序**：先 curl 单发 → 再并发（带/不带 UA 对比）→ 看 usage 里 reasoning_tokens 占比
4. 403 排查陷阱：dt 重试只覆盖 429/5xx，403 静默走 Err 分支——daemon 日志 grep "403" 是 0，
   别据此排除 403 嫌疑

## 验证（修复后实测）

- 修复后 warehouse-center 构建：84 次 LLM 分析成功，空响应/403 计数 → 0（修复前 414 空响应 + 157 传输失败）
- 配置生效确认：日志 `使用 OpenAI-Compatible LLM: deepseek-v4-flash @ https://opencode.ai/zen/go (max_tokens=512)`
