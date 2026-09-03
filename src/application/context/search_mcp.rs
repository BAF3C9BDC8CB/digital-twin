//! **CrossWorldSearch** — 跨世界语义搜索（4.8）。
//!
//! 在单次查询中跨所有可用的知识世界（code、knowledge graph、
//! documents、vector store）检索，返回混合结果。
//!
//! # MCP 工具：`dt_search`
//!
//! ```text
//! dt_search(query: str, world?: str, limit?: int)
//!   → CrossWorldResult JSON
//! ```

use std::sync::Arc;

use crate::application::knowledge::extract::retrieve::{
    RelationSnippet, RetrieveRequest, Retriever, ScoreBreakdown,
};
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};

// ---------------------------------------------------------------------------
// 查询增强与多路召回(搜索准确率升级:1A 标识符精确通道 / 1B 拆词扩写 /
// 2B 关键词兜底 / 4 长查询关键词)
// ---------------------------------------------------------------------------

/// 判定查询是否为"标识符型"(短 ASCII 代码标识符,如 saveToDb、_accept_embed)。
/// 要求含驼峰大写或蛇形下划线——纯小写长串(乱码)不算。
/// 中文查询与长句子天然不匹配,走语义通道。
fn is_identifier_query(query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() || q.len() > 50 {
        return false;
    }
    if q.chars().any(char::is_whitespace) {
        return false;
    }
    if !q.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    q.chars().any(|c| c.is_ascii_uppercase()) || q.contains('_')
}

/// 疑似乱码/无意义查询检测:全 ASCII 字母数字、无空格、无大写、无下划线、
/// 长度 >= 12(如 asdfghjklqwertyuiop1234567890)。此类查询向量检索会产生
/// 幻觉命中,直接短路返回空。
fn is_gibberish(query: &str) -> bool {
    let q = query.trim();
    if q.len() < 12 {
        return false;
    }
    if q.chars().any(char::is_whitespace) {
        return false;
    }
    if !q.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    !q.chars().any(|c| c.is_ascii_uppercase()) && !q.contains('_')
}

/// 驼峰/蛇形标识符拆词:saveToDb → [save,to,db];_accept_embed → [accept,embed]。
fn split_identifier(ident: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in ident.trim_start_matches('_').chars() {
        if ch.is_ascii_uppercase() {
            if !cur.is_empty() {
                words.push(cur.clone());
                cur.clear();
            }
            cur.push(ch.to_ascii_lowercase());
        } else if ch == '_' || ch.is_ascii_digit() {
            if !cur.is_empty() {
                words.push(cur.clone());
                cur.clear();
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// 从查询提取关键词——薄包装，直接委托统一实现
/// `crate::application::knowledge::extract::retrieve::keywords_of`，
/// 避免与 retrieve.rs 各持一份实现导致行为漂移。
///
/// 统一实现的行为：ASCII 整词保留 + CJK 虚词切分 + n-gram 扩展，
/// 全角标点作为分隔符处理（不再累积进关键词）。
fn extract_keywords(query: &str, max: usize) -> Vec<String> {
    crate::application::knowledge::extract::retrieve::keywords_of(query, max)
}

/// 由 Qdrant 命中/滚动 payload 构造 code 世界 SearchHit。
fn hit_from_payload(
    payload: &serde_json::Value,
    name: String,
    id: String,
    score: f64,
) -> SearchHit {
    let calls: Vec<String> = payload
        .get("calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    SearchHit {
        id,
        title: name.clone(),
        snippet: {
            let fp = payload
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let sl = payload
                .get("start_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let el = payload
                .get("end_line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if fp.is_empty() {
                String::new()
            } else {
                format!("{}: L{}-{}", fp, sl, el)
            }
        },
        content: None,
        source_world: "code".into(),
        // 从 payload 读取实体类型：方法点无 entity_type 字段→默认 Method；
        // 类点（code_classes）有 entity_type=Class（GAP-A）。
        entity_type: payload
            .get("entity_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Method")
            .into(),
        file_type: None,
        file_type_label: None,
        score,
        source_ref: None,
        metadata: None,
        file_path: payload
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        project: payload
            .get("project")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        start_line: payload
            .get("start_line")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        end_line: payload
            .get("end_line")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        signature: payload
            .get("signature")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        llm_analysis: payload
            .get("llm_analysis")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        calls,
        // 代码实体的图键:Qdrant payload 存 entity_id(dt://entity/...),与
        // Memgraph 节点 method_id 完全一致——用于 code 世界图增强匹配。
        element_id: payload
            .get("entity_id")
            .or_else(|| payload.get("method_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        score_breakdown: None,
        hop: None,
        via_same_as: None,
        relations: None,
        evidence: None,
        rerank_degraded: None,
    }
}

/// 由 Qdrant 命中解析稳定 id(优先 point id,回退 payload 身份)。
fn hit_id(hit: &serde_json::Value, payload: &serde_json::Value) -> String {
    match hit.get("id") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => payload
            .get("entity_id")
            .or(payload.get("method_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    payload
                        .get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?"),
                    payload.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                )
            }),
    }
}

/// 计算两个稠密向量的余弦相似度（rerank 用）。
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (xf, yf) = (*x as f64, *y as f64);
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// base 向量召回后，用 llm 向量做 rerank：拉取候选点的 llm 向量（限批 top-50），
/// 与 query 向量算 cosine 相似度，与 base 分数加权融合（0.5/0.5）后重排序。
///
/// 无 llm 向量的点保持 base 分数（不因增强层缺失而掉出候选）。返回重排后的
/// hit 列表（保留 `{id, score, payload}` 结构），并已过滤 project/exact_ids。
async fn rerank_with_llm_vectors(
    vector: &Arc<dyn crate::domain::traits::VectorRepository>,
    collection: &str,
    query_vec: &[f32],
    results: Vec<serde_json::Value>,
    min_score: f64,
    project: Option<&str>,
    exact_ids: &[String],
) -> Vec<serde_json::Value> {
    // 先做基础过滤（分数阈值 / 合法 name / project / 与精确通道去重）。
    let mut kept: Vec<serde_json::Value> = Vec::new();
    for hit in results {
        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if score < min_score {
            continue;
        }
        let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
        let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() || name == "?" {
            continue;
        }
        if let Some(p) = project {
            let pp = payload
                .get("project")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if pp != p {
                continue;
            }
        }
        let id = hit_id(&hit, payload);
        if exact_ids.contains(&id) {
            continue;
        }
        kept.push(hit);
    }
    if kept.is_empty() {
        return kept;
    }

    // 拉取 top-50 候选的 llm 向量（限批：rerank 只对头部候选有意义）。
    let ids: Vec<u64> = kept
        .iter()
        .take(50)
        .filter_map(|h| {
            h.get("id").and_then(|v| v.as_u64()).or_else(|| {
                h.get("payload")
                    .and_then(|p| p.get("entity_id"))
                    .and_then(|v| v.as_u64())
            })
        })
        .collect();
    let mut llm_by_id: std::collections::HashMap<u64, Vec<f32>> = std::collections::HashMap::new();
    match vector
        .fetch_vectors(
            collection,
            &ids,
            crate::shared::collections::VECTOR_NAME_LLM,
        )
        .await
    {
        Ok(vecs) => {
            for v in vecs {
                if let (Some(id), Some(vec_arr)) = (
                    v.get("id").and_then(|x| x.as_u64()),
                    v.get("vector").and_then(|x| x.as_array()),
                ) {
                    let vec_f32: Vec<f32> = vec_arr
                        .iter()
                        .filter_map(|x| x.as_f64().map(|f| f as f32))
                        .collect();
                    if !vec_f32.is_empty() {
                        llm_by_id.insert(id, vec_f32);
                    }
                }
            }
        }
        Err(e) => tracing::warn!("llm 向量 rerank 拉取失败（跳过 rerank）: {e}"),
    }

    // 融合：final = 0.5 * base_score + 0.5 * cosine(query, llm_vec)；无 llm 向量保持 base。
    for hit in kept.iter_mut() {
        let base_score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let id = hit
            .get("id")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                hit.get("payload")
                    .and_then(|p| p.get("entity_id"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        let final_score = match llm_by_id.get(&id) {
            Some(llm_vec) => {
                let sim = cosine_similarity(query_vec, llm_vec);
                // 余弦值域 [-1,1]，Qdrant base score 也是 cosine 相似度 →
                // 直接线性融合即可（0.5/0.5，用户拍板）。
                0.5 * base_score + 0.5 * sim
            }
            None => base_score,
        };
        if let Some(obj) = hit.as_object_mut() {
            obj.insert("score".to_string(), serde_json::json!(final_score));
        }
    }

    // 按融合分重排序（稳定降序）。
    kept.sort_by(|a, b| {
        let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    kept
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// 跨世界搜索的输入。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchRequest {
    /// 自然语言搜索查询。
    pub query: String,
    /// 目标世界："code"、"knowledge"、"doc"、"all"。默认："all"。
    pub world: Option<String>,
    /// 每个世界的最大结果数。
    pub limit: Option<usize>,
    /// 按项目过滤。
    pub project: Option<String>,
    /// 图扩展跳数（knowledge 世界），白名单 {1,2}，默认 1。
    pub max_hops: Option<u32>,
    /// knowledge top-5 实体从 doc_chunks 回填证据段落（仅 knowledge 世界生效）。
    pub with_evidence: Option<bool>,
    /// 按 kg_nodes payload origin 过滤召回种子（extracted/learned/manual）。
    pub origin: Option<String>,
    /// 仅 world=doc：限定单文档内检索证据块。
    pub doc_id: Option<String>,
    /// 按文件类型过滤（`--file-type`）：类别名（document/code/config）或具体后缀（md/yaml/java…）。
    pub file_type: Option<String>,
    /// 按内容类型过滤（`--content-type`）：LLM 语义类型（Config/Service/Standard…）或 AST 类型（Method/Class…）。
    pub entity_type_filter: Option<String>,
}

/// 来自任意世界的单个搜索命中。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    /// 唯一标识符。
    pub id: String,
    /// 短标签/标题。
    pub title: String,
    /// 内容或摘要片段。
    pub snippet: String,
    /// 原始正文片段（配置世界保留 Nacos 返回的原格式）。
    #[serde(default)]
    pub content: Option<String>,
    /// 来源世界（"code"、"knowledge"、"doc"、"vector"）。
    pub source_world: String,
    /// 实体类型（Method、Class、Knowledge、Document 等）。
    pub entity_type: String,
    /// 文件类型（文件后缀决定的类别：文档/代码/配置）。由 file_path/source_ref/doc_id 推断。
    #[serde(default)]
    pub file_type: Option<String>,
    /// 文件类型类别显示名（文档/代码/配置/其他）。
    #[serde(default)]
    pub file_type_label: Option<String>,
    /// 相关度分数 [0.0, 1.0]。
    pub score: f64,
    /// 源文件或引用 URL。
    pub source_ref: Option<String>,
    /// 配置来源元数据（namespace/dataId/group/environment 等）。
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// 源文件路径（code 世界）。
    pub file_path: Option<String>,
    /// 项目名（code 世界；从 Qdrant payload 读，用于展示层拼磁盘全路径）。
    #[serde(default)]
    pub project: Option<String>,
    /// 起始行号（code 世界）。
    pub start_line: Option<u32>,
    /// 结束行号（code 世界）。
    pub end_line: Option<u32>,
    /// 函数/方法签名（code 世界）。
    pub signature: Option<String>,
    /// 方法级 LLM 分析（code world，payload 直取；"用途：…\n逻辑：…"）。
    #[serde(default)]
    pub llm_analysis: Option<String>,
    /// 被调用的函数名（code 世界）。
    pub calls: Vec<String>,
    /// 来自知识图谱的元素 ID。
    pub element_id: Option<String>,
    /// 排序分解（knowledge 世界新链路填充）。
    #[serde(default)]
    pub score_breakdown: Option<ScoreBreakdown>,
    /// 图距离：0=直接命中（含 SAME_AS 别名），1/2=扩展邻居。
    #[serde(default)]
    pub hop: Option<u32>,
    /// 是否经 SAME_AS 别名归并命中。
    #[serde(default)]
    pub via_same_as: Option<bool>,
    /// 命中实体的关系摘要（去重聚合后，上限 5 条）。
    #[serde(default)]
    pub relations: Option<Vec<RelationSnippet>>,
    /// 证据段落（world=doc 或 with_evidence 回填）。
    #[serde(default)]
    pub evidence: Option<Vec<String>>,
    /// rerank 降级标记。
    #[serde(default)]
    pub rerank_degraded: Option<bool>,
}

/// 跨世界搜索的输出。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrossWorldResult {
    /// 原始查询。
    pub query: String,
    /// 被搜索的世界。
    pub world: String,
    /// 聚合结果。
    pub hits: Vec<SearchHit>,
    /// 命中总数。
    pub total: usize,
    /// 各世界的命中数，便于透明化。
    pub per_world_counts: std::collections::HashMap<String, usize>,
    /// 降级标记（"rerank_unavailable" / "graph_expansion_failed" / "embed_unavailable"）。
    #[serde(default)]
    pub degraded: Vec<String>,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// 执行跨世界语义搜索。
#[async_trait::async_trait]
pub trait CrossWorldSearchTrait: Send + Sync {
    /// 跨世界搜索。
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError>;
}

/// [`CrossWorldSearchTrait`] 的标准实现。
pub struct CrossWorldSearch {
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    rerank: Option<Arc<dyn RerankService>>,
}

impl CrossWorldSearch {
    /// 使用后端创建。
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            rerank,
        }
    }

    /// 创建无后端实例（用于测试）。
    pub fn empty() -> Self {
        Self {
            graph: None,
            vector: None,
            embed: None,
            rerank: None,
        }
    }

    /// 供同 crate 扩展方法（search_config/search_memory）访问图后端。
    pub(crate) fn graph_ref(&self) -> &Option<Arc<dyn GraphRepository>> {
        &self.graph
    }

    /// 供同 crate 扩展方法（search_config）访问向量后端。
    pub(crate) fn vector_ref(&self) -> &Option<Arc<dyn VectorRepository>> {
        &self.vector
    }

    /// 供同 crate 扩展方法（search_config）访问 embed 后端。
    pub(crate) fn embed_ref(&self) -> &Option<Arc<dyn EmbedService>> {
        &self.embed
    }

    /// 通过 Qdrant 向量搜索检索 Reality World（代码实体）。
    ///
    /// 查询 `{project}_methods` 集合（未指定项目时查询所有 `*_methods`），
    /// 并提取完整的 payload 字段，包括 start_line、end_line、
    /// calls 与 method_id。分数阈值从 `DT_SEARCH_MIN_SCORE` 读取
    /// （默认 0.3）。
    async fn search_code(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };

        // 1B:标识符查询做拆词扩写,让向量通道"看见"字面结构
        let ident_mode = is_identifier_query(query);
        let search_text = if ident_mode {
            let words = split_identifier(query);
            if words.len() > 1 {
                format!("{} {}", query, words.join(" "))
            } else {
                query.to_string()
            }
        } else {
            query.to_string()
        };

        let embeddings = embed.embed_batch(&[search_text]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        // U-D6：查全局 code_methods + code_classes（方法 + 类实体）；
        // project 过滤下沉 payload 级。类描述向量使类可被检索（GAP-A）。
        let method_cols = vec![
            crate::shared::collections::CODE_METHODS.to_string(),
            crate::shared::collections::CODE_CLASSES.to_string(),
        ];

        if method_cols.is_empty() {
            return Ok(Vec::new());
        }

        // 环境变量中的分数阈值（默认 0.3）
        let min_score = std::env::var("DT_SEARCH_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.3);

        // 1D:内部 TopK 放大,给精确通道/关键词兜底留出空间
        let internal_limit = (limit * 2).max(10);

        let mut all_hits: Vec<SearchHit> = Vec::new();
        // 精确通道命中的 id,向量通道跳过避免重复
        let mut exact_ids: Vec<String> = Vec::new();

        for col in &method_cols {
            // 1A:标识符精确匹配通道——payload name 精确过滤,命中强制置顶。
            // code_classes 是单向量集合：用单向量版 search_with_filter（无 named vectors）。
            // P1-4：此前跳过 classes 导致类名精确检索失效（向量分不足时无兜底）。
            let is_classes = col == crate::shared::collections::CODE_CLASSES;
            if ident_mode {
                let filter = serde_json::json!({
                    "must": [{"key": "name", "match": {"value": query}}]
                });
                let search_res = if is_classes {
                    vector
                        .search_with_filter(col, query_vec.clone(), internal_limit as u64, filter)
                        .await
                } else {
                    vector
                        .search_named_with_filter(
                            col,
                            crate::shared::collections::VECTOR_NAME_BASE,
                            query_vec.clone(),
                            internal_limit as u64,
                            filter,
                        )
                        .await
                };
                match search_res {
                    Ok(results) => {
                        for hit in results {
                            let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            if score < min_score {
                                continue;
                            }
                            let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if name.is_empty() || name == "?" {
                                continue;
                            }
                            if let Some(p) = project {
                                let pp = payload
                                    .get("project")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if pp != p {
                                    continue;
                                }
                            }
                            let id = hit_id(&hit, payload);
                            exact_ids.push(id.clone());
                            // 精确标识符完全匹配是强信号,给 0.95 高分确保 search()
                            // 主体重排序后仍居首(与 doc 关键词通道 0.90 同思路)
                            all_hits.push(hit_from_payload(payload, name.to_string(), id, 0.95));
                        }
                    }
                    Err(e) => tracing::warn!("标识符精确通道 {col} 搜索失败: {e}"),
                }
            }

            // 向量通道(语义召回)——base 向量召回，llm 向量 rerank。
            // code_methods 是 named 双向量（base/llm），code_classes 是单向量
            // （ensure_collection 非 code_methods 默认单向量）——按集合区分查询。
            let is_classes = col == crate::shared::collections::CODE_CLASSES;
            let search_res = if is_classes {
                vector
                    .search(col, query_vec.clone(), internal_limit as u64)
                    .await
            } else {
                vector
                    .search_named(
                        col,
                        crate::shared::collections::VECTOR_NAME_BASE,
                        query_vec.clone(),
                        internal_limit as u64,
                    )
                    .await
            };
            match search_res {
                Ok(results) => {
                    // llm 向量 rerank：拉取 top 候选的 llm 向量（限批），
                    // 与 query 算 cosine 相似度，与 base 分数加权融合后重排。
                    let reranked = if is_classes {
                        // 单向量集合无 llm 向量可 rerank，但需与 rerank 相同的基础过滤
                        // （score ≥ min_score、project 匹配、name 合法、exact 去重）——
                        // 否则其他项目类泄漏 + 低分噪声挤占 limit（P0-3）。
                        let mut kept = Vec::new();
                        for hit in results {
                            let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            if score < min_score {
                                continue;
                            }
                            let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if name.is_empty() || name == "?" {
                                continue;
                            }
                            if let Some(p) = project {
                                let pp = payload
                                    .get("project")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if pp != p {
                                    continue;
                                }
                            }
                            let id = hit_id(&hit, payload);
                            if exact_ids.contains(&id) {
                                continue;
                            }
                            kept.push(hit);
                        }
                        kept
                    } else {
                        rerank_with_llm_vectors(
                            &vector, col, &query_vec, results, min_score, project, &exact_ids,
                        )
                        .await
                    };
                    for hit in reranked {
                        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                        let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let id = hit_id(&hit, payload);
                        all_hits.push(hit_from_payload(payload, name.to_string(), id, score));
                    }
                }
                Err(e) => tracing::warn!("对 {col} 的 Qdrant 搜索失败: {e}"),
            }
        }

        // 2B/4:结果不足时关键词兜底(scroll payload + 本地 CONTAINS 匹配)。
        // P1-4：循环所有集合（method_cols[0]=code_methods + code_classes），
        // 使类名精确匹配也有兜底（此前只 scroll 方法集合，类精确检索全靠向量分）。
        if all_hits.len() < limit {
            let kws = extract_keywords(query, 3);
            'outer: for col in &method_cols {
                match vector.scroll_payloads(col, None, 5000).await {
                    Ok(payloads) => {
                        for kw in &kws {
                            let lkw = kw.to_lowercase();
                            for p in &payloads {
                                if all_hits.len() >= limit {
                                    break 'outer;
                                }
                                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                if name.is_empty() || name == "?" {
                                    continue;
                                }
                                if !name.to_lowercase().contains(&lkw) {
                                    continue;
                                }
                                if let Some(pr) = project {
                                    if p.get("project").and_then(|v| v.as_str()).unwrap_or("") != pr
                                    {
                                        continue;
                                    }
                                }
                                let id = p
                                    .get("entity_id")
                                    .or(p.get("method_id"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| {
                                        format!(
                                            "{}:{}",
                                            p.get("file_path")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("?"),
                                            name
                                        )
                                    });
                                if exact_ids.contains(&id) || all_hits.iter().any(|h| h.id == id) {
                                    continue;
                                }
                                // 兜底命中固定低分(低于向量阈值,仅保证可达)
                                all_hits.push(hit_from_payload(p, name.to_string(), id, 0.28));
                            }
                        }
                    }
                    Err(e) => tracing::warn!("关键词兜底 scroll {col} 失败: {e}"),
                }
                if all_hits.len() >= limit {
                    break;
                }
            }
        }

        // 精确通道命中保持前置,其余按分数降序,截断到 limit
        let exact_count = exact_ids.len();
        if exact_count < all_hits.len() {
            all_hits[exact_count..].sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        all_hits.truncate(limit);
        // 图增强:向量召回后,对 top 命中查 Memgraph 补 1 跳调用关系(非致命)。
        self.enrich_code_with_graph(&mut all_hits).await;
        Ok(all_hits)
    }

    /// 通过 GraphRAG 混合检索（S5）搜索 Knowledge World。
    ///
    /// 委托 retrieve.rs；返回 (hits, degraded)。vector/embed 缺失时无可召回手段，返回空。
    async fn search_knowledge(&self, request: &SearchRequest) -> (Vec<SearchHit>, Vec<String>) {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return (Vec::new(), Vec::new());
        };
        let retriever = Retriever::new(
            self.graph.clone(),
            vector.clone(),
            embed.clone(),
            self.rerank.clone(),
        );
        let req = RetrieveRequest {
            query: &request.query,
            project: request.project.as_deref(),
            limit: request.limit.unwrap_or(20),
            max_hops: request.max_hops.unwrap_or(1),
            origin: request.origin.as_deref(),
        };
        match retriever.search_knowledge(&req).await {
            Ok(mut outcome) => {
                if request.with_evidence == Some(true) {
                    retriever
                        .backfill_evidence(&request.query, &mut outcome.hits)
                        .await;
                }
                (outcome.hits, outcome.degraded)
            }
            Err(e) => {
                tracing::warn!("知识检索失败: {e}");
                (Vec::new(), Vec::new())
            }
        }
    }

    /// 通过 `doc_chunks` 搜索 Doc World（S5 §5.5）。
    ///
    /// filter 强制 `source="doc"`——排除 config_sync 写入的 nacos 配置点
    /// （payload 无 text/doc_id/block_index）。分数过 DT_SEARCH_MIN_SCORE。
    async fn search_doc(
        &self,
        query: &str,
        project: Option<&str>,
        doc_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };
        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        let mut must = vec![serde_json::json!({"key": "source", "match": {"value": "doc"}})];
        if let Some(p) = project {
            must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
        }
        if let Some(d) = doc_id {
            must.push(serde_json::json!({"key": "doc_id", "match": {"value": d}}));
        }
        let filter = serde_json::json!({ "must": must });

        let results = vector
            .search_with_filter(
                crate::shared::collections::DOC_CHUNKS,
                query_vec,
                limit as u64,
                filter,
            )
            .await?;
        let threshold = crate::application::knowledge::extract::retrieve::min_score();
        let mut hits: Vec<SearchHit> = results
            .into_iter()
            .filter_map(|hit| {
                let score = hit.get("score")?.as_f64()?;
                if score < threshold {
                    return None;
                }
                let payload = hit.get("payload")?;
                let doc = payload.get("doc_id")?.as_str()?;
                let block = payload.get("block_index")?.as_u64()? as u32;
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                // 文件类型：从 doc_id 后缀推断；内容类型：配置文件后缀启发式为 Config，其余 Doc。
                let (file_type, file_type_label) = infer_file_type(Some(doc));
                let entity_type = match file_type.as_deref() {
                    Some("config") => "Config".to_string(),
                    _ => "Doc".to_string(),
                };
                Some(SearchHit {
                    id: format!("{doc}:{block}"),
                    title: doc.rsplit('/').next().unwrap_or(doc).to_string(),
                    snippet: text.chars().take(200).collect(),
                    content: Some(text.to_string()),
                    source_world: "doc".into(),
                    entity_type,
                    file_type,
                    file_type_label,
                    score,
                    source_ref: Some(doc.to_string()),
                    metadata: None,
                    file_path: None,
                    project: None,
                    start_line: None,
                    end_line: None,
                    signature: None,
                    llm_analysis: None,
                    calls: vec![],
                    element_id: None,
                    score_breakdown: None,
                    hop: None,
                    via_same_as: None,
                    relations: None,
                    evidence: None,
                    rerank_degraded: None,
                })
            })
            .collect();

        // 4.1:长查询关键词通道(与向量并行,关键词命中前置——向量对长多关键词
        // 查询会泛化,关键词字面匹配保证设计文档/术语类文档可达)
        let kws = extract_keywords(query, 3);
        let is_long_query =
            query.chars().count() > 20 || kws.iter().any(|k| k.chars().count() >= 4);
        if is_long_query {
            match vector
                .scroll_payloads(crate::shared::collections::DOC_CHUNKS, None, 5000)
                .await
            {
                Ok(payloads) => {
                    let mut kw_hits: Vec<SearchHit> = Vec::new();
                    let mut seen: std::collections::HashSet<String> =
                        hits.iter().map(|h| h.id.clone()).collect();
                    'outer: for kw in &kws {
                        let lkw = kw.to_lowercase();
                        for p in &payloads {
                            if kw_hits.len() >= limit {
                                break 'outer;
                            }
                            if let Some(pr) = project {
                                if p.get("project").and_then(|v| v.as_str()).unwrap_or("") != pr {
                                    continue;
                                }
                            }
                            let text = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            if text.is_empty() || !text.to_lowercase().contains(&lkw) {
                                continue;
                            }
                            let doc = p.get("doc_id").and_then(|v| v.as_str()).unwrap_or("");
                            let block =
                                p.get("block_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let id = format!("{doc}:{block}");
                            if !seen.insert(id.clone()) {
                                continue;
                            }
                            // 文件类型：从 doc_id 后缀推断；内容类型：配置文件后缀启发式为 Config。
                            let (file_type, file_type_label) = infer_file_type(Some(&doc));
                            let entity_type = match file_type.as_deref() {
                                Some("config") => "Config".to_string(),
                                _ => "Doc".to_string(),
                            };
                            kw_hits.push(SearchHit {
                                id,
                                title: doc.rsplit('/').next().unwrap_or(doc).to_string(),
                                snippet: text.chars().take(200).collect(),
                                content: Some(text.to_string()),
                                source_world: "doc".into(),
                                entity_type,
                                file_type,
                                file_type_label,
                                // 关键词字面命中是强信号,给高分确保在 search() 主体
                                // 重排序后仍排在向量泛化结果之前
                                score: 0.90,
                                source_ref: Some(doc.to_string()),
                                metadata: None,
                                file_path: None,
                                project: None,
                                start_line: None,
                                end_line: None,
                                signature: None,
                                llm_analysis: None,
                                calls: vec![],
                                element_id: None,
                                score_breakdown: None,
                                hop: None,
                                via_same_as: None,
                                relations: None,
                                evidence: None,
                                rerank_degraded: None,
                            });
                        }
                    }
                    // 关键词命中前置,向量结果补位
                    if !kw_hits.is_empty() {
                        let mut merged = kw_hits;
                        merged.extend(hits);
                        hits = merged;
                    }
                }
                Err(e) => tracing::warn!("doc 关键词通道 scroll 失败: {e}"),
            }
        }
        Ok(hits)
    }

    /// 图增强:对 top-10 code 命中按 `method_id`(与 Qdrant payload entity_id
    /// 一致)匹配 Memgraph,一次 UNWIND 批量补:所属类(CONTAINS)、调用
    /// (CALLS out)、被调用(CALLS in),填充 relations 并标记 hop=1。
    ///
    /// 设计:向量召回保证准确率,图增强追加关系上下文;graph 缺失/查询
    /// 失败一律静默降级,不影响主结果。
    async fn enrich_code_with_graph(&self, hits: &mut Vec<SearchHit>) {
        let Some(graph) = &self.graph else {
            tracing::info!("code 图增强跳过: graph 不可用");
            return;
        };
        if hits.is_empty() {
            return;
        }
        const ENRICH_TOP: usize = 10;
        let ids: Vec<serde_json::Value> = hits
            .iter()
            .take(ENRICH_TOP)
            .filter_map(|h| h.element_id.as_ref())
            .map(|eid| serde_json::Value::String(eid.clone()))
            .collect();
        if ids.is_empty() {
            tracing::info!("code 图增强跳过: 无 element_id");
            return;
        }
        tracing::info!("code 图增强: 查询 {} 个实体", ids.len());
        let query = r#"
            UNWIND $ids AS mid
            MATCH (m:Method) WHERE m.method_id = mid
            OPTIONAL MATCH (c:Class)-[:CONTAINS]->(m)
            OPTIONAL MATCH (m)-[:CALLS]->(callee:Method)
            OPTIONAL MATCH (caller:Method)-[:CALLS]->(m)
            RETURN mid AS mid,
                   collect(DISTINCT c.name) AS classes,
                   collect(DISTINCT callee.name) AS callees,
                   collect(DISTINCT caller.name) AS callers
        "#;
        let params =
            std::collections::HashMap::from([("ids".to_string(), serde_json::Value::Array(ids))]);
        match graph.read_query(query, params).await {
            Ok(rows) => {
                let Some(arr) = rows.as_array() else {
                    tracing::warn!("code 图增强: 返回非数组");
                    return;
                };
                tracing::debug!("code 图增强: rows={} ids 查询完成", arr.len());
                let mut rel_map: std::collections::HashMap<String, Vec<RelationSnippet>> =
                    std::collections::HashMap::new();
                for row in arr {
                    let Some(mid) = row.get("mid").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let names = |key: &str| -> Vec<String> {
                        row.get(key)
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str())
                                    .filter(|s| !s.is_empty())
                                    .map(|s| s.to_string())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };
                    let (classes, callees, callers) =
                        (names("classes"), names("callees"), names("callers"));
                    if classes.is_empty() && callees.is_empty() && callers.is_empty() {
                        continue;
                    }
                    let mut snips: Vec<RelationSnippet> = Vec::new();
                    for c in classes.into_iter().take(2) {
                        snips.push(RelationSnippet {
                            rel_type: "belongs_to".into(),
                            other_end_id: String::new(),
                            other_end_name: c,
                            direction: "out".into(),
                            confidence: 1.0,
                            evidence: None,
                            supplementary_count: 0,
                        });
                    }
                    for cal in callees.into_iter().take(3) {
                        snips.push(RelationSnippet {
                            rel_type: "calls".into(),
                            other_end_id: String::new(),
                            other_end_name: cal,
                            direction: "out".into(),
                            confidence: 1.0,
                            evidence: None,
                            supplementary_count: 0,
                        });
                    }
                    for caller in callers.into_iter().take(3) {
                        snips.push(RelationSnippet {
                            rel_type: "called_by".into(),
                            other_end_id: String::new(),
                            other_end_name: caller,
                            direction: "in".into(),
                            confidence: 1.0,
                            evidence: None,
                            supplementary_count: 0,
                        });
                    }
                    rel_map.insert(mid.to_string(), snips);
                }
                for h in hits.iter_mut().take(ENRICH_TOP) {
                    if let Some(eid) = &h.element_id {
                        if let Some(snips) = rel_map.get(eid) {
                            h.relations = Some(snips.clone());
                            h.hop = Some(1);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("code 图增强失败(非致命): {e}"),
        }
    }
}

#[async_trait::async_trait]
impl CrossWorldSearchTrait for CrossWorldSearch {
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError> {
        let world = request.world.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(20);
        let project = request.project.as_deref();

        // 乱码/无意义查询直接返回空(避免 embed 幻觉命中)
        if is_gibberish(&request.query) {
            return Ok(CrossWorldResult {
                query: request.query.clone(),
                world: world.to_string(),
                hits: vec![],
                total: 0,
                per_world_counts: std::collections::HashMap::new(),
                degraded: vec![],
            });
        }

        let mut per_world = std::collections::HashMap::new();
        let mut degraded: Vec<String> = Vec::new();

        let all_hits = if world == "all" {
            // U-D2/U-D3：all = code+knowledge+doc，跨世界 RRF
            let mut lists: Vec<Vec<SearchHit>> = Vec::new();

            let code_hits = self
                .search_code(&request.query, project, limit)
                .await
                .unwrap_or_default();
            per_world.insert("code".to_string(), code_hits.len());
            lists.push(code_hits);

            let (kn_hits, dgr) = self.search_knowledge(request).await;
            degraded.extend(dgr);
            per_world.insert("knowledge".to_string(), kn_hits.len());
            lists.push(kn_hits);

            let doc_hits = self
                .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                .await
                .unwrap_or_default();
            per_world.insert("doc".to_string(), doc_hits.len());
            lists.push(doc_hits);

            crate::application::context::fusion::rrf_hits(lists, 60.0, limit)
        } else {
            let mut hits = match world {
                "code" => self
                    .search_code(&request.query, project, limit)
                    .await
                    .unwrap_or_default(),
                "knowledge" => {
                    let (h, dgr) = self.search_knowledge(request).await;
                    degraded.extend(dgr);
                    h
                }
                // "vector" 保留为 "doc" 别名（S5 §8.7）
                "doc" | "vector" => self
                    .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                    .await
                    .unwrap_or_default(),
                "config" => {
                    let (h, dgr) = self.search_config(&request.query, project, limit).await;
                    degraded.extend(dgr);
                    h
                }
                "memory" => self.search_memory(&request.query, limit, project).await,
                _ => Vec::new(),
            };
            per_world.insert(world.to_string(), hits.len());
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit);
            hits
        };

        let total = all_hits.len();
        // 统一后处理：推断 file_type + 按 file_type/entity_type 过滤（U-5 新能力）。
        let all_hits = postprocess_hits(all_hits, request);
        let total = all_hits.len();
        Ok(CrossWorldResult {
            query: request.query.clone(),
            world: world.to_string(),
            hits: all_hits,
            total,
            per_world_counts: per_world,
            degraded,
        })
    }
}

/// 从路径（file_path / source_ref / doc_id）推断文件类别显示信息。
fn infer_file_type(path: Option<&str>) -> (Option<String>, Option<String>) {
    infer_file_type_pub(path)
}

/// 公开版：从路径推断文件类别（供 retrieve.rs 等外部模块调用）。
pub fn infer_file_type_pub(path: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(p) = path else {
        return (None, None);
    };
    // 来源优先：dt://nacos/ 前缀 → NacosConfig（不走后缀映射）。
    if p.starts_with("dt://nacos/") {
        let c = crate::domain::file_type::FileCategory::NacosConfig;
        return (Some(c.slug().to_string()), Some(c.label().to_string()));
    }
    let cat = crate::domain::file_type::categorize_path(p);
    match cat {
        crate::domain::file_type::FileCategory::Other => (None, None),
        c => (Some(c.slug().to_string()), Some(c.label().to_string())),
    }
}

/// 搜索命中后处理：填充 file_type 字段，并按 file_type/entity_type 过滤。
fn postprocess_hits(hits: Vec<SearchHit>, req: &SearchRequest) -> Vec<SearchHit> {
    // 1) 填充 file_type（未填充的命中从路径推断）
    let mut enriched: Vec<SearchHit> = hits
        .into_iter()
        .map(|mut h| {
            if h.file_type.is_none() {
                let path = h
                    .file_path
                    .as_deref()
                    .or(h.source_ref.as_deref())
                    .or(Some(h.id.as_str()));
                let (ft, ftl) = infer_file_type(path);
                h.file_type = ft;
                h.file_type_label = ftl;
            }
            h
        })
        .collect();

    // 2) 按文件类型过滤
    if let Some(ft_spec) = req.file_type.as_deref() {
        let cats = crate::domain::file_type::resolve_file_types(ft_spec);
        if !cats.is_empty() {
            enriched.retain(|h| {
                h.file_type
                    .as_deref()
                    .map(|ft| cats.iter().any(|c| c.slug() == ft))
                    .unwrap_or(false)
            });
        }
    }

    // 3) 按内容类型过滤（大小写不敏感）
    if let Some(et_spec) = req.entity_type_filter.as_deref() {
        let want = et_spec.trim().to_lowercase();
        if !want.is_empty() {
            enriched.retain(|h| h.entity_type.to_lowercase() == want);
        }
    }

    enriched
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_hit_construction() {
        let hit = SearchHit {
            id: "4:abc".into(),
            title: "PaymentService".into(),
            snippet: "Handles payment processing".into(),
            content: None,
            source_world: "code".into(),
            entity_type: "Service".into(),
            file_type: None,
            file_type_label: None,
            score: 0.95,
            source_ref: Some("src/payment.rs".into()),
            metadata: None,
            file_path: Some("src/payment.rs".into()),
            project: Some("pay-center".into()),
            start_line: Some(10),
            end_line: Some(45),
            signature: Some("pub fn process()".into()),
            llm_analysis: None,
            calls: vec!["validate".into(), "save".into()],
            element_id: Some("4:xyz".into()),
            score_breakdown: Some(ScoreBreakdown {
                semantic: 0.71,
                rerank: 0.92,
                graph_boost: 1.0,
                final_score: 0.83,
            }),
            hop: Some(0),
            via_same_as: None,
            relations: Some(vec![RelationSnippet {
                rel_type: "routes_to".into(),
                other_end_id: "dt://entity/p/Service/s".into(),
                other_end_name: "PayChannelService".into(),
                direction: "out".into(),
                confidence: 0.9,
                evidence: Some("ifCode 决定路由".into()),
                supplementary_count: 2,
            }]),
            evidence: None,
            rerank_degraded: None,
        };
        assert_eq!(hit.source_world, "code");
        assert!(hit.score > 0.9);
        assert_eq!(hit.start_line, Some(10));
        assert_eq!(hit.calls.len(), 2);
        assert_eq!(hit.relations.as_ref().unwrap()[0].supplementary_count, 2);
    }

    #[test]
    fn search_hit_deserializes_legacy_json_without_new_fields() {
        let legacy = r#"{
            "id":"1","title":"t","snippet":"s","source_world":"knowledge",
            "entity_type":"Knowledge","score":0.5,"source_ref":null,
            "file_path":null,"start_line":null,"end_line":null,
            "signature":null,"calls":[],"element_id":null
        }"#;
        let hit: SearchHit = serde_json::from_str(legacy).unwrap();
        assert!(hit.score_breakdown.is_none());
        assert!(hit.hop.is_none());
        assert!(hit.relations.is_none());
        assert!(hit.rerank_degraded.is_none());
    }

    #[test]
    fn extract_keywords_delegates_to_unified_keywords_of() {
        // 薄包装：extract_keywords 必须与统一实现 keywords_of 输出完全一致
        //（委托而非各持一份实现，防止行为漂移）。覆盖中文整句/全角标点等
        // 边界输入——统一实现行为 = ASCII 整词 + CJK 虚词切分 + n-gram、
        // 全角标点为分隔符。
        for q in [
            "load balance",
            "实现负载均衡，支持高并发",
            "支付渠道，路由",
            "redis 缓存、限流",
            "！",
        ] {
            for max in [1usize, 3, 8] {
                assert_eq!(
                    extract_keywords(q, max),
                    crate::application::knowledge::extract::retrieve::keywords_of(q, max),
                    "委托结果不一致: query={q:?}, max={max}"
                );
            }
        }
    }

    #[test]
    fn cross_world_result_empty() {
        let result = CrossWorldResult {
            query: "test".into(),
            world: "all".into(),
            hits: vec![],
            total: 0,
            per_world_counts: std::collections::HashMap::new(),
            degraded: vec![],
        };
        assert_eq!(result.total, 0);
    }

    #[test]
    fn cross_world_result_serialization() {
        let mut counts = std::collections::HashMap::new();
        counts.insert("code".into(), 5);
        counts.insert("knowledge".into(), 3);

        let result = CrossWorldResult {
            query: "auth".into(),
            world: "all".into(),
            hits: vec![SearchHit {
                id: "1".into(),
                title: "AuthService".into(),
                snippet: "manages auth".into(),
                content: None,
                source_world: "code".into(),
                entity_type: "Service".into(),
                file_type: None,
                file_type_label: None,
                score: 0.98,
                source_ref: None,
                metadata: None,
                file_path: None,
                project: None,
                start_line: None,
                end_line: None,
                signature: None,
                llm_analysis: None,
                calls: vec![],
                element_id: None,
                score_breakdown: None,
                hop: None,
                via_same_as: None,
                relations: None,
                evidence: None,
                rerank_degraded: None,
            }],
            total: 8,
            per_world_counts: counts,
            degraded: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("AuthService"));
        assert!(json.contains("\"total\":8"));
    }

    #[test]
    fn search_request_defaults() {
        let req = SearchRequest {
            query: "payment".into(),
            world: None,
            limit: None,
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        assert_eq!(req.query, "payment");
        assert_eq!(req.world, None);
    }

    #[test]
    fn infer_file_type_nacos_prefix_wins_over_suffix() {
        // dt://nacos/ 前缀 → NacosConfig（来源优先，即使带 .yaml 后缀）
        assert_eq!(
            infer_file_type_pub(Some(
                "dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud"
            )),
            (Some("nacos_config".into()), Some("nacos配置".into()))
        );
        assert_eq!(
            infer_file_type_pub(Some("dt://nacos/test/DEFAULT_GROUP/common.yaml")),
            (Some("nacos_config".into()), Some("nacos配置".into()))
        );
        // 普通路径仍走后缀映射
        assert_eq!(
            infer_file_type_pub(Some("src/main.rs")),
            (Some("code".into()), Some("代码".into()))
        );
        assert_eq!(
            infer_file_type_pub(Some("a/b/config.yaml")),
            (Some("config".into()), Some("配置".into()))
        );
        // 空路径
        assert_eq!(infer_file_type_pub(None), (None, None));
    }

    #[tokio::test]
    async fn knowledge_world_with_empty_backends_returns_empty_and_no_panic() {
        let cws = CrossWorldSearch::empty();
        let req = SearchRequest {
            query: "q".into(),
            world: Some("knowledge".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 0);
        assert_eq!(result.per_world_counts.get("knowledge"), Some(&0));
        assert!(result.degraded.is_empty());
    }

    #[test]
    fn constructor_accepts_rerank_as_fourth_param() {
        fn _accept(_: Option<std::sync::Arc<dyn crate::domain::traits::RerankService>>) {}
        let cws = CrossWorldSearch::new(None, None, None, None);
        drop(cws);
    }

    struct StubVector {
        hits: Vec<serde_json::Value>,
        captured_filter: std::sync::Mutex<Option<serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for StubVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            // 双集合搜索（code_methods + code_classes）：桩仅对方法集合返回命中，
            // 类集合返回空——避免旧测试断言（1 条命中）被类集合重复命中破坏。
            if c == crate::shared::collections::CODE_CLASSES {
                return Ok(Vec::new());
            }
            Ok(self.hits.clone())
        }
        async fn search_with_filter(
            &self,
            c: &str,
            _v: Vec<f32>,
            _l: u64,
            filter: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            *self.captured_filter.lock().unwrap() = Some(filter);
            if c == crate::shared::collections::CODE_CLASSES {
                return Ok(Vec::new());
            }
            Ok(self.hits.clone())
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.into(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    struct StubEmbed;

    #[async_trait::async_trait]
    impl EmbedService for StubEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn code_world_extracts_llm_analysis_and_uses_single_collection() {
        let method = serde_json::json!({
            "id": "pt-m1", "score": 0.9,
            "payload": {
                "name": "createApp", "file_path": "test/project/app.js",
                "start_line": 32, "end_line": 36,
                "signature": "function createApp(port)",
                "llm_analysis": "用途：创建服务器实例。\n逻辑：实例化服务器对象。",
                "project": "test-pipeline", "calls": []
            }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![method],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "createApp".into(),
            world: Some("code".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(
            hit.llm_analysis.as_deref(),
            Some("用途：创建服务器实例。\n逻辑：实例化服务器对象。")
        );
        assert_eq!(hit.file_path.as_deref(), Some("test/project/app.js"));
        assert_eq!(hit.start_line, Some(32));
    }

    #[tokio::test]
    async fn code_world_handles_numeric_point_id_and_payload_fallback() {
        // Qdrant 数值型 point id：as_str() 会失败——必须按 number 提取
        let m1 = serde_json::json!({
            "id": 123, "score": 0.9,
            "payload": { "name": "m1", "file_path": "a.js",
                         "start_line": 1, "end_line": 2, "project": "p" }
        });
        // id 缺失：回退 payload entity_id
        let m2 = serde_json::json!({
            "score": 0.8,
            "payload": { "name": "m2", "file_path": "b.js", "entity_id": "dt://method/p/m2",
                         "start_line": 1, "end_line": 2, "project": "p" }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![m1, m2],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "q".into(),
            world: Some("code".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].id, "123");
        assert_eq!(result.hits[1].id, "dt://method/p/m2");
        // RRF 键唯一性：两条 hit 的 id 不得相同（防 "code:?" 坍缩回归）
        assert_ne!(result.hits[0].id, result.hits[1].id);
    }

    #[tokio::test]
    async fn doc_world_filters_source_and_maps_chunk_payload() {
        let chunk = serde_json::json!({
            "id": "pt-1", "score": 0.9,
            "payload": {
                "doc_id": "dt://doc/offen-pay/pay-design.md",
                "block_index": 3,
                "project": "offen-pay",
                "text": "路由规则：根据 ifCode 匹配渠道表",
                "entity_ids": ["dt://entity/offen-pay/Channel/ifcode"],
                "degraded": false,
                "source": "doc"
            }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![chunk],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector.clone()),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "ifCode 证据".into(),
            world: Some("doc".into()),
            limit: Some(5),
            project: Some("offen-pay".into()),
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: Some("dt://doc/offen-pay/pay-design.md".into()),
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("doc"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.id, "dt://doc/offen-pay/pay-design.md:3");
        assert_eq!(hit.source_world, "doc");
        assert_eq!(hit.entity_type, "Doc");
        assert_eq!(hit.title, "pay-design.md");
        assert!(hit.snippet.contains("ifCode"));
        assert_eq!(
            hit.source_ref.as_deref(),
            Some("dt://doc/offen-pay/pay-design.md")
        );
        // filter 硬条件：source="doc" + project + doc_id
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        let must = filter["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "source" && c["match"]["value"] == "doc"));
        assert!(must
            .iter()
            .any(|c| c["key"] == "project" && c["match"]["value"] == "offen-pay"));
        assert!(must.iter().any(|c| c["key"] == "doc_id"));
    }

    #[tokio::test]
    async fn doc_world_skips_nacos_shaped_points() {
        // nacos 配置点（config_sync 写入端）：无 text/doc_id/block_index → 解析丢弃
        let nacos = serde_json::json!({
            "id": "pt-9", "score": 0.9,
            "payload": {"entity_id": "x", "key": "k", "value": "v", "namespace": "public",
                         "source_type": "nacos_config", "project": "p"}
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![nacos],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "q".into(),
            world: Some("doc".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 0);
    }
}
