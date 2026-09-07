//! SiliconFlow OpenAI 兼容 HTTP 客户端（多厂商 × 多模型端点池）。
//!
//! 2026-09-06 重构：原「单一 SiliconFlowClient 承载 embed/rerank/llm 三种能力、
//! 固定一份 url+key+三模型」的结构，改为通用的 OpenAI 兼容端点层：
//!
//! - [`OpenAiEndpoint`]：一条「url + api_key(+代理/并发/模型名)」连接，
//!   只做 HTTP，为三种能力提供类型化方法（chat/embed/rerank/health）。
//! - [`EndpointPool`]：一组同能力端点 + 选择策略。请求按策略命中一个端点，
//!   单端点内保留指数退避 + Retry-After；端点级失败（重试耗尽/连接失败/
//!   401 等硬错误）自动顺延到池内下一个端点重发，全部失败才报错——
//!   多厂商、多模型，一个失败换下一个。
//!
//! 端点协议假设：chat/embeddings 为 OpenAI 标准协议，rerank 为 SiliconFlow
//! 私有扩展端点（`/rerank`）——普通 OpenAI 兼容网关无此端点，只能进
//! llm/embed 池（配置注释已写明）。

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::application::pipeline::config::{EndpointStrategy, ProviderEndpoint, ProvidersConfig};
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// 受速率限制请求的最大重试次数（单端点内）。
const MAX_RETRIES: u32 = 3;
const REQUEST_DEADLINE_SECS: u64 = 180;

/// 指数退避的基础延迟（毫秒）（1s、2s、4s）。
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// 端点内瞬时错误状态码——安全重试，不会重放永久性客户端错误。
pub fn is_transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// 解析 Retry-After 为秒数（HTTP-date 留给调用方——供应商时钟不同步）。
pub fn retry_after_delay(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| {
        let exponential = RETRY_BASE_DELAY_MS.saturating_mul(1u64 << attempt.min(6));
        // 确定性小抖动避免并发 worker 同步重试，而不引入进程级限速 claim。
        Duration::from_millis(exponential + ((attempt as u64 * 37) % 101))
    })
}

/// 默认模型名（无端点/能力默认时的兜底）。
pub const DEFAULT_EMBED_MODEL: &str = "BAAI/bge-m3";
pub const DEFAULT_RERANKER_MODEL: &str = "BAAI/bge-reranker-v2-m3";
pub const DEFAULT_LLM_MODEL: &str = "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B";

/// 构建一个代理感知的 reqwest ClientBuilder。
///
/// 供项目内全部 OpenAI 兼容 HTTP 客户端共用，保证统一代理行为：
/// 传入的 `proxy` 为 `Some(且 enabled)` 且 `url` 非空时经该代理出网，
/// 并对 `NO_PROXY`/`no_proxy` 中的地址豁免直连；未传入或缺省关闭时
/// 一律直连——不读取任何标准代理环境变量。
///
/// 背景：pipeline LLM 客户端曾用裸 reqwest Client（未显式接代理），导致内网
/// 网关 124.221.200.116 的请求被自动代理改写而失败；embed/rerank 客户端早已
/// 按此逻辑处理。2026-09-05 统一后所有请求共享同一构造。
///
/// 实现注意（2026-09-06 修复）：reqwest 0.12 的 `ClientBuilder` 默认
/// `auto_sys_proxy = true`，即使不显式 `.proxy()` 也会在 build 时自动追加
/// "系统代理"匹配器，直接读取环境变量 `HTTP_PROXY`/`http_proxy` 等——
/// 会让"未配置代理"的 client 依然把请求改走 shell export 的本地代理。
/// 因此必须先显式 `.no_proxy()` 关闭 auto_sys_proxy，再按需叠加显式代理。
pub fn proxy_aware_client_builder(
    proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        // 关闭 auto_sys_proxy（见上）。
        .no_proxy();

    // 仅当配置显式启用（proxy.enabled == true）且给了 URL 时才接代理。
    if let Some(p) = proxy {
        if p.enabled && !p.url.trim().is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(p.url.trim()) {
                let proxy = apply_no_proxy(proxy);
                builder = builder.proxy(proxy);
            }
        }
    }

    builder
}

/// 将环境变量 `NO_PROXY`/`no_proxy`（逗号分隔）应用到 reqwest 代理。
fn apply_no_proxy(mut proxy: reqwest::Proxy) -> reqwest::Proxy {
    let no_proxy = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default();
    let hosts: Vec<&str> = no_proxy
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !hosts.is_empty() {
        proxy = proxy.no_proxy(reqwest::NoProxy::from_string(&no_proxy));
    }
    proxy
}

// ---------------------------------------------------------------------------
// OpenAiEndpoint —— 单条「url + key + 模型 + 代理 + 并发」连接
// ---------------------------------------------------------------------------

/// 一条 OpenAI 兼容连接，承载 chat / embed / rerank / health 类型化方法。
///
/// 从 [`ProviderEndpoint`] 构造（密钥经 `api_key_env` 展开）；`model` 传入
/// 该端点实际使用的模型名（端点级覆盖后）。所有请求带 `Authorization: Bearer`。
pub struct OpenAiEndpoint {
    http: reqwest::Client,
    /// 端点显示名（日志/健康报告）。
    pub label: String,
    base_url: String,
    api_key: String,
    /// 该端点使用的模型名（embed/rerank/llm 三能力可同端点异模型，各持一份）。
    pub model_embed: String,
    pub model_reranker: String,
    pub model_llm: String,
    semaphore: Arc<Semaphore>,
}

impl OpenAiEndpoint {
    /// 从配置端点构建（模型名按能力分别传入；代理取端点级>全局）。
    pub fn from_config(
        ep: &ProviderEndpoint,
        global_proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
        default_model: &str,
        model_embed: &str,
        model_reranker: &str,
        model_llm: &str,
    ) -> Self {
        let effective_proxy = ep.proxy.as_ref().or(global_proxy);
        let proxy = effective_proxy;
        let model = if ep.model.trim().is_empty() {
            default_model.to_string()
        } else {
            ep.model.trim().to_string()
        };
        let max_concurrent = ep.max_concurrent.unwrap_or(20).max(1);
        Self {
            http: proxy_aware_client_builder(proxy)
                .build()
                .unwrap_or_default(),
            label: ep.label(),
            base_url: ep.effective_url().trim_end_matches('/').to_string(),
            api_key: ep.resolved_api_key(),
            model_embed: if model_embed.is_empty() {
                DEFAULT_EMBED_MODEL.to_string()
            } else {
                model_embed.to_string()
            },
            model_reranker: if model_reranker.is_empty() {
                DEFAULT_RERANKER_MODEL.to_string()
            } else {
                model_reranker.to_string()
            },
            // chat 用端点级 model 覆盖（不同网关模型 ID 不同）> 顶层默认模型。
            model_llm: model,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// 保留旧构造签名（测试/外部调用兼容；仍以「三模型一体」构建）。
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_embed: impl Into<String>,
        model_reranker: impl Into<String>,
        model_llm: impl Into<String>,
        max_concurrent: usize,
        proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
    ) -> Self {
        let mut ep = ProviderEndpoint {
            name: String::new(),
            url: base_url.into(),
            url_env: String::new(),
            api_key: api_key.into(),
            api_key_env: String::new(),
            model: String::new(),
            models: Vec::new(),
            max_concurrent: Some(max_concurrent),
            weight: 1,
            proxy: None,
        };
        ep.url = base_url_placeholder(&ep.url);
        Self::from_config(
            &ep,
            proxy,
            "",
            &model_embed.into(),
            &model_reranker.into(),
            &model_llm.into(),
        )
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
    }

    /// 带速率限制重试逻辑地执行一个请求（单端点内 MAX_RETRIES 次）。
    async fn request_with_retry(
        &self,
        req: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<reqwest::Response, DtError> {
        let deadline = Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS);
        let mut last_error = String::new();
        let mut server_delay: Option<Duration> = None;

        for attempt in 0..=MAX_RETRIES {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if attempt > 0 {
                let delay = retry_delay(attempt - 1, server_delay.take());
                tracing::warn!(
                    "{} {} 第 {}/{} 次尝试失败: {}，{:?} 后重试",
                    self.label,
                    operation,
                    attempt,
                    MAX_RETRIES,
                    last_error,
                    delay
                );
                tokio::time::sleep(delay.min(remaining)).await;
            }

            // 仅在实际发送时占用 permit，退避等待期间释放并发槽位。
            let _permit = self.semaphore.acquire().await.map_err(|e| {
                DtError::Network(format!("{} 并发信号量获取失败: {e}", self.label))
            })?;

            // 每次构建一个全新的请求（reqwest::RequestBuilder 不可 Clone）
            let req_built = req
                .try_clone()
                .ok_or_else(|| DtError::Repository("请求克隆失败".into()))?;

            match tokio::time::timeout(remaining, req_built.send()).await {
                Ok(Ok(resp)) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }

                    server_delay = retry_after_delay(resp.headers());
                    let body = resp.text().await.unwrap_or_default();
                    let body_snippet = body.chars().take(200).collect::<String>();

                    // 400/401/403/404 = 确定性硬错误（参数不被模型支持 / key 失效 /
                    // 配额耗尽 / 模型不存在）：重试必然同样失败，立即返回让
                    // 池顺延下一端点——避免坏端点占用并发槽空转重试拖慢整池。
                    // （多 key 并行下此路径很常见：某个 key 额度耗尽即 403。）
                    if status == reqwest::StatusCode::BAD_REQUEST
                        || status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN
                        || status == reqwest::StatusCode::NOT_FOUND
                    {
                        return Err(DtError::Repository(format!(
                            "{} {} 错误 ({}): {}",
                            self.label, operation, status, body_snippet
                        )));
                    }

                    if is_transient_status(status) {
                        last_error = format!("HTTP {}: {}", status, body_snippet);
                        continue;
                    }

                    // 其他错误——不可重试（硬错误：调用方将顺延到下一端点）。
                    return Err(DtError::Repository(format!(
                        "{} {} 错误 ({}): {}",
                        self.label, operation, status, body_snippet
                    )));
                }
                Ok(Err(e)) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {e}");
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "{} {} 请求失败: {}",
                        self.label, operation, e
                    )));
                }
                Err(_) => {
                    last_error = format!("请求超过总 deadline {} 秒", REQUEST_DEADLINE_SECS);
                    break;
                }
            }
        }

        Err(DtError::Repository(format!(
            "{} {} 重试 {} 次后仍失败: {}",
            self.label, operation, MAX_RETRIES, last_error
        )))
    }

    /// 健康检查：`GET /models`。
    async fn health_check_http(&self) -> HealthStatus {
        let url = format!("{}/models", self.base_url);
        match self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => HealthStatus::Healthy,
            Ok(resp) => HealthStatus::Unhealthy(format!(
                "{} 健康检查: HTTP {}",
                self.label,
                resp.status()
            )),
            Err(e) => HealthStatus::Unhealthy(format!("{} 健康检查: {e}", self.label)),
        }
    }

    /// 通过 `POST /embeddings` 为一批文本生成 embedding。
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let started = Instant::now();
        tracing::debug!(
            endpoint = %self.label,
            model = %self.model_embed,
            batch_size = texts.len(),
            total_chars = texts.iter().map(|t| t.chars().count()).sum::<usize>(),
            stage = "embed_request_start",
            "embed 请求开始"
        );

        let body = serde_json::json!({
            "model": self.model_embed,
            "input": texts,
            "encoding_format": "float",
        });

        let resp = self
            .request_with_retry(self.post("/embeddings").json(&body), "embed")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("{} embed 解析: {e}", self.label)))?;

        let data = json["data"].as_array().ok_or_else(|| {
            DtError::Repository(format!(
                "{}: embed 响应中缺少 'data'",
                self.label
            ))
        })?;

        let mut embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let embedding: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| {
                    DtError::Repository(format!(
                        "{}: 响应中缺少 'embedding'",
                        self.label
                    ))
                })?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            embeddings.push((index, embedding));
        }

        embeddings.sort_by_key(|(idx, _)| *idx);
        tracing::debug!(
            endpoint = %self.label,
            model = %self.model_embed,
            batch_size = texts.len(),
            returned = embeddings.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            stage = "embed_request_done",
            "embed 请求完成"
        );
        Ok(embeddings.into_iter().map(|(_, v)| v).collect())
    }

    /// 使用配置的 reranker 模型对 `query` 重新排序 `documents`。
    ///
    /// 按原始输入顺序返回每个文档的相关性得分（`/rerank` 私有端点）。
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        let body = serde_json::json!({
            "model": self.model_reranker,
            "query": query,
            "documents": documents,
            "return_documents": false,
        });

        let resp = self
            .request_with_retry(self.post("/rerank").json(&body), "rerank")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("{} rerank parse: {e}", self.label)))?;

        let results = json["results"].as_array().ok_or_else(|| {
            DtError::Repository(format!(
                "{}: rerank 响应中缺少 'results'",
                self.label
            ))
        })?;

        let mut scores: Vec<(usize, f32)> = Vec::with_capacity(results.len());
        for item in results {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let score = item["relevance_score"].as_f64().unwrap_or(0.0) as f32;
            scores.push((index, score));
        }

        scores.sort_by_key(|(idx, _)| *idx);
        Ok(scores.into_iter().map(|(_, s)| s).collect())
    }

    /// 发送带标准 system/user 提示词的 chat completion 请求。
    ///
    /// 返回第一个 choice 的文本内容（兼容 content / reasoning_content）。
    pub async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        let body = serde_json::json!({
            "model": self.model_llm,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let resp = self
            .request_with_retry(self.post("/chat/completions").json(&body), "chat")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("{} chat 解析: {e}", self.label)))?;

        let msg = &json["choices"][0]["message"];

        // 某些推理模型把实际响应放在 `reasoning_content` 里，而 `content` 为空。
        let content = msg["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| msg["reasoning_content"].as_str().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                DtError::Repository(format!(
                    "{}: chat 响应中缺少 content/reasoning_content",
                    self.label
                ))
            })?;

        Ok(content.to_string())
    }
}

fn base_url_placeholder(url: &str) -> String {
    if url.is_empty() {
        "https://api.siliconflow.cn/v1".to_string()
    } else {
        url.to_string()
    }
}

// ---------------------------------------------------------------------------
// EndpointPool —— 一组同能力端点 + 失败自动顺延
// ---------------------------------------------------------------------------

/// 单条已解析的池内端点（供策略选择）。
struct PoolMember {
    endpoint: Arc<OpenAiEndpoint>,
    /// 端点级模型名覆盖（chat 用，若无则用顶层默认——由 OpenAiEndpoint 已解析）。
    weight: u32,
}

/// 同一能力的端点池：按策略选中一个端点执行；失败自动顺延下一个。
///
/// 三种选择语义：
/// - `Failover`：按配置顺序取第一个端点试；失败（重试耗尽/连接/硬错误）→ 下一个。
/// - `RoundRobin`：轮流选端点，失败的端点本轮跳过。
///
/// 全部端点失败才返回错误（携带最后错误）。
pub struct EndpointPool {
    members: Vec<PoolMember>,
    strategy: EndpointStrategy,
    /// round_robin 游标。
    cursor: std::sync::atomic::AtomicUsize,
}

/// 池内成员的调度器：round_robin 的加权轮询采用确定性"序号"分配——
/// 每个成员获得一段可用的序号区间（长度=weight），游标取模于
/// total_weight 后按区间定位成员。O(1) 摊还、无锁、无浮点，保证
/// 权重 3:1 的成员获得 3:1 的流量份额（无需随机/洗牌即可摊平并发波动）。
fn pick_member_weighted(members: &[PoolMember], cursor: &std::sync::atomic::AtomicUsize) -> usize {
    let total: u64 = members.iter().map(|m| m.weight as u64).sum();
    if total == 0 {
        return 0;
    }
    let pos = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u64 % total;
    let mut acc: u64 = 0;
    for (i, m) in members.iter().enumerate() {
        acc += m.weight as u64;
        if pos < acc {
            return i;
        }
    }
    members.len() - 1
}

impl EndpointPool {
    /// 从 `providers.<cap>` 配置构建池。
    ///
    /// `default_model`：该能力顶层默认模型；`model_embed/reranker/llm`：
    /// 顶层各能力模型名（embed 池只关心 model_embed，rerank 池只关心
    /// model_reranker；chat 池按端点级 model 覆盖优先，其次 llm.model）。
    pub fn from_config(
        eps: &[ProviderEndpoint],
        strategy: EndpointStrategy,
        global_proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
        default_model: &str,
        model_embed: &str,
        model_reranker: &str,
        model_llm: &str,
    ) -> Self {
        let members = eps
            .iter()
            .map(|ep| PoolMember {
                endpoint: Arc::new(OpenAiEndpoint::from_config(
                    ep,
                    global_proxy,
                    default_model,
                    model_embed,
                    model_reranker,
                    model_llm,
                )),
                weight: ep.weight.max(1),
            })
            .collect();
        Self {
            members,
            strategy,
            cursor: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 池是否为空（调用方 fallback / 报错）。
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// 按策略选一个起始端点索引（failover 固定 0；round_robin 加权轮询推进）。
    fn start_index(&self) -> usize {
        if self.members.is_empty() {
            return 0;
        }
        match self.strategy {
            EndpointStrategy::Failover => 0,
            EndpointStrategy::RoundRobin => pick_member_weighted(&self.members, &self.cursor),
        }
    }

    /// 对池内端点执行 `op`：失败自动顺延到下一个，全部失败返回错误。
    ///
    /// `op` 收到端点与端点序号（用于日志）。同一请求会按序重发到多个端点，
    /// 因此 op 应保持幂等语义（chat/embed 类均幂等）。op 返回
    /// `Pin<Box<dyn Future + Send>>`——为规避借用冲突，调用方需把入参
    /// clone/owned 进闭包（见各 Pooled* 实现）。
    pub async fn run<T, F, E>(&self, op: F) -> Result<T, DtError>
    where
        F: Fn(
                Arc<OpenAiEndpoint>,
                usize,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'static>,
            > + Sync,
        E: Into<DtError> + std::fmt::Display,
    {
        if self.members.is_empty() {
            return Err(DtError::Repository("端点池为空：未配置任何端点".into()));
        }
        let mut last_err: Option<String> = None;
        let n = self.members.len();
        let start = self.start_index();

        for offset in 0..n {
            let idx = (start + offset) % n;
            let member = &self.members[idx];
            tracing::debug!(
                endpoint = %member.endpoint.label,
                strategy = ?self.strategy,
                pool_len = n,
                "尝试池内端点"
            );
            match op(member.endpoint.clone(), idx).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let msg = format!("{}", e);
                    tracing::warn!(
                        endpoint = %member.endpoint.label,
                        "端点调用失败，{}顺延下一端点: {}",
                        if offset + 1 < n { "自动" } else { "无可用端点，" },
                        msg
                    );
                    last_err = Some(format!("{}: {}", member.endpoint.label, msg));
                }
            }
        }
        Err(DtError::Repository(format!(
            "端点池全部失败（{} 个端点）: {}",
            n,
            last_err.unwrap_or_default()
        )))
    }

    /// 健康状态：Healthy = 至少一个端点可达；否则报告不可用端点明细。
    pub async fn health_check(&self) -> HealthStatus {
        if self.members.is_empty() {
            return HealthStatus::Unhealthy("未配置任何端点".into());
        }
        let mut failures: Vec<String> = Vec::new();
        for m in &self.members {
            match m.endpoint.health_check_http().await {
                HealthStatus::Healthy => return HealthStatus::Healthy,
                HealthStatus::Degraded(_) => return HealthStatus::Healthy,
                HealthStatus::Unhealthy(reason) => failures.push(reason),
            }
        }
        HealthStatus::Unhealthy(failures.join("; "))
    }

    /// 能力声明：只要池非空即声明具备该能力；chat 能力看是否配置了 llm 模型。
    pub fn capabilities(&self) -> LlmCapabilities {
        let chat = self
            .members
            .iter()
            .any(|m| !m.endpoint.model_llm.is_empty());
        LlmCapabilities {
            embed: !self.members.is_empty(),
            rerank: !self.members.is_empty(),
            chat,
            max_tokens: 4096,
        }
    }
}

// ---------------------------------------------------------------------------
// trait 实现：EmbedService / RerankService / LlmService（对池路由）
// ---------------------------------------------------------------------------

/// embed 池 trait 实现（路由到池内端点，失败自动顺延）。
pub struct PooledEmbedService {
    pool: EndpointPool,
}

impl PooledEmbedService {
    pub fn new(pool: EndpointPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EmbedService for PooledEmbedService {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        let texts = texts.to_vec();
        self.pool
            .run(move |endpoint, _| {
                let texts = texts.clone();
                Box::pin(async move { endpoint.embed_batch(&texts).await })
            })
            .await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(self.pool.health_check().await)
    }
}

/// rerank 池 trait 实现。
pub struct PooledRerankService {
    pool: EndpointPool,
}

impl PooledRerankService {
    pub fn new(pool: EndpointPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RerankService for PooledRerankService {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        let query = query.to_string();
        let documents = documents.to_vec();
        self.pool
            .run(move |endpoint, _| {
                let query = query.clone();
                let documents = documents.clone();
                Box::pin(async move { endpoint.rerank(&query, &documents).await })
            })
            .await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(self.pool.health_check().await)
    }
}

/// llm 池 trait 实现（chat 固定用端点自身解析后的模型）。
pub struct PooledLlmService {
    pool: EndpointPool,
}

impl PooledLlmService {
    pub fn new(pool: EndpointPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LlmService for PooledLlmService {
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        let system_prompt = system_prompt.to_string();
        let user_prompt = user_prompt.to_string();
        self.pool
            .run(move |endpoint, _| {
                let system_prompt = system_prompt.clone();
                let user_prompt = user_prompt.clone();
                Box::pin(async move {
                    endpoint
                        .chat(&system_prompt, &user_prompt, temperature, max_tokens)
                        .await
                })
            })
            .await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(self.pool.health_check().await)
    }

    fn capabilities(&self) -> LlmCapabilities {
        self.pool.capabilities()
    }
}

// ---------------------------------------------------------------------------
// SiliconFlowClient（兼容别名——旧构造/测试沿用；内部即单端点池）
// ---------------------------------------------------------------------------

/// 旧结构兼容别名：单端点即单成员池。
///
/// 内部不直接持有三种能力分池；由 embedder/router 工厂统一走 [`EndpointPool`]。
/// 保留此类型仅为不破坏既有测试引用面，新代码请直接用池。
#[deprecated(note = "多端点池化后请使用 EndpointPool / Pooled*Service")]
pub struct SiliconFlowClient {
    endpoint: Arc<OpenAiEndpoint>,
}

#[allow(deprecated)]
impl SiliconFlowClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_embed: impl Into<String>,
        model_reranker: impl Into<String>,
        model_llm: impl Into<String>,
        max_concurrent: usize,
        proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
    ) -> Self {
        Self {
            endpoint: Arc::new(OpenAiEndpoint::new(
                base_url,
                api_key,
                model_embed,
                model_reranker,
                model_llm,
                max_concurrent,
                proxy,
            )),
        }
    }

    /// 健康检查（单端点）。
    pub async fn health_check(&self) -> HealthStatus {
        self.endpoint.health_check_http().await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_classifies_status_and_retry_after() {
        assert!(is_transient_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_transient_status(reqwest::StatusCode::BAD_REQUEST));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after_delay(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "not-a-duration".parse().unwrap(),
        );
        assert_eq!(retry_after_delay(&headers), None);
    }

    #[test]
    fn permanent_client_errors_are_not_transient() {
        // 4xx 硬错误（400/401/403/404）走快速失败路径，绝不能进入重试循环。
        for code in [400u16, 401, 403, 404] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert!(!is_transient_status(status), "HTTP {code} 不应重试");
        }
        // 瞬时错误才重试：429 + 502/503/504（500 视为硬错误不重试）。
        assert!(is_transient_status(reqwest::StatusCode::from_u16(429).unwrap()));
        assert!(is_transient_status(reqwest::StatusCode::from_u16(502).unwrap()));
        assert!(is_transient_status(reqwest::StatusCode::from_u16(503).unwrap()));
        assert!(is_transient_status(reqwest::StatusCode::from_u16(504).unwrap()));
        assert!(!is_transient_status(reqwest::StatusCode::from_u16(500).unwrap()));
    }

    #[test]
    fn endpoint_from_config_resolves_key_env_and_model() {
        std::env::set_var("DT_TEST_SF_KEY", "sk-env-resolved");
        let ep = ProviderEndpoint {
            name: "gw".into(),
            url: "https://api.siliconflow.cn/v1".into(),
            url_env: String::new(),
            api_key: String::new(),
            api_key_env: "DT_TEST_SF_KEY".into(),
            model: "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B".into(),
            models: vec![],
            max_concurrent: Some(7),
            weight: 3,
            proxy: None,
        };
        let ep = OpenAiEndpoint::from_config(&ep, None, "", "", "", "");
        assert_eq!(ep.label, "gw");
        assert_eq!(ep.api_key, "sk-env-resolved");
        assert_eq!(ep.model_llm, "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B");
        assert_eq!(ep.model_embed, DEFAULT_EMBED_MODEL);
        assert_eq!(ep.model_reranker, DEFAULT_RERANKER_MODEL);
        assert_eq!(ep.semaphore.available_permits(), 7);
        std::env::remove_var("DT_TEST_SF_KEY");
    }

    #[test]
    fn endpoint_label_falls_back_to_host() {
        let ep = ProviderEndpoint {
            name: String::new(),
            url: "http://124.221.200.116:3000/v1".into(),
            url_env: String::new(),
            api_key: "k".into(),
            api_key_env: String::new(),
            model: String::new(),
            models: vec![],
            max_concurrent: None,
            weight: 1,
            proxy: None,
        };
        assert_eq!(ep.label(), "124.221.200.116:3000");
    }

    #[test]
    fn pooled_services_construct_empty_pool() {
        let pool = EndpointPool::from_config(&[], EndpointStrategy::Failover, None, "", "", "", "");
        assert!(pool.is_empty());
        let llm = PooledLlmService::new(pool);
        assert!(!llm.capabilities().chat);
    }

    #[test]
    fn round_robin_weighted_selection_respects_weights() {
        // 构造 2 个成员：weight 3:1 —— 每 4 次选择应命中 3 次成员 0、1 次成员 1
        let mk = |name: &str, w: u32| PoolMember {
            endpoint: Arc::new(OpenAiEndpoint::new(
                format!("http://{name}.test/v1"),
                "k",
                "e",
                "r",
                "l",
                4,
                None,
            )),
            weight: w,
        };
        let members = vec![mk("a", 3), mk("b", 1)];
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut hits = [0usize; 2];
        for _ in 0..4000 {
            hits[pick_member_weighted(&members, &cursor)] += 1;
        }
        assert_eq!(hits[0], 3000);
        assert_eq!(hits[1], 1000);
        // 单成员退化为恒选中它
        let one = vec![mk("solo", 5)];
        let cursor2 = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..10 {
            assert_eq!(pick_member_weighted(&one, &cursor2), 0);
        }
    }
}
