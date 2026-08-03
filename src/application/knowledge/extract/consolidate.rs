//! Consolidate 整合层——通用知识管线的第二阶段
//! （抽取 → 整合 → 检索），方案 §6。
//!
//! 消费 `Vec<ExtractedGraph>`（逐文档块的抽取输出）并：
//!
//! 1. **规范化** canonical name（转小写、修剪、全角→半角、
//!    对 URI 保留字符做百分号编码），得到稳定的 `entity_id`。
//! 2. **两级消歧**（§6.1）：精确 `entity_id` 短路，
//!    再向量近邻合并（score > 0.92 且类型一致）。
//! 3. **图写入**（§6.2）：`Document` / `Entity` / `RELATES` /
//!    `MENTIONED_IN` 通过四次独立的 `write_query` 调用（最终一致，
//!    无事务包装）。
//! 4. **双重向量写入**（§6.3）：实体 → `kg_nodes`（逐实体、写透，
//!    使消歧能立即看到）、块 → `doc_chunks`。
//! 5. **生命周期自治**（§6.5）：写入前先清空该文档的旧边/向量；
//!    删除文档走 [`purge_document`]。
//!
//! 此处遵守的硬约束：
//! - 消歧查询与实体入库共享同一个文本构造函数 [`entity_embed_text`]。
//! - 向量 upsert 逐实体进行，绝不批量（§12.1：消歧依赖立即可检索的写入）。
//! - 关系端点通过逐块的 `canonical_name → entity_id` 映射解析，
//!   绝不从 canonical 文本重新推导。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::shared::collections::{DOC_CHUNKS, KG_NODES, VECTOR_DIM};

use super::model::{EntityType, ExtractedEntity, ExtractedGraph};

/// 第二级（向量）消歧的余弦相似度阈值。
const MERGE_SCORE_THRESHOLD: f32 = 0.92;

/// 本层写入实体的 `kg_nodes` payload `origin` 值。
const ORIGIN_EXTRACTED: &str = "extracted";

// ---------------------------------------------------------------------------
// 公共统计
// ---------------------------------------------------------------------------

/// [`Consolidator::consolidate_document`] 产出的计数器；经 store
/// 处理器输出上浮到管线引擎的构建报告（R9）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidateStats {
    /// 合并进既有图节点的实体数（短路或向量命中）。
    pub entities_merged: usize,
    /// 创建了全新图节点的实体数。
    pub entities_created: usize,
    /// 成功写入的 `RELATES` 边数。
    pub relations_written: usize,
    /// 因端点无法解析而被丢弃的关系数。
    pub relations_orphaned: usize,
    /// 携带 §5.5 降级标记的块数。
    pub degraded_blocks: usize,
    /// 处理的块总数。
    pub blocks_processed: usize,
    /// entities/relations/summary 全空的非降级块（仅观测）。
    pub empty_blocks: usize,
}

// ---------------------------------------------------------------------------
// 文本规范化（§6.1）
// ---------------------------------------------------------------------------

/// 将 canonical name 规范化为 `entity_id` 的路径段：
/// 全角→半角、修剪、转小写，然后对 URI 保留字符做百分号编码
/// （`%` 最先处理——通过单遍字符处理实现，替换串绝不会被重扫）。
pub fn normalize(name: &str) -> String {
    let half: String = name.chars().map(to_half_width).collect();
    let lowered = half.trim().to_lowercase();
    percent_encode(&lowered)
}

/// 将全角 ASCII 变体（U+FF01..U+FF5E）与全角空格（U+3000）
/// 映射为半角等价物。
fn to_half_width(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        c if ('\u{FF01}'..='\u{FF5E}').contains(&c) => {
            char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
        }
        c => c,
    }
}

/// 对 URI 保留字符做百分号编码，使自由格式的 canonical name
/// 永远无法向 `entity_id` 注入额外的 URI 段。
///
/// 特意采用百分号编码（而非字符替换）：替换会使
/// "读/写分离" 与 "读_写分离" 碰撞成同一个 ID。
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

/// 推导抽取实体的稳定业务主键（§6.1）。
/// `{type}` 是枚举变体名（如 `Channel`）。
pub fn entity_id_for(project: &str, entity_type: EntityType, canonical_name: &str) -> String {
    format!(
        "dt://entity/{}/{}/{}",
        project,
        entity_type.as_str(),
        normalize(canonical_name)
    )
}

/// §6.1 消歧查询与 §6.3 实体入库共享的唯一文本构造函数——
/// 切勿在别处重复此格式化逻辑。
pub fn entity_embed_text(e: &ExtractedEntity) -> String {
    format!(
        "{}。{}。关键词: {}",
        e.canonical_name,
        e.summary,
        e.keywords.join(" ")
    )
}

// ---------------------------------------------------------------------------
// Consolidator（整合器）
// ---------------------------------------------------------------------------

/// Consolidate 层的工作器。持有三个后端句柄；每次调用的
/// 状态（块映射、统计）放在栈上。
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

    /// 将一个文档的抽取图整合进 KG 与向量库。
    ///
    /// `block_texts` 映射 `block_index` → 原始块文本（来自分块
    /// 处理器输出），用于 `doc_chunks` 双重写入。
    pub async fn consolidate_document(
        &self,
        project: &str,
        doc_id: &str,
        file_path: &str,
        doc_type: &str,
        graphs: &[ExtractedGraph],
        block_texts: &HashMap<u32, String>,
    ) -> Result<ConsolidateStats, DtError> {
        ensure_schema(&self.graph, &self.vector).await;

        let mut stats = ConsolidateStats::default();

        // ── §6.5 入口清理（自治、幂等）：写入任何新内容前，
        //    先清掉该文档的旧边与旧 doc_chunks 点。
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

        // ── §6.2.0 Document 节点：在任何 MENTIONED_IN 写入前先 MERGE。
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
                // 任务 1 遗留的小问题：静默为空的成功块——
                // 仅观测（不丢弃、不降级）。
                stats.empty_blocks += 1;
                tracing::warn!(
                    "consolidate: 非降级空块 doc={} block_index={}",
                    doc_id,
                    block.block_index
                );
            }

            // canonical_name → 消歧后的 entity_id，逐实体登记
            // （硬约束：关系端点通过此映射解析）。
            let mut block_map: HashMap<String, String> = HashMap::new();

            if !block.entities.is_empty() {
                // ── 第一级：批量精确 ID 检查（每块一次读）。
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

                // ── §6.3 embed 解耦：整块一次批量向量化，
                //    然后逐实体 search → write → upsert。
                let embed_texts: Vec<String> =
                    block.entities.iter().map(entity_embed_text).collect();
                let vectors = self.embed.embed_batch(&embed_texts).await?;

                for (entity, vector) in block.entities.iter().zip(vectors.iter()) {
                    let derived_id =
                        entity_id_for(project, entity.entity_type, &entity.canonical_name);

                    let (final_id, created) = if existing.contains(&derived_id) {
                        (derived_id, false) // 第一级短路合并
                    } else {
                        // ── 第二级：向量近邻合并。
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

                    // ── §6.2.1 Entity MERGE（返回 elementId 供 payload 使用）。
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

                    // ── §6.3 实体 → kg_nodes 写透（逐实体，
                    //    绝不批量——§12.1）。
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

                    // 图与向量都成功后才标记 _kg_synced_at。
                    let mut mp = HashMap::new();
                    mp.insert("entity_id".to_string(), serde_json::json!(final_id));
                    self.graph
                        .write_query(
                            "MATCH (e:Entity {entity_id: $entity_id}) \
                             SET e._kg_synced_at = datetime()",
                            mp,
                        )
                        .await?;

                    // 同时登记原始与规范化后的 canonical 作为
                    // 查找键——关系端点中 LLM 的大小写/空白差异
                    // 绝不能落到回退分支。
                    block_map.insert(entity.canonical_name.clone(), final_id.clone());
                    block_map
                        .entry(normalize(&entity.canonical_name))
                        .or_insert(final_id);
                }

                // ── §6.2.3 MENTIONED_IN 溯源（每块批量）。
                let ids: Vec<String> = block_map
                    .values()
                    .cloned()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
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

            // ── §6.2.2 关系：端点先经块映射，再走历史节点
            //    回退，否则记为孤儿（记日志 + 计数，不建占位节点）。
            for relation in &block.relations {
                let head_id = self
                    .resolve_endpoint(project, &block_map, &relation.head)
                    .await?;
                let tail_id = self
                    .resolve_endpoint(project, &block_map, &relation.tail)
                    .await?;
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
                // f32→f64 加宽有精度损失（0.9f32 → 0.899999976…）；
                // 在 f64 空间四舍五入，使存储的 confidence 干净（精确 0.9）。
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

            // ── §6.3 块 → doc_chunks 双重写入（每块）。
            let raw_text = block_texts
                .get(&block.block_index)
                .cloned()
                .unwrap_or_default();
            let block_embed_text = if block.block_summary.is_empty() {
                raw_text.clone() // 无摘要可前置（按 §5.5，降级块摘要恒为空）
            } else {
                format!("{}\n{}", block.block_summary, raw_text)
            };
            let block_vector = self.embed.embed_batch(&[block_embed_text]).await?;
            let entity_ids: Vec<String> = block_map
                .values()
                .cloned()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
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

    /// 解析关系端点：先查逐块消歧映射（原始键，再规范化键），
    /// 再做项目范围内的历史节点后缀查找（端点可能是旧构建的节点），
    /// 否则返回 `None`（调用方计为孤儿关系）。
    async fn resolve_endpoint(
        &self,
        project: &str,
        block_map: &HashMap<String, String>,
        canonical: &str,
    ) -> Result<Option<String>, DtError> {
        if let Some(id) = block_map
            .get(canonical)
            .or_else(|| block_map.get(normalize(canonical).as_str()))
        {
            return Ok(Some(id.clone()));
        }
        // `/` 锚定的后缀：canonical "网关" 不得匹配 "支付网关"；
        // 项目范围：绝不解析到其他项目的同名节点。
        let mut params = HashMap::new();
        params.insert(
            "prefix".to_string(),
            serde_json::json!(format!("dt://entity/{project}/")),
        );
        params.insert(
            "suffix".to_string(),
            serde_json::json!(format!("/{}", normalize(canonical))),
        );
        let rows = self
            .graph
            .read_query(
                "MATCH (e:Entity) \
                 WHERE e.entity_id STARTS WITH $prefix AND e.entity_id ENDS WITH $suffix \
                 RETURN e.entity_id AS entity_id LIMIT 1",
                params,
            )
            .await?;
        let id = rows
            .as_array()
            .and_then(|r| r.first()?.get("entity_id")?.as_str().map(String::from));
        Ok(id)
    }

    /// §6.5.2——文档被删除：移除其 `RELATES`/`MENTIONED_IN` 边、
    /// `Document` 节点本身以及该文档的所有 `doc_chunks` 向量点。
    /// 实体节点只要被别处引用就保留。
    ///
    /// 委托给自由函数 [`purge_document`]，使构建编排层（任务 3）
    /// 无需构造完整 `Consolidator` 即可清理。
    pub async fn purge_document(&self, doc_id: &str) -> Result<(), DtError> {
        purge_document(self.graph.as_ref(), self.vector.as_ref(), doc_id).await
    }
}

/// §6.5.2——清理一个文档的全部产物：其 `RELATES` 边、`MENTIONED_IN`
/// 溯源边、`Document` 节点及其所有 `doc_chunks` 向量点。
/// 实体节点只要被别处引用就保留；孤儿实体由 §6.5.4 定期清理处理。
///
/// 自由函数：构建编排层通过此入口消费 `deleted_paths`，无需
/// embed 服务（清理从不做向量化）。幂等——对同一 `doc_id` 可安全重跑。
pub async fn purge_document(
    graph: &dyn GraphRepository,
    vector: &dyn VectorRepository,
    doc_id: &str,
) -> Result<(), DtError> {
    let mut params = HashMap::new();
    params.insert("doc_id".to_string(), serde_json::json!(doc_id));
    graph
        .write_query(
            "MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r",
            params.clone(),
        )
        .await?;
    graph
        .write_query(
            "MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m",
            params.clone(),
        )
        .await?;
    graph
        .write_query("MATCH (d:Document {doc_id: $doc_id}) DELETE d", params)
        .await?;
    vector
        .delete_by_filter(DOC_CHUNKS, doc_id_filter(doc_id))
        .await?;
    Ok(())
}

/// 选中某文档所有 `doc_chunks` 点的 Qdrant 过滤器。
fn doc_id_filter(doc_id: &str) -> serde_json::Value {
    serde_json::json!({"must": [{"key": "doc_id", "match": {"value": doc_id}}]})
}

/// §6.2/I7 一次性迁移 + 集合预置，首次整合时每进程运行一次。
/// Memgraph 对重建已有索引/约束会报错——可容忍（debug 日志）。
/// 仅当至少一条语句成功后才置位 latch，因此瞬时后端故障会在
/// 下次调用重试，而不会在进程生命周期内被静默跳过。
async fn ensure_schema(graph: &Arc<dyn GraphRepository>, vector: &Arc<dyn VectorRepository>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.get().is_some() {
        return;
    }
    let mut any_ok = false;
    for stmt in [
        "CREATE INDEX ON :Entity(entity_id)",
        "CREATE CONSTRAINT ON (e:Entity) ASSERT e.entity_id IS UNIQUE",
    ] {
        match graph.write_query(stmt, HashMap::new()).await {
            Ok(_) => any_ok = true,
            Err(e) => tracing::debug!("consolidate schema 迁移已跳过（{e}）"),
        }
    }
    for collection in [KG_NODES, DOC_CHUNKS] {
        match vector.ensure_collection(collection, VECTOR_DIM).await {
            Ok(_) => any_ok = true,
            Err(e) => tracing::debug!("consolidate ensure_collection {collection} 已跳过（{e}）"),
        }
    }
    if any_ok {
        let _ = ONCE.set(());
    }
}

/// §6.4 `SAME_AS` 手动入口。当前流程中自动触发不可达
/// （第二级合并从不产生孪生节点）；这是人工纠正入口。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::extract::model::ExtractedRelation;
    use crate::domain::types::{CollectionInfo, HealthStatus};
    use async_trait::async_trait;
    use std::sync::Mutex;

    // ── 纯辅助函数 ──────────────────────────────────────────────────

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
        // ＩｆＣｏｄｅ（全角）→ ifcode
        assert_eq!(normalize("ＩｆＣｏｄｅ"), "ifcode");
        // 全角空格 U+3000 → 空格 → 百分号编码
        assert_eq!(normalize("读\u{3000}写"), "读%20写");
    }

    #[test]
    fn normalize_percent_encodes_reserved_chars() {
        assert_eq!(normalize("/api/pay/route"), "%2Fapi%2Fpay%2Froute");
        assert_eq!(normalize("读/写分离"), "读%2F写分离");
        assert_eq!(normalize("a b"), "a%20b");
        assert_eq!(normalize("a#b"), "a%23b");
        assert_eq!(normalize("a?b"), "a%3Fb");
        // '%' 自身必须被编码（且不会被重复处理）
        assert_eq!(normalize("100%"), "100%25");
        // 已编码的序列会再次编码 '%'——这是正确的，
        // 因为单遍处理中 '%' 最先被编码。（按规范 §6.1，转小写在
        // 编码之前执行，因此内嵌的十六进制也会一并转小写。）
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

    // ── Mock 后端 ───────────────────────────────────────────────────

    /// 记录每一条 Cypher（及其参数），供测试断言精确语句；
    /// 脚本化的读响应按子串匹配分发。
    struct MockGraph {
        writes: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
        reads: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
        read_handlers: Vec<(String, serde_json::Value)>,
    }

    impl MockGraph {
        fn new(read_handlers: Vec<(String, serde_json::Value)>) -> Self {
            Self {
                writes: Mutex::new(vec![]),
                reads: Mutex::new(vec![]),
                read_handlers,
            }
        }

        fn writes(&self) -> Vec<(String, HashMap<String, serde_json::Value>)> {
            self.writes.lock().unwrap().clone()
        }

        fn reads(&self) -> Vec<(String, HashMap<String, serde_json::Value>)> {
            self.reads.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.reads.lock().unwrap().push((query.to_string(), params));
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

    /// 返回由输入文本长度派生的向量，便于测试区分不同的
    /// 嵌入；并记录每一次批量。
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

    // ── consolidate_document 场景 ────────────────────────────────────

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
        // 实体已存在于图中（第一级批量检查命中）→
        // 不得发生任何向量搜索。
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
            "第一级命中不得触发向量搜索"
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

        // Entity MERGE 必须指向已存在的 entity_id（合并目标），
        // 而不是新推导出的那个。
        let writes = graph.writes();
        let merge = writes
            .iter()
            .find(|(q, _)| q.contains("MERGE (e:Entity {entity_id: $entity_id})"))
            .expect("实体合并必须执行");
        assert_eq!(
            merge.1.get("entity_id").and_then(|v| v.as_str()),
            Some(existing_id)
        );

        // kg_nodes 点 ID 必须由合并后的 business_id 推导。
        let upserts = vector.upserts.lock().unwrap();
        let kg = upserts
            .iter()
            .find(|(cname, _)| cname == KG_NODES)
            .expect("kg_nodes upsert 必须执行");
        assert_eq!(
            kg.1.len(),
            1,
            "实体 upsert 必须逐实体进行，不得批量"
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
            "score": 0.90, // 低于 0.92 阈值
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
        // "支付网关" 合并进既有节点；关系必须以合并后的
        // ID 为端点，而非新推导的那个。
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
            .expect("RELATES 合并必须执行");
        assert_eq!(
            rel.1.get("head_id").and_then(|v| v.as_str()),
            Some(merged_id),
            "head 必须来自消歧映射（合并后的 id）"
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
        // 关系端点 "幽灵" 未被抽取为实体，且历史节点
        // 回退也一无所获 → 记为孤儿。
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
        // 端点不在块映射中，但存在一个 entity_id 以规范化后的
        // canonical 结尾的历史节点。
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

        // 回退查找必须限定项目范围并以 `/` 锚定。
        let reads = graph.reads();
        let fallback = reads
            .iter()
            .find(|(q, _)| q.contains("ENDS WITH"))
            .expect("回退读必须执行");
        assert!(fallback.0.contains("STARTS WITH $prefix"));
        assert_eq!(
            fallback.1.get("prefix").and_then(|v| v.as_str()),
            Some("dt://entity/proj/")
        );
        assert_eq!(
            fallback.1.get("suffix").and_then(|v| v.as_str()),
            Some("/老节点")
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

        // doc_chunks 点：degraded=true，text 仅为原始块，
        // 且 embed 文本必须恰好是原始块（无摘要前缀）。
        let batches = embed.batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec!["原文块文本".to_string()]);

        let upserts = vector.upserts.lock().unwrap();
        let doc = upserts
            .iter()
            .find(|(cname, _)| cname == DOC_CHUNKS)
            .expect("doc_chunks upsert 必须执行");
        assert_eq!(doc.1[0]["payload"]["degraded"], serde_json::json!(true));
        assert_eq!(doc.1[0]["payload"]["text"].as_str(), Some("原文块文本"));
        assert_eq!(
            doc.1[0]["payload"]["entity_ids"].as_array().unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn empty_block_is_observed_not_dropped() {
        // 非降级但全空 → warn + empty_blocks += 1，
        // 仍会处理（写入 doc_chunks 点）。
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

        // 点 ID 确定性：make_point_id("{doc_id}:{block_index}")
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
            .expect("RELATES 清理必须执行");
        let purge_mentioned = writes
            .iter()
            .position(|(q, _)| {
                q.contains("MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m")
            })
            .expect("MENTIONED_IN 清理必须执行");
        let first_entity_write = writes
            .iter()
            .position(|(q, _)| q.contains("MERGE (e:Entity {entity_id: $entity_id})"))
            .expect("实体写入必须执行");
        assert!(
            purge_relates < first_entity_write && purge_mentioned < first_entity_write,
            "入口清理必须先于任何实体写入"
        );

        // 旧 doc_chunks 向量点必须按 doc_id 过滤器删除。
        let deletes = vector.deleted_filters.lock().unwrap();
        assert!(
            deletes.iter().any(|(cname, f)| cname == DOC_CHUNKS
                && f.to_string().contains("doc_id")
                && f.to_string().contains("dt://doc/proj/a.md")),
            "doc_chunks 必须按 doc_id 清理；实际：{:?}",
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
            .expect("文档合并必须执行");
        let mentioned = writes
            .iter()
            .position(|(q, _)| q.contains("MENTIONED_IN") && q.contains("MERGE"))
            .expect("MENTIONED_IN 必须执行");
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
        // §6.2 精确语句形态：命中时 aliases/keywords 用 REDUCE 合并。
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
            .expect("MENTIONED_IN 必须执行");
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
        assert_eq!(kg_upserts.len(), 3, "每个实体一次 upsert 调用");
        for (_, points) in kg_upserts {
            assert_eq!(points.len(), 1);
        }
        // 每次 upsert 成功后逐实体标记 _kg_synced_at。
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
            .expect("SAME_AS 合并必须执行");
        assert!(q.contains("r.created_by = $created_by"));
        assert_eq!(params.get("score").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(
            params.get("created_by").and_then(|v| v.as_str()),
            Some("manual")
        );
    }
}
