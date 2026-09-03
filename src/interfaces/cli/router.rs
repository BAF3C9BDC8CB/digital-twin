//! CLI handler for `dt router` command —— 智能路由搜索（`dt search` 的升级版）。
//!
//! 与 `dt search` 共享同一个检索后端 [`CrossWorldSearch`]，保证命中结果完全一致；
//! 在此之上叠加多层路由规则与可开关的结果过滤：
//!   第一层 意图识别   —— 分析查询，识别 code/knowledge/doc/config 意图；
//!   第二层 策略路由   —— 根据意图与 world 决定检索参数（世界/文件类型/实体类型/图跳数）；
//!   第三层 结果过滤   —— 复用现有 LLM（kg_router 已接入）判断每个命中相关性，移除不相关项。
//!
//! 过滤开关：`config/pipeline.yaml` 的 `kg_router.result_filter.enabled`（默认开），
//! 命令行 `--filter <bool>` 可覆盖；阈值 `kg_router.result_filter.threshold`（默认 0.6），
//! 命令行 `--threshold <f32>` 可覆盖。

use crate::application::context::search_mcp::{
    CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
};
use crate::application::pipeline::config::PipelineConfig;
use crate::domain::error::DtError;
use crate::domain::traits::LlmService;
use std::sync::Arc;

/// 处理 `dt router` —— 智能路由搜索。
pub async fn handle_router_search(
    query: &str,
    world: &str,
    limit: usize,
    json_output: bool,
    project: &Option<String>,
    enable_filter: Option<bool>, // --filter：Some(true) 强制开 / Some(false) 强制关 / None 跟随配置
    filter_threshold: f32,       // --threshold：覆盖配置阈值（<=0 表示沿用配置）
    explain: bool,               // --explain：打印路由决策过程
) -> Result<(), DtError> {
    // 读取配置开关
    let cfg = PipelineConfig::load().unwrap_or_default();
    let filter_cfg = cfg.kg_router.map(|r| r.result_filter).unwrap_or_default();

    // 生效的过滤开关：命令行 `--filter` 优先，其次配置。
    // 语义：Some(true)→强制开；Some(false)→强制关；None→跟随配置开关。
    let use_filter = match enable_filter {
        Some(v) => v,
        None => filter_cfg.enabled,
    };

    let threshold = if filter_threshold > 0.0 {
        filter_threshold
    } else {
        filter_cfg.threshold
    };

    // ---- 第一层：hint 意图识别 ----
    let intent = analyze_query_intent(query);

    // ---- 第二层：路由决策（决定检索参数）----
    let route = build_route(&intent, world, use_filter, threshold);

    if explain {
        println!("=== 路由决策 ===");
        println!("查询: {}", query);
        println!("意图: {:?}", intent);
        println!("世界: {}", route.world);
        println!("过滤: {}", if use_filter { "启用" } else { "禁用" });
        println!("阈值: {:.2}", threshold);
        println!(
            "检索参数: 世界={} 文件类型={:?} 实体类型={:?} 图跳数={:?}",
            route.world, route.file_type, route.entity_type, route.max_hops
        );
        println!();
    }

    // ---- 检索：复用 dt search 的 CrossWorldSearch 后端（结果与 dt search 一致）----
    let graph = crate::runtime::connect_graph().await;
    let vector = crate::runtime::connect_vector().await;

    let cws = CrossWorldSearch::new(
        graph,
        vector,
        Some(crate::interfaces::cli::build::create_search_embed_client()),
        Some(crate::interfaces::cli::build::create_search_rerank_client()),
    );

    let req = SearchRequest {
        query: query.to_string(),
        world: Some(route.world.clone()),
        limit: Some(route.query_limit(limit)),
        project: project.clone(),
        max_hops: route.max_hops,
        with_evidence: if route.world == "knowledge" {
            Some(true)
        } else {
            None
        },
        origin: None,
        doc_id: None,
        file_type: route.file_type,
        entity_type_filter: route.entity_type.clone(),
    };

    let result = cws.search(&req).await?;

    if explain {
        println!(
            "原始结果数: {}（各世界 {:?}）",
            result.total, result.per_world_counts
        );
        if !result.degraded.is_empty() {
            println!("降级标记: {:?}", result.degraded);
        }
        println!();
    }

    let mut final_hits = result.hits;

    // ---- 第三层：LLM 结果过滤（复用现有 LLM，可开关）----
    let mut removed_count = 0usize;
    if use_filter {
        let llm = crate::infrastructure::embedder::create_llm_router(
            crate::interfaces::cli::build::provider_config_from_pipeline(),
        );
        let pre_len = final_hits.len();
        let filtered =
            filter_hits_with_llm(query, final_hits, threshold, llm.as_ref(), explain).await?;
        removed_count = pre_len.saturating_sub(filtered.len());
        final_hits = filtered;
    }

    final_hits.truncate(limit);

    if explain && removed_count > 0 {
        println!("过滤移除: {} 条结果", removed_count);
        println!();
    }

    // ---- 输出（复用 dt search 渲染，展示一致）----
    let total = final_hits.len();
    let final_result = crate::application::context::search_mcp::CrossWorldResult {
        query: result.query.clone(),
        world: result.world.clone(),
        hits: final_hits,
        total,
        per_world_counts: result.per_world_counts.clone(),
        degraded: result.degraded.clone(),
    };

    if json_output {
        let out = crate::interfaces::cli::search_render::render_json(&final_result);
        println!("{}", out);
    } else {
        let resolver = crate::interfaces::cli::search_render::ProjectPathResolver::new(
            crate::interfaces::cli::build::project_roots_from_config(),
        );
        let out =
            crate::interfaces::cli::search_render::render_human(&final_result, false, &resolver);
        print!("{}", out);
    }

    Ok(())
}

/// 查询意图类型。
#[derive(Debug, Clone, PartialEq, Eq)]
enum QueryIntent {
    CodeSearch,     // 代码查询（符号、API、方法名）
    KnowledgeQuery, // 知识查询（如何做、为什么）
    DocumentSearch, // 文档查询（查找特定文档）
    ConfigSearch,   // 配置查询（查找配置项）
    HybridSearch,   // 混合/默认查询
}

/// 路由决策产物。
struct RoutePlan {
    world: String,               // 检索世界
    file_type: Option<String>,   // 文件类型过滤
    entity_type: Option<String>, // 实体类型过滤
    max_hops: Option<u32>,       // 图扩展跳数
    enable_filter: bool,
    filter_threshold: f32,
}

impl RoutePlan {
    /// 需要多拉取的结果数（过滤后可能裁减），留 2 倍余量。
    fn query_limit(&self, limit: usize) -> usize {
        if self.enable_filter {
            limit.saturating_mul(2).max(limit)
        } else {
            limit
        }
    }
}

/// 分析查询意图。
fn analyze_query_intent(query: &str) -> QueryIntent {
    let q = query.to_lowercase();

    // 代码查询特征：`::`、`(`、`fn `、代码文件后缀
    if query.contains("::")
        || query.contains('(')
        || query.contains("fn ")
        || query.contains(".rs")
        || query.contains(".py")
        || q.starts_with("fn ")
    {
        return QueryIntent::CodeSearch;
    }

    // 配置查询特征
    if q.contains("配置")
        || q.contains("config")
        || q.contains("yaml")
        || q.contains("nacos")
        || q.contains("数据源")
        || q.contains("datasource")
    {
        return QueryIntent::ConfigSearch;
    }

    // 知识查询特征：如何/怎么/为什么/how/why
    if q.contains("如何")
        || q.contains("怎么")
        || q.contains("为什么")
        || q.contains("how to")
        || q.contains("why")
        || q.starts_with("如何")
        || q.starts_with("怎么")
    {
        return QueryIntent::KnowledgeQuery;
    }

    // 文档查询特征
    if q.contains("文档")
        || q.contains("doc")
        || q.contains(".md")
        || q.contains("readme")
        || q.contains("手册")
    {
        return QueryIntent::DocumentSearch;
    }

    QueryIntent::HybridSearch
}

/// 根据意图 + world 生成路由决策。
fn build_route(
    intent: &QueryIntent,
    world: &str,
    enable_filter: bool,
    filter_threshold: f32,
) -> RoutePlan {
    // world 显式指定时优先尊重用户；否则用 `all`（跨世界检索，与 `dt search` 一致），
    // 避免凭意图擅自缩窄到单一世界导致结果不一致。
    let route_world = if world.is_empty() || world == "all" {
        "all".to_string()
    } else {
        world.to_string()
    };

    let (file_type, entity_type, max_hops) = if world.is_empty() || world == "all" {
        // 默认跨世界检索：不缩窄类型，保证与 `dt search` 结果一致，
        // 增强维度交给 LLM 过滤层（第三层）。
        (None, None, None)
    } else {
        match intent {
            QueryIntent::CodeSearch => (None, Some("Method".to_string()), None),
            QueryIntent::KnowledgeQuery => (None, None, Some(1)),
            QueryIntent::DocumentSearch => (Some("document".to_string()), None, None),
            QueryIntent::ConfigSearch => (Some("config".to_string()), None, None),
            QueryIntent::HybridSearch => (None, None, None),
        }
    };

    RoutePlan {
        world: route_world,
        file_type,
        entity_type,
        max_hops,
        enable_filter,
        filter_threshold,
    }
}

/// 使用 LLM 过滤结果（复用现有 LLM 接入）。
///
/// 逐条让 LLM 判断相关性，低于阈值移除；LLM 判断失败时按向量 score 降级过滤，
/// 保证路由始终可用。当未启用过滤或注入的 LLM 不可用时，返回原样。
async fn filter_hits_with_llm(
    query: &str,
    hits: Vec<crate::application::context::search_mcp::SearchHit>,
    threshold: f32,
    llm: &dyn LlmService,
    explain: bool,
) -> Result<Vec<crate::application::context::search_mcp::SearchHit>, DtError> {
    if hits.is_empty() {
        return Ok(hits);
    }

    // 探测 LLM 可用性
    match llm.health_check().await {
        Ok(_) => {}
        Err(_) => {
            tracing::warn!("LLM 不可用，按向量 score 降级过滤");
            return Ok(hits
                .into_iter()
                .filter(|h| h.score as f32 >= threshold)
                .collect());
        }
    }

    let mut filtered = Vec::new();
    for h in hits {
        let relevance = judge_relevance(llm, query, &h.title, &h.snippet).await?;
        if relevance {
            filtered.push(h);
        } else if explain {
            tracing::info!("LLM 过滤移除: {} (score={:.2})", h.title, h.score);
        }
    }
    Ok(filtered)
}

/// 让 LLM 判断单个命中是否与查询相关。
async fn judge_relevance(
    llm: &dyn LlmService,
    query: &str,
    title: &str,
    snippet: &str,
) -> Result<bool, DtError> {
    let system_prompt = r#"你是搜索结果相关性评估专家。根据用户查询和单条搜索结果，判断它是否真正相关。
只用一行输出，格式严格为：
相关 / 不相关
- "相关"：结果内容能直接回答或有力支撑查询
- "不相关"：结果与查询无关，或仅有表面关键词重合"#;

    let user_prompt = format!(
        "查询：{}\n\n结果标题：{}\n结果摘要：{}",
        query, title, snippet
    );

    let resp = match llm.chat(system_prompt, &user_prompt, 0.0, 200).await {
        Ok(r) => r,
        Err(e) => {
            // 个别 LLM 判断失败：保守保留该条结果，不因暂态错误删除有效命中。
            tracing::warn!("LLM 相关性判断失败({e})，保守保留该条: {} ", title);
            return Ok(true);
        }
    };

    // 解析判定：将响应规范化，仅当明确输出“不相关”时才移除。
    // 响应可能带前后空格/标点/追问，这里取首个判定词。
    let norm = resp.trim();

    // 若同时出现两个词（模型展开解释），保守保留——避免误删真实相关结果。
    if norm.contains("相关") && norm.contains("不相关") {
        return Ok(true);
    }

    // 明确以“不相关”开口 → 判定不相关，移除。
    if norm.starts_with("不相关") {
        return Ok(false);
    }

    // 包含“相关”→ 保留
    Ok(norm.contains("相关"))
}
