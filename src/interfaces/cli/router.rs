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

/// 处理 `dt router` —— 智能路由搜索（统一搜索入口）。
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

    // ---- 第零层：L0 提前拦截（early-exit）----
    // 纯规则判断查询是否值得检索：命中实体/意图特征放行，寒暄/算术/闲聊直接返回。
    let early_exit = cfg
        .kg_router
        .as_ref()
        .map(|r| r.early_exit.clone())
        .unwrap_or_default();

    if early_exit.enabled && !should_search(query) {
        // ---- LLM 门控二次确认（不对称策略）----
        // 规则 L0 判定"不该搜"时，若开启 llm_gate，则问 LLM 确认：
        // LLM 说该搜 → 放行（规则误杀兜底）；LLM 说不该搜 → 拦截返回空。
        // 规则放行时直接过，不调 LLM（省延迟）。
        let gate_cfg = cfg
            .kg_router
            .as_ref()
            .map(|r| r.llm_gate.clone())
            .unwrap_or_default();
        let mut llm_approves = false;
        if gate_cfg.enabled {
            let llm = crate::infrastructure::embedder::create_llm_router(
                crate::interfaces::cli::build::provider_config_from_pipeline(),
            );
            match judge_search_with_llm(
                llm.as_ref(),
                query,
                gate_cfg.max_tokens,
                gate_cfg.temperature,
            )
            .await
            {
                Ok(true) => {
                    llm_approves = true;
                    if explain {
                        println!("=== 路由决策 ===");
                        println!("查询: {}", query);
                        println!("L0 规则拦截，但 LLM 门控判定值得检索 → 放行");
                        println!();
                    }
                }
                Ok(false) => {
                    if explain {
                        println!("=== 路由决策 ===");
                        println!("查询: {}", query);
                        println!("L0 规则拦截 + LLM 门控确认无需检索 → 直接返回");
                        println!();
                    }
                }
                Err(e) => {
                    // LLM 判断失败：保守拦截（与纯规则 L0 行为一致），记日志
                    tracing::warn!("LLM 门控判断失败({e})，按规则拦截处理: {query}");
                }
            }
        }
        if !llm_approves {
            if explain && !gate_cfg.enabled {
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
        let out = crate::interfaces::cli::search_render::render_human(
            &final_result,
            show_content,
            &resolver,
        );
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
/// 两道闸：
/// 1. 闲聊词表完整切分（寒暄/天气/算术）→ 无需检索；
/// 2. 检索锚点检测（[`has_search_anchor`]）——任务性口头语（"帮我实现"、
///    "给我一些建议"、"分析某个文件的内容"）不含闲聊词（实现/建议/文件 不在闲聊表），
///    却也不指向任何具体可检索对象，同样拦截。
fn should_search(query: &str) -> bool {
    if is_casual_query(query, casual_vocab_external().as_slice()) {
        return false;
    }
    has_search_anchor(query)
}

/// 判断查询是否携带可检索的具体对象（检索锚点）。
///
/// 仅在闲聊闸放行后调用。规则：
/// 1. 强信号（任一命中即视为有锚点）：
///    - 代码语法：`::`、`(`、`fn `/`def `/`class ` 等；
///    - 文件后缀（.rs/.py/.php/.md/.yaml…）或路径（含 `/`、`\`、`://`）；
///    - ASCII 标识符样式（驼峰/蛇形，如 MemgraphClient、getBalance）；
///    - ASCII 内容词（≥2 字，如 api、config、redis、payment）——由 keywords_of 保留。
/// 2. 中文内容词（jieba 切词去虚词后，≥2 字）：
///    - 只要有一个词不在 [`GENERIC_ANCHORLESS_WORDS`] 通用词表里，即视为有具体对象
///      （支付/订单/幂等/超时/缓存…都是可检索业务概念）；
///    - 全部落在通用词表（或没有内容词）→ 无锚点（如"帮我实现"只剩"实现"）。
fn has_search_anchor(query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return false;
    }
    // 1. 强信号：代码语法 / 文件后缀 / 路径 / 标识符样式
    if query.contains("::")
        || query.contains('(')
        || query.contains("fn ")
        || query.contains("def ")
        || query.contains("class ")
        || query.contains("public ")
        || query.contains("private ")
    {
        return true;
    }
    if query.chars().any(|c| c == '/' || c == '\\')
        || query.contains("://")
        || FILE_SUFFIX_ANCHORS
            .iter()
            .any(|s| query.to_lowercase().contains(s))
    {
        return true;
    }
    // ASCII 标识符样式（驼峰大写 / 蛇形下划线）
    let ascii_tokens: Vec<&str> = q
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .collect();
    if ascii_tokens
        .iter()
        .any(|t| t.len() >= 2 && (t.chars().any(|c| c.is_ascii_uppercase()) || t.contains('_')))
    {
        return true;
    }
    // 2. 内容词：jieba 去虚词后，任一非通用词即视为锚点。
    //    通用词表 = 内置默认 + 外部 `config/anchorless-words.txt`（并集，只增不减）。
    let kws = crate::application::knowledge::extract::retrieve::keywords_of(query, 20);
    kws.iter()
        .any(|kw| !anchorless_vocab_external().contains(&kw.to_lowercase()))
}

/// 视为强检索锚点的文件后缀（命中即放行）。
const FILE_SUFFIX_ANCHORS: &[&str] = &[
    ".rs",
    ".py",
    ".php",
    ".java",
    ".js",
    ".ts",
    ".tsx",
    ".jsx",
    ".go",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".cs",
    ".kt",
    ".swift",
    ".rb",
    ".scala",
    ".md",
    ".yaml",
    ".yml",
    ".json",
    ".xml",
    ".toml",
    ".ini",
    ".conf",
    ".sql",
    ".sh",
    ".bash",
    ".css",
    ".html",
    ".vue",
    ".svelte",
    ".txt",
    ".log",
    ".properties",
    ".gradle",
    ".lock",
];

/// 通用任务/指代词表——单独出现不代表可检索的具体对象（无检索锚点）。
///
/// 供 [`has_search_anchor`] 使用：查询去虚词后的内容词若**全部**落在本表
/// （或无内容词），判为无锚点（如"帮我实现"只剩"实现"、"给我建议"只剩"建议"）。
/// ⚠️ 绝不收录真实技术/业务词（支付/订单/缓存/接口/幂等/超时/模型/api/redis…），
/// 否则会误拦"支付超时怎么配置 / 接口怎么了 / 服务怎么了"等有效查询。
const GENERIC_ANCHORLESS_WORDS: &[&str] = &[
    // 任务动词（无宾语时无具体对象可查）
    "实现",
    "帮助",
    "帮忙",
    "设计",
    "开发",
    "编写",
    "编写一",
    "写",
    "写一",
    "写一个",
    "改造",
    "重构",
    "修复",
    "解决",
    "处理",
    "排查",
    "检查",
    "测试",
    "调试",
    "优化",
    "改善",
    "提升",
    "改进",
    "分析",
    "介绍",
    "说明",
    "总结",
    "梳理",
    "描述",
    "解释",
    "建议",
    "推荐",
    "评估",
    "规划",
    "思考",
    "想想",
    "看看",
    "了解",
    "学习",
    "研究",
    "使用",
    "运用",
    "操作",
    "创建",
    "新增",
    "添加",
    "删除",
    "修改",
    "完成",
    "搞定",
    "考虑",
    "讨论",
    "聊聊",
    "看看",
    "查看",
    "介绍下",
    // 诊断/根因类（"帮我检查确定的原因"这类缺宾语，无具体可检索对象）
    "确定",
    "确认",
    "判断",
    "查明",
    "定位",
    "找出",
    "找到",
    "发现",
    "原因",
    "根因",
    "来源",
    "根本",
    "本质",
    "关键",
    "问题",
    "问题点",
    "异常",
    "故障",
    "错误",
    "报错",
    "当前",
    "现在",
    "之前",
    "刚才",
    "此时",
    "此刻",
    "整体",
    "全部",
    "整个",
    "本次",
    "这次",
    "上次",
    "近期",
    "最近",
    // 动词+一下 的 jieba 粘连词（"检查一下/看下/分析一下"整词出现）
    "检查一下",
    "看一下",
    "看下",
    "分析一下",
    "排查一下",
    "确定一下",
    "确认一下",
    "介绍一下",
    "讲解一下",
    "说一下",
    "讲一下",
    "试一下",
    "测一下",
    "研究一下",
    "了解一下",
    "评估一下",
    "想想看",
    // 抽象/泛指名词（无具体指向；服务/接口/配置等真实技术词绝不入表）
    "文件",
    "代码",
    "内容",
    "功能",
    "模块",
    "项目",
    "程序",
    "应用",
    "方案",
    "思路",
    "想法",
    "需求",
    "任务",
    "工作",
    "情况",
    "事情",
    "东西",
    "业务",
    "产品",
    "页面",
    "组件",
    "工具",
    "数据",
    "流程",
    "逻辑",
    "场景",
    "能力",
    "特性",
    "属性",
    "结构",
    "架构",
    "层面",
    "部分",
    "方面",
    "框架",
    "平台",
    "文档",
    "手册",
    "类型",
    "类别",
    "区别",
    "原理",
    "概述",
    "流程",
    "步骤",
    "操作",
    "用法",
    "用途",
    // 指代/量词/疑问（jieba 去虚词后可能残留）
    "一些",
    "某些",
    "某个",
    "某种",
    "一个",
    "那个",
    "这个",
    "哪些",
    "什么",
    "怎么",
    "怎样",
    "如何",
    "为啥",
    "为什么",
    "要",
    "会",
    "能",
    "可以",
    "应该",
    "想要",
    "需要",
    "就是",
    "还有",
    "然后",
    "之后",
    "的话",
    "的话",
    "吗",
    "呢",
    // jieba 误切碎片（常见于"介绍+一下/有+哪些/怎么+办"等粘连，非真实业务词）
    "下有",
    "一下",
    "有哪",
    "有哪些",
    "有一",
    "干嘛",
    "怎么办",
    "来",
    "去",
    "搞",
    "弄",
    "做",
    "整",
    "弄一",
    "搞一",
    "整一",
    "聊",
    "谈",
    "讲",
    "说",
    "看",
    "找",
    // 英文通用（任务动词/泛指）
    "implement",
    "implementation",
    "help",
    "advice",
    "suggestion",
    "suggest",
    "analyze",
    "analyse",
    "analysis",
    "explain",
    "introduce",
    "optimize",
    "improve",
    "design",
    "develop",
    "create",
    "write",
    "make",
    "build",
    "fix",
    "solve",
    "check",
    "test",
    "review",
    "how",
    "what",
    "why",
    "do",
    "some",
    "about",
    "thing",
    "stuff",
    "feature",
    "module",
    "project",
    "function",
    "method",
    "code",
    "file",
    "idea",
    "plan",
    "problem",
    "issue",
    "way",
];

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
    load_external_wordlist("DT_CASUAL_WORDS_FILE", "casual-words.txt", "闲聊词表")
}

/// 从 `config/anchorless-words.txt` 加载无锚点词表（每行一词，`#` 注释，空行忽略）。
///
/// 查找顺序（与闲聊词表一致）：
/// 1. 环境变量 `DT_ANCHORLESS_WORDS_FILE`
/// 2. `<CWD>/config/anchorless-words.txt`
/// 3. `~/.config/digital-twin/anchorless-words.txt`
/// 4. `<可执行文件目录>/config/anchorless-words.txt`
///
/// 返回空 Vec 表示未找到可用文件；调用方应回退到内置默认词表。
fn load_anchorless_words() -> Option<Vec<String>> {
    load_external_wordlist(
        "DT_ANCHORLESS_WORDS_FILE",
        "anchorless-words.txt",
        "无锚点词表",
    )
}

/// 通用外部词表加载器：按 环境变量 → CWD config → ~/.config/digital-twin → exe config
/// 顺序查找外部词表 txt（每行一词，`#` 注释，空行忽略），解析后返回首个非空文件。
///
/// casual 与 anchorless 两套词表共用此逻辑，仅 环境变量名/文件名/日志标签 不同。
/// 注：home 候选是 `~/.config/digital-twin/<name>`（config 前缀在 home 下不重复），
/// CWD/exe 候选才是 `<base>/config/<name>`。
fn load_external_wordlist(env_var: &str, filename: &str, label: &str) -> Option<Vec<String>> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(file) = std::env::var(env_var) {
        candidates.push(PathBuf::from(file));
    }
    candidates.push(PathBuf::from("config").join(filename));
    if let Some(home) = crate::shared::home_dir() {
        candidates.push(home.join(".config/digital-twin").join(filename));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("config").join(filename));
        }
    }

    for path in &candidates {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let words = parse_casual_words(&content);
        if !words.is_empty() {
            tracing::debug!(path = %path.display(), "加载{label}语料 {}", words.len());
            return Some(words);
        }
    }
    tracing::debug!("未找到{label}文件，使用内置默认词表");
    None
}

/// 解析词表文本：每行一词，`#` 开头为注释，空行忽略，行首尾空白剔除。
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

/// 外部无锚点词表（`config/anchorless-words.txt`），进程内只加载一次。
///
/// 返回合并后的完整词表：**外部文件中的词与内置 [`GENERIC_ANCHORLESS_WORDS`]
/// 总会并集合并**，外部只增不减——避免用户只加了几个词却静默丢失全部默认覆盖。
/// 若没有可用外部文件，则直接使用内置默认词表。
fn anchorless_vocab_external() -> &'static Vec<String> {
    static MERGED: OnceLock<Vec<String>> = OnceLock::new();
    MERGED.get_or_init(|| match load_anchorless_words() {
        Some(external) => merge_anchorless_vocab(&external),
        None => GENERIC_ANCHORLESS_WORDS
            .iter()
            .map(|s| s.to_lowercase())
            .collect(),
    })
}

/// 将外部词表与内置默认 [`GENERIC_ANCHORLESS_WORDS`] 做并集合并（只增不减），
/// 小写化后去重。返回完整词表。
fn merge_anchorless_vocab(external: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = external.iter().map(|s| s.to_lowercase()).collect();
    for &w in GENERIC_ANCHORLESS_WORDS {
        let lw = w.to_lowercase();
        if !merged.iter().any(|e| e == &lw) {
            merged.push(lw);
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
fn parse_relevance_json(resp: &str) -> Option<bool> {
    let trimmed = resp.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed)
        .trim_end_matches("```")
        .trim();
    let start = body.find('{')?;
    let end_rel = body.rfind('}')?;
    if end_rel <= start {
        return None;
    }
    let obj: serde_json::Value = serde_json::from_str(&body[start..=end_rel]).ok()?;
    obj.get("relevant").and_then(|v| v.as_bool())
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
/// 作为规则 L0 拦截后的二次确认（不对称策略）使用：规则说该搜就直接搜，
/// 规则说不该搜才调本函数问 LLM——LLM 说该搜则放行，防规则误杀。
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
    let trimmed = resp.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed)
        .trim_end_matches("```")
        .trim();
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end <= start {
        return None;
    }
    let obj: serde_json::Value = serde_json::from_str(&body[start..=end]).ok()?;
    obj.get("search").and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_query_intent, merge_casual_vocab, parse_casual_words, parse_gate_json,
        parse_relevance_json, parse_relevance_text, should_search, QueryIntent,
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
    fn anchorless_task_chatter_is_blocked_at_l0() {
        // 任务性口头语：含"实现/建议/分析"等通用词，但没有具体检索对象
        // （无代码符号/文件/配置/业务实体），L0 锚点闸应拦截 → 不触发 KG 检索。
        assert!(!should_search("帮我实现"));
        assert!(!should_search("给我一些建议"));
        assert!(!should_search("分析某个文件的内容"));
        assert!(!should_search("介绍一下有哪些模块"));
        assert!(!should_search("有什么问题"));
        // 有具体业务对象/文件名 → 放行
        assert!(should_search("帮我实现一个轮询功能"));
        assert!(should_search("帮我实现轮询"));
        assert!(should_search("支付超时怎么配置"));
        assert!(should_search("分析 Wxapp.php"));
        assert!(should_search("接口怎么了"));
        assert!(should_search("服务怎么了"));
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
