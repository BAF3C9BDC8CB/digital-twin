//! Nacos service registry synchronisation.
//!
//! Implements [`SyncSource`] for Nacos service discovery data, producing:
//! - [`NacosService`] nodes with `service_id = dt://nacos/{ns}/{service_name}`
//! - [`Service`] nodes linked via `REGISTERED_IN`
//! - Relationships: `REGISTERED_IN`

use async_trait::async_trait;
use chrono::Utc;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::collections::HashMap;

use super::client::NacosClient;
use crate::application::sync::traits::{SyncReport, SyncSource};

/// Convenience: build a Cypher params HashMap.
fn params(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// ServiceSyncSource
// ---------------------------------------------------------------------------

/// Synchronises Nacos service registry data into the knowledge graph.
///
/// # Produced graph nodes
///
/// - `NacosService` — `service_id = dt://nacos/{ns}/{service_name}`
/// - `Service` — the corresponding Digital Twin service node
///
/// # Relationships
///
/// - `(:Service)-[:REGISTERED_IN]->(:NacosService)`
pub struct ServiceSyncSource {
    client: NacosClient,
    env_name: String,
}

impl ServiceSyncSource {
    /// Create a new service sync source.
    pub fn new(client: NacosClient, env_name: String) -> Self {
        Self { client, env_name }
    }
}

#[async_trait]
impl SyncSource for ServiceSyncSource {
    fn name(&self) -> &str {
        "nacos/service"
    }

    async fn sync(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError> {
        let ts = Utc::now().to_rfc3339();

        // 1. Fetch namespaces
        let ns_resp = self.client.list_namespaces().await?;
        let mut namespaces = 0usize;
        let mut services_total = 0usize;
        let mut links = 0usize;

        for ns in &ns_resp.data {
            let ns_id = &ns.namespace_id;
            let ns_name = &ns.namespace_show_name;

            if ns_name.starts_with("old-") || ns_name == "public" {
                continue;
            }

            // 2. Fetch service list for this namespace
            let svc_list = match self.client.list_services(ns_id).await? {
                Some(l) => l,
                None => continue,
            };

            namespaces += 1;

            // 2. MERGE NacosNamespace
            let ns_node_id = format!("dt://nacos/ns/{}", ns_id);
            graph
                .write_query(
                    r#"MERGE (n:NacosNamespace {namespace_id: $ns_node_id})
SET n.namespace = $ns_name, n.description = $ns_name, n.updated_at = $ts"#,
                    params(&[
                        ("ns_node_id", serde_json::json!(&ns_node_id)),
                        ("ns_name", serde_json::json!(ns_name)),
                        ("ts", serde_json::json!(&ts)),
                    ]),
                )
                .await?;

            for svc_item in &svc_list.service_list {
                let service_id = format!("dt://nacos/{}/{}", ns_id, svc_item.name);

                // 3. MERGE NacosService
                let nacos_svc_cypher = r#"
MERGE (ns:NacosService {service_id: $service_id})
SET ns.name = $name,
    ns.namespace = $ns_name,
    ns.group_name = $group_name,
    ns.ip_count = $ip_count,
    ns.healthy_count = $healthy,
    ns.updated_at = $ts
"#;
                graph
                    .write_query(
                        nacos_svc_cypher,
                        params(&[
                            ("service_id", serde_json::json!(&service_id)),
                            ("name", serde_json::json!(&svc_item.name)),
                            ("ns_name", serde_json::json!(ns_name)),
                            ("group_name", serde_json::json!(&svc_item.group_name)),
                            ("ip_count", serde_json::json!(svc_item.ip_count)),
                            ("healthy", serde_json::json!(svc_item.healthy_instance_count)),
                            ("ts", serde_json::json!(&ts)),
                        ]),
                    )
                    .await?;

                // 4. MERGE Service and link via REGISTERED_IN
                let svc_cypher = r#"
MERGE (s:Service {service_id: $dt_service_id})
ON CREATE SET
    s.name = $name,
    s.service_type = $env,
    s.updated_at = $ts
ON MATCH SET
    s.updated_at = $ts
"#;
                let dt_service_id = format!("dt://service/{}", svc_item.name);
                graph
                    .write_query(
                        svc_cypher,
                        params(&[
                            ("dt_service_id", serde_json::json!(&dt_service_id)),
                            ("name", serde_json::json!(&svc_item.name)),
                            ("env", serde_json::json!(&self.env_name)),
                            ("ts", serde_json::json!(&ts)),
                        ]),
                    )
                    .await?;

                // 5. Link Service → NacosService (REGISTERED_IN)
                let link_cypher = r#"
MATCH (s:Service {service_id: $dt_service_id})
MATCH (ns:NacosService {service_id: $service_id})
MERGE (s)-[:REGISTERED_IN]->(ns)
"#;
                graph
                    .write_query(
                        link_cypher,
                        params(&[
                            ("dt_service_id", serde_json::json!(&dt_service_id)),
                            ("service_id", serde_json::json!(&service_id)),
                        ]),
                    )
                    .await?;
                links += 1;

                // 6. Link NacosService → NacosNamespace (IN_NAMESPACE)
                graph
                    .write_query(
                        "MATCH (svc:NacosService {service_id: $service_id}) MATCH (ns:NacosNamespace {namespace_id: $ns_node_id}) MERGE (svc)-[:IN_NAMESPACE]->(ns)",
                        params(&[
                            ("service_id", serde_json::json!(&service_id)),
                            ("ns_node_id", serde_json::json!(&ns_node_id)),
                        ]),
                    )
                    .await?;
                links += 1;

                // 7. Fetch and MERGE NacosInstance nodes
                if let Ok(Some(inst_resp)) = self.client.list_instances(&svc_item.name, ns_id).await {
                    if let Some(instances) = &inst_resp.list {
                        for inst in instances {
                            let instance_id = format!("dt://nacos/{}/{}/{}",
                                ns_id, svc_item.name, inst.instance_id);
                            graph
                                .write_query(
                                    r#"MERGE (i:NacosInstance {instance_id: $instance_id})
SET i.service_name = $svc_name,
    i.ip = $ip,
    i.port = $port,
    i.namespace = $ns_name,
    i.healthy = $healthy,
    i.enabled = $enabled,
    i.weight = $weight,
    i.cluster_name = $cluster,
    i.updated_at = $ts"#,
                                    params(&[
                                        ("instance_id", serde_json::json!(&instance_id)),
                                        ("svc_name", serde_json::json!(&svc_item.name)),
                                        ("ip", serde_json::json!(&inst.ip)),
                                        ("port", serde_json::json!(inst.port)),
                                        ("ns_name", serde_json::json!(ns_name)),
                                        ("healthy", serde_json::json!(inst.healthy)),
                                        ("enabled", serde_json::json!(inst.enabled)),
                                        ("weight", serde_json::json!(inst.weight)),
                                        ("cluster", serde_json::json!(inst.cluster_name)),
                                        ("ts", serde_json::json!(&ts)),
                                    ]),
                                )
                                .await?;

                            // Link NacosInstance → NacosService (INSTANCE_OF)
                            graph
                                .write_query(
                                    "MATCH (i:NacosInstance {instance_id: $instance_id}) MATCH (svc:NacosService {service_id: $service_id}) MERGE (i)-[:INSTANCE_OF]->(svc)",
                                    params(&[
                                        ("instance_id", serde_json::json!(&instance_id)),
                                        ("service_id", serde_json::json!(&service_id)),
                                    ]),
                                )
                                .await?;
                            links += 2;
                        }
                    }
                }

                services_total += 1;
            }
        }

        Ok(SyncReport {
            source: format!("nacos/{}/service", self.env_name),
            namespaces,
            configs: 0,
            services: services_total,
            links_created: links,
            items_fetched: services_total,
            items_created: services_total,
            items_updated: 0,
            items_skipped: 0,
            items_failed: 0,
            errors: vec![],
            elapsed_ms: 0,
            skipped: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_sync_source_name() {
        let client = NacosClient::new("https://example.com");
        let src = ServiceSyncSource::new(client, "test".into());
        assert_eq!(src.name(), "nacos/service");
    }

    #[test]
    fn helper_params_empty() {
        let p = params(&[]);
        assert!(p.is_empty());
    }

    #[test]
    fn helper_params_with_values() {
        let p = params(&[
            ("key", serde_json::json!("value")),
            ("num", serde_json::json!(42)),
        ]);
        assert_eq!(p.get("key").unwrap(), &serde_json::json!("value"));
        assert_eq!(p.get("num").unwrap(), &serde_json::json!(42));
    }
}
