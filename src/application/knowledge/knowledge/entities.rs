//! Knowledge 世界实体：Knowledge、KnowledgeVersion、Playbook、Experience、
//! Concept、Domain。
//!
//! 这些构成知识图谱的知识维度：
//! ```text
//! (:Domain)-[:CONTAINS]->(:Knowledge)
//! (:Domain)-[:CONTAINS]->(:Concept)
//! (:Knowledge)-[:EVOLVED_FROM]->(:Knowledge)
//! (:KnowledgeVersion)-[:RECORDS]->(:Knowledge)
//! (:Playbook)-[:USES_KNOWLEDGE]->(:Knowledge)
//! (:Experience)-[:RELATED_TO]->(:Knowledge)
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Knowledge — 核心知识实体
// ---------------------------------------------------------------------------

/// 表示概念、模式或洞见的知识条目。
///
/// Knowledge 可来源于 AI 会话、任务、文档、代码注释、用户口述或执行结果。
/// AI 生成的知识置信度较低；人工验证过的知识置信度 = 1.0。
///
/// 版本管理：更新**不**修改现有节点，而是新建 Knowledge 节点，
/// 并通过 `[:EVOLVED_FROM]` 关联到旧节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Knowledge {
    /// 唯一知识标识（dt://knowledge/{project}/{domain}/{name}）。
    pub knowledge_id: String,
    /// 知识条目的短名。
    pub name: String,
    /// 人类可读的标题。
    pub title: String,
    /// 领域分类（如 "支付"、"部署"、"配置"）。
    pub domain: String,
    /// 一句话摘要。
    pub summary: String,
    /// 完整 markdown 内容。
    pub content: String,
    /// 正式定义（面向概念型知识）。
    pub definition: String,
    /// 知识的来源。
    pub source: KnowledgeSource,
    /// 所属项目名（溯源字段；记忆统一全局后不参与检索过滤，2026-09-01）。
    pub project: String,
    /// 记忆作用域: "project" | "global" | ""。
    /// 兼容保留；记忆统一全局后检索不再按 scope 过滤（2026-09-01）。
    pub scope: String,
    /// 置信度 0.0–1.0。AI 生成 = 低，人工验证 = 1.0。
    pub confidence: f64,
    /// 验证者（"human" 或 null 等价物）。
    pub verified_by: Option<String>,
    /// 创建时间戳（ISO 8601）。
    pub created_at: String,
    /// 最后更新时间戳（ISO 8601）。
    pub updated_at: String,
    /// 版本号：新建为 1，每次更新递增。
    pub version: u32,
}

/// 知识的来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeSource {
    /// 源自 AI 对话会话。
    #[serde(rename = "ai_session")]
    AiSession,
    /// 作为 AI 任务执行的结果产生。
    #[serde(rename = "ai_task")]
    AiTask,
    /// 从项目文档中抽取。
    #[serde(rename = "document")]
    Document,
    /// 从代码注释 / 注解中抽取。
    #[serde(rename = "code_comment")]
    CodeComment,
    /// 由人类用户显式口述。
    #[serde(rename = "user_dictation")]
    UserDictation,
    /// 源自执行结果 / 日志。
    #[serde(rename = "execution_result")]
    ExecutionResult,
}

impl KnowledgeSource {
    /// 返回用于 Cypher/N4j 标签的字符串表示。
    pub fn as_str(&self) -> &'static str {
        match self {
            KnowledgeSource::AiSession => "ai_session",
            KnowledgeSource::AiTask => "ai_task",
            KnowledgeSource::Document => "document",
            KnowledgeSource::CodeComment => "code_comment",
            KnowledgeSource::UserDictation => "user_dictation",
            KnowledgeSource::ExecutionResult => "execution_result",
        }
    }

    /// 从字符串解析，未知值默认回退到 AiSession。
    pub fn parse(s: &str) -> Self {
        match s {
            "ai_session" => KnowledgeSource::AiSession,
            "ai_task" => KnowledgeSource::AiTask,
            "document" => KnowledgeSource::Document,
            "code_comment" => KnowledgeSource::CodeComment,
            "user_dictation" => KnowledgeSource::UserDictation,
            "execution_result" => KnowledgeSource::ExecutionResult,
            _ => KnowledgeSource::AiSession,
        }
    }
}

impl Default for Knowledge {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            knowledge_id: String::new(),
            name: String::new(),
            title: String::new(),
            domain: String::new(),
            summary: String::new(),
            content: String::new(),
            definition: String::new(),
            source: KnowledgeSource::AiSession,
            project: String::new(),
            scope: String::new(),
            confidence: 0.5,
            verified_by: None,
            created_at: now.clone(),
            updated_at: now,
            version: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// KnowledgeVersion — 记录知识条目的每次演化
// ---------------------------------------------------------------------------

/// 记录两个知识版本之间变化的版本记录。
///
/// 通过 `[:RECORDS]->(:Knowledge)` 关联到新版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeVersion {
    /// 唯一版本标识（dt://knowledge-version/{knowledge_id}/v{version}）。
    pub version_id: String,
    /// 该版本所描述的知识节点。
    pub knowledge_id: String,
    /// 版本号（1, 2, 3, ...）。
    pub version: u32,
    /// 人类可读的 diff / 变更摘要。
    pub diff: String,
    /// 创建该版本的会话。
    pub session_id: String,
    /// 版本记录时间（ISO 8601）。
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// Playbook — 可执行的操作手册
// ---------------------------------------------------------------------------

/// Playbook 是一份结构化的、可执行的 how-to 指南。
///
/// 由有序步骤组成，AI 或人类均可按步骤执行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    /// 唯一 playbook 标识（dt://playbook/{project}/{name}）。
    pub playbook_id: String,
    /// Playbook 的名称。
    pub name: String,
    /// 该 playbook 适用的场景。
    pub description: String,
    /// 有序执行步骤。
    pub steps: Vec<Step>,
    /// 领域分类。
    pub domain: String,
    /// 所属项目。
    pub project: String,
    /// 该 playbook 成功执行的次数。
    pub success_count: u64,
    /// 该 playbook 失败的次数。
    pub failure_count: u64,
    /// 成功率 < 70% 时自动标记。
    pub _needs_review: bool,
    /// 创建时间（ISO 8601）。
    pub created_at: String,
}

/// Playbook 中的单个步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// 执行顺序（从 1 开始）。
    pub order: u32,
    /// 要做什么。
    pub action: String,
    /// 使用哪个工具（如 "edit"、"bash"、"search"）。
    pub tool: String,
    /// 目标文件或实体。
    pub target: Option<String>,
    /// 预期结果是什么样。
    pub expected: String,
    /// 需要注意的坑与陷阱。
    pub pitfall: Option<String>,
}

// ---------------------------------------------------------------------------
// Experience — 经验教训 / 事故复盘
// ---------------------------------------------------------------------------

/// 记录经验教训或踩坑经历的经验条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    /// 唯一经验标识（dt://experience/{project}/{id}）。
    pub experience_id: String,
    /// 描述经验的短标题。
    pub title: String,
    /// 一句话要点。
    pub summary: String,
    /// 详细叙述。
    pub content: String,
    /// 领域分类。
    pub domain: String,
    /// 教训的严重程度。
    pub severity: ExperienceSeverity,
    /// 所属项目。
    pub project: String,
    /// 经验记录时间（ISO 8601）。
    pub created_at: String,
}

/// 经验的严重程度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExperienceSeverity {
    /// Critical——造成过宕机或数据丢失。
    #[serde(rename = "critical")]
    Critical,
    /// Warning——一次险情或潜在问题。
    #[serde(rename = "warning")]
    Warning,
    /// Informational——一般性提示。
    #[serde(rename = "info")]
    Info,
}

impl ExperienceSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExperienceSeverity::Critical => "critical",
            ExperienceSeverity::Warning => "warning",
            ExperienceSeverity::Info => "info",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "critical" => ExperienceSeverity::Critical,
            "warning" => ExperienceSeverity::Warning,
            _ => ExperienceSeverity::Info,
        }
    }
}

// ---------------------------------------------------------------------------
// Concept — 领域术语 / 定义
// ---------------------------------------------------------------------------

/// 概念是领域内被定义的术语。
///
/// 概念帮助 AI 理解项目特有的行话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concept {
    /// 唯一概念标识（dt://concept/{domain}/{name}）。
    pub concept_id: String,
    /// 术语或概念名。
    pub name: String,
    /// 正式定义。
    pub definition: String,
    /// 领域分类。
    pub domain: String,
    /// 扩展说明。
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Domain — 知识领域 / 分类
// ---------------------------------------------------------------------------

/// 领域将相关的知识、概念与 playbook 归类到一起。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    /// 唯一领域标识（dt://domain/{name}）。
    pub domain_id: String,
    /// 领域名（如 "支付"、"部署"、"配置"）。
    pub name: String,
    /// 人类可读的描述。
    pub description: String,
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_default_values() {
        let k = Knowledge::default();
        assert!(k.knowledge_id.is_empty());
        assert_eq!(k.version, 1);
        assert_eq!(k.source, KnowledgeSource::AiSession);
        assert!(k.confidence > 0.0);
        assert!(k.verified_by.is_none());
    }

    #[test]
    fn knowledge_source_parsing() {
        assert_eq!(
            KnowledgeSource::parse("ai_session"),
            KnowledgeSource::AiSession
        );
        assert_eq!(KnowledgeSource::parse("ai_task"), KnowledgeSource::AiTask);
        assert_eq!(
            KnowledgeSource::parse("document"),
            KnowledgeSource::Document
        );
        assert_eq!(
            KnowledgeSource::parse("code_comment"),
            KnowledgeSource::CodeComment
        );
        assert_eq!(
            KnowledgeSource::parse("user_dictation"),
            KnowledgeSource::UserDictation
        );
        assert_eq!(
            KnowledgeSource::parse("execution_result"),
            KnowledgeSource::ExecutionResult
        );
        // 未知值默认回退到 AiSession。
        assert_eq!(
            KnowledgeSource::parse("garbage"),
            KnowledgeSource::AiSession
        );
    }

    #[test]
    fn knowledge_source_as_str() {
        assert_eq!(KnowledgeSource::AiSession.as_str(), "ai_session");
        assert_eq!(KnowledgeSource::CodeComment.as_str(), "code_comment");
    }

    #[test]
    fn experience_severity_parsing() {
        assert_eq!(
            ExperienceSeverity::parse("critical"),
            ExperienceSeverity::Critical
        );
        assert_eq!(
            ExperienceSeverity::parse("CRITICAL"),
            ExperienceSeverity::Critical
        );
        assert_eq!(
            ExperienceSeverity::parse("warning"),
            ExperienceSeverity::Warning
        );
        assert_eq!(ExperienceSeverity::parse("info"), ExperienceSeverity::Info);
        // 未知值默认回退到 Info。
        assert_eq!(
            ExperienceSeverity::parse("unknown"),
            ExperienceSeverity::Info
        );
    }

    #[test]
    fn experience_severity_as_str() {
        assert_eq!(ExperienceSeverity::Critical.as_str(), "critical");
        assert_eq!(ExperienceSeverity::Warning.as_str(), "warning");
        assert_eq!(ExperienceSeverity::Info.as_str(), "info");
    }

    #[test]
    fn playbook_step_fields() {
        let step = Step {
            order: 1,
            action: "修改 ifCode".into(),
            tool: "edit".into(),
            target: Some("PayService.java".into()),
            expected: "ifCode 从 allinpay 改为 ysf".into(),
            pitfall: Some("别忘了同步改 channelExtra".into()),
        };
        assert_eq!(step.order, 1);
        assert_eq!(step.action, "修改 ifCode");
        assert!(step.target.is_some());
        assert!(step.pitfall.is_some());
    }

    #[test]
    fn concept_entity_fields() {
        let c = Concept {
            concept_id: "dt://concept/支付/ifCode".into(),
            name: "ifCode".into(),
            definition: "支付渠道编码".into(),
            domain: "支付".into(),
            summary: "用于标识不同支付渠道的编码".into(),
        };
        assert_eq!(c.name, "ifCode");
        assert_eq!(c.domain, "支付");
    }

    #[test]
    fn domain_entity_fields() {
        let d = Domain {
            domain_id: "dt://domain/支付".into(),
            name: "支付".into(),
            description: "支付相关知识和概念".into(),
        };
        assert_eq!(d.name, "支付");
        assert_eq!(d.description, "支付相关知识和概念");
    }

    #[test]
    fn knowledge_version_fields() {
        let kv = KnowledgeVersion {
            version_id: "dt://knowledge-version/dt://knowledge/test/支付/pay-platform/v2".into(),
            knowledge_id: "dt://knowledge/test/支付/pay-platform".into(),
            version: 2,
            diff: "新增 pitfall: pay-timeout.yml 容易遗漏".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:00:00Z".into(),
        };
        assert_eq!(kv.version, 2);
        assert!(kv.diff.contains("pitfall"));
    }

    #[test]
    fn knowledge_serialization_roundtrip() {
        let k = Knowledge {
            knowledge_id: "dt://knowledge/test/支付/test".into(),
            name: "test-knowledge".into(),
            title: "Test Knowledge".into(),
            domain: "支付".into(),
            summary: "A test entry".into(),
            content: "Some content".into(),
            definition: "A definition".into(),
            source: KnowledgeSource::AiSession,
            project: "test".into(),
            scope: "project".into(),
            confidence: 0.8,
            verified_by: Some("human".into()),
            created_at: "2026-07-09T00:00:00Z".into(),
            updated_at: "2026-07-09T00:00:00Z".into(),
            version: 1,
        };
        let json = serde_json::to_string(&k).expect("序列化应成功");
        let back: Knowledge = serde_json::from_str(&json).expect("反序列化应成功");
        assert_eq!(back.knowledge_id, k.knowledge_id);
        assert_eq!(back.confidence, 0.8);
        assert_eq!(back.source, KnowledgeSource::AiSession);
    }

    #[test]
    fn playbook_serialization_roundtrip() {
        let p = Playbook {
            playbook_id: "dt://playbook/test/migrate-payment".into(),
            name: "支付平台迁移".into(),
            description: "适用于支付平台切换场景".into(),
            steps: vec![Step {
                order: 1,
                action: "修改 ifCode".into(),
                tool: "edit".into(),
                target: Some("PayService.java".into()),
                expected: "改了".into(),
                pitfall: None,
            }],
            domain: "支付".into(),
            project: "test".into(),
            success_count: 10,
            failure_count: 2,
            _needs_review: false,
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&p).expect("序列化应成功");
        let back: Playbook = serde_json::from_str(&json).expect("反序列化应成功");
        assert_eq!(back.name, "支付平台迁移");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.success_count, 10);
    }
}
