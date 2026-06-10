use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::neo4j;

const NACOS_TEST: &str = "https://nacos.newoffen.net/nacos";
const NACOS_PROD: &str = "https://nacos.newoffen.com/nacos";

#[derive(Deserialize)]
struct NamespaceResp {
    data: Vec<NamespaceItem>,
}

#[derive(Deserialize)]
struct NamespaceItem {
    namespace: String,
    namespaceShowName: String,
    configCount: i64,
}

#[derive(Deserialize)]
struct ConfigListResp {
    totalCount: i64,
    pageItems: Vec<ConfigItem>,
}

#[derive(Deserialize)]
struct ConfigItem {
    dataId: String,
    group: String,
}

#[derive(Deserialize)]
struct ConfigDetailResp {
    dataId: Option<String>,
    group: Option<String>,
    content: Option<String>,
    #[allow(dead_code)]
    r#type: Option<String>,
}

pub async fn run_sync(env: &str) -> Result<()> {
    match env {
        "test" => sync_single("test", NACOS_TEST).await?,
        "prod" => sync_single("prod", NACOS_PROD).await?,
        "all" => {
            sync_single("test", NACOS_TEST).await?;
            sync_single("prod", NACOS_PROD).await?;
        }
        _ => return Err(anyhow!("环境必须是 test / prod / all")),
    }
    Ok(())
}

async fn sync_single(env_name: &str, base_url: &str) -> Result<()> {
    println!("[Nacos] 同步 {} 环境 ({})...", env_name, base_url);

    neo4j::ensure_schema().await?;

    // 1. Ensure Environment node
    let env_cypher = "\
MERGE (env:Environment {name: $name})
SET env.nacos_url = $url,
    env.type = $type,
    env.updated_at = $ts";
    neo4j::run_cypher_raw(env_cypher, json!({
        "name": env_name,
        "url": base_url,
        "type": env_name,
        "ts": Utc::now().to_rfc3339(),
    })).await?;

    // 2. Fetch namespaces
    let client = reqwest::Client::new();
    let ns_url = format!("{}/v1/console/namespaces", base_url);
    let ns_resp: NamespaceResp = client.get(&ns_url).send().await?
        .json().await.map_err(|e| anyhow!("获取命名空间失败: {}", e))?;

    let mut total_configs = 0i64;
    let mut active_ns = 0i64;

    for ns in &ns_resp.data {
        let ns_id = &ns.namespace;
        let ns_name = &ns.namespaceShowName;

        // Skip backup namespaces (old-*)
        if ns_name.starts_with("old-") || ns_name == "public" {
            continue;
        }
        if ns.configCount == 0 {
            continue;
        }

        active_ns += 1;

        // 3. MERGE NacosNamespace
        let ns_cypher = "\
MERGE (n:NacosNamespace {namespace_id: $ns_id})
SET n.name = $name,
    n.config_count = $count,
    n.updated_at = $ts";
        neo4j::run_cypher_raw(ns_cypher, json!({
            "ns_id": ns_id,
            "name": ns_name,
            "count": ns.configCount,
            "ts": Utc::now().to_rfc3339(),
        })).await?;

        // 4. Link Environment → NacosNamespace
        neo4j::run_cypher_raw("\
MATCH (env:Environment {name: $env})
MATCH (ns:NacosNamespace {namespace_id: $ns_id})
MERGE (env)-[:HAS_NAMESPACE]->(ns)", json!({
            "env": env_name,
            "ns_id": ns_id,
        })).await?;

        // 5. Fetch config list (with pagination)
        let mut page = 1;
        let page_size = 100;
        let mut fetched = 0i64;

        loop {
            let list_url = format!(
                "{}/v1/cs/configs?dataId=&group=&appName=&config_tags=&pageNo={}&pageSize={}&search=blur&tenant={}",
                base_url, page, page_size, ns_id
            );
            let list_resp: ConfigListResp = client.get(&list_url).send().await?
                .json().await.map_err(|e| anyhow!("获取配置列表失败 ({}): {}", ns_name, e))?;

            if list_resp.pageItems.is_empty() {
                break;
            }

            for cfg in &list_resp.pageItems {
                // 6. Fetch config detail (content)
                let detail_url = format!(
                    "{}/v1/cs/configs?dataId={}&group={}&tenant={}&show=all",
                    base_url, urlencode(&cfg.dataId), urlencode(&cfg.group), ns_id
                );
                let detail_resp = client.get(&detail_url).send().await;

                let content = match detail_resp {
                    Ok(r) => {
                        if let Ok(d) = r.json::<ConfigDetailResp>().await {
                            d.content.unwrap_or_default()
                        } else {
                            String::new()
                        }
                    }
                    Err(_) => String::new(),
                };

                let content_hash = {
                    let mut h = Sha256::new();
                    h.update(content.as_bytes());
                    hex::encode(h.finalize())
                };

                // 7. MERGE NacosGroup
                neo4j::run_cypher_raw("\
MERGE (g:NacosGroup {name: $group})
SET g.namespace = $ns_name,
    g.updated_at = $ts", json!({
                    "group": cfg.group,
                    "ns_name": ns_name,
                    "ts": Utc::now().to_rfc3339(),
                })).await?;

                // 8. Link NacosNamespace → NacosGroup
                neo4j::run_cypher_raw("\
MATCH (ns:NacosNamespace {namespace_id: $ns_id})
MATCH (g:NacosGroup {name: $group})
MERGE (ns)-[:HAS_GROUP]->(g)", json!({
                    "ns_id": ns_id,
                    "group": cfg.group,
                })).await?;

                // 9. MERGE NacosConfig
                neo4j::run_cypher_raw("\
MERGE (c:NacosConfig {config_id: $config_id})
ON CREATE SET
    c.data_id = $data_id,
    c.group = $group,
    c.namespace = $ns_name,
    c.content = $content,
    c.content_hash = $content_hash,
    c.config_type = $config_type,
    c.updated_at = $ts
ON MATCH SET
    c.content_hash = CASE WHEN c.content_hash <> $content_hash THEN $content_hash ELSE c.content_hash END,
    c.content = CASE WHEN c.content_hash <> $content_hash THEN $content ELSE c.content END,
    c.updated_at = CASE WHEN c.content_hash <> $content_hash THEN $ts ELSE c.updated_at END", json!({
                    "config_id": format!("{}:{}:{}", ns_id, cfg.dataId, cfg.group),
                    "data_id": cfg.dataId,
                    "group": cfg.group,
                    "ns_name": ns_name,
                    "content": content,
                    "content_hash": content_hash,
                    "config_type": detect_type(&cfg.dataId),
                    "ts": Utc::now().to_rfc3339(),
                })).await?;

                // 10. Link NacosGroup → NacosConfig AND NacosNamespace → NacosConfig
                neo4j::run_cypher_raw("\
MATCH (g:NacosGroup {name: $group})
MATCH (c:NacosConfig {config_id: $config_id})
MERGE (g)-[:HAS_CONFIG]->(c)", json!({
                    "group": cfg.group,
                    "config_id": format!("{}:{}:{}", ns_id, cfg.dataId, cfg.group),
                })).await?;

                neo4j::run_cypher_raw("\
MATCH (ns:NacosNamespace {namespace_id: $ns_id})
MATCH (c:NacosConfig {config_id: $config_id})
MERGE (ns)-[:CONTAINS]->(c)", json!({
                    "ns_id": ns_id,
                    "config_id": format!("{}:{}:{}", ns_id, cfg.dataId, cfg.group),
                })).await?;

                total_configs += 1;
                fetched += 1;
            }

            if fetched >= list_resp.totalCount {
                break;
            }
            page += 1;
        }

        println!("  {}: {} configs", ns_name, fetched);
    }

    // 11. Write sync Event
    let event_type = format!("NacosConfigSynced_{}", env_name);
    let eid = {
        let mut h = Sha256::new();
        h.update(format!("{}::{}", event_type, Utc::now().to_rfc3339()).as_bytes());
        hex::encode(&h.finalize()[..20])
    };
    neo4j::run_cypher_raw("\
MERGE (e:Event {event_id: $eid})
ON CREATE SET
    e.type = $type,
    e.entity_id = $entity_id,
    e.entity_type = 'NacosConfig',
    e.project = $env,
    e.details = $details,
    e.timestamp = $ts", json!({
        "eid": eid,
        "type": event_type,
        "entity_id": format!("nacos:{}", env_name),
        "env": env_name,
        "details": format!("Nacos {} sync: {} configs across {} namespaces", env_name, total_configs, active_ns),
        "ts": Utc::now().to_rfc3339(),
    })).await?;

    println!("[完成] {} 环境同步结束: {} 命名空间, {} 配置", env_name, active_ns, total_configs);
    Ok(())
}

fn urlencode(s: &str) -> String {
    urlencoding(s)  // manual percent-encoding for Nacos
}

fn urlencoding(s: &str) -> String {
    let mut result = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

fn detect_type(data_id: &str) -> String {
    if data_id.ends_with(".yaml") || data_id.ends_with(".yml") {
        "yaml".to_string()
    } else if data_id.ends_with(".properties") {
        "properties".to_string()
    } else if data_id.ends_with(".txt") || data_id.ends_with(".text") {
        "text".to_string()
    } else if data_id.ends_with(".json") {
        "json".to_string()
    } else if data_id.ends_with(".xml") {
        "xml".to_string()
    } else {
        "text".to_string()
    }
}
