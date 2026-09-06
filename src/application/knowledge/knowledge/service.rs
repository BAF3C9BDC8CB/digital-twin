//! KnowledgeService trait——知识维度操作的契约。
//!
//! 实现负责 Knowledge 世界五种实体类型的增删改查：
//! Knowledge、Experience、Concept、Domain 与 Playbook。
//!
//! trait 通过 [`GraphRepository`] 抽象与任何具体存储后端解耦。
//!
//! [`DefaultKnowledgeService`] 是规范的生产实现。
//!
//! # 节点向量化
//!
//! 当 [`DefaultKnowledgeService`] 配置了 [`EmbedService`] 与 [`VectorRepository`]
//! 后，任何 `write_*()` 方法被调用时，节点的文本（按适用取
//! name / title / summary / content / definition）会经 [`embed_kg_node`]
//! 自动向量化并 upsert 到统一的 Qdrant `kg_nodes` 集合。覆盖 Knowledge、
//! Experience、Concept 与 Playbook 节点。向量化失败仅记录警告，
//! 不会导致底层图写入失败。
//!
//! [`embed_kg_node`]: crate::application::sync::kg_bridge::embed_kg_node

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use async_trait::async_trait;
use std::sync::Arc;

use super::annotation::parse_details;
use super::entities::{
    Concept, Domain, Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook,
};

/// 管理 Knowledge 世界实体的服务。
///
/// # 典型用法
///
/// ```ignore
/// let svc = DefaultKnowledgeService::new(graph_repo);
/// svc.write_knowledge(&Knowledge {
///     knowledge_id: "dt://knowledge/test/支付/payment-migration".into(),
///     name: "payment-migration".into(),
///     title: "支付平台迁移模式".into(),
///     domain: "支付".into(),
///     summary: "通联→银盛切换的标准模式".into(),
///     content: "# 支付平台迁移\n...".into(),
///     source: KnowledgeSource::AiSession,
///     ..
/// }).await?;
/// svc.update_knowledge("dt://knowledge/test/支付/payment-migration",
///     "新增 pitfall: pay-timeout.yml 容易遗漏", "2026-07-09-001").await?;
/// ```
#[async_trait]
pub trait KnowledgeService: Send + Sync {
    /// 将 Knowledge 节点写入（MERGE）图。
    async fn write_knowledge(&self, knowledge: &Knowledge) -> Result<(), DtError>;

    /// 将 Experience 节点写入（MERGE）图。
    async fn write_experience(&self, experience: &Experience) -> Result<(), DtError>;

    /// 将 Concept 节点写入（MERGE）图。
    async fn write_concept(&self, concept: &Concept) -> Result<(), DtError>;

    /// 将 Domain 节点写入（MERGE）图。
    async fn write_domain(&self, domain: &Domain) -> Result<(), DtError>;

    /// 将 Playbook 节点写入（MERGE）图。
    async fn write_playbook(&self, playbook: &Playbook) -> Result<(), DtError>;

    /// 通过版本化演化更新知识。
    ///
    /// 不是修改现有节点，而是新建一个 `version = old.version + 1` 的
    /// Knowledge 节点，通过 `[:EVOLVED_FROM]` 关联到旧节点，
    /// 并创建一条 KnowledgeVersion 记录。
    async fn update_knowledge(
        &self,
        knowledge_id: &str,
        diff: &str,
        session_id: &str,
    ) -> Result<(), DtError>;

    /// 删除一条知识/记忆节点（图 + 向量原子删除）。
    ///
    /// 用于 AI 验证记忆失效后的处置：
    /// - 图侧：`MATCH (n) WHERE n.knowledge_id = $id DELETE n`
    /// - 向量侧：删除 Qdrant `kg_nodes` 中 business_id 匹配的 point
    ///
    /// 若节点不存在返回 Ok(())（幂等删除）。
    async fn delete_knowledge(&self, entity_id: &str) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// DefaultKnowledgeService — 规范实现
// ---------------------------------------------------------------------------

/// 由 [`GraphRepository`] 支撑的 [`KnowledgeService`] 规范实现。
///
/// # 生命周期
///
/// ```text
/// write_knowledge   → MERGE (:Knowledge {knowledge_id}) SET ...
///                     → embed_kg_node → Qdrant kg_nodes upsert（如已配置）
/// write_experience  → MERGE (:Experience {experience_id}) SET ...
///                     → embed_kg_node → Qdrant kg_nodes upsert（如已配置）
/// write_concept     → MERGE (:Concept {concept_id}) SET ...
///                     → embed_kg_node → Qdrant kg_nodes upsert（如已配置）
/// write_playbook    → MERGE (:Playbook {playbook_id}) SET ...
///                     → embed_kg_node → Qdrant kg_nodes upsert（如已配置）
/// update_knowledge  → 1. MATCH 旧节点
///                     2. CREATE 新版本节点
///                     3. CREATE (new)-[:EVOLVED_FROM]->(old)
///                     4. CREATE (:KnowledgeVersion)-[:RECORDS]->(new)
/// ```
pub struct DefaultKnowledgeService {
    graph: Arc<dyn GraphRepository>,
    /// 可选的嵌入服务，用于自动向量化知识世界节点。
    embed: Option<Arc<dyn EmbedService>>,
    /// 可选的向量仓库，用于存储知识世界节点的向量。
    vector: Option<Arc<dyn VectorRepository>>,
}

impl DefaultKnowledgeService {
    /// 创建由给定图仓库支撑的 [`DefaultKnowledgeService`]
    /// （不含向量化支持）。
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self {
            graph,
            embed: None,
            vector: None,
        }
    }

    /// 创建带向量化支持的 [`DefaultKnowledgeService`]。
    ///
    /// 以这种方式配置后，[`write_experience`] 会自动将 title + summary
    /// 文本向量化并 upsert 到 Qdrant。
    pub fn with_vectorization(
        graph: Arc<dyn GraphRepository>,
        embed: Arc<dyn EmbedService>,
        vector: Arc<dyn VectorRepository>,
    ) -> Self {
        Self {
            graph,
            embed: Some(embed),
            vector: Some(vector),
        }
    }

    /// 该服务实例是否具备向量化能力。
    pub fn has_vectorization(&self) -> bool {
        self.embed.is_some() && self.vector.is_some()
    }

    /// 将经验的 title + summary 自动向量化进 Qdrant `kg_nodes`。
    ///
    /// 当 `embed` 与 `vector` 后端都已配置时，由 [`write_experience`]
    /// 自动调用；否则为 no-op。
    ///
    /// 经 [`embed_kg_node`] 写入统一的 `kg_nodes` 集合
    /// （替代旧的 `{project}_semantic` 集合），使所有知识世界节点
    /// 共享同一个可检索索引。
    async fn auto_vectorize_experience(&self, experience: &Experience) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": experience.title,
            "title": experience.title,
            "description": experience.summary,
            "domain": experience.domain,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Experience",
            "experience_id",
            &experience.experience_id,
            &props,
        )
        .await
    }

    /// 将知识节点的 name + title + summary 自动向量化进 Qdrant kg_nodes。
    async fn auto_vectorize_knowledge(&self, knowledge: &Knowledge) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "knowledge_id": knowledge.knowledge_id,
            "name": knowledge.name,
            "title": knowledge.title,
            "domain": knowledge.domain,
            "summary": knowledge.summary,
            "content": knowledge.content,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Knowledge",
            "knowledge_id",
            &knowledge.knowledge_id,
            &props,
        )
        .await
    }

    /// 将概念节点自动向量化进 Qdrant kg_nodes。
    async fn auto_vectorize_concept(&self, concept: &Concept) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": concept.name,
            "definition": concept.definition,
            "domain": concept.domain,
            "description": concept.summary,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Concept",
            "concept_id",
            &concept.concept_id,
            &props,
        )
        .await
    }

    /// 将 playbook 节点自动向量化进 Qdrant kg_nodes。
    async fn auto_vectorize_playbook(&self, playbook: &Playbook) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": playbook.name,
            "title": playbook.name,
            "description": playbook.description,
            "domain": playbook.domain,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Playbook",
            "playbook_id",
            &playbook.playbook_id,
            &props,
        )
        .await
    }
}

#[async_trait]
impl KnowledgeService for DefaultKnowledgeService {
    async fn write_knowledge(&self, knowledge: &Knowledge) -> Result<(), DtError> {
        let cypher = r#"
            MERGE (k:Knowledge {knowledge_id: $knowledge_id})
            ON CREATE SET
                k.name        = $name,
                k.title       = $title,
                k.domain      = $domain,
                k.summary     = $summary,
                k.content     = $content,
                k.definition  = $definition,
                k.source      = $source,
                k.project     = $project,
                k.scope       = $scope,
                k.confidence  = $confidence,
                k.verified_by = $verified_by,
                k.created_at  = $created_at,
                k.updated_at  = $updated_at,
                k.version     = $version
            ON MATCH SET
                k.title       = $title,
                k.summary     = $summary,
                k.content     = $content,
                k.definition  = $definition,
                k.scope       = $scope,
                k.confidence  = $confidence,
                k.verified_by = $verified_by,
                k.updated_at  = $updated_at
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "knowledge_id".into(),
            serde_json::Value::String(knowledge.knowledge_id.clone()),
        );
        params.insert(
            "name".into(),
            serde_json::Value::String(knowledge.name.clone()),
        );
        params.insert(
            "title".into(),
            serde_json::Value::String(knowledge.title.clone()),
        );
        params.insert(
            "domain".into(),
            serde_json::Value::String(knowledge.domain.clone()),
        );
        params.insert(
            "summary".into(),
            serde_json::Value::String(knowledge.summary.clone()),
        );
        params.insert(
            "content".into(),
            serde_json::Value::String(knowledge.content.clone()),
        );
        params.insert(
            "definition".into(),
            serde_json::Value::String(knowledge.definition.clone()),
        );
        params.insert(
            "source".into(),
            serde_json::Value::String(knowledge.source.as_str().into()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(knowledge.project.clone()),
        );
        params.insert(
            "scope".into(),
            serde_json::Value::String(knowledge.scope.clone()),
        );
        params.insert("confidence".into(), serde_json::json!(knowledge.confidence));
        params.insert(
            "verified_by".into(),
            knowledge
                .verified_by
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        params.insert(
            "created_at".into(),
            serde_json::Value::String(knowledge.created_at.clone()),
        );
        params.insert(
            "updated_at".into(),
            serde_json::Value::String(knowledge.updated_at.clone()),
        );
        params.insert("version".into(), serde_json::json!(knowledge.version));

        self.graph.write_query(cypher, params).await?;

        if self.has_vectorization() {
            let v_started = std::time::Instant::now();
            if let Err(e) = self.auto_vectorize_knowledge(knowledge).await {
                tracing::warn!("auto_vectorize_knowledge 失败：{e}");
            }
            tracing::info!(
                task = "memorize",
                action = "write_knowledge",
                entity_id = %knowledge.knowledge_id,
                name = %knowledge.name,
                domain = %knowledge.domain,
                project = %knowledge.project,
                vectorized = self.has_vectorization(),
                vectorize_ms = v_started.elapsed().as_millis() as u64,
                "知识节点已写入图并完成向量化"
            );
        } else {
            tracing::info!(
                task = "memorize",
                action = "write_knowledge",
                entity_id = %knowledge.knowledge_id,
                name = %knowledge.name,
                domain = %knowledge.domain,
                project = %knowledge.project,
                vectorized = false,
                "知识节点已写入图（无向量化后端）"
            );
        }
        Ok(())
    }

    async fn write_experience(&self, experience: &Experience) -> Result<(), DtError> {
        let cypher = r#"
            MERGE (e:Experience {experience_id: $experience_id})
            ON CREATE SET
                e.title      = $title,
                e.summary    = $summary,
                e.content    = $content,
                e.domain     = $domain,
                e.severity   = $severity,
                e.project    = $project,
                e.created_at = $created_at
            ON MATCH SET
                e.title      = $title,
                e.summary    = $summary,
                e.content    = $content,
                e.severity   = $severity
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "experience_id".into(),
            serde_json::Value::String(experience.experience_id.clone()),
        );
        params.insert(
            "title".into(),
            serde_json::Value::String(experience.title.clone()),
        );
        params.insert(
            "summary".into(),
            serde_json::Value::String(experience.summary.clone()),
        );
        params.insert(
            "content".into(),
            serde_json::Value::String(experience.content.clone()),
        );
        params.insert(
            "domain".into(),
            serde_json::Value::String(experience.domain.clone()),
        );
        params.insert(
            "severity".into(),
            serde_json::Value::String(experience.severity.as_str().into()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(experience.project.clone()),
        );
        params.insert(
            "created_at".into(),
            serde_json::Value::String(experience.created_at.clone()),
        );

        self.graph.write_query(cypher, params).await?;

        // 若已配置 embed+vector 后端则自动向量化经验。
        // 向量化失败仅记录日志，不导致 write_experience 调用失败：
        // 图写入已成功，向量化属于尽力而为的增强。
        if let Err(e) = self.auto_vectorize_experience(experience).await {
            tracing::warn!("经验已写入图，但向量化失败：{}", e);
        }
        tracing::info!(
            task = "memorize",
            action = "write_experience",
            entity_id = %experience.experience_id,
            title = %experience.title,
            domain = %experience.domain,
            severity = experience.severity.as_str(),
            vectorized = self.has_vectorization(),
            "经验节点已写入图"
        );

        Ok(())
    }

    async fn write_concept(&self, concept: &Concept) -> Result<(), DtError> {
        let cypher = r#"
            MERGE (c:Concept {concept_id: $concept_id})
            ON CREATE SET
                c.name       = $name,
                c.definition = $definition,
                c.domain     = $domain,
                c.summary    = $summary
            ON MATCH SET
                c.definition = $definition,
                c.summary    = $summary
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "concept_id".into(),
            serde_json::Value::String(concept.concept_id.clone()),
        );
        params.insert(
            "name".into(),
            serde_json::Value::String(concept.name.clone()),
        );
        params.insert(
            "definition".into(),
            serde_json::Value::String(concept.definition.clone()),
        );
        params.insert(
            "domain".into(),
            serde_json::Value::String(concept.domain.clone()),
        );
        params.insert(
            "summary".into(),
            serde_json::Value::String(concept.summary.clone()),
        );

        self.graph.write_query(cypher, params).await?;

        if self.has_vectorization() {
            if let Err(e) = self.auto_vectorize_concept(concept).await {
                tracing::warn!("auto_vectorize_concept 失败：{e}");
            }
        }
        tracing::info!(
            task = "memorize",
            action = "write_concept",
            entity_id = %concept.concept_id,
            name = %concept.name,
            domain = %concept.domain,
            vectorized = self.has_vectorization(),
            "概念节点已写入图"
        );
        Ok(())
    }

    async fn write_domain(&self, domain: &Domain) -> Result<(), DtError> {
        let cypher = r#"
            MERGE (d:Domain {domain_id: $domain_id})
            ON CREATE SET
                d.name        = $name,
                d.description = $description
            ON MATCH SET
                d.description = $description
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "domain_id".into(),
            serde_json::Value::String(domain.domain_id.clone()),
        );
        params.insert(
            "name".into(),
            serde_json::Value::String(domain.name.clone()),
        );
        params.insert(
            "description".into(),
            serde_json::Value::String(domain.description.clone()),
        );

        self.graph.write_query(cypher, params).await?;
        tracing::info!(
            task = "memorize",
            action = "write_domain",
            entity_id = %domain.domain_id,
            name = %domain.name,
            "领域节点已写入图"
        );
        Ok(())
    }

    async fn write_playbook(&self, playbook: &Playbook) -> Result<(), DtError> {
        // 将步骤序列化为 JSON 数组字符串以便存储
        let steps_json =
            serde_json::to_string(&playbook.steps).unwrap_or_else(|_| "[]".to_string());

        let cypher = r#"
            MERGE (p:Playbook {playbook_id: $playbook_id})
            ON CREATE SET
                p.name          = $name,
                p.description   = $description,
                p.steps         = $steps,
                p.domain        = $domain,
                p.project       = $project,
                p.success_count = $success_count,
                p.failure_count = $failure_count,
                p._needs_review = $_needs_review,
                p.created_at    = $created_at
            ON MATCH SET
                p.description   = $description,
                p.steps         = $steps,
                p.success_count = $success_count,
                p.failure_count = $failure_count,
                p._needs_review = $_needs_review
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "playbook_id".into(),
            serde_json::Value::String(playbook.playbook_id.clone()),
        );
        params.insert(
            "name".into(),
            serde_json::Value::String(playbook.name.clone()),
        );
        params.insert(
            "description".into(),
            serde_json::Value::String(playbook.description.clone()),
        );
        params.insert("steps".into(), serde_json::Value::String(steps_json));
        params.insert(
            "domain".into(),
            serde_json::Value::String(playbook.domain.clone()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(playbook.project.clone()),
        );
        params.insert(
            "success_count".into(),
            serde_json::json!(playbook.success_count),
        );
        params.insert(
            "failure_count".into(),
            serde_json::json!(playbook.failure_count),
        );
        params.insert(
            "_needs_review".into(),
            serde_json::json!(playbook._needs_review),
        );
        params.insert(
            "created_at".into(),
            serde_json::Value::String(playbook.created_at.clone()),
        );

        self.graph.write_query(cypher, params).await?;

        if self.has_vectorization() {
            if let Err(e) = self.auto_vectorize_playbook(playbook).await {
                tracing::warn!("auto_vectorize_playbook 失败：{e}");
            }
        }
        tracing::info!(
            task = "memorize",
            action = "write_playbook",
            entity_id = %playbook.playbook_id,
            name = %playbook.name,
            domain = %playbook.domain,
            project = %playbook.project,
            vectorized = self.has_vectorization(),
            "剧本节点已写入图"
        );
        Ok(())
    }

    async fn update_knowledge(
        &self,
        knowledge_id: &str,
        diff: &str,
        session_id: &str,
    ) -> Result<(), DtError> {
        // 1. 从现有知识节点读取当前版本。
        let read_cypher = r#"
            MATCH (k:Knowledge {knowledge_id: $knowledge_id})
            RETURN k.version AS version, k.name AS name, k.title AS title,
                   k.domain AS domain, k.summary AS summary, k.content AS content,
                   k.definition AS definition, k.source AS source, k.project AS project,
                   k.confidence AS confidence, k.verified_by AS verified_by
        "#;

        let mut read_params = std::collections::HashMap::new();
        read_params.insert(
            "knowledge_id".into(),
            serde_json::Value::String(knowledge_id.to_string()),
        );

        let result = self.graph.read_query(read_cypher, read_params).await?;

        // 解析当前版本；未找到时默认 0（首个版本将为 1）。
        let current_version = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let new_version = current_version + 1;
        let new_knowledge_id = format!("{}/v{}", knowledge_id, new_version);
        let version_id = format!("dt://knowledge-version/{}/v{}", knowledge_id, new_version);
        let now = chrono::Utc::now().to_rfc3339();

        // 2. 创建新版本节点，从旧节点复制属性，应用 diff 更新内容。
        // Cypher 从旧节点读取、创建新节点并将二者关联。
        // 旧节点置 status=archived（不再被检索召回），新节点 status=active。
        let write_cypher = r#"
            MATCH (old:Knowledge {knowledge_id: $knowledge_id})
            CREATE (new:Knowledge {
                knowledge_id: $new_knowledge_id,
                name: old.name,
                title: old.title,
                domain: old.domain,
                summary: $diff,
                content: $diff,
                definition: old.definition,
                source: old.source,
                project: old.project,
                confidence: old.confidence,
                verified_by: old.verified_by,
                created_at: old.created_at,
                updated_at: $updated_at,
                version: $new_version,
                status: 'active'
            })
            CREATE (new)-[:EVOLVED_FROM]->(old)
            CREATE (kv:KnowledgeVersion {
                version_id: $version_id,
                knowledge_id: $new_knowledge_id,
                version: $new_version,
                diff: $diff,
                session_id: $session_id,
                timestamp: $timestamp
            })
            CREATE (kv)-[:RECORDS]->(new)
            SET old.updated_at = $updated_at,
                old.status = 'archived',
                old.superseded_by = $new_knowledge_id
        "#;

        let mut write_params = std::collections::HashMap::new();
        write_params.insert(
            "knowledge_id".into(),
            serde_json::Value::String(knowledge_id.to_string()),
        );
        write_params.insert(
            "new_knowledge_id".into(),
            serde_json::Value::String(new_knowledge_id.clone()),
        );
        write_params.insert("new_version".into(), serde_json::json!(new_version));
        write_params.insert("version_id".into(), serde_json::Value::String(version_id));
        write_params.insert("diff".into(), serde_json::Value::String(diff.to_string()));
        write_params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );
        write_params.insert("timestamp".into(), serde_json::Value::String(now.clone()));
        write_params.insert("updated_at".into(), serde_json::Value::String(now.clone()));

        self.graph.write_query(write_cypher, write_params).await?;

        // 3. 新版本节点向量化（否则新内容检索不到）。
        // 同时删除旧节点向量，防止旧内容继续被召回。
        tracing::debug!(
            "[update_knowledge] 向量化条件: self.embed={} self.vector={}",
            self.embed.is_some(),
            self.vector.is_some()
        );
        if let Some(ref vector) = self.vector {
            crate::application::sync::kg_bridge::delete_kg_vector(vector.as_ref(), knowledge_id)
                .await?;
            let new_knowledge = Knowledge {
                knowledge_id: new_knowledge_id.clone(),
                name: diff
                    .split([';', '\n'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                title: String::new(),
                domain: String::new(),
                summary: diff.to_string(),
                content: diff.to_string(),
                definition: String::new(),
                source: KnowledgeSource::AiSession,
                project: String::new(),
                scope: String::new(),
                confidence: 0.7,
                verified_by: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                version: new_version,
            };
            if let Err(e) = self.auto_vectorize_knowledge(&new_knowledge).await {
                tracing::warn!("update_knowledge 新版本向量化失败：{e}");
            }
        }

        Ok(())
    }

    async fn delete_knowledge(&self, entity_id: &str) -> Result<(), DtError> {
        // 图侧：探测所有业务 ID 键（与 business_id 的 21 键优先级一致），
        // 删除匹配的节点及其关系。幂等：无匹配则无操作。
        let delete_cypher = r#"
            MATCH (n)
            WHERE n.entity_id = $id OR n.knowledge_id = $id OR n.concept_id = $id
               OR n.experience_id = $id OR n.playbook_id = $id OR n.domain_id = $id
               OR n.server_id = $id OR n.database_id = $id OR n.service_id = $id
               OR n.instance_id = $id OR n.endpoint_id = $id OR n.doc_id = $id
               OR n.config_id = $id OR n.thread_id = $id OR n.requirement_id = $id
               OR n.decision_id = $id OR n.event_id = $id OR n.session_id = $id
               OR n.version_id = $id OR n.observation_id = $id OR n.analysis_id = $id
            DETACH DELETE n
        "#;
        let mut params = std::collections::HashMap::new();
        params.insert(
            "id".into(),
            serde_json::Value::String(entity_id.to_string()),
        );
        self.graph.write_query(delete_cypher, params).await?;

        // 向量侧：删除 kg_nodes 中 business_id 匹配的 point。
        // 与图侧删除保持同步，防止向量残留导致 stale 记忆被召回。
        if let Some(ref vector) = self.vector {
            crate::application::sync::kg_bridge::delete_kg_vector(vector.as_ref(), entity_id)
                .await?;
        }

        tracing::info!("[knowledge] 已删除实体: {entity_id}");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 便捷构造：从 details 字符串构建 Knowledge
// ---------------------------------------------------------------------------

/// 从 CLI details 字符串构建 [`Knowledge`] 结构体。
///
/// 期望格式（分号分隔的 key: value 对）：
/// ```text
/// "title: 支付平台迁移; domain: 支付; summary: 通联→银盛; content: ...;
///  source: ai_session; project: test; confidence: 0.8"
/// ```
pub fn knowledge_from_details(
    knowledge_id: &str,
    entity_type: &str,
    project: &str,
    details: &str,
) -> Knowledge {
    let kv = parse_details(details);
    let now = chrono::Utc::now().to_rfc3339();

    Knowledge {
        knowledge_id: knowledge_id.to_string(),
        // name 语义保持不变：仅取 `name` 键，缺失时回退 entity_type（兼容旧行为，
        // 避免依赖 name=entity_type 的消费方被破坏）。
        name: kv
            .get("name")
            .cloned()
            .unwrap_or_else(|| entity_type.to_string()),
        // title 对齐插件：dt-memory 统一写 `name:`，旧调用写 `title:`。
        // title 优先取 `title`，缺失时回退 `name`（插件只写 name 时 title 不再为空）。
        title: kv
            .get("title")
            .or_else(|| kv.get("name"))
            .cloned()
            .unwrap_or_default(),
        domain: kv.get("domain").cloned().unwrap_or_default(),
        summary: kv.get("summary").cloned().unwrap_or_default(),
        content: kv.get("content").cloned().unwrap_or_default(),
        definition: kv.get("definition").cloned().unwrap_or_default(),
        source: KnowledgeSource::parse(
            kv.get("source").map(|s| s.as_str()).unwrap_or("ai_session"),
        ),
        project: project.to_string(),
        // scope: 解析 details 里的 scope 键(记忆作用域)。LLM 提取的
        // 记忆条目带 scope=project|global; 显式 dt_memorize 可带 scope 覆盖。
        // 归一化: 只接受 project/global, 其余(含空)统一为 ""(未标注)。
        scope: match kv.get("scope").map(|s| s.trim().to_lowercase()) {
            Some(ref s) if s == "project" => "project".to_string(),
            Some(ref s) if s == "global" => "global".to_string(),
            _ => String::new(),
        },
        confidence: kv
            .get("confidence")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.5),
        verified_by: kv.get("verified_by").cloned(),
        created_at: now.clone(),
        updated_at: now,
        version: 1,
    }
}

/// 从 CLI details 字符串构建 [`Experience`] 结构体。
///
/// 期望格式：
/// ```text
/// "title: Redis超时; summary: 教训; content: ...; domain: 支付;
///  severity: warning; project: test"
/// ```
pub fn experience_from_details(experience_id: &str, project: &str, details: &str) -> Experience {
    let kv = parse_details(details);
    let now = chrono::Utc::now().to_rfc3339();

    // 标题键对齐：插件（dt-memory）统一写 `name:`，旧调用写 `title:`。
    // 二者都识别为标题来源（title 优先向后兼容，name 兜底对齐插件）。
    let title = kv
        .get("title")
        .or_else(|| kv.get("name"))
        .cloned()
        .unwrap_or_default();

    Experience {
        experience_id: experience_id.to_string(),
        title,
        summary: kv.get("summary").cloned().unwrap_or_default(),
        content: kv.get("content").cloned().unwrap_or_default(),
        domain: kv.get("domain").cloned().unwrap_or_default(),
        severity: ExperienceSeverity::parse(
            kv.get("severity").map(|s| s.as_str()).unwrap_or("info"),
        ),
        project: project.to_string(),
        created_at: now,
    }
}

/// 从 CLI details 字符串构建 [`Concept`] 结构体。
pub fn concept_from_details(concept_id: &str, details: &str) -> Concept {
    let kv = parse_details(details);
    // name/title 双向对齐（与 knowledge/experience 一致）
    let name = kv
        .get("name")
        .or_else(|| kv.get("title"))
        .cloned()
        .unwrap_or_default();

    Concept {
        concept_id: concept_id.to_string(),
        name,
        definition: kv.get("definition").cloned().unwrap_or_default(),
        domain: kv.get("domain").cloned().unwrap_or_default(),
        summary: kv.get("summary").cloned().unwrap_or_default(),
    }
}

/// 从 CLI details 字符串构建 [`Domain`] 结构体。
pub fn domain_from_details(domain_id: &str, details: &str) -> Domain {
    let kv = parse_details(details);
    // name/title 双向对齐
    let name = kv
        .get("name")
        .or_else(|| kv.get("title"))
        .cloned()
        .unwrap_or_default();

    Domain {
        domain_id: domain_id.to_string(),
        name,
        description: kv.get("description").cloned().unwrap_or_default(),
    }
}

/// 从 CLI details 字符串构建 [`Playbook`] 结构体。
pub fn playbook_from_details(playbook_id: &str, project: &str, details: &str) -> Playbook {
    let kv = parse_details(details);
    let now = chrono::Utc::now().to_rfc3339();
    // name/title 双向对齐
    let name = kv
        .get("name")
        .or_else(|| kv.get("title"))
        .cloned()
        .unwrap_or_default();

    Playbook {
        playbook_id: playbook_id.to_string(),
        name,
        description: kv.get("description").cloned().unwrap_or_default(),
        steps: vec![], // 步骤需要 JSON；无法从扁平 details 字符串解析
        domain: kv.get("domain").cloned().unwrap_or_default(),
        project: project.to_string(),
        success_count: kv
            .get("success_count")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        failure_count: kv
            .get("failure_count")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        _needs_review: kv
            .get("_needs_review")
            .map(|s| s == "true")
            .unwrap_or(false),
        created_at: now,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::knowledge::entities::{
        Concept, Domain, Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook, Step,
    };
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 统计 write_query 调用次数的 mock 图仓库。
    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepo {
        fn new(write_count: Arc<AtomicUsize>, read_count: Arc<AtomicUsize>) -> Self {
            Self {
                write_count,
                read_count,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            // 返回 version=1 的有效行，让 update_knowledge 能找到旧节点。
            Ok(serde_json::json!([{"version": 1}]))
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// 验证 trait 是对象安全的。
    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn KnowledgeService) {}
    }

    /// 验证所有 trait 方法在被引用时可编译。
    #[test]
    fn trait_method_signatures_exist() {
        fn _assert_methods<T: KnowledgeService>() {}
    }

    fn make_knowledge() -> Knowledge {
        Knowledge {
            knowledge_id: "dt://knowledge/test/支付/test-k".into(),
            name: "test-k".into(),
            title: "Test Knowledge".into(),
            domain: "支付".into(),
            summary: "A test".into(),
            content: "Content".into(),
            definition: "A def".into(),
            source: KnowledgeSource::AiSession,
            project: "test".into(),
            scope: "project".into(),
            confidence: 0.8,
            verified_by: None,
            created_at: "2026-07-09T00:00:00Z".into(),
            updated_at: "2026-07-09T00:00:00Z".into(),
            version: 1,
        }
    }

    #[tokio::test]
    async fn write_knowledge_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        let k = make_knowledge();
        svc.write_knowledge(&k)
            .await
            .expect("write_knowledge 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_experience_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        let e = Experience {
            experience_id: "dt://experience/test/001".into(),
            title: "Test Exp".into(),
            summary: "Summary".into(),
            content: "Content".into(),
            domain: "支付".into(),
            severity: ExperienceSeverity::Warning,
            project: "test".into(),
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        svc.write_experience(&e)
            .await
            .expect("write_experience 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_concept_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        let c = Concept {
            concept_id: "dt://concept/支付/ifCode".into(),
            name: "ifCode".into(),
            definition: "支付渠道编码".into(),
            domain: "支付".into(),
            summary: "用于标识".into(),
        };
        svc.write_concept(&c).await.expect("write_concept 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_domain_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        let d = Domain {
            domain_id: "dt://domain/支付".into(),
            name: "支付".into(),
            description: "支付领域".into(),
        };
        svc.write_domain(&d).await.expect("write_domain 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_playbook_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        let p = Playbook {
            playbook_id: "dt://playbook/test/migrate".into(),
            name: "支付平台迁移".into(),
            description: "适用".into(),
            steps: vec![Step {
                order: 1,
                action: "edit".into(),
                tool: "edit".into(),
                target: Some("f".into()),
                expected: "done".into(),
                pitfall: None,
            }],
            domain: "支付".into(),
            project: "test".into(),
            success_count: 0,
            failure_count: 0,
            _needs_review: false,
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        svc.write_playbook(&p).await.expect("write_playbook 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn update_knowledge_creates_new_version() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);

        svc.update_knowledge(
            "dt://knowledge/test/支付/test-k",
            "新增注意事项",
            "2026-07-09-001",
        )
        .await
        .expect("update_knowledge 应成功");

        // 应触发：1 次读（找旧节点）+ 1 次写（创建新节点 + 版本）
        assert!(read.load(Ordering::SeqCst) >= 1);
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    // -------------------------------------------------------------------
    // details → 实体构造函数
    // -------------------------------------------------------------------

    #[test]
    fn knowledge_from_details_parses_fields() {
        let k = knowledge_from_details(
            "dt://knowledge/test/支付/test-k",
            "Decision",
            "test",
            "title: 支付平台迁移; domain: 支付; summary: 通联→银盛; \
             source: ai_session; confidence: 0.9",
        );
        assert_eq!(k.title, "支付平台迁移");
        assert_eq!(k.domain, "支付");
        assert_eq!(k.summary, "通联→银盛");
        assert_eq!(k.source, KnowledgeSource::AiSession);
        assert!((k.confidence - 0.9).abs() < 0.001);
        assert_eq!(k.project, "test");
        assert_eq!(k.version, 1);
    }

    #[test]
    fn knowledge_from_details_defaults() {
        let k = knowledge_from_details(
            "dt://knowledge/test/通用/default",
            "KnowledgeAdded",
            "test",
            "title: Some Knowledge",
        );
        assert_eq!(k.title, "Some Knowledge");
        assert_eq!(k.name, "KnowledgeAdded");
        assert_eq!(k.confidence, 0.5); // 默认值
    }

    #[test]
    fn experience_from_details_parses_severity() {
        let e = experience_from_details(
            "dt://experience/test/001",
            "test",
            "title: channelExtra坑; severity: warning; domain: 支付",
        );
        assert_eq!(e.title, "channelExtra坑");
        assert_eq!(e.severity, ExperienceSeverity::Warning);
        assert_eq!(e.domain, "支付");
        assert_eq!(e.project, "test");
    }

    #[test]
    fn experience_from_details_default_severity() {
        let e = experience_from_details(
            "dt://experience/test/002",
            "test",
            "title: Something happened",
        );
        assert_eq!(e.severity, ExperienceSeverity::Info);
    }

    #[test]
    fn experience_from_details_accepts_name_key() {
        // 回归：dt-memory 插件 details 统一用 `name:` 键，旧调用用 `title:`。
        // experience_from_details 需同时识别 name/title 为标题来源。
        let e = experience_from_details(
            "dt://experience/test/003",
            "test",
            "name: Nacos 配置中心连接经验; summary: nacos 用 8848; severity: warning",
        );
        assert_eq!(e.title, "Nacos 配置中心连接经验"); // name 键应映射到 title
        assert_eq!(e.summary, "nacos 用 8848");
        assert_eq!(e.severity, ExperienceSeverity::Warning);
    }

    #[test]
    fn knowledge_from_details_accepts_name_key_for_title() {
        // 回归：插件只写 `name:`，knowledge.title 应回退到 name 而非为空。
        let k = knowledge_from_details(
            "dt://knowledge/test/004",
            "KnowledgeAdded",
            "test",
            "name: Redis 超时配置; origin: user_explicit; content: 建议 300ms",
        );
        assert_eq!(k.name, "Redis 超时配置");
        assert_eq!(k.title, "Redis 超时配置"); // title 回退到 name（对齐插件）
        assert_eq!(k.content, "建议 300ms");
    }

    #[test]
    fn knowledge_from_details_title_precedence() {
        // 同时提供 name+title 时，title 优先作为 title（向后兼容旧调用）
        let k = knowledge_from_details(
            "dt://knowledge/test/005",
            "KnowledgeAdded",
            "test",
            "name: 备用名; title: 正式标题",
        );
        assert_eq!(k.name, "备用名");
        assert_eq!(k.title, "正式标题");
    }

    #[test]
    fn concept_from_details_parses_fields() {
        let c = concept_from_details(
            "dt://concept/支付/ifCode",
            "name: ifCode; definition: 支付渠道编码; domain: 支付; summary: 用于标识",
        );
        assert_eq!(c.name, "ifCode");
        assert_eq!(c.definition, "支付渠道编码");
        assert_eq!(c.domain, "支付");
    }

    #[test]
    fn domain_from_details_parses_fields() {
        let d = domain_from_details("dt://domain/支付", "name: 支付; description: 支付相关知识");
        assert_eq!(d.name, "支付");
        assert_eq!(d.description, "支付相关知识");
    }

    #[test]
    fn playbook_from_details_parses_fields() {
        let p = playbook_from_details(
            "dt://playbook/test/migrate",
            "test",
            "name: 支付平台迁移; domain: 支付; success_count: 10; failure_count: 2",
        );
        assert_eq!(p.name, "支付平台迁移");
        assert_eq!(p.domain, "支付");
        assert_eq!(p.success_count, 10);
        assert_eq!(p.failure_count, 2);
        assert!(p.steps.is_empty());
    }

    // -------------------------------------------------------------------
    // Experience 向量化测试
    // -------------------------------------------------------------------

    use crate::domain::traits::{EmbedService, VectorRepository};
    use crate::domain::types::CollectionInfo;

    struct MockEmbed;
    #[async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.0_f32; 1024]).collect())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// 跟踪 upsert 调用的 mock VectorRepository。
    struct TrackingVectorRepo {
        upsert_count: Arc<AtomicUsize>,
    }

    impl TrackingVectorRepo {
        fn new(upsert_count: Arc<AtomicUsize>) -> Self {
            Self { upsert_count }
        }
    }

    #[async_trait]
    impl VectorRepository for TrackingVectorRepo {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            self.upsert_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(&self, _n: &str) -> Result<CollectionInfo, DtError> {
            Err(DtError::NotFound("mock 不存在".into()))
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// CountingRepoExtended——包装 CountingRepo 作为 GraphRepository 并跟踪调用。
    /// 图调用直接复用 write_count。
    struct CountingRepoVector {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepoVector {
        fn new(write_count: Arc<AtomicUsize>, read_count: Arc<AtomicUsize>) -> Self {
            Self {
                write_count,
                read_count,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for CountingRepoVector {
        async fn read_query(
            &self,
            _q: &str,
            _p: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            // 响应必须包含 "eid" 字段，以便
            // `embed_kg_node`（由 auto_vectorize_* 调用）在 C1 修复后
            // 能取到真实的 Memgraph elementId。保留旧 "version"
            // 字段供读取它的消费者使用。
            Ok(serde_json::json!([{"version": 1, "eid": "4:1:mock-experience"}]))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn write_experience_triggers_vectorization() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let upsert = Arc::new(AtomicUsize::new(0));

        let graph: Arc<dyn GraphRepository> =
            Arc::new(CountingRepoVector::new(write.clone(), read.clone()));
        let embed: Arc<dyn EmbedService> = Arc::new(MockEmbed);
        let vector: Arc<dyn VectorRepository> = Arc::new(TrackingVectorRepo::new(upsert.clone()));

        let svc = DefaultKnowledgeService::with_vectorization(graph, embed, vector);
        assert!(svc.has_vectorization());

        let e = Experience {
            experience_id: "dt://experience/test/vec-001".into(),
            title: "Redis timeout".into(),
            summary: "Redis connection timeout caused payment failure".into(),
            content: "Full details...".into(),
            domain: "支付".into(),
            severity: ExperienceSeverity::Warning,
            project: "test".into(),
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        svc.write_experience(&e)
            .await
            .expect("write_experience 应成功");

        // 图写入应已发生
        assert!(write.load(Ordering::SeqCst) >= 1);
        // 向量 upsert 应已发生
        assert!(upsert.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_experience_without_vectorization_does_not_panic() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo: Arc<dyn GraphRepository> =
            Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultKnowledgeService::new(repo);
        assert!(!svc.has_vectorization());

        let e = Experience {
            experience_id: "dt://experience/test/no-vec".into(),
            title: "Some lesson".into(),
            summary: "Summary".into(),
            content: "Content".into(),
            domain: "通用".into(),
            severity: ExperienceSeverity::Info,
            project: "test".into(),
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        // 无向量化也应成功
        svc.write_experience(&e)
            .await
            .expect("write_experience 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
        // 读计数不变（未涉及图调用）
    }

    #[test]
    fn default_service_has_no_vectorization() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo: Arc<dyn GraphRepository> = Arc::new(CountingRepo::new(write, read));
        let svc = DefaultKnowledgeService::new(repo);
        assert!(!svc.has_vectorization());
    }
}
