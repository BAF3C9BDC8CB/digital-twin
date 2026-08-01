//! Consolidate 整合层 — the second stage of the universal knowledge pipeline
//! (抽取 → 整合 → 检索), 方案 §6.
//!
//! Consumes `Vec<ExtractedGraph>` (per-document block extraction output) and:
//!
//! 1. **Normalises** canonical names (lowercase, trim, full→half width,
//!    percent-encoding of URI-reserved chars) into stable `entity_id`s.
//! 2. **Two-level disambiguation** (§6.1): exact `entity_id` short-circuit,
//!    then vector near-neighbour merge (score > 0.92 + type一致).
//! 3. **Graph writes** (§6.2): `Document` / `Entity` / `RELATES` /
//!    `MENTIONED_IN` via four independent `write_query` calls (final
//!    consistency, no transaction wrapper).
//! 4. **Dual vector writes** (§6.3): entity → `kg_nodes` (per-entity, write-
//!    through so disambiguation can see it immediately), block → `doc_chunks`.
//! 5. **Lifecycle autonomy** (§6.5): entry purge of the document's old
//!    edges/vectors before writing; [`purge_document`] for deleted docs.
//!
//! Hard constraints honoured here:
//! - Disambiguation query and entity ingestion share one text constructor,
//!   [`entity_embed_text`].
//! - Vector upserts are per-entity, never batched (§12.1: disambiguation
//!   depends on immediately searchable writes).
//! - Relation endpoints resolve through the per-block
//!   `canonical_name → entity_id` map, never re-derived from canonical text.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::shared::collections::{DOC_CHUNKS, KG_NODES, VECTOR_DIM};

use super::model::{EntityType, ExtractedEntity, ExtractedGraph};

/// Cosine-similarity threshold for the second-level (vector) disambiguation.
const MERGE_SCORE_THRESHOLD: f32 = 0.92;

/// `kg_nodes` payload `origin` value for entities written by this layer.
const ORIGIN_EXTRACTED: &str = "extracted";

// ---------------------------------------------------------------------------
// Public statistics
// ---------------------------------------------------------------------------

/// Counters produced by [`Consolidator::consolidate_document`]; surfaced to
/// the pipeline engine's build report via the store processor output (R9).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidateStats {
    /// Entities merged into an existing graph node (short-circuit or vector hit).
    pub entities_merged: usize,
    /// Entities that created a brand-new graph node.
    pub entities_created: usize,
    /// `RELATES` edges successfully written.
    pub relations_written: usize,
    /// Relations dropped because an endpoint could not be resolved.
    pub relations_orphaned: usize,
    /// Blocks carrying the §5.5 degradation marker.
    pub degraded_blocks: usize,
    /// Total blocks processed.
    pub blocks_processed: usize,
    /// Non-degraded blocks with empty entities/relations/summary (observed only).
    pub empty_blocks: usize,
    /// Non-fatal per-item error messages.
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Text normalisation (§6.1)
// ---------------------------------------------------------------------------

/// Normalise a canonical name into the path segment of an `entity_id`:
/// full→half width, trim, lowercase, then percent-encode URI-reserved
/// characters (`%` first — implemented as a single char pass so replacement
/// strings are never re-scanned).
pub fn normalize(name: &str) -> String {
    let half: String = name.chars().map(to_half_width).collect();
    let lowered = half.trim().to_lowercase();
    percent_encode(&lowered)
}

/// Map full-width ASCII variants (U+FF01..U+FF5E) and the full-width space
/// (U+3000) to their half-width equivalents.
fn to_half_width(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        c if ('\u{FF01}'..='\u{FF5E}').contains(&c) => {
            char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
        }
        c => c,
    }
}

/// Percent-encode URI-reserved characters so a free-form canonical name can
/// never inject extra URI segments into an `entity_id`.
///
/// Percent-encoding (not character replacement) is deliberate: replacement
/// would collide "读/写分离" with "读_写分离" into the same ID.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '&' => out.push_str("%26"),
            ':' => out.push_str("%3A"),
            '@' => out.push_str("%40"),
            _ => out.push(ch),
        }
    }
    out
}

/// Derive the stable business primary key of an extracted entity (§6.1).
/// `{type}` is the enum variant name (e.g. `Channel`).
pub fn entity_id_for(project: &str, entity_type: EntityType, canonical_name: &str) -> String {
    format!(
        "dt://entity/{}/{}/{}",
        project,
        entity_type.as_str(),
        normalize(canonical_name)
    )
}

/// The single text constructor shared by the §6.1 disambiguation query and
/// the §6.3 entity ingestion — never duplicate this formatting elsewhere.
pub fn entity_embed_text(e: &ExtractedEntity) -> String {
    format!(
        "{}。{}。关键词: {}",
        e.canonical_name,
        e.summary,
        e.keywords.join(" ")
    )
}

// ---------------------------------------------------------------------------
// Consolidator
// ---------------------------------------------------------------------------

/// The Consolidate layer worker. Holds the three backend handles; per-call
/// state (block maps, stats) lives on the stack.
pub struct Consolidator {
    graph: Arc<dyn GraphRepository>,
    vector: Arc<dyn VectorRepository>,
    embed: Arc<dyn EmbedService>,
}

impl Consolidator {
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
        }
    }

    /// Consolidate one document's extracted graphs into KG + vectors.
    ///
    /// `block_texts` maps `block_index` → raw chunk text (from the chunk
    /// processor output), used for the `doc_chunks` dual write.
    pub async fn consolidate_document(
        &self,
        project: &str,
        doc_id: &str,
        file_path: &str,
        doc_type: &str,
        graphs: &[ExtractedGraph],
        block_texts: &HashMap<u32, String>,
    ) -> Result<ConsolidateStats, DtError> {
        ensure_schema(&self.graph).await;

        let mut stats = ConsolidateStats::default();

        // ── §6.5 entry purge (autonomous, idempotent): wipe this document's
        //    old edges and doc_chunks points before writing anything new.
        let mut purge_params = HashMap::new();
        purge_params.insert("doc_id".to_string(), serde_json::json!(doc_id));
        self.graph
            .write_query(
                "MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r",
                purge_params.clone(),
            )
            .await?;
        self.graph
            .write_query(
                "MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m",
                purge_params.clone(),
            )
            .await?;
        self.vector
            .delete_by_filter(DOC_CHUNKS, doc_id_filter(doc_id))
            .await?;

        // ── §6.2.0 Document node: MERGE before any MENTIONED_IN write.
        let mut doc_params = HashMap::new();
        doc_params.insert("doc_id".to_string(), serde_json::json!(doc_id));
        doc_params.insert("project".to_string(), serde_json::json!(project));
        doc_params.insert("file_path".to_string(), serde_json::json!(file_path));
        doc_params.insert("doc_type".to_string(), serde_json::json!(doc_type));
        self.graph
            .write_query(
                "MERGE (d:Document {doc_id: $doc_id}) \
                 ON CREATE SET d.project = $project, d.file_path = $file_path, \
                               d.doc_type = $doc_type",
                doc_params,
            )
            .await?;

        for block in graphs {
            stats.blocks_processed += 1;
            if block.degraded {
                stats.degraded_blocks += 1;
            } else if block.entities.is_empty()
                && block.relations.is_empty()
                && block.block_summary.is_empty()
            {
                // Task-1 carried minor: silently-empty successful block —
                // observe only (never dropped, never degraded).
                stats.empty_blocks += 1;
                tracing::warn!(
                    "consolidate: 非降级空块 doc={} block_index={}",
                    doc_id,
                    block.block_index
                );
            }

            // canonical_name → disambiguated entity_id, registered per entity
            // (hard constraint: relation endpoints resolve through this map).
            let mut block_map: HashMap<String, String> = HashMap::new();

            if !block.entities.is_empty() {
                // ── First level: batch exact-id check (one read per block).
                let derived: Vec<String> = block
                    .entities
                    .iter()
                    .map(|e| entity_id_for(project, e.entity_type, &e.canonical_name))
                    .collect();
                let mut check_params = HashMap::new();
                check_params.insert("ids".to_string(), serde_json::json!(derived));
                let existing_rows = self
                    .graph
                    .read_query(
                        "UNWIND $ids AS eid MATCH (e:Entity {entity_id: eid}) \
                         RETURN eid AS entity_id",
                        check_params,
                    )
                    .await?;
                let existing: HashSet<String> = existing_rows
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|r| r.get("entity_id")?.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // ── §6.3 embed decoupling: one batch for the whole block,
                //    then per-entity search → write → upsert.
                let embed_texts: Vec<String> =
                    block.entities.iter().map(entity_embed_text).collect();
                let vectors = self.embed.embed_batch(&embed_texts).await?;

                for (entity, vector) in block.entities.iter().zip(vectors.iter()) {
                    let derived_id =
                        entity_id_for(project, entity.entity_type, &entity.canonical_name);

                    let (final_id, created) = if existing.contains(&derived_id) {
                        (derived_id, false) // first-level short-circuit merge
                    } else {
                        // ── Second level: vector near-neighbour merge.
                        let hits = self
                            .vector
                            .search_with_filter(
                                KG_NODES,
                                vector.clone(),
                                5,
                                serde_json::json!({"must": [
                                    {"key": "project", "match": {"value": project}}
                                ]}),
                            )
                            .await?;
                        let merge_hit = hits.iter().find(|h| {
                            let score = h.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                            let ty = h
                                .get("payload")
                                .and_then(|p| p.get("type"))
                                .and_then(|t| t.as_str());
                            score > MERGE_SCORE_THRESHOLD as f64
                                && ty == Some(entity.entity_type.as_str())
                        });
                        match merge_hit.and_then(|h| {
                            h.get("payload")?
                                .get("business_id")?
                                .as_str()
                                .map(String::from)
                        }) {
                            Some(target) => (target, false),
                            None => (derived_id, true),
                        }
                    };

                    // ── §6.2.1 Entity MERGE (returns elementId for payload).
                    let mut ep = HashMap::new();
                    ep.insert("entity_id".to_string(), serde_json::json!(final_id));
                    ep.insert("name".to_string(), serde_json::json!(entity.canonical_name));
                    ep.insert(
                        "type".to_string(),
                        serde_json::json!(entity.entity_type.as_str()),
                    );
                    ep.insert("summary".to_string(), serde_json::json!(entity.summary));
                    ep.insert("keywords".to_string(), serde_json::json!(entity.keywords));
                    ep.insert("project".to_string(), serde_json::json!(project));
                    ep.insert("mention".to_string(), serde_json::json!(entity.mention));
                    ep.insert(
                        "new_aliases".to_string(),
                        serde_json::json!([entity.mention]),
                    );
                    let merge_resp = self
                        .graph
                        .write_query(
                            "MERGE (e:Entity {entity_id: $entity_id}) \
                             ON CREATE SET e.name = $name, e.type = $type, \
                                           e.summary = $summary, e.keywords = $keywords, \
                                           e.project = $project, e.aliases = [$mention] \
                             ON MATCH  SET e.summary = $summary, \
                                           e.aliases = REDUCE(acc = coalesce(e.aliases, []), x IN $new_aliases | \
                                                 CASE WHEN x IN acc THEN acc ELSE acc + x END), \
                                           e.keywords = REDUCE(kacc = coalesce(e.keywords, []), x IN $keywords | \
                                                 CASE WHEN x IN kacc THEN kacc ELSE kacc + x END) \
                             RETURN elementId(e) AS eid",
                            ep,
                        )
                        .await?;
                    let element_id = merge_resp
                        .as_array()
                        .and_then(|rows| rows.first()?.get("eid")?.as_str().map(String::from))
                        .unwrap_or_default();

                    if created {
                        stats.entities_created += 1;
                    } else {
                        stats.entities_merged += 1;
                    }

                    // ── §6.3 entity → kg_nodes write-through (per entity,
                    //    never batched — §12.1).
                    let point = serde_json::json!({
                        "id": crate::application::sync::kg_bridge::make_point_id(&final_id),
                        "vector": vector,
                        "payload": {
                            "elementId": element_id,
                            "business_id": final_id,
                            "name": entity.canonical_name,
                            "type": entity.entity_type.as_str(),
                            "summary": entity.summary,
                            "keywords": entity.keywords,
                            "project": project,
                            "labels": ["Entity"],
                            "doc_id": doc_id,
                            "origin": ORIGIN_EXTRACTED,
                            "source": "kg",
                        },
                    });
                    self.vector.upsert(KG_NODES, vec![point]).await?;

                    // _kg_synced_at only after graph + vector both succeeded.
                    let mut mp = HashMap::new();
                    mp.insert("entity_id".to_string(), serde_json::json!(final_id));
                    self.graph
                        .write_query(
                            "MATCH (e:Entity {entity_id: $entity_id}) \
                             SET e._kg_synced_at = datetime()",
                            mp,
                        )
                        .await?;

                    block_map.insert(entity.canonical_name.clone(), final_id);
                }

                // ── §6.2.3 MENTIONED_IN provenance (batched per block).
                let ids: Vec<String> = block_map.values().cloned().collect();
                let mut mp = HashMap::new();
                mp.insert("doc_id".to_string(), serde_json::json!(doc_id));
                mp.insert("ids".to_string(), serde_json::json!(ids));
                self.graph
                    .write_query(
                        "MATCH (d:Document {doc_id: $doc_id}) \
                         UNWIND $ids AS eid \
                         MATCH (e:Entity {entity_id: eid}) \
                         MERGE (e)-[:MENTIONED_IN]->(d)",
                        mp,
                    )
                    .await?;
            }

            // ── §6.2.2 Relations: endpoints via block map, then historical
            //    fallback, else orphan (log + count, no placeholder nodes).
            for relation in &block.relations {
                let head_id = self.resolve_endpoint(&block_map, &relation.head).await?;
                let tail_id = self.resolve_endpoint(&block_map, &relation.tail).await?;
                let (Some(head_id), Some(tail_id)) = (head_id, tail_id) else {
                    stats.relations_orphaned += 1;
                    tracing::warn!(
                        "consolidate: 孤儿关系丢弃 doc={} {} -{}-> {}",
                        doc_id,
                        relation.head,
                        relation.relation,
                        relation.tail
                    );
                    continue;
                };

                let mut rp = HashMap::new();
                rp.insert("head_id".to_string(), serde_json::json!(head_id));
                rp.insert("tail_id".to_string(), serde_json::json!(tail_id));
                rp.insert("rel_type".to_string(), serde_json::json!(relation.relation));
                rp.insert("doc_id".to_string(), serde_json::json!(doc_id));
                rp.insert(
                    "evidence".to_string(),
                    serde_json::json!(relation.evidence.clone().unwrap_or_default()),
                );
                // f32→f64 widening is lossy (0.9f32 → 0.899999976…); round in
                // f64 space so the stored confidence is clean (0.9 exactly).
                let confidence = ((relation.confidence.unwrap_or(0.5) as f64) * 1e6).round() / 1e6;
                rp.insert("confidence".to_string(), serde_json::json!(confidence));
                self.graph
                    .write_query(
                        "MATCH (h:Entity {entity_id: $head_id}), \
                               (t:Entity {entity_id: $tail_id}) \
                         MERGE (h)-[r:RELATES {type: $rel_type, doc_id: $doc_id}]->(t) \
                         SET r.evidence = $evidence, r.confidence = $confidence",
                        rp,
                    )
                    .await?;
                stats.relations_written += 1;
            }

            // ── §6.3 block → doc_chunks dual write (per block).
            let raw_text = block_texts
                .get(&block.block_index)
                .cloned()
                .unwrap_or_default();
            let block_embed_text = if block.block_summary.is_empty() {
                raw_text.clone() // degraded blocks: raw chunk only (§5.5)
            } else {
                format!("{}\n{}", block.block_summary, raw_text)
            };
            let block_vector = self.embed.embed_batch(&[block_embed_text]).await?;
            let entity_ids: Vec<String> = block_map.values().cloned().collect();
            let point = serde_json::json!({
                "id": crate::application::sync::kg_bridge::make_point_id(
                    &format!("{doc_id}:{}", block.block_index)
                ),
                "vector": block_vector.into_iter().next().unwrap_or_default(),
                "payload": {
                    "doc_id": doc_id,
                    "block_index": block.block_index,
                    "project": project,
                    "entity_ids": entity_ids,
                    "degraded": block.degraded,
                    "source": "doc",
                    "text": raw_text,
                },
            });
            self.vector.upsert(DOC_CHUNKS, vec![point]).await?;
        }

        Ok(stats)
    }

    /// Resolve a relation endpoint: the per-block disambiguation map first,
    /// then a historical-node suffix lookup (the endpoint may be a node from
    /// an older build), else `None` (caller counts an orphan relation).
    async fn resolve_endpoint(
        &self,
        block_map: &HashMap<String, String>,
        canonical: &str,
    ) -> Result<Option<String>, DtError> {
        if let Some(id) = block_map.get(canonical) {
            return Ok(Some(id.clone()));
        }
        let mut params = HashMap::new();
        params.insert(
            "suffix".to_string(),
            serde_json::json!(normalize(canonical)),
        );
        let rows = self
            .graph
            .read_query(
                "MATCH (e:Entity) WHERE e.entity_id ENDS WITH $suffix \
                 RETURN e.entity_id AS entity_id LIMIT 1",
                params,
            )
            .await?;
        let id = rows
            .as_array()
            .and_then(|r| r.first()?.get("entity_id")?.as_str().map(String::from));
        Ok(id)
    }

    /// §6.5.2 — a document was deleted: remove its `RELATES`/`MENTIONED_IN`
    /// edges, the `Document` node itself, and every `doc_chunks` vector point
    /// of the document. Entity nodes survive while referenced elsewhere.
    ///
    /// Wiring into the build orchestration layer is Task 3's job; this
    /// function is the public entry point.
    pub async fn purge_document(&self, doc_id: &str) -> Result<(), DtError> {
        let mut params = HashMap::new();
        params.insert("doc_id".to_string(), serde_json::json!(doc_id));
        self.graph
            .write_query(
                "MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r",
                params.clone(),
            )
            .await?;
        self.graph
            .write_query(
                "MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m",
                params.clone(),
            )
            .await?;
        self.graph
            .write_query("MATCH (d:Document {doc_id: $doc_id}) DELETE d", params)
            .await?;
        self.vector
            .delete_by_filter(DOC_CHUNKS, doc_id_filter(doc_id))
            .await?;
        Ok(())
    }
}

/// Qdrant filter selecting every `doc_chunks` point of one document.
fn doc_id_filter(doc_id: &str) -> serde_json::Value {
    serde_json::json!({"must": [{"key": "doc_id", "match": {"value": doc_id}}]})
}

/// §6.2/I7 one-off migration, run once per process on first consolidation.
/// Memgraph errors on re-creating an existing index/constraint — tolerated
/// (debug-logged) so this is safe to call on every process start.
async fn ensure_schema(graph: &Arc<dyn GraphRepository>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.get().is_some() {
        return;
    }
    for stmt in [
        "CREATE INDEX ON :Entity(entity_id)",
        "CREATE CONSTRAINT ON (e:Entity) ASSERT e.entity_id IS UNIQUE",
    ] {
        if let Err(e) = graph.write_query(stmt, HashMap::new()).await {
            tracing::debug!("consolidate schema migration skipped ({e})");
        }
    }
    let _ = ONCE.set(());
}

/// §6.4 `SAME_AS` manual entry point. Auto-triggering is unreachable in the
/// current flow (second-level merges never produce twin nodes); this is the
/// human correction entry.
pub async fn link_same_as(
    graph: &Arc<dyn GraphRepository>,
    from_id: &str,
    to_id: &str,
    score: f32,
    created_by: &str,
    reason: &str,
) -> Result<(), DtError> {
    // §6.4：单向一条即可，查询时按无向对待 MATCH (a)-[:SAME_AS]-(b)。
    let query = "MATCH (a:Entity {entity_id: $from_id}), (b:Entity {entity_id: $to_id}) \
                 MERGE (a)-[r:SAME_AS]->(b) \
                 SET r.score = $score, r.created_by = $created_by, \
                     r.reason = $reason, r.created_at = datetime()";
    let mut params = HashMap::new();
    params.insert("from_id".to_string(), serde_json::json!(from_id));
    params.insert("to_id".to_string(), serde_json::json!(to_id));
    params.insert("score".to_string(), serde_json::json!(score));
    params.insert("created_by".to_string(), serde_json::json!(created_by));
    params.insert("reason".to_string(), serde_json::json!(reason));
    graph.write_query(query, params).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::extract::model::ExtractedRelation;
    use crate::domain::types::{CollectionInfo, HealthStatus};
    use async_trait::async_trait;
    use std::sync::Mutex;

    // ── Pure helpers ─────────────────────────────────────────────────

    fn entity(canonical: &str, ty: EntityType) -> ExtractedEntity {
        ExtractedEntity {
            mention: format!("{canonical}(提及)"),
            canonical_name: canonical.to_string(),
            entity_type: ty,
            summary: format!("{canonical}的摘要"),
            keywords: vec!["关键词甲".to_string(), "关键词乙".to_string()],
        }
    }

    #[test]
    fn normalize_lowercases_and_trims() {
        assert_eq!(normalize("  IfCode  "), "ifcode");
    }

    #[test]
    fn normalize_full_width_to_half_width() {
        // ＩｆＣｏｄｅ (full-width) → ifcode
        assert_eq!(normalize("ＩｆＣｏｄｅ"), "ifcode");
        // full-width space U+3000 → space → percent-encoded
        assert_eq!(normalize("读\u{3000}写"), "读%20写");
    }

    #[test]
    fn normalize_percent_encodes_reserved_chars() {
        assert_eq!(normalize("/api/pay/route"), "%2Fapi%2Fpay%2Froute");
        assert_eq!(normalize("读/写分离"), "读%2F写分离");
        assert_eq!(normalize("a b"), "a%20b");
        assert_eq!(normalize("a#b"), "a%23b");
        assert_eq!(normalize("a?b"), "a%3Fb");
        // '%' itself must be encoded (and not double-handled)
        assert_eq!(normalize("100%"), "100%25");
        // An already-encoded sequence encodes the '%' again — correct,
        // because '%' is encoded first in a single pass. (Lowercasing runs
        // before encoding per spec §6.1, so the embedded hex lowercases too.)
        assert_eq!(normalize("a%2Fb"), "a%252fb");
    }

    #[test]
    fn normalize_does_not_collide_slash_with_underscore() {
        assert_ne!(normalize("读/写分离"), normalize("读_写分离"));
    }

    #[test]
    fn entity_id_structure() {
        let id = entity_id_for("offen-pay", EntityType::Channel, "ifCode");
        assert_eq!(id, "dt://entity/offen-pay/Channel/ifcode");
    }

    #[test]
    fn entity_id_encodes_slashes() {
        let id = entity_id_for("p", EntityType::Api, "/api/pay/route");
        assert_eq!(id, "dt://entity/p/Api/%2Fapi%2Fpay%2Froute");
    }

    #[test]
    fn entity_embed_text_format_is_exact() {
        let e = ExtractedEntity {
            mention: "m".into(),
            canonical_name: "ifCode".into(),
            entity_type: EntityType::Channel,
            summary: "渠道路由字段".into(),
            keywords: vec!["路由".into(), "支付".into()],
        };
        assert_eq!(
            entity_embed_text(&e),
            "ifCode。渠道路由字段。关键词: 路由 支付"
        );
    }

    #[test]
    fn entity_embed_text_empty_keywords() {
        let e = ExtractedEntity {
            keywords: vec![],
            ..entity("支付网关", EntityType::Service)
        };
        assert_eq!(entity_embed_text(&e), "支付网关。支付网关的摘要。关键词: ");
    }

    // ── Mock backends ────────────────────────────────────────────────

    /// Records every Cypher (and its params) so tests can assert exact
    /// statements; scripted read responses keyed by substring match.
    struct MockGraph {
        writes: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
        read_handlers: Vec<(String, serde_json::Value)>,
    }

    impl MockGraph {
        fn new(read_handlers: Vec<(String, serde_json::Value)>) -> Self {
            Self {
                writes: Mutex::new(vec![]),
                read_handlers,
            }
        }

        fn writes(&self) -> Vec<(String, HashMap<String, serde_json::Value>)> {
            self.writes.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            for (needle, response) in &self.read_handlers {
                if query.contains(needle.as_str()) {
                    return Ok(response.clone());
                }
            }
            Ok(serde_json::json!([]))
        }

        async fn write_query(
            &self,
            query: &str,
            params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.writes
                .lock()
                .unwrap()
                .push((query.to_string(), params));
            // MERGE ... RETURN elementId(e) AS eid
            if query.contains("RETURN elementId(e)") {
                return Ok(serde_json::json!([{"eid": "4:0:999"}]));
            }
            Ok(serde_json::json!([]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// Returns vectors derived from the input text length so tests can tell
    /// embeddings apart; records every batch.
    struct MockEmbed {
        batches: Mutex<Vec<Vec<String>>>,
    }

    impl MockEmbed {
        fn new() -> Self {
            Self {
                batches: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            self.batches
                .lock()
                .unwrap()
                .push(texts.iter().map(|s| s.to_string()).collect());
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32, 1.0, 0.0])
                .collect())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// Scripted near-neighbour hits; records searches and upserts.
    struct MockVector {
        hits: Mutex<Vec<serde_json::Value>>,
        searches: Mutex<Vec<(String, u64)>>,
        upserts: Mutex<Vec<(String, Vec<serde_json::Value>)>>,
        deleted_filters: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockVector {
        fn new() -> Self {
            Self {
                hits: Mutex::new(vec![]),
                searches: Mutex::new(vec![]),
                upserts: Mutex::new(vec![]),
                deleted_filters: Mutex::new(vec![]),
            }
        }

        fn with_hits(self, hits: Vec<serde_json::Value>) -> Self {
            *self.hits.lock().unwrap() = hits;
            self
        }
    }

    #[async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }

        async fn search(
            &self,
            collection: &str,
            _v: Vec<f32>,
            limit: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            self.searches
                .lock()
                .unwrap()
                .push((collection.to_string(), limit));
            Ok(self.hits.lock().unwrap().clone())
        }

        async fn upsert(
            &self,
            collection: &str,
            points: Vec<serde_json::Value>,
        ) -> Result<(), DtError> {
            self.upserts
                .lock()
                .unwrap()
                .push((collection.to_string(), points));
            Ok(())
        }

        async fn delete_by_filter(
            &self,
            collection: &str,
            filter: serde_json::Value,
        ) -> Result<(), DtError> {
            self.deleted_filters
                .lock()
                .unwrap()
                .push((collection.to_string(), filter));
            Ok(())
        }

        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }

        async fn collection_info(&self, n: &str) -> Result<CollectionInfo, DtError> {
            Ok(CollectionInfo {
                name: n.to_string(),
                points_count: 0,
                vector_dim: 1024,
                model_version: "bge-m3".into(),
            })
        }

        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    // ── consolidate_document scenarios ───────────────────────────────

    fn graph_block(
        entities: Vec<ExtractedEntity>,
        relations: Vec<ExtractedRelation>,
    ) -> ExtractedGraph {
        ExtractedGraph {
            doc_id: "dt://doc/proj/a.md".into(),
            block_index: 0,
            block_summary: "块摘要".into(),
            entities,
            relations,
            degraded: false,
        }
    }

    fn block_texts() -> HashMap<u32, String> {
        HashMap::from([(0u32, "原文块文本".to_string())])
    }

    #[tokio::test]
    async fn first_level_exact_hit_short_circuits_vector_search() {
        // Entity already exists in graph (first-level batch check hits) →
        // no vector search may happen at all.
        let derived = entity_id_for("proj", EntityType::Service, "支付网关");
        let graph = Arc::new(MockGraph::new(vec![(
            "MATCH (e:Entity {entity_id: eid})".to_string(),
            serde_json::json!([{"entity_id": derived}]),
        )]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("支付网关", EntityType::Service)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.entities_merged, 1);
        assert_eq!(stats.entities_created, 0);
        assert!(
            vector.searches.lock().unwrap().is_empty(),
            "first-level hit must not trigger a vector search"
        );
    }

    #[tokio::test]
    async fn second_level_high_score_same_type_merges_to_existing_id() {
        let existing_id = "dt://entity/proj/Service/支付服务网关";
        let vector = Arc::new(MockVector::new().with_hits(vec![serde_json::json!({
            "id": "abc",
            "score": 0.95,
            "payload": {"business_id": existing_id, "type": "Service", "project": "proj"}
        })]));
        let graph = Arc::new(MockGraph::new(vec![]));
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("支付网关", EntityType::Service)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.entities_merged, 1);
        assert_eq!(stats.entities_created, 0);

        // The Entity MERGE must target the EXISTING entity_id (the merge
        // target), not the freshly derived one.
        let writes = graph.writes();
        let merge = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (e:Entity {entity_id: $entity_id})"))
            .expect("entity merge must run");
        assert_eq!(
            merge.1.get("entity_id").and_then(|v| v.as_str()),
            Some(existing_id)
        );

        // kg_nodes point id must derive from the merged business_id.
        let upserts = vector.upserts.lock().unwrap();
        let kg = upserts
            .iter()
            .find(|(cname, _)| cname == KG_NODES)
            .expect("kg_nodes upsert");
        assert_eq!(
            kg.1.len(),
            1,
            "entity upsert must be per-entity, not batched"
        );
        assert_eq!(
            kg.1[0]["payload"]["business_id"].as_str(),
            Some(existing_id)
        );
        assert_eq!(kg.1[0]["payload"]["origin"].as_str(), Some("extracted"));
    }

    #[tokio::test]
    async fn second_level_low_score_creates_new() {
        let vector = Arc::new(MockVector::new().with_hits(vec![serde_json::json!({
            "score": 0.90, // below the 0.92 threshold
            "payload": {"business_id": "dt://entity/proj/Service/other", "type": "Service", "project": "proj"}
        })]));
        let graph = Arc::new(MockGraph::new(vec![]));
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("支付网关", EntityType::Service)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.entities_created, 1);
        assert_eq!(stats.entities_merged, 0);
    }

    #[tokio::test]
    async fn second_level_type_mismatch_creates_new() {
        let vector = Arc::new(MockVector::new().with_hits(vec![serde_json::json!({
            "score": 0.99,
            "payload": {"business_id": "dt://entity/proj/Channel/支付网关", "type": "Channel", "project": "proj"}
        })]));
        let graph = Arc::new(MockGraph::new(vec![]));
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("支付网关", EntityType::Service)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.entities_created, 1);
    }

    #[tokio::test]
    async fn relation_endpoints_resolve_through_block_map() {
        // "支付网关" merges into an existing node; the relation must use the
        // MERGED id as its endpoint, not the derived one.
        let merged_id = "dt://entity/proj/Service/支付服务网关";
        let tail_id = entity_id_for("proj", EntityType::Channel, "银联渠道");
        let vector = Arc::new(MockVector::new().with_hits(vec![serde_json::json!({
            "score": 0.97,
            "payload": {"business_id": merged_id, "type": "Service", "project": "proj"}
        })]));
        let graph = Arc::new(MockGraph::new(vec![]));
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![
                        entity("支付网关", EntityType::Service),
                        entity("银联渠道", EntityType::Channel),
                    ],
                    vec![ExtractedRelation {
                        head: "支付网关".into(),
                        relation: "routes_to".into(),
                        tail: "银联渠道".into(),
                        evidence: Some("支付网关路由到银联".into()),
                        confidence: Some(0.9),
                    }],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.relations_written, 1);
        assert_eq!(stats.relations_orphaned, 0);

        let writes = graph.writes();
        let rel = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (h)-[r:RELATES"))
            .expect("RELATES merge must run");
        assert_eq!(
            rel.1.get("head_id").and_then(|v| v.as_str()),
            Some(merged_id),
            "head must come from the disambiguation map (merged id)"
        );
        assert_eq!(
            rel.1.get("tail_id").and_then(|v| v.as_str()),
            Some(tail_id.as_str())
        );
        assert_eq!(
            rel.1.get("rel_type").and_then(|v| v.as_str()),
            Some("routes_to")
        );
        assert_eq!(
            rel.1.get("doc_id").and_then(|v| v.as_str()),
            Some("dt://doc/proj/a.md")
        );
        assert_eq!(
            rel.1.get("evidence").and_then(|v| v.as_str()),
            Some("支付网关路由到银联")
        );
        assert_eq!(rel.1.get("confidence").and_then(|v| v.as_f64()), Some(0.9));
    }

    #[tokio::test]
    async fn relation_field_normalisation_defaults() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![
                        entity("甲", EntityType::Service),
                        entity("乙", EntityType::Channel),
                    ],
                    vec![ExtractedRelation {
                        head: "甲".into(),
                        relation: "depends_on".into(),
                        tail: "乙".into(),
                        evidence: None,   // → unwrap_or_default → ""
                        confidence: None, // → unwrap_or(0.5)
                    }],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let writes = graph.writes();
        let rel = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (h)-[r:RELATES"))
            .unwrap();
        assert_eq!(rel.1.get("evidence").and_then(|v| v.as_str()), Some(""));
        assert_eq!(rel.1.get("confidence").and_then(|v| v.as_f64()), Some(0.5));
    }

    #[tokio::test]
    async fn relation_unresolvable_endpoint_is_orphaned() {
        // The relation endpoint "幽灵" was not extracted as an entity and
        // the historical-node fallback finds nothing → orphan.
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("甲", EntityType::Service)],
                    vec![ExtractedRelation {
                        head: "甲".into(),
                        relation: "depends_on".into(),
                        tail: "幽灵".into(),
                        evidence: None,
                        confidence: None,
                    }],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.relations_written, 0);
        assert_eq!(stats.relations_orphaned, 1);
    }

    #[tokio::test]
    async fn relation_fallback_resolves_historical_node() {
        // Endpoint not in block map, but a historical node exists whose
        // entity_id ends with the normalised canonical.
        let historical = "dt://entity/proj/Service/老节点";
        let graph = Arc::new(MockGraph::new(vec![(
            "ENDS WITH".to_string(),
            serde_json::json!([{"entity_id": historical}]),
        )]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("甲", EntityType::Service)],
                    vec![ExtractedRelation {
                        head: "甲".into(),
                        relation: "depends_on".into(),
                        tail: "老节点".into(),
                        evidence: None,
                        confidence: None,
                    }],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.relations_written, 1);
        let writes = graph.writes();
        let rel = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (h)-[r:RELATES"))
            .unwrap();
        assert_eq!(
            rel.1.get("tail_id").and_then(|v| v.as_str()),
            Some(historical)
        );
    }

    #[tokio::test]
    async fn degraded_block_writes_no_entities_and_marks_payload() {
        let mut block = graph_block(vec![], vec![]);
        block.degraded = true;
        block.block_summary = String::new();

        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[block],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.degraded_blocks, 1);
        assert_eq!(stats.blocks_processed, 1);
        assert_eq!(stats.entities_created, 0);

        // doc_chunks point: degraded=true, text = raw chunk only, and the
        // embed text must be exactly the raw chunk (no summary prefix).
        let batches = embed.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec!["原文块文本".to_string()]);

        let upserts = vector.upserts.lock().unwrap();
        let doc = upserts
            .iter()
            .find(|(cname, _)| cname == DOC_CHUNKS)
            .expect("doc_chunks upsert");
        assert_eq!(doc.1[0]["payload"]["degraded"], serde_json::json!(true));
        assert_eq!(doc.1[0]["payload"]["text"].as_str(), Some("原文块文本"));
        assert_eq!(
            doc.1[0]["payload"]["entity_ids"].as_array().unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn empty_block_is_observed_not_dropped() {
        // Non-degraded but everything empty → warn + empty_blocks += 1,
        // still processed (doc_chunks point written).
        let mut block = graph_block(vec![], vec![]);
        block.block_summary = String::new();

        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let stats = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[block],
                &block_texts(),
            )
            .await
            .unwrap();

        assert_eq!(stats.empty_blocks, 1);
        assert_eq!(stats.degraded_blocks, 0);
        assert_eq!(stats.blocks_processed, 1);
    }

    #[tokio::test]
    async fn kg_nodes_payload_matches_schema() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "offen-pay",
                "dt://doc/offen-pay/pay-design.md",
                "pay-design.md",
                "markdown",
                &[graph_block(
                    vec![entity("ifCode", EntityType::Channel)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let upserts = vector.upserts.lock().unwrap();
        let kg = upserts.iter().find(|(cname, _)| cname == KG_NODES).unwrap();
        let payload = &kg.1[0]["payload"];

        assert_eq!(payload["elementId"].as_str(), Some("4:0:999"));
        assert_eq!(
            payload["business_id"].as_str(),
            Some("dt://entity/offen-pay/Channel/ifcode")
        );
        assert_eq!(payload["name"].as_str(), Some("ifCode"));
        assert_eq!(payload["type"].as_str(), Some("Channel"));
        assert_eq!(payload["summary"].as_str(), Some("ifCode的摘要"));
        assert_eq!(
            payload["keywords"].as_array().unwrap(),
            &vec![serde_json::json!("关键词甲"), serde_json::json!("关键词乙")]
        );
        assert_eq!(payload["project"].as_str(), Some("offen-pay"));
        assert_eq!(
            payload["labels"].as_array().unwrap()[0].as_str(),
            Some("Entity")
        );
        assert_eq!(
            payload["doc_id"].as_str(),
            Some("dt://doc/offen-pay/pay-design.md")
        );
        assert_eq!(payload["origin"].as_str(), Some("extracted"));
        assert_eq!(payload["source"].as_str(), Some("kg"));
    }

    #[tokio::test]
    async fn doc_chunks_payload_matches_schema() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("ifCode", EntityType::Channel)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let upserts = vector.upserts.lock().unwrap();
        let doc = upserts
            .iter()
            .find(|(cname, _)| cname == DOC_CHUNKS)
            .unwrap();
        let payload = &doc.1[0]["payload"];

        assert_eq!(payload["doc_id"].as_str(), Some("dt://doc/proj/a.md"));
        assert_eq!(payload["block_index"].as_u64(), Some(0));
        assert_eq!(payload["project"].as_str(), Some("proj"));
        assert_eq!(
            payload["entity_ids"].as_array().unwrap(),
            &vec![serde_json::json!("dt://entity/proj/Channel/ifcode")]
        );
        assert_eq!(payload["degraded"], serde_json::json!(false));
        assert_eq!(payload["source"].as_str(), Some("doc"));
        assert_eq!(payload["text"].as_str(), Some("原文块文本"));

        // point id determinism: make_point_id("{doc_id}:{block_index}")
        let expected_pid =
            crate::application::sync::kg_bridge::make_point_id("dt://doc/proj/a.md:0");
        assert_eq!(doc.1[0]["id"].as_str(), Some(expected_pid.as_str()));
    }

    #[tokio::test]
    async fn entry_purge_runs_before_writes() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(vec![entity("甲", EntityType::Service)], vec![])],
                &block_texts(),
            )
            .await
            .unwrap();

        let writes = graph.writes();
        let purge_relates = writes
            .iter()
            .position(|(q, _)| q.contains("MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r"))
            .expect("RELATES purge must run");
        let purge_mentioned = writes
            .iter()
            .position(|(q, _)| {
                q.contains("MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m")
            })
            .expect("MENTIONED_IN purge must run");
        let first_entity_write = writes
            .iter()
            .position(|(q, _)| q.contains("MERGE (e:Entity {entity_id: $entity_id})"))
            .expect("entity write must run");
        assert!(
            purge_relates < first_entity_write && purge_mentioned < first_entity_write,
            "entry purge must run before any entity write"
        );

        // Old doc_chunks vector points must be deleted by doc_id filter.
        let deletes = vector.deleted_filters.lock().unwrap();
        assert!(
            deletes.iter().any(|(cname, f)| cname == DOC_CHUNKS
                && f.to_string().contains("doc_id")
                && f.to_string().contains("dt://doc/proj/a.md")),
            "doc_chunks purge by doc_id must run; got: {:?}",
            *deletes
        );
    }

    #[tokio::test]
    async fn document_node_merged_before_mentioned_in() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(vec![entity("甲", EntityType::Service)], vec![])],
                &block_texts(),
            )
            .await
            .unwrap();

        let writes = graph.writes();
        let doc_merge = writes
            .iter()
            .position(|(q, _)| q.contains("MERGE (d:Document {doc_id: $doc_id})"))
            .expect("document merge must run");
        let mentioned = writes
            .iter()
            .position(|(q, _)| q.contains("MENTIONED_IN") && q.contains("MERGE"))
            .expect("MENTIONED_IN must run");
        assert!(doc_merge < mentioned);

        let (_, params) = &writes[doc_merge];
        assert_eq!(params.get("project").and_then(|v| v.as_str()), Some("proj"));
        assert_eq!(
            params.get("file_path").and_then(|v| v.as_str()),
            Some("a.md")
        );
        assert_eq!(
            params.get("doc_type").and_then(|v| v.as_str()),
            Some("markdown")
        );
    }

    #[tokio::test]
    async fn entity_merge_uses_exact_schema_cypher() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![entity("ifCode", EntityType::Channel)],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let writes = graph.writes();
        let (q, params) = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (e:Entity {entity_id: $entity_id})"))
            .unwrap();
        // §6.2 exact statement shape: aliases/keywords REDUCE merge on match.
        assert!(q.contains("e.aliases = REDUCE(acc = coalesce(e.aliases, []), x IN $new_aliases |"));
        assert!(q.contains("e.keywords = REDUCE(kacc = coalesce(e.keywords, []), x IN $keywords |"));
        assert_eq!(params.get("name").and_then(|v| v.as_str()), Some("ifCode"));
        assert_eq!(params.get("type").and_then(|v| v.as_str()), Some("Channel"));
        assert_eq!(
            params
                .get("new_aliases")
                .and_then(|v| v.as_array())
                .unwrap(),
            &vec![serde_json::json!("ifCode(提及)")]
        );
    }

    #[tokio::test]
    async fn mentioned_in_edge_written_per_entity() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![
                        entity("甲", EntityType::Service),
                        entity("乙", EntityType::Channel),
                    ],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let writes = graph.writes();
        let mentioned = writes
            .iter()
            .find(|(q, _)| q.contains("MENTIONED_IN") && q.contains("MERGE"))
            .expect("MENTIONED_IN must run");
        let ids: Vec<&str> = mentioned
            .1
            .get("ids")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&entity_id_for("proj", EntityType::Service, "甲").as_str()));
        assert!(ids.contains(&entity_id_for("proj", EntityType::Channel, "乙").as_str()));
    }

    #[tokio::test]
    async fn entity_vector_upsert_is_per_entity_not_batched() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        let _ = c
            .consolidate_document(
                "proj",
                "dt://doc/proj/a.md",
                "a.md",
                "markdown",
                &[graph_block(
                    vec![
                        entity("甲", EntityType::Service),
                        entity("乙", EntityType::Channel),
                        entity("丙", EntityType::Api),
                    ],
                    vec![],
                )],
                &block_texts(),
            )
            .await
            .unwrap();

        let upserts = vector.upserts.lock().unwrap();
        let kg_upserts: Vec<_> = upserts
            .iter()
            .filter(|(cname, _)| cname == KG_NODES)
            .collect();
        assert_eq!(kg_upserts.len(), 3, "one upsert call per entity");
        for (_, points) in kg_upserts {
            assert_eq!(points.len(), 1);
        }
        // _kg_synced_at marked per entity after successful upsert.
        let writes = graph.writes();
        let marks = writes
            .iter()
            .filter(|(q, _)| q.contains("_kg_synced_at"))
            .count();
        assert_eq!(marks, 3);
    }

    #[tokio::test]
    async fn purge_document_removes_edges_node_and_vectors() {
        let graph = Arc::new(MockGraph::new(vec![]));
        let vector = Arc::new(MockVector::new());
        let embed = Arc::new(MockEmbed::new());
        let c = Consolidator::new(graph.clone(), vector.clone(), embed.clone());

        c.purge_document("dt://doc/proj/gone.md").await.unwrap();

        let writes = graph.writes();
        assert!(writes
            .iter()
            .any(|(q, _)| q.contains("MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r")));
        assert!(writes.iter().any(|(q, _)| q
            .contains("MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m")));
        assert!(writes
            .iter()
            .any(|(q, _)| q.contains("MATCH (d:Document {doc_id: $doc_id}) DELETE d")));

        let deletes = vector.deleted_filters.lock().unwrap();
        assert!(deletes
            .iter()
            .any(|(cname, f)| cname == DOC_CHUNKS && f.to_string().contains("gone.md")));
    }

    #[tokio::test]
    async fn link_same_as_writes_single_directional_edge() {
        let mock = Arc::new(MockGraph::new(vec![]));
        let graph: Arc<dyn GraphRepository> = mock.clone();
        link_same_as(
            &graph,
            "dt://entity/p/Service/a",
            "dt://entity/p/Service/b",
            1.0,
            "manual",
            "人工确认同一实体",
        )
        .await
        .unwrap();

        let writes = mock.writes();
        let (q, params) = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (a)-[r:SAME_AS]->(b)"))
            .expect("SAME_AS merge must run");
        assert!(q.contains("r.created_by = $created_by"));
        assert_eq!(params.get("score").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(
            params.get("created_by").and_then(|v| v.as_str()),
            Some("manual")
        );
    }
}
