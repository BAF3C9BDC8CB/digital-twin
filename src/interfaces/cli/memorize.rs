//! `dt memorize` 的 CLI 处理器——将结构化知识写入 KG。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::{
    DefaultKnowledgeService, KnowledgeService,
};
use crate::application::sync::batch::SyncAccumulator;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

/// 处理 `dt memorize`——写入知识条目（Knowledge、Experience、Concept、Domain、Playbook）。
///
/// `graph` 必须由调用方预先连接。
/// `sync_acc` 将节点入队，供后台（非阻塞）同步到 Qdrant。
///
/// 支持三种 action：
/// - 默认（write）：写入/覆盖知识条目
/// - `--delete <entity_id>`：删除一条知识/记忆（图 + 向量）
/// - `--supersede <old_id>`：版本化更新（新节点 EVOLVED_FROM 旧节点，旧节点置 archived）
pub async fn handle_memorize(
    knowledge_type: String,
    entity_id: String,
    entity_type: Option<String>,
    project: Option<String>,
    details: String,
    graph: Option<Arc<dyn GraphRepository>>,
    sync_acc: Option<Arc<SyncAccumulator>>,
    action: Option<String>,
    supersede: Option<String>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt CLI: memorize --type {knowledge_type} --entity-id {entity_id} --details {details}",
    );
    tracing::info!(
        task = "memorize",
        action = %action.as_deref().unwrap_or("write"),
        knowledge_type = %knowledge_type,
        entity_id = %entity_id,
        entity_type = ?entity_type,
        project = ?project,
        details_chars = details.chars().count(),
        stage = "memorize_start",
        "memorize 调用开始"
    );

    let action = action.as_deref().unwrap_or("write").to_lowercase();

    // ── 删除路径：AI 验证记忆失效后处置 ──────────────────────────
    if action == "delete" {
        let svc = graph
            .as_ref()
            .map(|g| DefaultKnowledgeService::new(Arc::clone(g)));
        match svc {
            Some(svc) => {
                let entity_id_for_delete = if entity_id.is_empty() {
                    // 兼容：--delete <id> 时 entity_id 参数位可能为空，用第一个非空参数
                    details.trim()
                } else {
                    entity_id.as_str()
                };
                if entity_id_for_delete.is_empty() {
                    eprintln!("删除失败: 未提供 entity_id");
                    return Ok(());
                }
                match svc.delete_knowledge(entity_id_for_delete).await {
                    Ok(()) => println!("已删除: id={}", entity_id_for_delete),
                    Err(e) => eprintln!("删除失败: {e}"),
                }
                // 显式删除 Qdrant 向量（svc 无 vector 时兜底；delete_knowledge 内部
                // 有 vector 时也会删，这里是双保险）
                if let Some(ref v) = vector {
                    if let Err(e) = crate::application::sync::kg_bridge::delete_kg_vector(
                        v.as_ref(),
                        entity_id_for_delete,
                    )
                    .await
                    {
                        eprintln!("向量删除失败(图已删): {e}");
                    }
                }
            }
            None => {
                tracing::warn!("图数据库不可用——删除未执行");
                eprintln!("删除失败: 图数据库不可用");
            }
        }
        return Ok(());
    }

    // ── 版本化更新路径：AI 验证记忆部分过时后处置 ────────────────
    if action == "update" || action == "supersede" {
        let old_id = supersede
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&entity_id);
        // 带向量化构造：update_knowledge 内部会删旧向量 + 建新向量
        let svc = match (graph.as_ref(), embed.as_ref(), vector.as_ref()) {
            (Some(g), Some(e), Some(v)) => Some(DefaultKnowledgeService::with_vectorization(
                Arc::clone(g),
                Arc::clone(e),
                Arc::clone(v),
            )),
            (Some(g), _, _) => Some(DefaultKnowledgeService::new(Arc::clone(g))),
            _ => None,
        };
        match svc {
            Some(svc) => {
                match svc
                    .update_knowledge(old_id, &details, "ai-verification")
                    .await
                {
                    Ok(()) => {
                        println!(
                            "已更新: {} → 新版本 (diff={})",
                            old_id,
                            details.chars().take(50).collect::<String>()
                        );
                        // 旧节点向量删除兜底（update_knowledge 有 vector 时内部已删）
                        if let Some(ref v) = vector {
                            if let Err(e) = crate::application::sync::kg_bridge::delete_kg_vector(
                                v.as_ref(),
                                old_id,
                            )
                            .await
                            {
                                eprintln!("旧版本向量删除失败(图已更新): {e}");
                            }
                        }
                    }
                    Err(e) => eprintln!("更新失败: {e}"),
                }
            }
            None => {
                tracing::warn!("图数据库不可用——更新未执行");
                eprintln!("更新失败: 图数据库不可用");
            }
        }
        return Ok(());
    }

    let project_name = project.as_deref().unwrap_or("unknown");
    let etype = entity_type.as_deref().unwrap_or(&knowledge_type);

    // 带向量化构建：write 分支（knowledge/experience/concept/domain/playbook）
    // 写入图节点后应自动向量化进 kg_nodes。若 embed+vector 均可用则用
    // with_vectorization（触发 auto_vectorize_*），否则回退纯图写入。
    // 修复前 write 分支统一用 DefaultKnowledgeService::new()（无向量化），
    // 导致记忆只进 Memgraph 从不进 kg_nodes —— 走向量检索永远召不回记忆。
    let svc = match (graph.as_ref(), embed.as_ref(), vector.as_ref()) {
        (Some(g), Some(e), Some(v)) => Some(DefaultKnowledgeService::with_vectorization(
            Arc::clone(g),
            Arc::clone(e),
            Arc::clone(v),
        )),
        (Some(g), _, _) => Some(DefaultKnowledgeService::new(Arc::clone(g))),
        _ => None,
    };

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
                    Ok(()) => println!("领域已写入: id={} name={}", domain.domain_id, domain.name,),
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
    tracing::info!(
        task = "memorize",
        label,
        entity_id = %entity_id,
        stage = "sync_enqueued",
        "知识节点已入队同步到向量库"
    );
}
