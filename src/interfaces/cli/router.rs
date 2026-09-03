//! CLI handler for `dt router` command —— 智能路由搜索（`dt search` 的升级版）。
//!
//! 与 `dt search` 共享同一个检索后端 [`CrossWorldSearch`]，保证命中结果完全一致；
//! 在此之上叠加多层路由规则与可开关的结果过滤：
//!   第零层 提前拦截（L0 gate）—— 纯规则判断查询是否值得检索，寒暄/算术/闲聊直接返回
//!                              「无需检索」，省掉一次 KG 搜索；
//!   第一层 意图识别   —— 分析查询，识别 code/knowledge/doc/config 意图；
//!   第二层 策略路由   —— 根据意图与 world 决定检索参数（世界/文件类型/实体类型/图跳数）；
//!   第三层 结果过滤   —— 复用现有 LLM（kg_router 已接入）判断每个命中相关性，移除不相关项。
//!
//! L0 开关：`config/pipeline.yaml` 的 `kg_router.early_exit.enabled`（默认开）；
//! 意图识别/策略路由/结果过滤沿用以下配置：
//! `kg_router.result_filter.enabled`（默认关）+ 命令行 `--filter <bool>` 覆盖，
//! 阈值 `kg_router.result_filter.threshold`（默认 0.6）命令行 `--threshold <f32>` 覆盖。

use crate::application::context::search_mcp::{
    CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
};
use crate::application::pipeline::config::PipelineConfig;
use crate::domain::error::DtError;
use crate::domain::traits::LlmService;
use std::path::PathBuf;
use std::sync::OnceLock;

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

    // ---- 第零层：L0 提前拦截（early-exit）----
    // 纯规则判断查询是否值得检索：命中实体/意图特征放行，寒暄/算术/闲聊直接返回。
    let early_exit = cfg
        .kg_router
        .as_ref()
        .map(|r| r.early_exit.clone())
        .unwrap_or_default();

    if early_exit.enabled && !should_search(query) {
        if explain {
            println!("=== 路由决策 ===");
            println!("查询: {}", query);
            println!("L0 拦截: 无需检索（纯闲聊/寒暄/算术，未命中实体或意图特征）");
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
            println!("无需检索：该查询未命中项目/服务/类名等实体或检索意图特征。");
        }
        return Ok(());
    }

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

/// L0 提前拦截：判断查询是否值得检索。
///
/// 采用**单一贪心分词分类器**（无子串白名单）：只有当查询能用 [`CASUAL_VOCAB`]
/// 从头到尾完整切分（每个词都是闲聊/寒暄/天气/算术/应答词）时才判断为无需检索；
/// 只要带进任一技术词（类名/服务名/配置/api/接口/方法…，都不在闲聊词表里）
/// 即无法完整切分 → 放行检索。
///
/// 这样天然泛化、不依赖固定短语："天气怎么样/天气咋样/今天多少度/how is the weather"
/// 都能被完整切分而拦截；而"支付超时怎么配置/天气api接口/幂等逻辑在哪"因含技术词被放行。
/// 对应文档「三级 Router」的 L0 Gate 层。
fn should_search(query: &str) -> bool {
    // 统一拦闲聊：能完整切分 → 无需检索；否则 → 检索。
    // 词表 = 外部 `casual-words.txt` 与内置 [`CASUAL_VOCAB`] 的并集（外部只增不减）。
    !is_casual_query(query, casual_vocab_external().as_slice())
}

/// 闲聊词表（内容级，非短语级）。
///
/// 只要查询的每个内容 token 都落在本词表里，即认为属于寒暄/天气/状态等闲聊。
/// 因为是"逐 token 判断 + 全量覆盖"，任何同义改写（天气不错/天气咋样/今天多少度）都会被检出，
/// 而真实领域查询只要带进一个非闲聊词（类名/服务名/配置/幂等/接口…）就不会被误拦。
const CASUAL_VOCAB: &[&str] = &[
    // 寒暄/应答
    "你好",
    "您好",
    "嗨",
    "哈喽",
    "嗨喽",
    "在吗",
    "在不在",
    "忙不忙",
    "谢谢",
    "感谢",
    "辛苦",
    "不客气",
    "再见",
    "拜拜",
    "回见",
    "先这样",
    "那好",
    "好的",
    "收到",
    "嗯",
    "哦",
    "噢",
    "啊",
    "唉",
    "哈",
    "呵呵",
    "行",
    "好",
    "嗯嗯",
    // 英文寒暄
    "hello",
    "hi",
    "hey",
    "thanks",
    "thank",
    "thx",
    "ok",
    "okay",
    "bye",
    "goodbye",
    "great",
    "nice",
    "good",
    "fine",
    "yes",
    "no",
    "yeah",
    "yep",
    "nope",
    "sure",
    "right",
    "well",
    "so",
    "weather",
    "hot",
    "cold",
    "rain",
    "sunny",
    "rainy",
    "cloudy",
    "temperature",
    // 英文疑问词/冠词/介词
    "how",
    "is",
    "are",
    "the",
    "a",
    "an",
    "what",
    "where",
    "when",
    "who",
    "which",
    "why",
    "do",
    "does",
    "did",
    "it",
    "its",
    "you",
    "your",
    "me",
    "my",
    "and",
    "or",
    "to",
    "in",
    "on",
    "at",
    "for",
    "of",
    "please",
    // 代词/量词/副词填充
    "你",
    "您",
    "我",
    "他",
    "她",
    "它",
    "我们",
    "你们",
    "他们",
    "她们",
    "这",
    "那",
    "吗",
    "呢",
    "吧",
    "呀",
    "嘛",
    "了",
    "的",
    "着",
    "过",
    "是",
    "在",
    "有",
    // 时间/程度
    "今天",
    "昨天",
    "明天",
    "早上",
    "晚上",
    "下午",
    "中午",
    "现在",
    "刚才",
    "最近",
    "几点",
    "几",
    "几点钟",
    "很",
    "挺",
    "太",
    "真",
    "非常",
    "比较",
    "死",
    "怎么样",
    "咋样",
    "咋",
    "为啥",
    "咋了",
    "如何",
    "好不好",
    "好不好呢",
    "还能行",
    "怎么了",
    "为什么",
    "怎么办",
    "怎么回事",
    "什么",
    "怎么",
    "怎么样呢",
    "何时",
    "帮",
    "帮我",
    "帮忙",
    "麻烦",
    "请问",
    "打扰",
    "请教",
    "一下",
    // 日常寒暄（做了什么/吃没吃/忙不忙/睡没睡）
    "在干嘛",
    "干",
    "啥",
    "吃",
    "没",
    "空",
    "忙",
    "睡",
    "醒",
    "忙活",
    "来",
    "来了",
    "太阳",
    "出太阳",
    // 天气/气象话题
    "天气",
    "天",
    "气温",
    "外面",
    "外边",
    "室外",
    "室内",
    "温度",
    "冷",
    "热",
    "暖和",
    "凉快",
    "下雨",
    "下雪",
    "刮风",
    "晴",
    "阴",
    "雾",
    "霾",
    "雷",
    "闪电",
    "潮湿",
    "干燥",
    "多云",
    "晴朗",
    "降温",
    "升温",
    "台风",
    "地震",
    "暴雨",
    "中到大雨",
    "多少度",
    "几度",
    "冷不冷",
    "热不热",
    // 天气评价（"换说法"同族）
    "不错",
    "还行",
    "还好",
    "挺好的",
    "挺好",
    "一般",
    "糟糕",
    // 纯算术
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "0",
    "+",
    "-",
    "×",
    "÷",
    "等于",
    "算",
    "算一下",
    "计算",
    "合计",
    "一",
    "二",
    "三",
    "四",
    "五",
    "六",
    "七",
    "八",
    "九",
    "十",
    "百",
    "千",
    "万",
    "亿",
    "一百",
    "二百",
    "三百",
    "千万",
    "乘",
    "乘以",
    // 状态/通用
    "没事",
    "没什么",
    "就这样",
    "算了",
    "随便",
    "聊聊",
    "梳理",
    "说下",
    "说一下",
];

/// 闲聊分类：采用**贪心最长前缀匹配**，判断查询能否用给定词表 [`vocab`] 完整切分。
///
/// 对查询从左往右反复取"最长的、能作为词表前缀"的词条消费；若剩余文本无法被
/// 任何词条匹配，则存在非闲聊内容 → 不拦截。这样：
/// - "天气怎么样/天气咋样/今天冷吗/天气不错"都能被完整切分 → 拦截；
/// - "天气接口文档/支付超时怎么配置/幂等逻辑"一旦剩有未命中内容 → 放行检索。
fn is_casual_query<S: AsRef<str>>(query: &str, vocab: &[S]) -> bool {
    // 归一化：小写 + 去掉标点与空白（分词按词条 + 分隔符处理）
    let mut rest: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if rest.is_empty() {
        return false;
    }

    // 贪心切分：优先匹配更长词条，尽可能一次消费更多
    let mut i = 0usize;
    while i < rest.len() {
        let tail: String = rest[i..].iter().collect();
        // 找最长的、位于 tail 开头的词条
        let best = vocab
            .iter()
            .filter(|w| tail.starts_with(w.as_ref()))
            .max_by_key(|w| w.as_ref().chars().count());
        match best {
            Some(w) => {
                i += w.as_ref().chars().count();
            }
            None => {
                // 剩余无法被词表切分 → 含非闲聊内容
                return false;
            }
        }
    }
    true
}

/// 外部闲聊词表（`casual-words.txt`），进程内只加载一次。
///
/// 返回合并后的完整词表：**外部文件中的词与内置 [`CASUAL_VOCAB`] 总会并集合并**，
/// 外部文件只会新增词、绝不会顶掉默认词——避免用户只加了几个词却静默丢失全部默认覆盖。
/// 若没有可用的外部文件，则直接使用内置 [`CASUAL_VOCAB`]。
fn casual_vocab_external() -> &'static Vec<String> {
    static MERGED: OnceLock<Vec<String>> = OnceLock::new();
    MERGED.get_or_init(|| match load_casual_words() {
        Some(external) => merge_casual_vocab(&external),
        None => CASUAL_VOCAB.iter().map(|s| s.to_string()).collect(),
    })
}

/// 从 `config/casual-words.txt` 加载闲聊词表（每行一词，`#` 注释，空行忽略）。
///
/// 查找顺序：
/// 1. 环境变量 `DT_CASUAL_WORDS_FILE`
/// 2. `<CWD>/config/casual-words.txt`
/// 3. `~/.config/digital-twin/casual-words.txt`
/// 4. `<可执行文件目录>/config/casual-words.txt`
///
/// 返回空 Vec 表示未找到可用文件；调用方应回退到内置默认词表。
fn load_casual_words() -> Option<Vec<String>> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(file) = std::env::var("DT_CASUAL_WORDS_FILE") {
        candidates.push(PathBuf::from(file));
    }
    candidates.push(PathBuf::from("config/casual-words.txt"));
    if let Some(home) = crate::shared::home_dir() {
        candidates.push(home.join(".config/digital-twin/casual-words.txt"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("config/casual-words.txt"));
        }
    }

    for path in &candidates {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let words = parse_casual_words(&content);
        if !words.is_empty() {
            tracing::debug!(path = %path.display(), "加载闲聊词表语料 {}", words.len());
            return Some(words);
        }
    }
    tracing::debug!("未找到闲聊词表文件，使用内置默认词表");
    None
}

/// 解析闲聊词表文本：每行一词，`#` 开头为注释，空行忽略，行首尾空白剔除。
/// 仅保留含至少一个字母/数字/中日韩字符的行——纯符号分隔线（` ``` `、`***`、`---`）被丢弃。
fn parse_casual_words(content: &str) -> Vec<String> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with('#') && line.chars().any(|c| c.is_alphanumeric())
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// 将外部词表与内置默认 [`CASUAL_VOCAB`] 做并集合并（只增不减）。返回去重后的完整词表。
fn merge_casual_vocab(external: &[String]) -> Vec<String> {
    let mut merged = external.to_vec();
    for &w in CASUAL_VOCAB {
        if !merged.iter().any(|e| e == w) {
            merged.push(w.to_string());
        }
    }
    merged
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

#[cfg(test)]
mod tests {
    use super::{
        analyze_query_intent, merge_casual_vocab, parse_casual_words, should_search, QueryIntent,
    };

    #[test]
    fn should_search_trivial_chitchat() {
        // 寒暄 / 应答 / 纯算术 / 通用闲聊 → 无需检索
        assert!(!should_search("你好"));
        assert!(!should_search("谢谢"));
        assert!(!should_search("帮我算一下 1+1"));
        assert!(!should_search("天气不错"));
        assert!(!should_search("hello"));
        assert!(!should_search("好的 收到"));
    }

    #[test]
    fn should_search_casual_paraphrase_generalizes() {
        // 核心目标：固定短语列表被"换个说法"绕过的问题。
        // "天气*"家族无论怎么改说法都应被拦下——因为每个 token 都在闲聊词表里。
        assert!(!should_search("天气怎么样"));
        assert!(!should_search("天气咋样"));
        assert!(!should_search("天气如何"));
        assert!(!should_search("今天冷吗"));
        assert!(!should_search("今天多少度"));
        assert!(!should_search("外面天气还行"));
        assert!(!should_search("how is the weather"));
    }

    #[test]
    fn should_search_pass_technical_phrasing() {
        // 即便接近闲聊句式，只要带进领域实体/技术词 → 必须放行检索
        assert!(should_search("天气api接口文档"));
        assert!(should_search("天气模型构建服务"));
        assert!(should_search("temperature 配置在哪"));
        assert!(should_search("支付超时怎么配置"));
        assert!(should_search("payment callback 幂等逻辑在哪"));
    }

    #[test]
    fn should_search_passes_intent_feature() {
        // 命中实体/意图/历史特征 → 放行
        assert!(should_search("MemgraphClient"));
        assert!(should_search("getBalance"));
        assert!(should_search("如何实现支付功能"));
        assert!(should_search("支付超时怎么配置"));
        assert!(should_search("昨天那个订单重复扣款的问题怎么解决"));
        assert!(should_search("payment callback 幂等逻辑在哪"));
        assert!(should_search("修改订单中心支付超时配置"));
        assert!(should_search("之前怎么处理重复扣款"));
        assert!(should_search("上次改过 payment"));
        assert!(should_search("config/datasource.yaml"));
    }

    #[test]
    fn looks_like_identifier_heuristic_is_redundant() {
        // 标识符不需要单独的启发式：贪心闲聊分类里，标识符因含非闲聊字符而天然无法完整切分，
        // 从而必然放行检索（见 should_search_pass_technical_phrasing）。
        assert!(should_search("MemgraphClient"));
        assert!(should_search("getBalance"));
        assert!(should_search("CrossWorldSearchTrait"));
        assert!(!should_search("how are you"));
    }

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
    fn parse_casual_words_handles_comments_and_blanks() {
        let content = "\n  你好  \n# 天气话题\n\n天气\n";
        let words = parse_casual_words(content);
        assert_eq!(words, vec!["你好", "天气"]);
        // 注释、空行、行首尾空白全被剔除
        assert!(!words.contains(&"# 天气话题".to_string()));
        assert!(!words.contains(&"".to_string()));
    }

    #[test]
    fn parse_casual_words_drops_pure_symbol_lines() {
        // 审查发现的解析缺陷：纯符号分隔线曾被当成"词"注入词表，现应被丢弃。
        let content = "天气\n```\n***\n---\n======\n下雨\n";
        let words = parse_casual_words(content);
        assert_eq!(words, vec!["天气", "下雨"]);
        assert!(!words.contains(&"```".to_string()));
        assert!(!words.contains(&"***".to_string()));
        assert!(!words.contains(&"---".to_string()));
        assert!(!words.contains(&"======".to_string()));
    }

    #[test]
    fn merge_casual_vocab_is_union_never_shrinks() {
        // 外部只加一个词，默认词表必须完整保留（不会因小文件顶掉默认覆盖）
        let small = vec!["新词".to_string()];
        let merged = merge_casual_vocab(&small);

        assert!(merged.contains(&"新词".to_string()));
        for w in super::CASUAL_VOCAB {
            assert!(merged.contains(&w.to_string()), "默认词 {w} 被合并后丢失");
        }
        // 去重：默认词不应重复出现
        for w in super::CASUAL_VOCAB {
            let count = merged.iter().filter(|e| e.as_str() == *w).count();
            assert_eq!(count, 1, "默认词 {w} 出现 {count} 次");
        }
    }

    #[test]
    fn subject_predicated_queries_must_search() {
        // 审查发现的回归风险：掉了主语的诊断问句不能误拦（"服务怎么了"这类）
        assert!(should_search("服务怎么了"));
        assert!(should_search("订单怎么了"));
        assert!(should_search("支付超时怎么配置"));
        assert!(should_search("接口怎么了"));
    }
}
