//! KnowledgeService trait — contract for knowledge-dimension operations.
//!
//! Implementations handle CRUD for the five Knowledge World entity types:
//! Knowledge, Experience, Concept, Domain, and Playbook.
//!
//! The trait is decoupled from any specific storage backend via the
//! [`GraphRepository`] abstraction.
//!
//! [`DefaultKnowledgeService`] is the canonical production implementation.
//!
//! # Experience vectorisation
//!
//! When `write_experience()` is called on a [`DefaultKnowledgeService`]
//! that has been configured with an [`EmbedService`] and [`VectorRepository`],
//! the experience's title + summary text is automatically embedded and
//! upserted into the Qdrant `{project}_semantic` collection.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use std::sync::Arc;

use super::annotation::parse_details;
use super::entities::{
    Concept, Domain, Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook,
};

/// Service for managing Knowledge World entities.
///
/// # Typical usage
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
    /// Write (MERGE) a Knowledge node into the graph.
    async fn write_knowledge(&self, knowledge: &Knowledge) -> Result<(), DtError>;

    /// Write (MERGE) an Experience node into the graph.
    async fn write_experience(&self, experience: &Experience) -> Result<(), DtError>;

    /// Write (MERGE) a Concept node into the graph.
    async fn write_concept(&self, concept: &Concept) -> Result<(), DtError>;

    /// Write (MERGE) a Domain node into the graph.
    async fn write_domain(&self, domain: &Domain) -> Result<(), DtError>;

    /// Write (MERGE) a Playbook node into the graph.
    async fn write_playbook(&self, playbook: &Playbook) -> Result<(), DtError>;

    /// Update knowledge via versioned evolution.
    ///
    /// Instead of mutating the existing node, this creates a new Knowledge
    /// node with `version = old.version + 1`, links it to the old node via
    /// `[:EVOLVED_FROM]`, and creates a KnowledgeVersion record.
    async fn update_knowledge(
        &self,
        knowledge_id: &str,
        diff: &str,
        session_id: &str,
    ) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// DefaultKnowledgeService — canonical implementation
// ---------------------------------------------------------------------------

/// Canonical implementation of [`KnowledgeService`] backed by
/// a [`GraphRepository`].
///
/// # Lifecycle
///
/// ```text
/// write_knowledge → MERGE (:Knowledge {knowledge_id}) SET ...
/// write_experience → MERGE (:Experience {experience_id}) SET ...
///                    → embed(title + summary) → Qdrant upsert (if configured)
/// update_knowledge → 1. MATCH old node
///                    2. CREATE new version node
///                    3. CREATE (new)-[:EVOLVED_FROM]->(old)
///                    4. CREATE (:KnowledgeVersion)-[:RECORDS]->(new)
/// ```
pub struct DefaultKnowledgeService {
    graph: Arc<dyn GraphRepository>,
    /// Optional embedding service for auto-vectorising experiences.
    embed: Option<Arc<dyn EmbedService>>,
    /// Optional vector repository for storing experience vectors.
    vector: Option<Arc<dyn VectorRepository>>,
}

impl DefaultKnowledgeService {
    /// Create a new [`DefaultKnowledgeService`] backed by the given
    /// graph repository (without vectorisation support).
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self {
            graph,
            embed: None,
            vector: None,
        }
    }

    /// Create a new [`DefaultKnowledgeService`] with vectorisation support.
    ///
    /// When configured this way, [`write_experience`] will automatically
    /// embed the title + summary text and upsert into Qdrant.
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

    /// Whether vectorisation is available for this service instance.
    pub fn has_vectorization(&self) -> bool {
        self.embed.is_some() && self.vector.is_some()
    }

    /// Auto-vectorise an experience's title + summary into Qdrant.
    ///
    /// Called automatically by [`write_experience`] when both
    /// `embed` and `vector` backends are configured. No-op otherwise.
    async fn auto_vectorize_experience(
        &self,
        experience: &Experience,
    ) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let collection = format!("{}_semantic", experience.project);
        vector.ensure_collection(&collection, 1024).await?;

        let text = format!("{}: {}", experience.title, experience.summary);
        let vectors = embed.embed_batch(std::slice::from_ref(&text)).await?;
        let vec = match vectors.into_iter().next() {
            Some(v) => v,
            None => return Ok(()),
        };

        let point = serde_json::json!({
            "id": experience.experience_id,
            "vector": vec,
            "payload": {
                // ---- identity ----
                "entity_id": experience.experience_id,
                "title": experience.title,
                // ---- content ----
                "summary": experience.summary,
                "domain": experience.domain,
                "severity": experience.severity.as_str(),
                // ---- metadata ----
                "project": experience.project,
                "source_type": "experience",
                "search_text": text,
            }
        });

        vector.upsert(&collection, vec![point]).await?;
        Ok(())
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
            "confidence".into(),
            serde_json::json!(knowledge.confidence),
        );
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
        params.insert(
            "version".into(),
            serde_json::json!(knowledge.version),
        );

        self.graph.write_query(cypher, params).await?;
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

        // Auto-vectorise the experience if embed+vector backends are configured.
        // Failure to vectorise is logged but does NOT fail the write_experience call:
        // the graph write succeeded, and vectorisation is a best-effort enhancement.
        if let Err(e) = self.auto_vectorize_experience(experience).await {
            tracing::warn!(
                "Experience written to graph but vectorisation failed: {}",
                e
            );
        }

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
        Ok(())
    }

    async fn write_playbook(&self, playbook: &Playbook) -> Result<(), DtError> {
        // Serialize steps as JSON array string for storage
        let steps_json = serde_json::to_string(&playbook.steps)
            .unwrap_or_else(|_| "[]".to_string());

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
        params.insert(
            "steps".into(),
            serde_json::Value::String(steps_json),
        );
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
        Ok(())
    }

    async fn update_knowledge(
        &self,
        knowledge_id: &str,
        diff: &str,
        session_id: &str,
    ) -> Result<(), DtError> {
        // 1. Read current version from the existing knowledge node.
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

        // Parse current version; default to 0 if not found (first version will be 1).
        let current_version = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("version"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let new_version = current_version + 1;
        let new_knowledge_id = format!("{}/v{}", knowledge_id, new_version);
        let version_id = format!(
            "dt://knowledge-version/{}/v{}",
            knowledge_id, new_version
        );
        let now = chrono::Utc::now().to_rfc3339();

        // 2. Create new version node, copying properties from old node.
        // The Cypher reads from the old node, creates a new one, and links them.
        let write_cypher = r#"
            MATCH (old:Knowledge {knowledge_id: $knowledge_id})
            CREATE (new:Knowledge {
                knowledge_id: $new_knowledge_id,
                name: old.name,
                title: old.title,
                domain: old.domain,
                summary: old.summary,
                content: old.content,
                definition: old.definition,
                source: old.source,
                project: old.project,
                confidence: old.confidence,
                verified_by: old.verified_by,
                created_at: old.created_at,
                updated_at: $updated_at,
                version: $new_version
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
            SET old.updated_at = $updated_at
        "#;

        let mut write_params = std::collections::HashMap::new();
        write_params.insert(
            "knowledge_id".into(),
            serde_json::Value::String(knowledge_id.to_string()),
        );
        write_params.insert(
            "new_knowledge_id".into(),
            serde_json::Value::String(new_knowledge_id),
        );
        write_params.insert(
            "new_version".into(),
            serde_json::json!(new_version),
        );
        write_params.insert(
            "version_id".into(),
            serde_json::Value::String(version_id),
        );
        write_params.insert(
            "diff".into(),
            serde_json::Value::String(diff.to_string()),
        );
        write_params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );
        write_params.insert(
            "timestamp".into(),
            serde_json::Value::String(now.clone()),
        );
        write_params.insert(
            "updated_at".into(),
            serde_json::Value::String(now),
        );

        self.graph.write_query(write_cypher, write_params).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Convenience: build Knowledge from details string
// ---------------------------------------------------------------------------

/// Build a [`Knowledge`] struct from the CLI details string.
///
/// Expected format (semicolon-separated key: value pairs):
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
        name: kv.get("name").cloned().unwrap_or_else(|| entity_type.to_string()),
        title: kv.get("title").cloned().unwrap_or_default(),
        domain: kv.get("domain").cloned().unwrap_or_default(),
        summary: kv.get("summary").cloned().unwrap_or_default(),
        content: kv.get("content").cloned().unwrap_or_default(),
        definition: kv.get("definition").cloned().unwrap_or_default(),
        source: KnowledgeSource::parse(
            kv.get("source").map(|s| s.as_str()).unwrap_or("ai_session"),
        ),
        project: project.to_string(),
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

/// Build an [`Experience`] struct from the CLI details string.
///
/// Expected format:
/// ```text
/// "title: Redis超时; summary: 教训; content: ...; domain: 支付;
///  severity: warning; project: test"
/// ```
pub fn experience_from_details(
    experience_id: &str,
    project: &str,
    details: &str,
) -> Experience {
    let kv = parse_details(details);
    let now = chrono::Utc::now().to_rfc3339();

    Experience {
        experience_id: experience_id.to_string(),
        title: kv.get("title").cloned().unwrap_or_default(),
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

/// Build a [`Concept`] struct from the CLI details string.
pub fn concept_from_details(concept_id: &str, details: &str) -> Concept {
    let kv = parse_details(details);

    Concept {
        concept_id: concept_id.to_string(),
        name: kv.get("name").cloned().unwrap_or_default(),
        definition: kv.get("definition").cloned().unwrap_or_default(),
        domain: kv.get("domain").cloned().unwrap_or_default(),
        summary: kv.get("summary").cloned().unwrap_or_default(),
    }
}

/// Build a [`Domain`] struct from the CLI details string.
pub fn domain_from_details(domain_id: &str, details: &str) -> Domain {
    let kv = parse_details(details);

    Domain {
        domain_id: domain_id.to_string(),
        name: kv.get("name").cloned().unwrap_or_default(),
        description: kv.get("description").cloned().unwrap_or_default(),
    }
}

/// Build a [`Playbook`] struct from the CLI details string.
pub fn playbook_from_details(playbook_id: &str, project: &str, details: &str) -> Playbook {
    let kv = parse_details(details);
    let now = chrono::Utc::now().to_rfc3339();

    Playbook {
        playbook_id: playbook_id.to_string(),
        name: kv.get("name").cloned().unwrap_or_default(),
        description: kv.get("description").cloned().unwrap_or_default(),
        steps: vec![], // Steps require JSON; not parseable from flat details string
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::knowledge::entities::{
        Concept, Domain, Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook,
        Step,
    };
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A mock graph repository that counts write_query calls.
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
            // Return a valid version=1 row so update_knowledge finds the old node.
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

    /// Verify the trait is object-safe.
    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn KnowledgeService) {}
    }

    /// Verify all trait methods compile when referenced.
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
        svc.write_knowledge(&k).await.expect("write_knowledge");
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
        svc.write_experience(&e).await.expect("write_experience");
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
        svc.write_concept(&c).await.expect("write_concept");
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
        svc.write_domain(&d).await.expect("write_domain");
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
        svc.write_playbook(&p).await.expect("write_playbook");
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
        .expect("update_knowledge");

        // Should trigger: 1 read (find old) + 1 write (create new + version)
        assert!(read.load(Ordering::SeqCst) >= 1);
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    // -------------------------------------------------------------------
    // details → entity constructors
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
        assert_eq!(k.confidence, 0.5); // default
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
        let d = domain_from_details(
            "dt://domain/支付",
            "name: 支付; description: 支付相关知识",
        );
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
    // Experience vectorisation tests
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

    /// Mock VectorRepository that tracks upsert calls.
    struct TrackingVectorRepo {
        upsert_count: Arc<AtomicUsize>,
    }

    impl TrackingVectorRepo {
        fn new(
            upsert_count: Arc<AtomicUsize>,
        ) -> Self {
            Self { upsert_count }
        }
    }

    #[async_trait]
    impl VectorRepository for TrackingVectorRepo {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> { Ok(()) }
        async fn search(&self, _c: &str, _v: Vec<f32>, _l: u64) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            self.upsert_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> { Ok(()) }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> { Ok(vec![]) }
        async fn collection_info(&self, _n: &str) -> Result<CollectionInfo, DtError> {
            Err(DtError::NotFound("mock".into()))
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> { Ok(()) }
        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }

    /// CountingRepoExtended — wraps CountingRepo as GraphRepository + tracks calls.
    /// We reuse the write_count for the graph calls already.
    struct CountingRepoVector {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepoVector {
        fn new(write_count: Arc<AtomicUsize>, read_count: Arc<AtomicUsize>) -> Self {
            Self { write_count, read_count }
        }
    }

    #[async_trait]
    impl GraphRepository for CountingRepoVector {
        async fn read_query(&self, _q: &str, _p: std::collections::HashMap<String, serde_json::Value>) -> Result<serde_json::Value, DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"version": 1}]))
        }
        async fn write_query(&self, _q: &str, _p: std::collections::HashMap<String, serde_json::Value>) -> Result<serde_json::Value, DtError> {
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

        let graph: Arc<dyn GraphRepository> = Arc::new(CountingRepoVector::new(write.clone(), read.clone()));
        let embed: Arc<dyn EmbedService> = Arc::new(MockEmbed);
        let vector: Arc<dyn VectorRepository> = Arc::new(TrackingVectorRepo::new(
            upsert.clone(),
        ));

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
        svc.write_experience(&e).await.expect("write_experience");

        // Graph write should have occurred
        assert!(write.load(Ordering::SeqCst) >= 1);
        // Vector upsert should have occurred
        assert!(upsert.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn write_experience_without_vectorization_does_not_panic() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo: Arc<dyn GraphRepository> = Arc::new(CountingRepo::new(write.clone(), read.clone()));
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
        // Should succeed without vectorisation
        svc.write_experience(&e).await.expect("write_experience");
        assert!(write.load(Ordering::SeqCst) >= 1);
        // read count unchanged (no call graph involved)
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
