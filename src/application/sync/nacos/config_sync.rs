//! Nacos configuration synchronisation.
//!
//! Implements [`SyncSource`] for Nacos config data, producing:
//! - [`NacosConfig`] nodes with `config_id = dt://nacos/{ns}/{data_id}`
//! - [`ConfigKey`] nodes extracted from config content
//! - [`Database`] nodes for detected JDBC/Redis/Kafka connection strings
//! - Relationships: `BELONGS_TO`, `CONTAINS`

use async_trait::async_trait;
use chrono::Utc;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

use super::client::NacosClient;
use crate::application::sync::traits::{SyncReport, SyncSource};
use crate::shared::chunker::{chunk_config_by_sections, chunk_config_adaptive, parse_kv_line, ChunkConfig};

// ---------------------------------------------------------------------------
// Convenience: build a Cypher params HashMap
// ---------------------------------------------------------------------------

fn params(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

// ---------------------------------------------------------------------------
// Regex patterns for connection-string extraction
// ---------------------------------------------------------------------------

/// JDBC URL: `jdbc:mysql://host:port/db?...` or `jdbc:postgresql://...`
static JDBC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)jdbc:(mysql|postgresql|mariadb|sqlserver|oracle|h2|dm)://([^/\s?]+)(/\S+)?",
    )
    .expect("JDBC regex")
});

/// Redis connection: `redis://host:port/db`, `rediss://...`, or `host:port` in redis context
static REDIS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:redis|rediss)://([^/\s?]+)").expect("Redis regex")
});

/// Redis host:port pattern (e.g. `redis.host=127.0.0.1`, `redis.port=6379`)
static REDIS_HOST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:spring\.)?redis\.host\s*[:=]\s*(\S+)").expect("Redis host regex")
});

/// Kafka bootstrap servers: `kafka.bootstrap-servers=host:port,host:port`
static KAFKA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:spring\.)?kafka\.bootstrap-servers\s*[:=]\s*(\S+)").expect("Kafka regex")
});

// ---------------------------------------------------------------------------
// Config key extraction
// ---------------------------------------------------------------------------

/// Extract `ConfigKey` entries from configuration content.
fn extract_config_keys(namespace: &str, content: &str) -> Vec<ConfigKeyEntry> {
    let is_yaml = content.lines().any(|l| {
        let trimmed = l.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.contains('=')
            && trimmed.ends_with(':')
    });

    if is_yaml {
        extract_yaml_keys(namespace, content)
    } else {
        extract_properties_keys(namespace, content)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigKeyEntry {
    name: String,
    value: String,
    namespace: String,
    purpose: String,
}

fn extract_properties_keys(namespace: &str, content: &str) -> Vec<ConfigKeyEntry> {
    let mut keys = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        if let Some((key, value)) = parse_kv_line(trimmed) {
            let purpose = classify_key(key);
            keys.push(ConfigKeyEntry {
                name: key.to_string(),
                value: value.to_string(),
                namespace: namespace.to_string(),
                purpose,
            });
        }
    }
    keys
}

fn extract_yaml_keys(namespace: &str, content: &str) -> Vec<ConfigKeyEntry> {
    let mut keys = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains(':') {
            continue;
        }
        if trimmed.starts_with('-') {
            continue;
        }
        if let Some((key, value)) = parse_kv_line(trimmed) {
            if !key.contains('.') || value.len() > 200 {
                continue;
            }
            let purpose = classify_key(key);
            keys.push(ConfigKeyEntry {
                name: key.to_string(),
                value: value.to_string(),
                namespace: namespace.to_string(),
                purpose,
            });
        }
    }
    keys
}


fn classify_key(key: &str) -> String {
    let lower = key.to_lowercase();
    if lower.contains("datasource") || lower.contains("jdbc") || lower.contains("db.") {
        "Database".into()
    } else if lower.contains("redis") {
        "Cache".into()
    } else if lower.contains("kafka") {
        "MessageQueue".into()
    } else if lower.contains("server") || lower.contains("port") {
        "Server".into()
    } else if lower.contains("log") {
        "Logging".into()
    } else if lower.contains("security") || lower.contains("oauth") || lower.contains("jwt") {
        "Security".into()
    } else {
        "General".into()
    }
}

// ---------------------------------------------------------------------------
// Database extraction from content
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DbConnection {
    db_type: String,
    host: String,
    port: u16,
    database: String,
}

fn extract_databases(_namespace: &str, content: &str) -> Vec<DbConnection> {
    let mut dbs = Vec::new();

    for caps in JDBC_RE.captures_iter(content) {
        let db_type = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        let host_port = caps.get(2).map(|m| m.as_str()).unwrap_or("localhost");
        let db_path = caps.get(3).map(|m| m.as_str()).unwrap_or("");

        let (host, port) = parse_host_port(host_port, default_port_for(&db_type));
        let database = db_path.trim_start_matches('/').split('?').next().unwrap_or("");

        dbs.push(DbConnection {
            db_type,
            host: host.to_string(),
            port,
            database: database.to_string(),
        });
    }

    for caps in REDIS_RE.captures_iter(content) {
        let host_port = caps.get(1).map(|m| m.as_str()).unwrap_or("localhost:6379");
        let (host, port) = parse_host_port(host_port, 6379);
        dbs.push(DbConnection {
            db_type: "redis".into(),
            host: host.to_string(),
            port,
            database: "0".into(),
        });
    }

    if let Some(caps) = REDIS_HOST_RE.captures(content) {
        let host = caps.get(1).map(|m| m.as_str()).unwrap_or("localhost");
        if !dbs.iter().any(|d| d.db_type == "redis" && d.host == host) {
            dbs.push(DbConnection {
                db_type: "redis".into(),
                host: host.to_string(),
                port: 6379,
                database: "0".into(),
            });
        }
    }

    if let Some(caps) = KAFKA_RE.captures(content) {
        let servers = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        for server in servers.split(',') {
            let (host, port) = parse_host_port(server.trim(), 9092);
            dbs.push(DbConnection {
                db_type: "kafka".into(),
                host: host.to_string(),
                port,
                database: "".into(),
            });
        }
    }

    dbs
}

fn parse_host_port(s: &str, default_port: u16) -> (&str, u16) {
    if let Some(pos) = s.rfind(':') {
        if let Ok(port) = s[pos + 1..].parse::<u16>() {
            return (&s[..pos], port);
        }
    }
    (s, default_port)
}

fn default_port_for(db_type: &str) -> u16 {
    match db_type.to_lowercase().as_str() {
        "mysql" => 3306,
        "postgresql" => 5432,
        "mariadb" => 3306,
        "sqlserver" => 1433,
        "oracle" => 1521,
        "redis" => 6379,
        "kafka" => 9092,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Config type detection
// ---------------------------------------------------------------------------

fn detect_config_type(data_id: &str) -> String {
    let lower = data_id.to_lowercase();
    if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "yaml".into()
    } else if lower.ends_with(".properties") {
        "properties".into()
    } else if lower.ends_with(".json") {
        "json".into()
    } else if lower.ends_with(".xml") {
        "xml".into()
    } else {
        "text".into()
    }
}

// ---------------------------------------------------------------------------
// NacosConfigEntry — lightweight config item for vectorisation
// ---------------------------------------------------------------------------

/// A lightweight representation of a single Nacos config key-value pair
/// for vectorisation into Qdrant.
#[derive(Debug, Clone)]
pub struct NacosConfigEntry {
    /// Unique identifier (e.g. "dt://nacos/{ns}/{data_id}#{key}").
    pub entity_id: String,
    /// Configuration key name.
    pub key: String,
    /// Configuration value.
    pub value: String,
    /// Nacos namespace name.
    pub namespace: String,
    /// The data_id this key belongs to.
    pub data_id: String,
    /// Config group name.
    pub group: String,
}

// ---------------------------------------------------------------------------
// ConfigVectorizer
// ---------------------------------------------------------------------------

/// Vectorises Nacos config keys and values into Qdrant for semantic search.
///
/// After nacos-sync writes config keys/values to Neo4j, this vectoriser
/// embeds the key name + value text using the configured embedding service,
/// then upserts the resulting vectors into a Qdrant collection named
/// `{project}_semantic`.
///
/// # Usage
///
/// ```ignore
/// let vectorizer = ConfigVectorizer::new(embed_service, vector_repo);
/// let count = vectorizer.vectorize_configs(&entries, "my-project").await?;
/// ```
pub struct ConfigVectorizer {
    embed: Arc<dyn EmbedService>,
    vector: Arc<dyn VectorRepository>,
}

impl ConfigVectorizer {
    /// Create a new ConfigVectorizer.
    pub fn new(embed: Arc<dyn EmbedService>, vector: Arc<dyn VectorRepository>) -> Self {
        Self { embed, vector }
    }

    /// Embed and upsert config entries into the Qdrant semantic collection.
    ///
    /// # Process
    /// 1. Collect all search texts from key + value
    /// 2. `embed_batch(texts)` → vectors
    /// 3. Build Qdrant points with full payload
    /// 4. Upsert into `{project}_semantic` collection
    ///
    /// Returns the number of entries successfully vectorised.
    pub async fn vectorize_configs(
        &self,
        entries: &[NacosConfigEntry],
        project: &str,
    ) -> Result<usize, DtError> {
        if entries.is_empty() {
            return Ok(0);
        }

        let collection = format!("{}_semantic", project);
        self.vector.ensure_collection(&collection, 384).await?;

        // Build search texts: concatenate key + value for richer semantics
        let texts: Vec<String> = entries
            .iter()
            .map(|e| format!("{}: {}", e.key, e.value))
            .collect();

        // Generate embeddings
        let vectors = self.embed.embed_batch(&texts).await?;

        // Build Qdrant points
        let points: Vec<serde_json::Value> = entries
            .iter()
            .zip(vectors.iter())
            .map(|(entry, vec)| {
                serde_json::json!({
                    "id": entry.entity_id,
                    "vector": vec,
                    "payload": {
                        "entity_id": entry.entity_id,
                        "key": entry.key,
                        "value": entry.value,
                        "namespace": entry.namespace,
                        "data_id": entry.data_id,
                        "group": entry.group,
                        "source_type": "nacos_config",
                        "project": project,
                    }
                })
            })
            .collect();

        self.vector.upsert(&collection, points).await?;
        Ok(entries.len())
    }

    /// Vectorise config keys extracted during sync — convenience wrapper
    /// over [`vectorize_configs`].
    ///
    /// Builds `NacosConfigEntry` from the raw fields, then calls
    /// `vectorize_configs`.
    #[allow(dead_code)]
    pub(crate) async fn vectorize_config_keys(
        &self,
        keys: &[ConfigKeyEntry],
        namespace: &str,
        data_id: &str,
        group: &str,
        project: &str,
    ) -> Result<usize, DtError> {
        let entries: Vec<NacosConfigEntry> = keys
            .iter()
            .map(|k| NacosConfigEntry {
                entity_id: format!(
                    "dt://nacos/{}/{}/{}",
                    namespace,
                    data_id,
                    k.name.replace(['.', '/', ' '], "_")
                ),
                key: k.name.clone(),
                value: k.value.clone(),
                namespace: namespace.to_string(),
                data_id: data_id.to_string(),
                group: group.to_string(),
            })
            .collect();

        self.vectorize_configs(&entries, project).await
    }
}

// ---------------------------------------------------------------------------
// ConfigChunkVectorizer — adaptive chunk → vector embedding
// ---------------------------------------------------------------------------

/// A chunk to be vectorised — section name and its key-value pairs.
#[derive(Debug, Clone)]
pub struct ChunkToVectorize {
    pub section_name: String,
    pub key_values: Vec<(String, String)>,
}

/// Vectorises config chunks into the dedicated `config_chunks` Qdrant
/// collection (dim=1024, BGE-M3).
///
/// Each chunk contains all key-value pairs from one adaptive section,
/// formatted as `key=value\n...` text for embedding.
pub struct ConfigChunkVectorizer {
    embed: Arc<dyn EmbedService>,
    vector: Arc<dyn VectorRepository>,
}

impl ConfigChunkVectorizer {
    pub const COLLECTION: &str = "config_chunks";
    pub const VECTOR_DIM: u32 = 1024;

    pub fn new(embed: Arc<dyn EmbedService>, vector: Arc<dyn VectorRepository>) -> Self {
        Self { embed, vector }
    }

    /// Vectorise one or more config chunks for a given config file.
    pub async fn vectorize_chunks(
        &self,
        chunks: &[ChunkToVectorize],
        namespace: &str,
        data_id: &str,
        group: &str,
        config_type: &str,
        environment: Option<&str>,
    ) -> Result<usize, DtError> {
        if chunks.is_empty() {
            return Ok(0);
        }

        self.vector
            .ensure_collection(Self::COLLECTION, Self::VECTOR_DIM)
            .await?;

        // Build full text for each chunk: section_name + all key=value lines
        let texts: Vec<String> = chunks
            .iter()
            .map(|c| {
                let mut t = c.section_name.clone();
                for (k, v) in &c.key_values {
                    t.push('\n');
                    t.push_str(&format!("{}={}", k, v));
                }
                t
            })
            .collect();

        let vectors = self.embed.embed_batch(&texts).await?;

        let points: Vec<serde_json::Value> = chunks
            .iter()
            .zip(vectors.iter())
            .map(|(chunk, vec)| {
                let id = format!("dt://nacos/{}/{}#{}", namespace, data_id, chunk.section_name);
                serde_json::json!({
                    "id": id,
                    "vector": vec,
                    "payload": {
                        "section_name": chunk.section_name,
                        "namespace": namespace,
                        "data_id": data_id,
                        "group": group,
                        "config_type": config_type,
                        "text": build_chunk_text(chunk),
                        "source_type": "config_chunk",
                        "key_count": chunk.key_values.len(),
                        "environment": environment.unwrap_or(""),
                    }
                })
            })
            .collect();

        self.vector.upsert(Self::COLLECTION, points).await?;
        Ok(chunks.len())
    }

    /// Delete stale chunk vectors for a config file before re-upserting.
    pub async fn delete_by_data_id(
        &self,
        namespace: &str,
        data_id: &str,
    ) -> Result<(), DtError> {
        self.vector
            .delete_by_filter(
                Self::COLLECTION,
                serde_json::json!({
                    "must": [
                        {"key": "namespace", "match": {"value": namespace}},
                        {"key": "data_id", "match": {"value": data_id}},
                    ]
                }),
            )
            .await
    }
}

/// Build the chunk text used for embedding.
pub fn build_chunk_text(chunk: &ChunkToVectorize) -> String {
    let mut text = chunk.section_name.clone();
    for (k, v) in &chunk.key_values {
        text.push('\n');
        text.push_str(&format!("{}={}", k, v));
    }
    text
}

// ---------------------------------------------------------------------------
// ConfigSyncSource
// ---------------------------------------------------------------------------

/// Synchronises Nacos configuration data into the knowledge graph.
///
/// # Produced Neo4j nodes
///
/// - `NacosConfig` — `config_id = dt://nacos/{ns}/{data_id}`
/// - `NacosGroup` — configuration group
/// - `ConfigKey` — individual configuration key-value pairs
/// - `Database` — auto-detected from JDBC/Redis/Kafka connection strings
///
/// # Change detection
///
/// Content is hashed with SHA256. A node is only updated when the hash differs
/// from the stored value, avoiding unnecessary writes.
pub struct ConfigSyncSource {
    client: NacosClient,
    env_name: String,
}

impl ConfigSyncSource {
    /// Create a new config sync source.
    pub fn new(client: NacosClient, env_name: String) -> Self {
        Self { client, env_name }
    }
}

#[async_trait]
impl SyncSource for ConfigSyncSource {
    fn name(&self) -> &str {
        "nacos/config"
    }

    #[allow(clippy::too_many_lines)]
    async fn sync(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError> {
        let ts = Utc::now().to_rfc3339();

        // 1. Fetch namespaces
        let ns_resp = self.client.list_namespaces().await?;
        let mut namespaces = 0usize;
        let mut configs_total = 0usize;
        let mut links = 0usize;

        for ns in &ns_resp.data {
            let ns_id = &ns.namespace_id;
            let ns_name = &ns.namespace_show_name;

            if ns_name.starts_with("old-") || ns_name == "public" || ns.config_count == 0 {
                continue;
            }

            tracing::debug!("[nacos/config] syncing namespace: {ns_name}");
            namespaces += 1;

            // 2. MERGE NacosNamespace
            let ns_node_id = format!("dt://nacos/ns/{}", ns_id);
            let ns_cypher = r#"
MERGE (n:NacosNamespace {namespace_id: $ns_node_id})
SET n.namespace = $ns_name,
    n.description = $ns_name,
    n.updated_at = $ts
"#;
            graph
                .write_query(
                    ns_cypher,
                    params(&[
                        ("ns_node_id", serde_json::json!(&ns_node_id)),
                        ("ns_name", serde_json::json!(ns_name)),
                        ("ts", serde_json::json!(&ts)),
                    ]),
                )
                .await?;

            let mut page: i64 = 1;
            let page_size: i64 = 100;
            let mut fetched: i64 = 0;

            loop {
                let list = match self.client.list_configs(ns_id, page, page_size).await? {
                    Some(l) => l,
                    None => break,
                };

                for cfg_item in &list.page_items {
                    let detail = self
                        .client
                        .get_config_detail(&cfg_item.data_id, &cfg_item.group, ns_id)
                        .await?;
                    let content = detail.content.unwrap_or_default();

                    let content_hash = {
                        let mut h = Sha256::new();
                        h.update(content.as_bytes());
                        hex::encode(h.finalize())
                    };

                    let config_type = detect_config_type(&cfg_item.data_id);
                    let config_id = format!("dt://nacos/{}/{}", ns_id, cfg_item.data_id);

                    // MERGE NacosGroup
                    graph
                        .write_query(
                            "MERGE (g:NacosGroup {name: $group}) SET g.namespace = $ns_name, g.updated_at = $ts",
                            params(&[
                                ("group", serde_json::json!(&cfg_item.group)),
                                ("ns_name", serde_json::json!(ns_name)),
                                ("ts", serde_json::json!(&ts)),
                            ]),
                        )
                        .await?;
                    links += 1;

                    // MERGE NacosConfig (only update if hash changed)
                    graph
                        .write_query(
                            r#"MERGE (c:NacosConfig {config_id: $config_id})
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
    c.updated_at = CASE WHEN c.content_hash <> $content_hash THEN $ts ELSE c.updated_at END"#,
                            params(&[
                                ("config_id", serde_json::json!(&config_id)),
                                ("data_id", serde_json::json!(&cfg_item.data_id)),
                                ("group", serde_json::json!(&cfg_item.group)),
                                ("ns_name", serde_json::json!(ns_name)),
                                ("content", serde_json::json!(&content)),
                                ("content_hash", serde_json::json!(&content_hash)),
                                ("config_type", serde_json::json!(&config_type)),
                                ("ts", serde_json::json!(&ts)),
                            ]),
                        )
                        .await?;

                    // Link NacosConfig → NacosGroup (BELONGS_TO)
                    graph
                        .write_query(
                            "MATCH (g:NacosGroup {name: $group}) MATCH (c:NacosConfig {config_id: $config_id}) MERGE (c)-[:BELONGS_TO]->(g)",
                            params(&[
                                ("group", serde_json::json!(&cfg_item.group)),
                                ("config_id", serde_json::json!(&config_id)),
                            ]),
                        )
                        .await?;
                    links += 1;

                    // Link NacosConfig → NacosNamespace (IN_NAMESPACE)
                    graph
                        .write_query(
                            "MATCH (ns:NacosNamespace {namespace_id: $ns_node_id}) MATCH (c:NacosConfig {config_id: $config_id}) MERGE (c)-[:IN_NAMESPACE]->(ns)",
                            params(&[
                                ("ns_node_id", serde_json::json!(&ns_node_id)),
                                ("config_id", serde_json::json!(&config_id)),
                            ]),
                        )
                        .await?;
                    links += 1;

                    // Extract and upsert ConfigKeys
                    if !content.is_empty() {
                        let keys = extract_config_keys(ns_name, &content);
                        for key_entry in &keys {
                            graph
                                .write_query(
                                    r#"MERGE (k:ConfigKey {name: $name, namespace: $ns})
ON CREATE SET k.value = $value, k.purpose = $purpose, k.updated_at = $ts
ON MATCH SET
    k.value = CASE WHEN k.value <> $value THEN $value ELSE k.value END,
    k.purpose = CASE WHEN k.purpose <> $purpose THEN $purpose ELSE k.purpose END,
    k.updated_at = CASE WHEN k.value <> $value THEN $ts ELSE k.updated_at END"#,
                                    params(&[
                                        ("name", serde_json::json!(&key_entry.name)),
                                        ("ns", serde_json::json!(&key_entry.namespace)),
                                        ("value", serde_json::json!(&key_entry.value)),
                                        ("purpose", serde_json::json!(&key_entry.purpose)),
                                        ("ts", serde_json::json!(&ts)),
                                    ]),
                                )
                                .await?;

                            // Link ConfigKey → NacosConfig (CONTAINS)
                            graph
                                .write_query(
                                    "MATCH (k:ConfigKey {name: $name, namespace: $ns}) MATCH (c:NacosConfig {config_id: $config_id}) MERGE (k)-[:CONTAINS]->(c)",
                                    params(&[
                                        ("name", serde_json::json!(&key_entry.name)),
                                        ("ns", serde_json::json!(&key_entry.namespace)),
                                        ("config_id", serde_json::json!(&config_id)),
                                    ]),
                                )
                                .await?;
                            links += 1;
                        }
                    }

                    // --- ConfigSection 提取与写入 (复用 chunker) ---
                    if !content.is_empty() {
                        let chunk_config = ChunkConfig::default();
                        let is_yaml = config_type == "yaml" || config_type == "yml";
                        let sections =
                            chunk_config_by_sections(&content, &config_id, &chunk_config, is_yaml);

                        for (section_name, sec_chunks) in &sections {
                            let section_id =
                                format!("{}#{}", config_id, section_name.replace('.', "_"));

                            // Build a concise summary from leaf key=value entries only
                            // Skips structural lines (keys with no value on the same line)
                            let summary = {
                                let text: Vec<&str> =
                                    sec_chunks.iter().map(|c| c.text.as_str()).collect();
                                let combined = text.join("\n");
                                let pairs: Vec<String> = combined
                                    .lines()
                                    .filter_map(|l| {
                                        let trimmed = l.trim();
                                        // Skip structural lines: empty, comment, or key: (no value)
                                        if trimmed.is_empty()
                                            || trimmed.starts_with('#')
                                            || trimmed.ends_with(':')
                                        {
                                            return None;
                                        }
                                        // Normalise "key: value" → "key=value"
                                        if let Some(pos) = trimmed.find(':') {
                                            let k = trimmed[..pos].trim();
                                            let v = trimmed[pos + 1..].trim();
                                            if !v.is_empty() && !v.contains(':') {
                                                return Some(format!("{}={}", k, v));
                                            }
                                        }
                                        // Already "key=value" format
                                        if let Some(pos) = trimmed.find('=') {
                                            let k = trimmed[..pos].trim();
                                            let v = trimmed[pos + 1..].trim();
                                            if !v.is_empty() {
                                                return Some(format!("{}={}", k, v));
                                            }
                                        }
                                        None
                                    })
                                    .take(10) // limit to 10 pairs to keep summary concise
                                    .collect();
                                if pairs.is_empty() {
                                    format!("{}: (structural keys only)", section_name)
                                } else {
                                    format!("{}: {}", section_name, pairs.join(", "))
                                }
                            };

                            // MERGE ConfigSection node
                            graph
                                .write_query(
                                    r#"MERGE (s:ConfigSection {section_id: $section_id})
ON CREATE SET s.name = $name, s.summary = $summary, s.namespace = $ns,
              s.data_id = $data_id, s.config_type = $config_type, s.updated_at = $ts
ON MATCH SET s.summary = $summary, s.updated_at = $ts"#,
                                    params(&[
                                        ("section_id", serde_json::json!(&section_id)),
                                        ("name", serde_json::json!(section_name)),
                                        ("summary", serde_json::json!(&summary)),
                                        ("ns", serde_json::json!(ns_name)),
                                        ("data_id", serde_json::json!(&cfg_item.data_id)),
                                        ("config_type", serde_json::json!(&config_type)),
                                        ("ts", serde_json::json!(&ts)),
                                    ]),
                                )
                                .await?;

                            // Link NacosConfig → ConfigSection
                            graph
                                .write_query(
                                    "MATCH (c:NacosConfig {config_id: $config_id})
                                     MATCH (s:ConfigSection {section_id: $section_id})
                                     MERGE (c)-[:HAS_SECTION]->(s)",
                                    params(&[
                                        ("config_id", serde_json::json!(&config_id)),
                                        ("section_id", serde_json::json!(&section_id)),
                                    ]),
                                )
                                .await?;
                            links += 1;
                        }
                    }

                    // Extract and upsert Database nodes from connection strings
                    if !content.is_empty() {
                        let databases = extract_databases(ns_name, &content);
                        for db in &databases {
                            let db_id = format!("dt://db/{}/{}:{}", db.db_type, db.host, db.port);
                            graph
                                .write_query(
                                    r#"MERGE (d:Database {database_id: $db_id})
ON CREATE SET d.db_type = $db_type, d.host = $host, d.port = $port, d.database = $database, d.namespace = $ns, d.updated_at = $ts
ON MATCH SET d.updated_at = $ts"#,
                                    params(&[
                                        ("db_id", serde_json::json!(&db_id)),
                                        ("db_type", serde_json::json!(&db.db_type)),
                                        ("host", serde_json::json!(&db.host)),
                                        ("port", serde_json::json!(db.port)),
                                        ("database", serde_json::json!(&db.database)),
                                        ("ns", serde_json::json!(ns_name)),
                                        ("ts", serde_json::json!(&ts)),
                                    ]),
                                )
                                .await?;
                            links += 1;

                            // Link Database → NacosConfig (DETECTED_IN)
                            graph
                                .write_query(
                                    "MATCH (d:Database {database_id: $db_id}) MATCH (c:NacosConfig {config_id: $config_id}) MERGE (d)-[:DETECTED_IN]->(c)",
                                    params(&[
                                        ("db_id", serde_json::json!(&db_id)),
                                        ("config_id", serde_json::json!(&config_id)),
                                    ]),
                                )
                                .await?;
                            links += 1;
                        }
                    }

                    configs_total += 1;
                    fetched += 1;
                }

                if fetched >= list.total_count {
                    break;
                }
                page += 1;
            }
        }

        Ok(SyncReport {
            source: format!("nacos/{}/config", self.env_name),
            namespaces,
            configs: configs_total,
            services: 0,
            links_created: links,
            items_fetched: configs_total,
            items_created: configs_total,
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
    fn detect_type_yaml() {
        assert_eq!(detect_config_type("application.yaml"), "yaml");
        assert_eq!(detect_config_type("app.yml"), "yaml");
    }

    #[test]
    fn detect_type_properties() {
        assert_eq!(detect_config_type("app.properties"), "properties");
    }

    #[test]
    fn detect_type_json() {
        assert_eq!(detect_config_type("config.json"), "json");
    }

    #[test]
    fn detect_type_unknown() {
        assert_eq!(detect_config_type("readme.txt"), "text");
    }

    #[test]
    fn parse_host_port_with_port() {
        assert_eq!(parse_host_port("localhost:3306", 0), ("localhost", 3306));
    }

    #[test]
    fn parse_host_port_without_port() {
        assert_eq!(parse_host_port("db.example.com", 5432), ("db.example.com", 5432));
    }

    #[test]
    fn parse_kv_equals() {
        assert_eq!(
            parse_kv_line("spring.datasource.url=jdbc:mysql://localhost/test"),
            Some(("spring.datasource.url", "jdbc:mysql://localhost/test"))
        );
    }

    #[test]
    fn parse_kv_colon() {
        assert_eq!(
            parse_kv_line("server.port: 8080"),
            Some(("server.port", "8080"))
        );
    }

    #[test]
    fn parse_kv_empty() {
        assert!(parse_kv_line("").is_none());
    }

    #[test]
    fn extract_jdbc_mysql() {
        let content = "spring.datasource.url=jdbc:mysql://10.0.0.1:3306/testdb?useSSL=false";
        let dbs = extract_databases("test", content);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].db_type, "mysql");
        assert_eq!(dbs[0].host, "10.0.0.1");
        assert_eq!(dbs[0].port, 3306);
        assert_eq!(dbs[0].database, "testdb");
    }

    #[test]
    fn extract_jdbc_postgresql() {
        let content = "spring.datasource.url=jdbc:postgresql://pg.example.com:5432/mydb";
        let dbs = extract_databases("test", content);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].db_type, "postgresql");
        assert_eq!(dbs[0].host, "pg.example.com");
        assert_eq!(dbs[0].port, 5432);
    }

    #[test]
    fn extract_redis_url() {
        let content = "spring.redis.url=redis://redis.internal:6379/1";
        let dbs = extract_databases("test", content);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].db_type, "redis");
        assert_eq!(dbs[0].host, "redis.internal");
    }

    #[test]
    fn extract_redis_host() {
        let content = "spring.redis.host=10.0.0.5\nspring.redis.port=6379";
        let dbs = extract_databases("test", content);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].db_type, "redis");
        assert_eq!(dbs[0].host, "10.0.0.5");
    }

    #[test]
    fn extract_kafka_bootstrap() {
        let content = "spring.kafka.bootstrap-servers=kafka1:9092,kafka2:9093";
        let dbs = extract_databases("test", content);
        assert!(dbs.len() >= 2);
        assert_eq!(dbs[0].db_type, "kafka");
    }

    #[test]
    fn extract_config_keys_properties() {
        let content =
            "spring.datasource.url=jdbc:mysql://localhost/db\nserver.port=8080\nlogging.level=DEBUG";
        let keys = extract_config_keys("test", content);
        assert!(keys.len() >= 2);
        let port_key = keys.iter().find(|k| k.name == "server.port").unwrap();
        assert_eq!(port_key.value, "8080");
        assert_eq!(port_key.purpose, "Server");
    }

    #[test]
    fn classify_database_key() {
        assert_eq!(classify_key("spring.datasource.url"), "Database");
        assert_eq!(classify_key("jdbc.url"), "Database");
    }

    #[test]
    fn classify_redis_key() {
        assert_eq!(classify_key("spring.redis.host"), "Cache");
    }

    #[test]
    fn classify_kafka_key() {
        assert_eq!(classify_key("spring.kafka.bootstrap-servers"), "MessageQueue");
    }

    #[test]
    fn default_port_values() {
        assert_eq!(default_port_for("mysql"), 3306);
        assert_eq!(default_port_for("postgresql"), 5432);
        assert_eq!(default_port_for("redis"), 6379);
        assert_eq!(default_port_for("kafka"), 9092);
        assert_eq!(default_port_for("unknown"), 0);
    }

    #[test]
    fn config_sync_source_name() {
        let client = NacosClient::new("https://example.com");
        let src = ConfigSyncSource::new(client, "test".into());
        assert_eq!(src.name(), "nacos/config");
    }

    // -------------------------------------------------------------------
    // ConfigVectorizer tests
    // -------------------------------------------------------------------

    use crate::domain::types::CollectionInfo;

    /// Mock EmbedService — returns zero vectors of correct dimension.
    struct MockEmbed;
    #[async_trait::async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.0_f32; 384]).collect())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    /// Mock VectorRepository — captures upsert calls.
    use std::sync::Mutex;
    struct MockVector {
        upserted: Mutex<Vec<serde_json::Value>>,
    }
    impl MockVector {
        fn new() -> Self {
            Self {
                upserted: Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait::async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _collection: &str, _dim: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self, _collection: &str, _vector: Vec<f32>, _limit: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
        }
        async fn upsert(
            &self, _collection: &str, points: Vec<serde_json::Value>,
        ) -> Result<(), DtError> {
            if let Ok(mut v) = self.upserted.lock() {
                v.extend(points);
            }
            Ok(())
        }
        async fn delete_by_filter(
            &self, _collection: &str, _filter: serde_json::Value,
        ) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(&self, _name: &str) -> Result<CollectionInfo, DtError> {
            Err(DtError::NotFound("mock".into()))
        }
        async fn delete_collection(&self, _name: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn vectorize_configs_empty_returns_zero() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigVectorizer::new(embed, vector);
        let count = cv.vectorize_configs(&[], "test").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn vectorize_configs_upserts_points() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigVectorizer::new(embed, vector.clone());

        let entries = vec![
            NacosConfigEntry {
                entity_id: "dt://nacos/ns1/app.yaml/server.port".into(),
                key: "server.port".into(),
                value: "8080".into(),
                namespace: "ns1".into(),
                data_id: "app.yaml".into(),
                group: "DEFAULT_GROUP".into(),
            },
            NacosConfigEntry {
                entity_id: "dt://nacos/ns1/app.yaml/spring.datasource.url".into(),
                key: "spring.datasource.url".into(),
                value: "jdbc:mysql://localhost:3306/db".into(),
                namespace: "ns1".into(),
                data_id: "app.yaml".into(),
                group: "DEFAULT_GROUP".into(),
            },
        ];

        let count = cv.vectorize_configs(&entries, "test-proj").await.unwrap();
        assert_eq!(count, 2);

        let upserted = vector.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 2);

        // Verify payload shape
        let p0 = &upserted[0];
        assert_eq!(p0["id"], "dt://nacos/ns1/app.yaml/server.port");
        assert_eq!(p0["payload"]["key"], "server.port");
        assert_eq!(p0["payload"]["value"], "8080");
        assert_eq!(p0["payload"]["namespace"], "ns1");
        assert_eq!(p0["payload"]["source_type"], "nacos_config");
    }

    #[tokio::test]
    async fn vectorize_config_keys_builds_entries() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigVectorizer::new(embed, vector.clone());

        let keys = vec![
            ConfigKeyEntry {
                name: "server.port".into(),
                value: "8080".into(),
                namespace: "prod".into(),
                purpose: "Server".into(),
            },
        ];

        let count = cv
            .vectorize_config_keys(&keys, "prod", "app.yaml", "DEFAULT", "p")
            .await
            .unwrap();
        assert_eq!(count, 1);

        let upserted = vector.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 1);
        // Entity ID uses underscores for replacements
        assert_eq!(
            upserted[0]["id"].as_str().unwrap(),
            "dt://nacos/prod/app.yaml/server_port"
        );
    }

    #[test]
    fn nacos_config_entry_constructs() {
        let e = NacosConfigEntry {
            entity_id: "dt://nacos/test/app.properties/server.port".into(),
            key: "server.port".into(),
            value: "8080".into(),
            namespace: "test".into(),
            data_id: "app.properties".into(),
            group: "DEFAULT".into(),
        };
        assert_eq!(e.key, "server.port");
        assert_eq!(e.value, "8080");
        assert_eq!(e.entity_id, "dt://nacos/test/app.properties/server.port");
    }

    // -------------------------------------------------------------------
    // ConfigChunkVectorizer tests
    // -------------------------------------------------------------------

    #[test]
    fn build_chunk_text_formats_correctly() {
        let chunk = ChunkToVectorize {
            section_name: "spring.datasource".into(),
            key_values: vec![
                ("spring.datasource.url".into(), "jdbc:mysql://localhost/db".into()),
                ("spring.datasource.username".into(), "admin".into()),
            ],
        };
        let text = build_chunk_text(&chunk);
        assert!(text.starts_with("spring.datasource\n"));
        assert!(text.contains("spring.datasource.url=jdbc:mysql://localhost/db"));
        assert!(text.contains("spring.datasource.username=admin"));
    }

    #[tokio::test]
    async fn chunk_vectorizer_empty_returns_zero() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigChunkVectorizer::new(embed, vector);
        let count = cv
            .vectorize_chunks(&[], "test", "app.yaml", "DEFAULT", "yaml", None)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn chunk_vectorizer_upserts_to_config_chunks() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigChunkVectorizer::new(embed, vector.clone());

        let chunks = vec![ChunkToVectorize {
            section_name: "spring.datasource".into(),
            key_values: vec![
                ("spring.datasource.url".into(), "jdbc:mysql://localhost/db".into()),
                ("spring.datasource.username".into(), "admin".into()),
            ],
        }];

        let count = cv
            .vectorize_chunks(&chunks, "prod", "app.properties", "DEFAULT", "properties", Some("test"))
            .await
            .unwrap();
        assert_eq!(count, 1);

        let upserted = vector.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 1);
        assert_eq!(upserted[0]["payload"]["section_name"], "spring.datasource");
        assert_eq!(upserted[0]["payload"]["key_count"], 2);
        assert_eq!(upserted[0]["payload"]["source_type"], "config_chunk");
        assert_eq!(upserted[0]["payload"]["environment"], "test");
        assert!(upserted[0]["payload"]["text"]
            .as_str()
            .unwrap()
            .contains("spring.datasource.url=jdbc:mysql://localhost/db"));
    }

    #[tokio::test]
    async fn chunk_vectorizer_delete_by_data_id() {
        let embed = Arc::new(MockEmbed);
        let vector = Arc::new(MockVector::new());
        let cv = ConfigChunkVectorizer::new(embed, vector);
        // Just verify it doesn't panic — mock delete_by_filter returns Ok
        cv.delete_by_data_id("test", "app.yaml").await.unwrap();
    }

    /// Integration: adaptive chunk → vectorize for a YAML snippet.
    #[test]
    fn adaptive_chunk_to_chunk_vectorize_integration() {
        let yaml = "\
spring:\n  datasource:\n    url: jdbc:mysql://localhost/db\n    username: admin\n\
  redis:\n    host: 127.0.0.1\n    port: 6379";

        let sections = chunk_config_adaptive(yaml, true);
        eprintln!("Adaptive sections: {:?}", sections.iter().map(|(n,_)| n.as_str()).collect::<Vec<_>>());
        assert!(!sections.is_empty(), "expected at least 1 section");

        // Verify druid-style grouping works on chunk text
        for (name, pairs) in &sections {
            assert!(!name.is_empty());
            assert!(!pairs.is_empty());
        }
    }

    /// Integration: properties adaptive chunk → vectorize
    #[test]
    fn adaptive_props_chunk_to_vectorize_integration() {
        let props = "\
spring.datasource.url=jdbc:mysql://localhost/db\n\
spring.datasource.username=admin\n\
spring.datasource.password=secret\n\
spring.redis.host=127.0.0.1\n\
spring.redis.port=6379";

        let sections = chunk_config_adaptive(props, false);
        eprintln!("Props sections: {:?}", sections.iter().map(|(n,_)| n.as_str()).collect::<Vec<_>>());
        assert!(sections.len() >= 2, "expected >=2 sections, got {}", sections.len());
        assert!(sections.iter().any(|(n,_)| n == "spring.datasource"));
        assert!(sections.iter().any(|(n,_)| n == "spring.redis"));
    }
}
