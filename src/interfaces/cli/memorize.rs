//! `dt memorize` 的 CLI 处理器——将结构化知识写入 KG。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::{
    DefaultKnowledgeService, KnowledgeService,
};
use crate::application::sync::batch::SyncAccumulator;
use crate::domain::traits::GraphRepository;

/// 处理 `dt memorize`——写入知识条目（Knowledge、Experience、Concept、Domain、Playbook）。
///
/// `graph` 必须由调用方预先连接。
/// `sync_acc` 将节点入队，供后台（非阻塞）同步到 Qdrant。
pub async fn handle_memorize(
    knowledge_type: String,
    entity_id: String,
    entity_type: Option<String>,
    project: Option<String>,
    details: String,
    graph: Option<Arc<dyn GraphRepository>>,
    sync_acc: Option<Arc<SyncAccumulator>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: memorize --type {knowledge_type} --entity-id {entity_id} --details {details}",
    );

    let project_name = project.as_deref().unwrap_or("unknown");
    let etype = entity_type.as_deref().unwrap_or(&knowledge_type);

    let svc = graph
        .as_ref()
        .map(|g| DefaultKnowledgeService::new(Arc::clone(g)));

    // 根据 knowledge_type 路由到正确的实体构造函数。
    match knowledge_type.to_lowercase().as_str() {
        "decision" | "knowledgeadded" | "environment" | "dependencies" => {
            let knowledge = crate::application::knowledge::knowledge::knowledge_from_details(
                &entity_id,
                etype,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_knowledge(&knowledge).await {
                    Ok(()) => println!(
                        "知识已写入: id={} name={} title={} domain={} project={}",
                        knowledge.knowledge_id,
                        knowledge.name,
                        knowledge.title,
                        knowledge.domain,
                        knowledge.project,
                    ),
                    Err(e) => eprintln!("知识写入失败: {e}"),
                }
            } else {
                tracing::warn!("图数据库不可用——知识未持久化");
                println!(
                    "知识（未持久化）: id={} name={} title={} domain={} project={}",
                    knowledge.knowledge_id,
                    knowledge.name,
                    knowledge.title,
                    knowledge.domain,
                    knowledge.project,
                );
            }
        }
        "experience" => {
            let experience = crate::application::knowledge::knowledge::experience_from_details(
                &entity_id,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_experience(&experience).await {
                    Ok(()) => println!(
                        "经验已写入: id={} title={} severity={} domain={}",
                        experience.experience_id,
                        experience.title,
                        experience.severity.as_str(),
                        experience.domain,
                    ),
                    Err(e) => eprintln!("经验写入失败: {e}"),
                }
            } else {
                tracing::warn!("图数据库不可用——经验未持久化");
                println!(
                    "经验（未持久化）: id={} title={} severity={} domain={}",
                    experience.experience_id,
                    experience.title,
                    experience.severity.as_str(),
                    experience.domain,
                );
            }
        }
        "concept" => {
            let concept = crate::application::knowledge::knowledge::concept_from_details(
                &entity_id, &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_concept(&concept).await {
                    Ok(()) => println!(
                        "概念已写入: id={} name={} domain={}",
                        concept.concept_id, concept.name, concept.domain,
                    ),
                    Err(e) => eprintln!("概念写入失败: {e}"),
                }
            } else {
                tracing::warn!("图数据库不可用——概念未持久化");
                println!(
                    "概念（未持久化）: id={} name={} domain={}",
                    concept.concept_id, concept.name, concept.domain,
                );
            }
        }
        "domain" => {
            let domain =
                crate::application::knowledge::knowledge::domain_from_details(&entity_id, &details);
            if let Some(ref svc) = svc {
                match svc.write_domain(&domain).await {
                    Ok(()) => println!(
                        "领域已写入: id={} name={}",
                        domain.domain_id, domain.name,
                    ),
                    Err(e) => eprintln!("领域写入失败: {e}"),
                }
            } else {
                tracing::warn!("图数据库不可用——领域未持久化");
                println!(
                    "领域（未持久化）: id={} name={}",
                    domain.domain_id, domain.name,
                );
            }
        }
        "playbook" => {
            let playbook = crate::application::knowledge::knowledge::playbook_from_details(
                &entity_id,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_playbook(&playbook).await {
                    Ok(()) => println!(
                        "剧本已写入: id={} name={} domain={}",
                        playbook.playbook_id, playbook.name, playbook.domain,
                    ),
                    Err(e) => eprintln!("剧本写入失败: {e}"),
                }
            } else {
                tracing::warn!("图数据库不可用——剧本未持久化");
                println!(
                    "剧本（未持久化）: id={} name={} domain={}",
                    playbook.playbook_id, playbook.name, playbook.domain,
                );
            }
        }
        other => {
            eprintln!(
                "未知的知识类型: {other}. \
                 应为以下之一: Decision、KnowledgeAdded、Environment、\
                 Dependencies、Experience、Concept、Domain、Playbook"
            );
            return Ok(());
        }
    }

    // ── 自动同步到 Qdrant ──────────────────────────────────────────
    auto_sync_kg(&knowledge_type, &entity_id, sync_acc).await;

    Ok(())
}

/// 将 `knowledge_type` 映射到图谱标签 + id 属性键，然后把
/// 新写入的节点排队，等待后台同步到 Qdrant。
///
/// 该函数立即返回——真正的 embed + upsert 在后台 worker 中
/// 累积批次执行，以提高 GPU 效率。
///
/// 返回前会 flush 队列，确保同步在
/// CLI 进程生命周期内完成。
async fn auto_sync_kg(knowledge_type: &str, entity_id: &str, acc: Option<Arc<SyncAccumulator>>) {
    let acc = match acc {
        Some(a) => a,
        None => return,
    };

    let (label, key) = match knowledge_type.to_lowercase().as_str() {
        "decision" | "knowledgeadded" | "environment" | "dependencies" => {
            ("Knowledge", "knowledge_id")
        }
        "experience" => ("Experience", "experience_id"),
        "concept" => ("Concept", "concept_id"),
        "domain" => ("Domain", "domain_id"),
        "playbook" | "pattern" | "patch" | "orchestrator" => ("Playbook", "playbook_id"),
        _ => return,
    };

    acc.enqueue(label, key, entity_id);
    acc.flush().await;
}
