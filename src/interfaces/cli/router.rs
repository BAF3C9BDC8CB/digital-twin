//! CLI handler for `dt search` 命令 —— 统一检索入口（前身 `dt router`，
//! 已并入本命令，文件名为历史遗留）。
//!
//! 检索后端为 [`CrossWorldSearch`]，在此基础上叠加多层路由规则与可开关的结果过滤：
//!   第零层 LLM 门控 —— 搜索发起前由 LLM 判断查询是否值得检索（寒暄/闲聊/无检索目标
//!                       直接返回「无需检索」）；2026-09-06 起替代纯规则词表拦截
//!                       （闲聊词表/无锚点词表打地鼠式维护已整体移除）；
//!   第一层 意图识别   —— 分析查询，识别 code/knowledge/doc/config 意图；
//!   第二层 策略路由   —— 根据意图与 world 决定检索参数（世界/文件类型/实体类型/图跳数）；
//!   第三层 结果过滤   —— 复用现有 LLM 判断每个命中相关性，移除不相关项。
//!
//! L0 开关：`config/pipeline.yaml` 的 `kg_router.llm_gate.enabled`（默认开）；
//! 意图识别/策略路由/结果过滤沿用以下配置：
//! `kg_router.result_filter.enabled`（默认关）+ 命令行 `--filter <bool>` 覆盖，
//! 阈值 `kg_router.result_filter.threshold`（默认 0.6）命令行 `--threshold <f32>` 覆盖。

use crate::application::context::search_mcp::{
    CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
};
use crate::application::pipeline::config::PipelineConfig;
use crate::domain::error::DtError;
use crate::domain::traits::LlmService;

/// 处理 `dt search` —— 统一检索入口（智能路由搜索，融合原 dt router 能力）。
///
/// 自 dt search 统一后，router 吸收其全部能力：显式 `--file-type` /
/// `--content-type` / `--show-content` 过滤优先于意图推断，展示层共用。
pub async fn handle_router_search(
    query: &str,
    world: &str,
    limit: usize,
    json_output: bool,
    project: &Option<String>,
    enable_filter: Option<bool>, // --filter：Some(true) 强制开 / Some(false) 强制关 / None 跟随配置
    filter_threshold: f32,       // --threshold：覆盖配置阈值（<=0 表示沿用配置）
    explain: bool,               // --explain：打印路由决策过程
    file_type: Option<String>,   // --file-type：显式文件类型/后缀过滤（优先于意图推断）
    content_type: Option<String>, // --content-type：显式实体类型过滤（优先于意图推断）
    show_content: bool,          // --show-content：展开命中正文原文块
) -> Result<(), DtError> {
    // 读取配置开关
    let cfg = PipelineConfig::load().unwrap_or_default();
    let filter_cfg = cfg
        .kg_router
        .as_ref()
        .map(|r| r.result_filter.clone())
        .unwrap_or_default();

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

    // ---- 第零层：L0 LLM 门控（判断是否值得搜索）----
    // 2026-09-06 起：闲聊词表 / 无锚点词表的纯规则拦截（打地鼠式维护、误判率高）
    // 已整体移除，统一由 LLM 判断"是否要搜索知识图谱"——查询先问 LLM：
    // 值得检索 → 放行；不值得（寒暄/闲聊/纯任务指令无检索对象/与代码库无关）
    // → 直接返回「无需检索」。
    let gate_cfg = cfg
        .kg_router
        .as_ref()
        .map(|r| r.llm_gate.clone())
        .unwrap_or_default();

    if gate_cfg.enabled {
        let llm =
            crate::interfaces::cli::build::create_search_llm_client();
        match judge_search_with_llm(
            llm.as_ref(),
            query,
            gate_cfg.max_tokens,
            gate_cfg.temperature,
        )
        .await
        {
            Ok(true) => {
                if explain {
                    println!("=== 路由决策 ===");
                    println!("查询: {}", query);
                    println!("LLM 门控判定值得检索 → 放行");
                    println!();
                }
            }
            Ok(false) => {
                if explain {
                    println!("=== 路由决策 ===");
                    println!("查询: {}", query);
                    println!("LLM 门控判定无需检索（寒暄/闲聊/无检索目标）→ 直接返回");
                    println!();
                }
                if json_output {
                    println!(
                        "{}",
                        crate::interfaces::cli::search_render::render_json(
                            &crate::application::context::search_mcp::CrossWorldResult {
                                query: query.to_string(),
                                world: "none".to_string(),
                                hits: Vec::new(),
                                total: 0,
                                per_world_counts: Default::default(),
                                degraded: Vec::new(),
                            }
                        )
                    );
                } else {
                    println!(
                        "无需检索：该查询经 LLM 判定没有值得检索的对象（寒暄/闲聊/无检索目标）。"
                    );
                }
                return Ok(());
            }
            Err(e) => {
                // LLM 判断失败：降级放行（fail-open），避免门控故障阻塞正常检索。
                // 旧规则拦截场景下失败曾保守拦截，但规则层已移除，此处放行更稳。
                tracing::warn!("LLM 门控判断失败({e})，降级放行检索: {query}");
            }
        }
    }

    // ---- 第一层：hint 意图识别 ----
    let intent = analyze_query_intent(query);

    // ---- 第二层：路由决策（决定检索参数）----
    let route = build_route(
        &intent,
        world,
        use_filter,
        threshold,
        file_type.clone(),
        content_type.clone(),
    );

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

    let mut result = cws.search(&req).await?;

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
        let llm =
            crate::interfaces::cli::build::create_search_llm_client();
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
    // 直接修改 result，避免重建整个结构体（省 5 个字段的 clone）
    result.hits = final_hits;
    result.total = result.hits.len();

    if json_output {
        let out = crate::interfaces::cli::search_render::render_json(&result);
        println!("{}", out);
    } else {
        let resolver = crate::interfaces::cli::search_render::ProjectPathResolver::new(
            crate::interfaces::cli::build::project_roots_from_config(),
        );
        let out =
            crate::interfaces::cli::search_render::render_human(&result, show_content, &resolver);
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
        || q.contains(".yml")
        || q.contains(".yaml")
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

/// 根据意图 + world + 显式过滤生成路由决策。
fn build_route(
    intent: &QueryIntent,
    world: &str,
    enable_filter: bool,
    filter_threshold: f32,
    explicit_file_type: Option<String>,
    explicit_content_type: Option<String>,
) -> RoutePlan {
    // world 显式指定时优先尊重用户；否则用 `all`（跨世界检索，与 `dt search` 一致），
    // 避免凭意图擅自缩窄到单一世界导致结果不一致。
    let route_world = if world.is_empty() || world == "all" {
        "all".to_string()
    } else {
        world.to_string()
    };

    // 意图推断的默认过滤（world 缩窄时按意图给文件/实体类型 + 图跳数）。
    let (mut file_type, mut entity_type, max_hops) = if world.is_empty() || world == "all" {
        // 默认跨世界检索：不缩窄类型，保证与 `dt search` 结果一致，
        // 增强维度交给 LLM 过滤层（第三层）。
        (None, None, None)
    } else {
        match intent {
            QueryIntent::CodeSearch => (None, Some("Method".to_string()), None),
            QueryIntent::KnowledgeQuery => (None, None, Some(1)),
            QueryIntent::DocumentSearch => (Some("document".to_string()), None, None),
            // ConfigSearch 是"软"提示，不强制 file_type=config：含 "config" 关键字的查询
            // 往往是"处理配置的代码"（如 resolve_roots / ConfigPath），硬缩窄到 config 文件
            // 会把这些相关代码命中全部屏蔽，导致语义检索退化。配置文件本身会由向量检索
            // 依据语义自然浮现，无需在此硬性过滤。
            QueryIntent::ConfigSearch => (None, None, None),
            QueryIntent::HybridSearch => (None, None, None),
        }
    };
    // 用户显式 --file-type / --content-type 优先于意图推断（吸收 dt search 能力）。
    if explicit_file_type.is_some() {
        file_type = explicit_file_type;
    }
    if explicit_content_type.is_some() {
        entity_type = explicit_content_type;
    }

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
/// 三层安全策略，保证「高置信命中绝不被误删」且不串行卡死：
/// 1. 快速通道：score 高（>=0.9）或查询词与命中标题/签名/文件名精确命中 → 直接保留，不调 LLM。
/// 2. 歧义通道：仅 score 在阈值~0.9 之间的候选才逐条交给 LLM 判断相关性。
/// 3. 并行 + 超时：LLM 判断并发执行且单条带超时，LLM 失败/超时保守保留该条。
///
/// score 低于阈值的直接丢弃（与旧行为一致）。当未启用过滤或注入的 LLM 不可用时，返回原样。
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

    let query_tokens = normalize_query_tokens(query);

    // ---- 分桶：高置信直接保留，其余进歧义桶交给 LLM ----
    let mut keep: Vec<crate::application::context::search_mcp::SearchHit> = Vec::new();
    let mut ambiguous: Vec<crate::application::context::search_mcp::SearchHit> = Vec::new();
    for h in hits {
        let high_conf = h.score as f32 >= 0.9 || hit_exact_matches(&query_tokens, query, &h);
        if high_conf {
            tracing::debug!(
                "LLM 过滤直接保留(高置信): {} (score={:.2})",
                h.title,
                h.score
            );
            keep.push(h);
        } else if h.score as f32 >= threshold {
            ambiguous.push(h);
        } else if explain {
            tracing::info!(
                "LLM 过滤按 score 移除(低于阈值): {} (world={} score={:.2})",
                h.title,
                h.source_world,
                h.score
            );
        }
    }

    // ---- 并行评判歧义桶（单条带超时；失败/超时保守保留）----
    let judged: Vec<bool> = futures::future::join_all(
        ambiguous
            .iter()
            .map(|h| judge_relevance_guarded(llm, query, h)),
    )
    .await;

    for (i, h) in ambiguous.into_iter().enumerate() {
        if judged[i] {
            keep.push(h);
        } else if explain {
            tracing::info!(
                "LLM 过滤移除: {} (world={} score={:.2})",
                h.title,
                h.source_world,
                h.score
            );
        }
    }

    Ok(keep)
}

/// 归一化查询为词元：小写、按非字母数字/非 CJK 分隔，取长度>=2 的词。
fn normalize_query_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || is_cjk(c)))
        .map(|s| s.trim())
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_string())
        .collect()
}

/// 判断字符是否属于 CJK 统一表意文字区（`is_alphanumeric` 已覆盖大部分，此处兜底常用区）。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0xF900..=0xFAFF
    )
}

/// 高置信精确命中判定：查询**每个**词元都命中（强相关），或命中与查询共享足够多的词元
/// 且向量分不低（次强相关）。
///
/// 覆盖两类场景：
/// 1. 全词命中：查询所有词元都出现在命中标题/签名/实体类型/文件路径中。
/// 2. 部分但实质命中：≥2 个词元命中且向量 score ≥ 0.55 —— 这类通常是"同一功能但命名不同"
///    的代码（如 query="config root alias mapping" 命中 resolve_roots_*[root/alias]），
///    语义上是正确答案，交给 LLM 反而可能被误杀（LLM 对部分重叠倾向判不相关）。
///
/// `query` 原文同时作为兜底（词元为空时退化为原文子串匹配）。
fn hit_exact_matches(
    query_tokens: &[String],
    query: &str,
    h: &crate::application::context::search_mcp::SearchHit,
) -> bool {
    let hay = format!(
        "{} {} {} {}",
        h.title,
        h.signature.as_deref().unwrap_or(""),
        h.entity_type,
        h.file_path.as_deref().unwrap_or(""),
    )
    .to_lowercase();

    if !hay.is_empty() {
        // 1. 词元全部命中 → 强相关（无需看分）
        if !query_tokens.is_empty() && query_tokens.iter().all(|t| hay.contains(t)) {
            return true;
        }
        // 2. 部分命中且向量分不低：≥2 个非空词元命中 + score>=0.55 → 次强相关
        if !query_tokens.is_empty() {
            let hit_count = query_tokens.iter().filter(|t| hay.contains(*t)).count();
            if hit_count >= 2 && h.score as f32 >= 0.55 {
                return true;
            }
        }
        // 3. 空词元（纯 CJK 长串且切分不可用）→ 退化为原文子串匹配
        if query_tokens.is_empty()
            && !query.trim().is_empty()
            && hay.contains(&query.to_lowercase())
        {
            return true;
        }
    }
    false
}

/// 单条 LLM 相关性判断的守卫：加 20s 超时，失败/超时保守保留（返回 true）。
async fn judge_relevance_guarded(
    llm: &dyn LlmService,
    query: &str,
    h: &crate::application::context::search_mcp::SearchHit,
) -> bool {
    match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        judge_relevance(llm, query, h),
    )
    .await
    {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => {
            tracing::warn!("LLM 相关性判断失败({e})，保守保留该条: {}", h.title);
            true
        }
        Err(_) => {
            tracing::warn!("LLM 相关性判断超时，保守保留该条: {}", h.title);
            true
        }
    }
}

/// 组装单条搜索命中的展示上下文（供 LLM 相关性判断），按世界类型差异化：
/// code 命中带 文件路径/签名/所属类/LLM 分析；knowledge 带摘要/图关系；doc 带原文块；
/// 所有命中统一带 来源世界/项目/实体类型/文件类型/图距离。
fn format_hit_context(h: &crate::application::context::search_mcp::SearchHit) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("标题: {}", h.title));
    if let Some(p) = &h.project {
        parts.push(format!("项目: {}", p));
    }
    parts.push(format!("来源世界: {}", h.source_world));
    if !h.entity_type.is_empty() {
        parts.push(format!("实体类型: {}", h.entity_type));
    }
    if let Some(ft) = &h.file_type_label {
        parts.push(format!("文件类型: {}", ft));
    }
    if let Some(sig) = &h.signature {
        parts.push(format!("签名: {}", sig));
    }
    if let Some(fp) = &h.file_path {
        parts.push(format!("文件路径: {}", fp));
        if let (Some(sl), Some(el)) = (h.start_line, h.end_line) {
            parts.push(format!("行号: L{}-{}", sl, el));
        }
    }
    if let Some(sr) = &h.source_ref {
        parts.push(format!("来源: {}", sr));
    }
    // code 世界：llm_analysis（用途/逻辑）是方法级别的实质内容
    if let Some(la) = &h.llm_analysis {
        if !la.trim().is_empty() {
            parts.push(format!("方法分析: {}", la));
        }
    }
    // knowledge 世界：snippet 即摘要（可能已含 evidence 的引用）
    if !h.snippet.is_empty() && h.source_world == "knowledge" {
        parts.push(format!("摘要: {}", h.snippet));
    }
    // doc 世界：content 为原文块（比 snippet 更完整）；无 content 用 snippet
    if h.source_world == "doc" {
        let body = h
            .content
            .clone()
            .or_else(|| Some(h.snippet.clone()))
            .unwrap_or_default();
        if !body.is_empty() {
            parts.push(format!("文档内容: {}", body));
        }
    }
    // 图关系（code CONTAINS/CALLS，knowledge RELATES）辅助判断所属上下文
    if let Some(rels) = &h.relations {
        let names: Vec<String> = rels
            .iter()
            .filter_map(|r| {
                let n = r.other_end_name.trim();
                if n.is_empty() {
                    None
                } else {
                    Some(format!("{} {}", r.rel_type, n))
                }
            })
            .collect();
        if !names.is_empty() {
            parts.push(format!("图关系: {}", names.join("; ")));
        }
    }
    if let Some(hop) = h.hop {
        parts.push(format!("图距离(hop): {}", hop));
    }
    parts.join("\n")
}

/// 让 LLM 判断单个命中是否与查询相关。
///
/// 提示词包含三层规则：
/// 1. 「检索锚点判定」——查询若只由通用动词/寒暄/无具体对象构成（如"帮我实现"、
///    "给我一些建议"、"介绍一下"），则任何命中都不该被检索到 → 判「不相关」，
///    避免过度检索的噪声结果进入上下文。
/// 2. 「code 命中」必须看方法签名/所属类/文件路径，仅在查询明确涉及该文件/方法时相关；
///    只撞上方法名（如 help / execute / recommend 等通用动词名）不算相关。
/// 3. 「doc/knowledge 命中」看摘要/原文与查询的具体对象（业务实体/配置项/文件）一致。
async fn judge_relevance(
    llm: &dyn LlmService,
    query: &str,
    h: &crate::application::context::search_mcp::SearchHit,
) -> Result<bool, DtError> {
    let system_prompt = r#"你是搜索结果相关性评估专家。根据用户查询和单条搜索结果，判断它是否真正相关。

判定规则：
1. 查询若只含通用意图词（如"帮我实现"、"给我建议"、"介绍一下"、"分析一下"），不含任何具体对象（代码符号、文件名、配置项、业务名、报错等），则任何命中都是多余检索 → 判 relevant=false。
2. 代码类命中（Method/Class）：查询只要涉及该文件/方法/类所在的项目、路径、文件、业务领域，或方法名与查询关键词一致，即判 relevant=true；仅撞上方法名（如 help / execute / recommend 等通用动词名）但业务无关 → relevant=false。
3. 文档/知识类命中：摘要/原文与查询对象（含业务概念、配置项、技术术语）相关即判 relevant=true；仅字符串表面相同但语义无关时判 relevant=false。
4. 高置信命中（相关度分数≥0.90，或命中标题/签名与查询关键词完全一致）→ 必判 relevant=true，除非命中明显属于另一个不相关项目。

只输出 JSON，不要任何解释，格式：
{"relevant": true或false, "reason": "15字以内理由"}"#;

    let user_prompt = format!(
        "用户查询：{}\n\n单条搜索结果（相关度分数 {:.2}）：\n{}\n\n请判断。",
        query,
        h.score,
        format_hit_context(h)
    );

    let resp = match llm.chat(system_prompt, &user_prompt, 0.0, 200).await {
        Ok(r) => r,
        Err(e) => {
            // 个别 LLM 判断失败：保守保留该条结果，不因暂态错误删除有效命中。
            tracing::warn!("LLM 相关性判断失败({e})，保守保留该条: {} ", h.title);
            return Ok(true);
        }
    };

    // 解析判定：优先 JSON（{"relevant": bool}），失败时降级旧文本格式解析。
    if let Some(b) = parse_relevance_json(&resp) {
        return Ok(b);
    }

    // 降级：文本格式解析（兼容旧模型/输出不稳定场景）。
    parse_relevance_text(&resp)
}

/// 从 LLM 相关性判断输出中解析 JSON `{"relevant": bool}`。
/// 返回 None 表示无法解析（调用方降级文本解析）。容忍 ```json 围栏与前后杂讯。
/// 从相关性过滤 LLM 输出中解析 JSON `{"relevant": bool}`。
/// 返回 None 表示无法解析（调用方降级到文本解析）。容忍 ```json 围栏与前后杂讯。
fn parse_relevance_json(resp: &str) -> Option<bool> {
    #[derive(serde::Deserialize)]
    struct Response {
        relevant: bool,
    }

    let parsed: Response = crate::shared::llm_parse::parse_llm_json(resp)?;
    Some(parsed.relevant)
}

/// 旧文本格式降级解析：响应中出现"不相关"则 false，否则视为相关（保守保留）。
fn parse_relevance_text(resp: &str) -> Result<bool, DtError> {
    let norm = resp.trim();
    // 1. 明确以"不相关"开头 → 不相关，移除。
    if norm.starts_with("不相关") {
        return Ok(false);
    }
    // 2. 同时出现两个词（模型展开解释/带条件），保守保留。
    if norm.contains("相关") && norm.contains("不相关") {
        return Ok(true);
    }
    // 3. 其余位置明确出现"不相关" → 移除。
    if norm.contains("不相关") {
        return Ok(false);
    }
    // 4. 包含"相关"→ 保留
    Ok(norm.contains("相关"))
}

/// LLM 门控：判断查询是否值得发起检索。
///
/// 输出严格 JSON `{"search": true/false, "reason": "..."}`。
/// 2026-09-06 起作为 **L0 唯一判断**（纯规则词表拦截已移除）：每次搜索发起前
/// 先问 LLM——LLM 说该搜才放行，说不该搜（寒暄/闲聊/无检索目标）直接拦截。
///
/// 返回：
/// - `Ok(true)` — LLM 判定值得搜索（放行）；
/// - `Ok(false)` — LLM 判定无需搜索（拦截）；
/// - `Err` — LLM 调用/解析失败（调用方决定降级策略）。
async fn judge_search_with_llm(
    llm: &dyn LlmService,
    query: &str,
    max_tokens: u32,
    temperature: f32,
) -> Result<bool, DtError> {
    let system_prompt = r#"你是代码库检索助手的前置闸门。用户发来一句话, 你要判断: 这句话是否值得触发一次代码/文档/知识库检索?

判定规则:
1. search=true (值得检索): 查询包含任何具体可检索对象 —— 代码符号(类名/方法名/变量名)、文件名或路径、配置项、业务概念(支付/订单/服务/接口/幂等)、技术术语、报错信息、或明确指向"某个东西在哪/怎么用/为什么/是什么"的检索意图。
2. search=false (不值得检索): 纯寒暄/问候/道谢/闲聊(你好/谢谢/在吗/天气不错)、纯任务指令且无检索对象(帮我实现/给我建议/介绍一下有哪些模块——除非提到了具体模块名)、纯算术、与代码库无关的话题。
3. 拿不准时倾向 search=true(宁可多搜一次, 不要漏掉真实需求)。

只输出 JSON, 不要任何解释, 格式:
{"search": true或false, "reason": "10字以内理由"}"#;

    let user_prompt = format!("用户说: {}\n\n请判断。", query);

    let resp = match llm
        .chat(system_prompt, &user_prompt, temperature, max_tokens)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("LLM 门控调用失败({e})");
            return Err(e.into());
        }
    };

    // 解析 JSON：容忍 ```json 围栏与前后杂讯。
    match parse_gate_json(&resp) {
        Some(b) => Ok(b),
        None => {
            tracing::warn!("LLM 门控输出无法解析或缺 search 字段: {resp}");
            Err(DtError::General(format!(
                "LLM 门控输出无法解析: {}",
                resp.chars().take(120).collect::<String>()
            )))
        }
    }
}

/// 从 LLM 门控输出中解析 JSON `{"search": bool}`。
/// 返回 None 表示无法解析（调用方按失败处理）。容忍 ```json 围栏与前后杂讯。
fn parse_gate_json(resp: &str) -> Option<bool> {
    #[derive(serde::Deserialize)]
    struct Response {
        search: bool,
    }

    let parsed: Response = crate::shared::llm_parse::parse_llm_json(resp)?;
    Some(parsed.search)
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_query_intent, parse_gate_json, parse_relevance_json, parse_relevance_text,
        QueryIntent,
    };

    #[test]
    fn analyze_intent_still_matches() {
        assert_eq!(
            analyze_query_intent("MemgraphClient::new"),
            QueryIntent::CodeSearch
        );
        assert_eq!(
            analyze_query_intent("如何实现支付"),
            QueryIntent::KnowledgeQuery
        );
        assert_eq!(
            analyze_query_intent("配置文件在哪"),
            QueryIntent::ConfigSearch
        );
        assert_eq!(
            analyze_query_intent("查找某个文档"),
            QueryIntent::DocumentSearch
        );
    }

    #[test]
    fn config_intent_does_not_force_file_type_filter() {
        // 回归：含 "config" 关键字的查询（如"parse config roots into aliases"）判为
        // ConfigSearch，但 ConfigSearch 是"软"提示——不得再强制 file_type=config，
        // 否则会屏蔽掉真正相关的代码命中（resolve_roots 是 .rs 方法而非 config 文件）。
        // 修复前 ConfigSearch => (Some("config"), None, None)，导致该查询在 code 世界 0 命中。
        use crate::interfaces::cli::router::{build_route, QueryIntent};
        let q = "parse config roots into aliases";
        let intent = analyze_query_intent(q);
        assert_eq!(intent, QueryIntent::ConfigSearch);
        // world=code 显式时，ConfigSearch 不应产生 file_type=config 的硬过滤
        let route = build_route(&intent, "code", false, 0.6, None, None);
        assert_eq!(
            route.file_type, None,
            "ConfigSearch 不得强制 file_type=config"
        );
        assert_eq!(route.world, "code");
    }

    #[test]
    fn pure_keyword_config_still_detected_as_config_intent() {
        // 纯配置关键词仍应保留 ConfigSearch 意图（不影响意图识别，只去掉硬过滤）
        assert_eq!(
            analyze_query_intent("nacos 配置中心怎么连接"),
            QueryIntent::ConfigSearch
        );
        assert_eq!(
            analyze_query_intent("datasource"),
            QueryIntent::ConfigSearch
        );
        assert_eq!(
            analyze_query_intent("application.yml"),
            QueryIntent::ConfigSearch
        );
    }

    #[test]
    fn hit_exact_matches_keeps_partial_token_overlap() {
        // 回归：查询 "config root alias mapping"，命中 resolve_roots_* 的标题含 root/alias
        // 但非全部词元 —— 这种"同功能不同命名"的部分重叠命中应被视为高置信保留，
        // 不交给 LLM（LLM 对部分重叠倾向误判不相关，导致默认 filter 下 0 命中）。
        use crate::application::context::search_mcp::SearchHit;
        let tokens =
            crate::interfaces::cli::router::normalize_query_tokens("config root alias mapping");
        let hit = SearchHit {
            id: "1".into(),
            title: "resolve_roots_single_mapping_alias_to_path".into(),
            snippet: String::new(),
            content: None,
            source_world: "code".into(),
            entity_type: "Method".into(),
            file_type: None,
            file_type_label: None,
            score: 0.591,
            source_ref: None,
            metadata: None,
            file_path: Some("src/runtime.rs".into()),
            project: Some("digital-twin-v2".into()),
            start_line: None,
            end_line: None,
            signature: Some("fn resolve_roots_single_mapping_alias_to_path() -> ".into()),
            llm_analysis: None,
            calls: Vec::new(),
            element_id: None,
            score_breakdown: None,
            hop: None,
            via_same_as: None,
            relations: None,
            evidence: None,
            rerank_degraded: None,
        };
        // 该命中含 root/alias/mapping 共3个词元 >= 2 且 score 0.59>=0.55 → 应判定高置信
        assert!(crate::interfaces::cli::router::hit_exact_matches(
            &tokens,
            "config root alias mapping",
            &hit
        ));
    }

    #[test]
    fn hit_exact_matches_requires_min_score_for_partial() {
        // 部分重叠但向量分过低(如 0.02 噪音) → 不应算高置信，交给 LLM 判定
        use crate::application::context::search_mcp::SearchHit;
        let tokens = crate::interfaces::cli::router::normalize_query_tokens("foo bar baz");
        let hit = SearchHit {
            id: "1".into(),
            title: "some_unrelated_thing".into(),
            snippet: String::new(),
            content: None,
            source_world: "code".into(),
            entity_type: "Method".into(),
            file_type: None,
            file_type_label: None,
            score: 0.02,
            source_ref: None,
            metadata: None,
            file_path: None,
            project: Some("p".into()),
            start_line: None,
            end_line: None,
            signature: None,
            llm_analysis: None,
            calls: Vec::new(),
            element_id: None,
            score_breakdown: None,
            hop: None,
            via_same_as: None,
            relations: None,
            evidence: None,
            rerank_degraded: None,
        };
        // 标题不含 foo/bar/baz → 不应命中
        assert!(!crate::interfaces::cli::router::hit_exact_matches(
            &tokens,
            "foo bar baz",
            &hit
        ));
    }

    #[test]
    fn parse_gate_json_accepts_clean_and_fenced() {
        // 门控 JSON 解析：干净输出 / ```json 围栏 / 前后杂讯都能解析
        assert_eq!(
            parse_gate_json(r#"{"search": true, "reason": "类名"}"#),
            Some(true)
        );
        assert_eq!(
            parse_gate_json(r#"{"search": false, "reason": "寒暄"}"#),
            Some(false)
        );
        assert_eq!(
            parse_gate_json("```json\n{\"search\": true, \"reason\": \"x\"}\n```"),
            Some(true)
        );
        assert_eq!(
            parse_gate_json("好的，判断如下：{\"search\": false, \"reason\": \"闲聊\"} 完毕"),
            Some(false)
        );
        // 无法解析 → None
        assert_eq!(parse_gate_json("相关"), None);
        assert_eq!(parse_gate_json(""), None);
        assert_eq!(parse_gate_json("{\"wrong_field\": true}"), None);
    }

    #[test]
    fn parse_relevance_json_preferred_over_text() {
        // JSON 判定优先
        assert_eq!(
            parse_relevance_json(r#"{"relevant": true, "reason": "业务相关"}"#),
            Some(true)
        );
        assert_eq!(
            parse_relevance_json("```json\n{\"relevant\": false}\n```"),
            Some(false)
        );
        // 无 JSON 时文本降级
        assert_eq!(parse_relevance_text("相关").unwrap(), true);
        assert_eq!(parse_relevance_text("不相关").unwrap(), false);
        // 明确以"不相关"开头 → false（必须先于子串检查，否则"不相关"含"相关"被保守保留）
        assert_eq!(parse_relevance_text("不相关，与查询无关").unwrap(), false);
        // 非开头位置的"不相关"：子串"相关"同现 → 保守保留 true（既有降级行为）
        assert_eq!(parse_relevance_text("该结果不相关").unwrap(), true);
        // 同时含相关/不相关 → 保守保留 true
        assert_eq!(parse_relevance_text("可能相关也可能不相关").unwrap(), true);
        // JSON 失败场景回退文本
        assert_eq!(parse_relevance_json("不相关"), None);
        let fallback = if let Some(_) = parse_relevance_json("不相关") {
            true
        } else {
            parse_relevance_text("不相关").unwrap()
        };
        assert_eq!(fallback, false);
    }
}
