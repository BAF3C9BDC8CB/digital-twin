use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use base64::Engine;

use crate::client::neo4j;
use crate::config;

fn c(q: &str) -> String {
    q.replace("$$", "$")
}

// ── API Response Structs ─────────────────────────────────────

#[derive(Deserialize)]
struct K8sItemList<T> {
    items: Vec<T>,
}

#[derive(Deserialize)]
struct NamespaceItem {
    #[serde(rename = "metadata")]
    meta: K8sMeta,
}

#[derive(Deserialize, Clone)]
struct K8sMeta {
    name: String,
    namespace: Option<String>,
    uid: Option<String>,
    labels: Option<std::collections::HashMap<String, String>>,
    owner_references: Option<Vec<OwnerRef>>,
}

#[derive(Deserialize, Clone)]
struct OwnerRef {
    kind: String,
    name: String,
}

#[derive(Deserialize)]
struct PodItem {
    metadata: K8sMeta,
    spec: PodSpec,
    status: PodStatus,
}

#[derive(Deserialize)]
struct PodSpec {
    #[serde(rename = "nodeName")]
    node_name: Option<String>,
    containers: Vec<ContainerItem>,
}

#[derive(Deserialize)]
struct ContainerItem {
    name: String,
    image: String,
    ports: Option<Vec<ContainerPort>>,
    resources: Option<ResourceSpec>,
}

#[derive(Deserialize)]
struct ContainerPort {
    #[serde(rename = "containerPort")]
    container_port: Option<i64>,
    name: Option<String>,
    protocol: Option<String>,
}

#[derive(Deserialize)]
struct ResourceSpec {
    limits: Option<ResourceEntry>,
    requests: Option<ResourceEntry>,
}

#[derive(Deserialize)]
struct ResourceEntry {
    cpu: Option<String>,
    memory: Option<String>,
}

#[derive(Deserialize)]
struct PodStatus {
    #[serde(rename = "podIP")]
    pod_ip: Option<String>,
    phase: Option<String>,
    #[serde(rename = "containerStatuses")]
    container_statuses: Option<Vec<ContainerStatus>>,
}

#[derive(Deserialize)]
struct ContainerStatus {
    name: String,
    #[serde(rename = "restartCount")]
    restart_count: i64,
}

#[derive(Deserialize)]
struct DeploymentItem {
    metadata: K8sMeta,
    spec: DeploymentSpec,
    status: DeploymentStatus,
}

#[derive(Deserialize)]
struct DeploymentSpec {
    replicas: Option<i64>,
    strategy: Option<DeploymentStrategy>,
    template: PodTemplate,
}

#[derive(Deserialize)]
struct DeploymentStrategy {
    #[serde(rename = "type")]
    strat_type: Option<String>,
}

#[derive(Deserialize)]
struct PodTemplate {
    spec: PodSpec,
}

#[derive(Deserialize)]
struct DeploymentStatus {
    #[serde(rename = "availableReplicas")]
    available_replicas: Option<i64>,
    conditions: Option<Vec<DeploymentCondition>>,
}

#[derive(Deserialize)]
struct DeploymentCondition {
    #[serde(rename = "type")]
    cond_type: Option<String>,
}

#[derive(Deserialize)]
struct ServiceItem {
    metadata: K8sMeta,
    spec: ServiceSpec,
}

#[derive(Deserialize)]
struct ServiceSpec {
    #[serde(rename = "type")]
    svc_type: Option<String>,
    #[serde(rename = "clusterIP")]
    cluster_ip: Option<String>,
    selector: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct ConfigMapItem {
    metadata: K8sMeta,
    data: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct IngressItem {
    metadata: K8sMeta,
    spec: IngressSpec,
}

#[derive(Deserialize)]
struct IngressSpec {
    rules: Option<Vec<IngressRule>>,
}

#[derive(Deserialize)]
struct IngressRule {
    host: Option<String>,
    http: Option<IngressHTTP>,
}

#[derive(Deserialize)]
struct IngressHTTP {
    paths: Vec<IngressPath>,
}

#[derive(Deserialize)]
struct IngressPath {
    path: Option<String>,
    backend: Option<IngressBackend>,
}

#[derive(Deserialize)]
struct IngressBackend {
    service: Option<IngressService>,
}

#[derive(Deserialize)]
struct IngressService {
    name: String,
}

#[derive(Deserialize)]
struct NodeItem {
    metadata: K8sMeta,
    status: NodeStatus,
}

#[derive(Deserialize)]
struct NodeStatus {
    capacity: Option<NodeResources>,
}

#[derive(Deserialize)]
struct NodeResources {
    cpu: Option<String>,
    memory: Option<String>,
    pods: Option<String>,
}

#[derive(Deserialize)]
struct PVCItem {
    metadata: K8sMeta,
    spec: PVCSpec,
    status: PVCStatus,
}

#[derive(Deserialize)]
struct PVCSpec {
    #[serde(rename = "accessModes")]
    access_modes: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PVCStatus {
    phase: Option<String>,
}

#[derive(Deserialize)]
struct KuboardLoginResp {
    code: i64,
    data: Option<KuboardLoginData>,
}

#[derive(Deserialize)]
struct KuboardLoginData {
    #[serde(rename = "accessToken")]
    access_token: String,
}

// ── Kuboard Auth ────────────────────────────────────────────

async fn kuboard_login(client: &reqwest::Client, server: &str, username: &str, password: &str) -> Result<String> {
    let b64_pass = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        password.as_bytes(),
    );
    let login_url = format!("{0}/api/login.kuboard.cn/v4/login", server);
    let body = json!({"username": username, "password": b64_pass});

    let resp: KuboardLoginResp = client
        .post(&login_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("Kuboard login failed: {0}", e))?
        .json()
        .await
        .map_err(|e| anyhow!("Kuboard login parse failed: {0}", e))?;

    if resp.code != 200 {
        return Err(anyhow!("Kuboard login failed: code={0}", resp.code));
    }
    resp.data
        .map(|d| d.access_token)
        .ok_or_else(|| anyhow!("Kuboard login missing accessToken"))
}

fn k8s_api_url(cfg: &config::K8sConfig) -> String {
    format!("{0}/k8s-api/{1}", cfg.server.trim_end_matches('/'), cfg.cluster_id)
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {0}", token))
        .send()
        .await
        .map_err(|e| anyhow!("Request failed {0}: {1}", url, e))?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {0}: {1}", resp.status(), url));
    }
    resp.json()
        .await
        .map_err(|e| anyhow!("JSON parse failed {0}: {1}", url, e))
}

async fn fetch_items<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Vec<T> {
    match fetch_json::<K8sItemList<T>>(client, url, token).await {
        Ok(list) => list.items,
        Err(e) => {
            eprintln!("  [WARN] {0}", e);
            vec![]
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────

fn sha256(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(&h.finalize()[..20])
}

fn now_ts() -> String {
    Utc::now().to_rfc3339()
}

// ── Sync Logic ──────────────────────────────────────────────

pub async fn run_sync(limit: Option<usize>) -> Result<()> {
    let cfg = config::load();
    let k8s_cfg = match &cfg.services.k8s {
        Some(c) => c.clone(),
        None => return Err(anyhow!("config.yaml missing services.k8s")),
    };

    println!("[K8s] Login to Kuboard ({0})...", k8s_cfg.server);
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(k8s_cfg.skip_tls_verify)
        .build()
        .map_err(|e| anyhow!("HTTP client failed: {0}", e))?;
    let token = kuboard_login(&client, &k8s_cfg.server, &k8s_cfg.username, &k8s_cfg.password).await?;
    println!("[K8s] Login OK");

    let base = k8s_api_url(&k8s_cfg);

    neo4j::ensure_schema().await?;

    // 1. Namespaces (hardcoded because user may not have cluster-scope list permission)
    println!("[K8s] Processing known namespaces...");
    let known_ns = vec!["newoffen".to_string(), "newoffen-test".to_string()];
    let ns_count = known_ns.len();
    let mut total_pods = 0usize;
    let mut total_deployments = 0usize;
    let mut total_services = 0usize;
    let mut total_configmaps = 0usize;
    let mut total_ingresses = 0usize;
    let mut total_pvcs = 0usize;

    for ns_item in &known_ns {
        let ns = ns_item;
        println!("  Namespace: {0}", ns);

        neo4j::run_cypher_raw(
            &c("MERGE (ns:Namespace {name: $$_name}) SET ns.updated_at = $$_ts"),
            json!({"_name": ns, "_ts": now_ts()}),
        )
        .await?;

        // 2. Deployments
        let deploys = fetch_items::<DeploymentItem>(
            &client,
            &format!("{0}/apis/apps/v1/namespaces/{1}/deployments", base, ns),
            &token,
        ).await;
        let deploys: Vec<_> = match limit {
            Some(l) => deploys.into_iter().take(l).collect(),
            None => deploys,
        };

        for dep in &deploys {
            let name = &dep.metadata.name;
            let image = dep.spec.template.spec.containers.first()
                .map(|c| &c.image).cloned().unwrap_or_default();
            let replicas = dep.spec.replicas.unwrap_or(0);
            let strategy = dep.spec.strategy.as_ref()
                .and_then(|s| s.strat_type.as_deref()).unwrap_or("RollingUpdate").to_string();
            let avail = dep.status.available_replicas.unwrap_or(0);
            let condition = dep.status.conditions.as_ref()
                .and_then(|c| c.first())
                .map(|c| c.cond_type.as_deref().unwrap_or("")).unwrap_or("").to_string();

            neo4j::run_cypher_raw(
                &c("MERGE (d:Deployment {name: $$_name, namespace: $$_ns})
                   SET d.replicas = $$_replicas, d.available = $$_avail,
                       d.image = $$_image, d.strategy = $$_strategy,
                       d.condition = $$_cond, d.updated_at = $$_ts"),
                json!({
                    "_name": name, "_ns": ns, "_replicas": replicas, "_avail": avail,
                    "_image": image, "_strategy": strategy, "_cond": condition, "_ts": now_ts(),
                }),
            )
            .await?;

            neo4j::run_cypher_raw(
                &c("MATCH (ns:Namespace {name: $$_ns})
                   MATCH (d:Deployment {name: $$_name, namespace: $$_ns})
                   MERGE (ns)-[:HAS_DEPLOYMENT]->(d)"),
                json!({"_ns": ns, "_name": name}),
            )
            .await?;

            neo4j::run_cypher_raw(
                &c("MATCH (d:Deployment {name: $$_name, namespace: $$_ns})
                   OPTIONAL MATCH (p:Project {k8s_namespace: $$_ns})
                   FOREACH (_ IN CASE WHEN p IS NOT NULL THEN [1] END |
                       MERGE (p)-[:HAS_DEPLOYMENT]->(d))"),
                json!({"_ns": ns, "_name": name}),
            )
            .await?;

            for ctr in &dep.spec.template.spec.containers {
                let ctr_id = sha256(&format!("Container::{0}::{1}::{2}", ns, name, ctr.name));
                let ports_str = ctr.ports.as_ref()
                    .map(|p| p.iter().filter_map(|x| x.container_port).map(|v| v.to_string()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                let cpu_limit = ctr.resources.as_ref().and_then(|r| r.limits.as_ref())
                    .and_then(|l| l.cpu.as_deref()).unwrap_or("").to_string();
                let mem_limit = ctr.resources.as_ref().and_then(|r| r.limits.as_ref())
                    .and_then(|l| l.memory.as_deref()).unwrap_or("").to_string();

                neo4j::run_cypher_raw(
                    &c("MERGE (c:Container {container_id: $$_id})
                       SET c.name = $$_name, c.image = $$_image,
                           c.ports = $$_ports, c.cpu_limit = $$_cpu,
                           c.mem_limit = $$_mem, c.namespace = $$_ns,
                           c.updated_at = $$_ts"),
                    json!({
                        "_id": ctr_id, "_name": ctr.name, "_image": ctr.image,
                        "_ports": ports_str, "_cpu": cpu_limit, "_mem": mem_limit,
                        "_ns": ns, "_ts": now_ts(),
                    }),
                )
                .await?;

                neo4j::run_cypher_raw(
                    &c("MATCH (d:Deployment {name: $$_dep, namespace: $$_ns})
                       MATCH (c:Container {container_id: $$_id})
                       MERGE (d)-[:CONTAINS]->(c)"),
                    json!({"_dep": name, "_ns": ns, "_id": ctr_id}),
                )
                .await?;
            }
            total_deployments += 1;
        }

        // 3. Pods
        let pods = fetch_items::<PodItem>(
            &client,
            &format!("{0}/api/v1/namespaces/{1}/pods", base, ns),
            &token,
        ).await;
        let pods: Vec<_> = match limit {
            Some(l) => pods.into_iter().take(l).collect(),
            None => pods,
        };

        for pod in &pods {
            let pname = &pod.metadata.name;
            let pip = pod.status.pod_ip.as_deref().unwrap_or("").to_string();
            let phase = pod.status.phase.as_deref().unwrap_or("Unknown").to_string();
            let node = pod.spec.node_name.as_deref().unwrap_or("").to_string();
            let restarts: i64 = pod.status.container_statuses.as_ref()
                .map(|cs| cs.iter().map(|c| c.restart_count).sum()).unwrap_or(0);

            neo4j::run_cypher_raw(
                &c("MERGE (p:K8sPod {name: $$_name, namespace: $$_ns})
                   SET p.ip = $$_ip, p.phase = $$_phase,
                       p.node = $$_node, p.restarts = $$_restarts,
                       p.updated_at = $$_ts"),
                json!({
                    "_name": pname, "_ns": ns, "_ip": pip, "_phase": phase,
                    "_node": node, "_restarts": restarts, "_ts": now_ts(),
                }),
            )
            .await?;

            neo4j::run_cypher_raw(
                &c("MATCH (ns:Namespace {name: $$_ns})
                   MATCH (p:K8sPod {name: $$_name, namespace: $$_ns})
                   MERGE (ns)-[:HAS_POD]->(p)"),
                json!({"_ns": ns, "_name": pname}),
            )
            .await?;

            if let Some(refs) = &pod.metadata.owner_references {
                for owner in refs {
                    if owner.kind == "ReplicaSet" || owner.kind == "Deployment" {
                        neo4j::run_cypher_raw(
                            &c("MATCH (d:Deployment {name: $$_dep, namespace: $$_ns})
                               MATCH (p:K8sPod {name: $$_pod, namespace: $$_ns})
                               MERGE (d)-[:MANAGES]->(p)"),
                            json!({"_dep": &owner.name, "_ns": ns, "_pod": pname}),
                        )
                        .await
                        .ok();
                    }
                }
            }
            total_pods += 1;
        }

        // 4. Services
        let svcs = fetch_items::<ServiceItem>(
            &client,
            &format!("{0}/api/v1/namespaces/{1}/services", base, ns),
            &token,
        ).await;
        let svcs: Vec<_> = match limit {
            Some(l) => svcs.into_iter().take(l).collect(),
            None => svcs,
        };

        for svc in &svcs {
            let svc_name = &svc.metadata.name;
            let svc_type = svc.spec.svc_type.as_deref().unwrap_or("ClusterIP").to_string();
            let cluster_ip = svc.spec.cluster_ip.as_deref().unwrap_or("").to_string();

            neo4j::run_cypher_raw(
                &c("MERGE (s:K8sService {name: $$_name, namespace: $$_ns})
                   SET s.type = $$_type, s.cluster_ip = $$_cluster_ip,
                       s.updated_at = $$_ts"),
                json!({
                    "_name": svc_name, "_ns": ns, "_type": svc_type,
                    "_cluster_ip": cluster_ip, "_ts": now_ts(),
                }),
            )
            .await?;

            neo4j::run_cypher_raw(
                &c("MATCH (ns:Namespace {name: $$_ns})
                   MATCH (s:K8sService {name: $$_name, namespace: $$_ns})
                   MERGE (ns)-[:HAS_SERVICE]->(s)"),
                json!({"_ns": ns, "_name": svc_name}),
            )
            .await?;

            total_services += 1;
        }

        // 5. ConfigMaps
        let cms = fetch_items::<ConfigMapItem>(
            &client,
            &format!("{0}/api/v1/namespaces/{1}/configmaps", base, ns),
            &token,
        ).await;
        let cms: Vec<_> = match limit {
            Some(l) => cms.into_iter().take(l).collect(),
            None => cms,
        };

        for cm in &cms {
            let cm_name = &cm.metadata.name;
            let data_keys: Vec<&String> = cm.data.as_ref()
                .map(|d| d.keys().collect()).unwrap_or_default();

            neo4j::run_cypher_raw(
                &c("MERGE (c:ConfigMap {name: $$_name, namespace: $$_ns})
                   SET c.data_keys = $$_keys, c.updated_at = $$_ts"),
                json!({"_name": cm_name, "_ns": ns, "_keys": data_keys, "_ts": now_ts()}),
            )
            .await?;

            total_configmaps += 1;
        }

        // 6. Ingresses
        let ingresses = fetch_items::<IngressItem>(
            &client,
            &format!("{0}/apis/networking.k8s.io/v1/namespaces/{1}/ingresses", base, ns),
            &token,
        ).await;
        let ingresses: Vec<_> = match limit {
            Some(l) => ingresses.into_iter().take(l).collect(),
            None => ingresses,
        };

        for ing in &ingresses {
            let ing_name = &ing.metadata.name;

            neo4j::run_cypher_raw(
                &c("MERGE (i:Ingress {name: $$_name, namespace: $$_ns})
                   SET i.updated_at = $$_ts"),
                json!({"_name": ing_name, "_ns": ns, "_ts": now_ts()}),
            )
            .await?;

            neo4j::run_cypher_raw(
                &c("MATCH (ns:Namespace {name: $$_ns})
                   MATCH (i:Ingress {name: $$_name, namespace: $$_ns})
                   MERGE (ns)-[:HAS_INGRESS]->(i)"),
                json!({"_ns": ns, "_name": ing_name}),
            )
            .await?;

            if let Some(rules) = &ing.spec.rules {
                for rule in rules {
                    if let Some(http) = &rule.http {
                        for path in &http.paths {
                            if let Some(backend) = &path.backend {
                                if let Some(svc_ref) = &backend.service {
                                    neo4j::run_cypher_raw(
                                        &c("MATCH (i:Ingress {name: $$_ing, namespace: $$_ns})
                                           MATCH (s:K8sService {name: $$_svc, namespace: $$_ns})
                                           MERGE (i)-[:PROXIES_TO]->(s)"),
                                        json!({"_ing": ing_name, "_ns": ns, "_svc": &svc_ref.name}),
                                    )
                                    .await
                                    .ok();
                                }
                            }
                        }
                    }
                }
            }
            total_ingresses += 1;
        }

        // 7. PVCs
        let pvcs = fetch_items::<PVCItem>(
            &client,
            &format!("{0}/api/v1/namespaces/{1}/persistentvolumeclaims", base, ns),
            &token,
        ).await;
        let pvcs: Vec<_> = match limit {
            Some(l) => pvcs.into_iter().take(l).collect(),
            None => pvcs,
        };

        for pvc in &pvcs {
            let pvc_name = &pvc.metadata.name;
            let phase = pvc.status.phase.as_deref().unwrap_or("").to_string();

            neo4j::run_cypher_raw(
                &c("MERGE (p:PersistentVolumeClaim {name: $$_name, namespace: $$_ns})
                   SET p.phase = $$_phase, p.updated_at = $$_ts"),
                json!({"_name": pvc_name, "_ns": ns, "_phase": phase, "_ts": now_ts()}),
            )
            .await?;

            total_pvcs += 1;
        }
    }

    // 8. Nodes (full sync only)
    if limit.is_none() {
        println!("[K8s] Fetching nodes...");
        let nodes = fetch_items::<NodeItem>(
            &client,
            &format!("{0}/api/v1/nodes", base),
            &token,
        ).await;
        for node in &nodes {
            let nname = &node.metadata.name;
            let cpu = node.status.capacity.as_ref().and_then(|c| c.cpu.as_deref()).unwrap_or("").to_string();
            let mem = node.status.capacity.as_ref().and_then(|c| c.memory.as_deref()).unwrap_or("").to_string();
            let pods_max = node.status.capacity.as_ref().and_then(|c| c.pods.as_deref()).unwrap_or("").to_string();

            neo4j::run_cypher_raw(
                &c("MERGE (n:K8sNode {name: $$_name})
                   SET n.cpu = $$_cpu, n.memory = $$_mem,
                       n.pod_capacity = $$_pods, n.updated_at = $$_ts"),
                json!({"_name": nname, "_cpu": cpu, "_mem": mem, "_pods": pods_max, "_ts": now_ts()}),
            )
            .await?;
        }
    }

    // 9. Link NacosInstance -> K8sPod by IP
    if limit.is_none() {
        println!("[K8s] Linking NacosInstance -> K8sPod by IP...");
        neo4j::run_cypher_raw(
            &c("MATCH (n:NacosInstance)
               MATCH (p:K8sPod)
               WHERE n.ip = p.ip
               MERGE (n)-[:RUNS_ON]->(p)"),
            json!({}),
        )
        .await?;
    }

    // Cross-linking: Environment→Namespace, NacosService→K8sService, Deployment→NacosConfig
    // Runs even with limit (partial sync still tries to link available data)
    for ns_item in &["newoffen", "newoffen-test"] {
        let env_name = if *ns_item == "newoffen" { "prod" } else { "test" };

        // Environment → Namespace
        neo4j::run_cypher_raw(
            &c("MATCH (env:Environment {name: $$_env})
               MATCH (ns:Namespace {name: $$_k8s_ns})
               MERGE (env)-[:DEPLOYS_TO]->(ns)"),
            json!({"_env": env_name, "_k8s_ns": ns_item}),
        )
        .await
        .ok();

        // NacosService → K8sService (by name, exact or prefix+suffix)
        neo4j::run_cypher_raw(
            &c("MATCH (env:Environment {name: $$_env})-[:HAS_NAMESPACE]->(nacns:NacosNamespace)
               MATCH (kns:Namespace {name: $$_k8s_ns})
               MATCH (nacns)-[:REGISTERS]->(s:NacosService)
               MATCH (kns)-[:HAS_SERVICE]->(svc:K8sService)
               WHERE svc.name = s.name
                  OR (svc.name STARTS WITH s.name
                      AND (svc.name ENDS WITH '-stable' OR svc.name ENDS WITH '-svc'))
               MERGE (s)-[:EXPOSED_BY]->(svc)"),
            json!({"_env": env_name, "_k8s_ns": ns_item}),
        )
        .await
        .ok();

        // Deployment → NacosConfig (by extracted base name)
        neo4j::run_cypher_raw(
            &c("MATCH (d:Deployment {namespace: $$_k8s_ns})
               MATCH (env:Environment {name: $$_env})-[:HAS_NAMESPACE]->(nacns:NacosNamespace)
               MATCH (nacns)-[:CONTAINS]->(c:NacosConfig)
               WITH d, c,
                 replace(replace(d.name, '-stable', ''), '-svc', '') AS dep_base,
                 split(c.data_id, '.')[0] AS cfg_raw
               WITH d, c, dep_base,
                 replace(replace(replace(cfg_raw, '-prod', ''), '-test', ''), '_test', '') AS cfg_base
               WHERE dep_base = cfg_base
               MERGE (d)-[:CONFIGURED_BY]->(c)"),
            json!({"_k8s_ns": ns_item, "_env": env_name}),
        )
        .await
        .ok();
    }

    // 10. Write sync Event
    let event_type = "K8sSynced";
    let eid = sha256(&format!("K8sSync::{0}::{1}", now_ts(), ns_count));
    neo4j::run_cypher_raw(
        &c("MERGE (e:Event {event_id: $$_eid})
           ON CREATE SET
               e.type = $$_type,
               e.entity_id = $$_entity_id,
               e.entity_type = 'K8s',
               e.details = $$_details,
               e.timestamp = $$_ts"),
        json!({
            "_eid": eid,
            "_type": event_type,
            "_entity_id": format!("k8s:{0}", k8s_cfg.cluster_id),
            "_details": format!(
                "K8s sync: {0} namespaces, {1} deployments, {2} pods, {3} services, {4} configmaps, {5} ingresses, {6} pvcs",
                ns_count, total_deployments, total_pods, total_services, total_configmaps, total_ingresses, total_pvcs
            ),
            "_ts": now_ts(),
        }),
    )
    .await?;

    println!("[Done] K8s sync: {0} namespaces, {1} deployments, {2} pods, {3} services, {4} configmaps, {5} ingresses, {6} pvcs",
        ns_count, total_deployments, total_pods, total_services, total_configmaps, total_ingresses, total_pvcs);

    Ok(())
}
