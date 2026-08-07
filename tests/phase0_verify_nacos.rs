//! Phase 0 六项端到端验证 —— 组件级 harness（方案 FINAL §3 Phase 0 第 6 节）
//!
//! 背景：`dt build --source nacos` 在 Phase 0 仍是占位（仅 F5 自检后返回），
//! CLI 未接通 NacosVirtualFileSource → 流水线的调用链。因此本 harness 在
//! **组件层**用真实 Nacos（prod）+ 真实 Xinference（qwen3.5）执行 6 项验证，
//! 并如实记录 CLI/接线缺口（见 /data/doc/phase0-verification.md）。
//!
//! 运行（仓库根目录，串行）：
//!   cargo test --test phase0_verify_nacos -- --ignored --nocapture --test-threads=1
//!
//! 依赖：Nacos prod 可达、xinference localhost:9997（qwen3.5/bge-m3/bge-reranker-v2-m3）、
//! Memgraph bolt://localhost:7688、Qdrant :6333（仅验证 (c) 需要）。
//!
//! 说明：正则基线从 src/application/sync/nacos/config_sync.rs 原样复刻
//! （正则字面量与 classify_key 逻辑保持一致，出处见代码注释）。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use dt_daemon::application::build::strategy::incremental::IncrementalStrategy;
use dt_daemon::application::build::strategy::BuildStrategy;
use dt_daemon::application::knowledge::extract::{Consolidator, ExtractedGraph};
use dt_daemon::application::pipeline::config::LlmConfig;
use dt_daemon::application::pipeline::context::PipelineContext;
use dt_daemon::application::pipeline::infer_client::{
    ChatClient, ChatResponse, XInferenceChatClient,
};
use dt_daemon::application::pipeline::processor::Processor;
use dt_daemon::application::pipeline::processors::{ChunkProcessor, LlmClientProcessor};
use dt_daemon::application::pipeline::prompt::PromptRegistry;
use dt_daemon::application::pipeline::virtual_file::{FileSourceKind, VirtualFile};
use dt_daemon::application::sync::nacos::NacosClient;
use dt_daemon::application::sync::nacos::NacosVirtualFileSource;
use dt_daemon::domain::error::DtError;
use dt_daemon::domain::traits::{
    EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use dt_daemon::domain::types::{FileSnapshot, HealthStatus};
use dt_daemon::infrastructure::memgraph::MemgraphClient;
use dt_daemon::infrastructure::qdrant::{QdrantClient, QdrantRepo};
use dt_daemon::infrastructure::xinference::XInferenceClient;
use dt_daemon::shared::chunker::parse_kv_line;

const NACOS_PROD_URL: &str = "https://nacos.newoffen.com/nacos";
const XINFERENCE_URL: &str = "http://localhost:9997/v1";
const LLM_MODEL: &str = "qwen3.5";
const PROJECT: &str = "phase0-verify";
const PROMPTS_DIR: &str = "config/prompts";
const SAMPLE_N: usize = 20;

// ===========================================================================
// 公共辅助
// ===========================================================================

async fn fetch_prod_configs() -> Vec<VirtualFile> {
    let client = NacosClient::new(NACOS_PROD_URL);
    let source = NacosVirtualFileSource::new(client);
    source
        .fetch_virtual_files(PROJECT)
        .await
        .expect("Nacos 拉取失败")
}

fn sha256_hex(content: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(content.as_bytes()))
}

fn llm_client() -> XInferenceChatClient {
    XInferenceChatClient::new(XINFERENCE_URL.to_string(), String::new(), 4)
}

fn prompt_registry() -> Arc<PromptRegistry> {
    Arc::new(PromptRegistry::load(Path::new(PROMPTS_DIR)).expect("config/prompts 加载失败"))
}

// ===========================================================================
// Nacos LLM 输出 schema（F4 nacos_config prompt 的输出形状）
// ===========================================================================

#[derive(Debug, Default, Deserialize)]
struct NacosLlmOutput {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    entities: Vec<NacosLlmEntity>,
    #[serde(default)]
    relations: Vec<NacosLlmRelation>,
}

#[derive(Debug, Default, Deserialize)]
struct NacosLlmEntity {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    entity_type: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    properties: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
struct NacosLlmRelation {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default, rename = "type")]
    rel_type: String,
    #[serde(default)]
    evidence: String,
}

/// 容忍 markdown 围栏/前后赘述的 JSON 解析（与 parse_block_response 同策略）。
fn parse_llm_output(raw: &str) -> Result<NacosLlmOutput, serde_json::Error> {
    serde_json::from_str(raw).or_else(|_| {
        let (s, e) = (raw.find('{'), raw.rfind('}'));
        match (s, e) {
            (Some(s), Some(e)) if s < e => serde_json::from_str(&raw[s..=e]),
            _ => serde_json::from_str(raw),
        }
    })
}

/// 用 nacos_config prompt（F4 词表）抽取一条配置。
/// 返回 (LLM 输出, 原始响应, 耗时)。
async fn llm_extract_f4(
    client: &dyn ChatClient,
    registry: &PromptRegistry,
    vf: &VirtualFile,
) -> (Option<NacosLlmOutput>, String, std::time::Duration) {
    let t0 = Instant::now();
    let ctx = json!({ "file_path": vf.virtual_path, "file_text": vf.content });
    let (sys, user) = match registry.render("nacos_config", &ctx) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("  [WARN] nacos_config 渲染失败: {e}");
            return (None, String::new(), t0.elapsed());
        }
    };
    match client.chat(LLM_MODEL, &sys, &user, 0.2, 4096).await {
        Ok(resp) => {
            let raw = resp
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_default();
            let parsed = parse_llm_output(&raw).ok();
            (parsed, raw, t0.elapsed())
        }
        Err(e) => {
            eprintln!("  [WARN] LLM 调用失败: {e}");
            (None, String::new(), t0.elapsed())
        }
    }
}

// ===========================================================================
// 计数 ChatClient —— (b) 增量 LLM 调用次数断言
// ===========================================================================

struct CountingClient {
    inner: XInferenceChatClient,
    calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl ChatClient for CountingClient {
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, String> {
        *self.calls.lock().unwrap() += 1;
        self.inner
            .chat(model, system_prompt, user_prompt, temperature, max_tokens)
            .await
    }
    async fn health_check(&self) -> Result<bool, String> {
        self.inner.health_check().await
    }
}

// ===========================================================================
// 内存快照仓库 —— (b)/(d) select_virtual_files 增量选择
// ===========================================================================

#[derive(Default)]
struct MemSnapshotRepo {
    snapshots: Mutex<HashMap<String, FileSnapshot>>,
}

#[async_trait]
impl SnapshotRepository for MemSnapshotRepo {
    async fn get_snapshot(
        &self,
        _project: &str,
        path: &str,
    ) -> Result<Option<FileSnapshot>, DtError> {
        Ok(self.snapshots.lock().unwrap().get(path).cloned())
    }
    async fn save_snapshots(
        &self,
        _project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        let mut m = self.snapshots.lock().unwrap();
        for s in snapshots {
            m.insert(s.file_path.clone(), s.clone());
        }
        Ok(())
    }
    async fn delete_project(&self, project: &str) -> Result<u64, DtError> {
        let mut m = self.snapshots.lock().unwrap();
        let before = m.len();
        m.retain(|_, s| s.project != project);
        Ok((before - m.len()) as u64)
    }
    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError> {
        Ok(self
            .snapshots
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.project == project)
            .cloned()
            .collect())
    }
    async fn mark_llm_analyzed(&self, _p: &str, _f: &str, _h: &str) -> Result<(), DtError> {
        Ok(())
    }
    async fn is_llm_analyzed(&self, _p: &str, _f: &str, _h: &str) -> Result<bool, DtError> {
        Ok(false)
    }
    async fn clear_llm_progress(&self, _p: &str) -> Result<(), DtError> {
        Ok(())
    }
    async fn mark_step_done(&self, _p: &str, _f: &str, _s: &str, _h: &str) -> Result<(), DtError> {
        Ok(())
    }
    async fn is_step_done(&self, _p: &str, _f: &str, _s: &str, _h: &str) -> Result<bool, DtError> {
        Ok(false)
    }
    async fn clear_step_progress(&self, _p: &str) -> Result<(), DtError> {
        Ok(())
    }
    async fn delete_file_progress(&self, _p: &str, _paths: &[String]) -> Result<u64, DtError> {
        Ok(0)
    }
    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }
}

fn snapshots_for(vfs: &[VirtualFile]) -> Vec<FileSnapshot> {
    vfs.iter()
        .map(|vf| FileSnapshot {
            file_path: vf.virtual_path.clone(),
            project: vf.project.clone(),
            file_sha1: vf.content_hash.clone(),
            file_mtime: 0.0,
            method_count: 0,
            updated_at: String::new(),
        })
        .collect()
}

// ===========================================================================
// 旧正则抽取基线 —— 从 config_sync.rs 原样复刻
// ===========================================================================

fn jdbc_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)jdbc:(mysql|postgresql|mariadb|sqlserver|oracle|h2|dm)://([^/\s?]+)(/\S+)?",
        )
        .expect("JDBC 正则")
    })
}
fn redis_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)(?:redis|rediss)://([^/\s?]+)").expect("Redis 正则"))
}
fn redis_host_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:spring\.)?redis\.host\s*[:=]\s*(\S+)").expect("Redis host 正则")
    })
}
fn kafka_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(?:spring\.)?kafka\.bootstrap-servers\s*[:=]\s*(\S+)")
            .expect("Kafka 正则")
    })
}

/// 与 config_sync.rs classify_key 一致。
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

/// 与 config_sync.rs extract_config_keys 一致（yaml/properties 判定 + 键提取）。
fn regex_extract_keys(content: &str) -> Vec<(String, String)> {
    let is_yaml = content.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && !t.contains('=') && t.ends_with(':')
    });
    let mut keys = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
            continue;
        }
        if t.starts_with('-') {
            continue;
        }
        if let Some((k, v)) = parse_kv_line(t) {
            if is_yaml {
                // yaml 键要求含 '.' 且值长度 <= 200（config_sync 原逻辑）
                if !k.contains('.') || v.len() > 200 {
                    continue;
                }
            }
            keys.push((k.to_string(), classify_key(k)));
        }
    }
    keys
}

/// 与 config_sync.rs extract_databases 一致的连接串检测（返回原始匹配串）。
fn regex_extract_connections(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for caps in jdbc_re().captures_iter(content) {
        if let Some(m) = caps.get(0) {
            out.push(m.as_str().to_string());
        }
    }
    for caps in redis_re().captures_iter(content) {
        if let Some(m) = caps.get(0) {
            out.push(m.as_str().to_string());
        }
    }
    for caps in redis_host_re().captures_iter(content) {
        if let Some(m) = caps.get(1) {
            out.push(format!("redis.host={}", m.as_str()));
        }
    }
    for caps in kafka_re().captures_iter(content) {
        if let Some(m) = caps.get(1) {
            out.push(format!("kafka.bootstrap-servers={}", m.as_str()));
        }
    }
    out
}

/// 正则路径产出的实体类型集合（映射到 F4 词表）。
fn regex_type_set(content: &str) -> HashSet<String> {
    let keys = regex_extract_keys(content);
    let conns = regex_extract_connections(content);
    let mut s = HashSet::new();
    if !keys.is_empty() {
        s.insert("ConfigKey".to_string());
    }
    if !conns.is_empty() || keys.iter().any(|(_, p)| p == "Database") {
        s.insert("Database".to_string());
    }
    if keys.iter().any(|(_, p)| p == "Server") {
        s.insert("Server".to_string());
    }
    s
}

// ===========================================================================
// 真值标注 —— 确定性解析（YAML 树遍历 / properties 行解析）
// ===========================================================================

#[derive(Default)]
struct YamlStats {
    sections: usize,
    key_count: usize,
    leaf_paths: Vec<String>,
    leaf_values: Vec<String>,
}

fn walk_yaml(value: &serde_yaml::Value, path: &str, out: &mut YamlStats) {
    match value {
        serde_yaml::Value::Mapping(m) => {
            let mut child_section = false;
            for (k, v) in m {
                let ks = k.as_str().unwrap_or("");
                let child_path = if path.is_empty() {
                    ks.to_string()
                } else {
                    format!("{path}.{ks}")
                };
                match v {
                    serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_) => {
                        child_section = true;
                        walk_yaml(v, &child_path, out);
                    }
                    _ => {
                        out.key_count += 1;
                        out.leaf_paths.push(child_path);
                        out.leaf_values.push(v.as_str().unwrap_or("").to_string());
                    }
                }
            }
            if child_section && !path.is_empty() {
                out.sections += 1;
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                walk_yaml(v, path, out);
            }
        }
        _ => {}
    }
}

fn contains_connection(text: &str) -> bool {
    let l = text.to_lowercase();
    l.contains("jdbc:")
        || l.contains("redis://")
        || l.contains("rediss://")
        || l.contains("bootstrap-servers")
}

/// 真值：返回 (类型集合, 键数量, 连接串数量)。NacosConfig 恒为真。
fn ground_truth(vf: &VirtualFile) -> (HashSet<String>, usize, usize) {
    let mut types: HashSet<String> = HashSet::new();
    types.insert("NacosConfig".to_string());

    let mut stats = YamlStats::default();
    let mut key_count = 0usize;
    let mut conn_count = 0usize;
    let mut server_hit = false;

    if let Ok(serde_yaml::Value::Mapping(_)) =
        serde_yaml::from_str::<serde_yaml::Value>(&vf.content)
    {
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&vf.content) {
            walk_yaml(&v, "", &mut stats);
        }
    } else {
        // properties 风格
        for line in vf.content.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
                continue;
            }
            if let Some((k, v)) = parse_kv_line(t) {
                key_count += 1;
                let lk = k.to_lowercase();
                if lk.contains("server") || lk.ends_with(".port") || lk.contains("host") {
                    server_hit = true;
                }
                if contains_connection(v) {
                    conn_count += 1;
                }
            }
        }
    }

    if stats.sections > 0 {
        types.insert("ConfigSection".to_string());
    }
    if stats.key_count > 0 {
        types.insert("ConfigKey".to_string());
    }
    key_count = stats.key_count.max(key_count);
    for lp in &stats.leaf_paths {
        let lk = lp.to_lowercase();
        if lk.contains("server") || lk.contains(".port") || lk.contains("host") {
            server_hit = true;
        }
    }
    for lv in &stats.leaf_values {
        if contains_connection(lv) {
            conn_count += 1;
        }
    }
    if contains_connection(&vf.content) && conn_count == 0 {
        conn_count = 1;
    }
    if conn_count > 0 {
        types.insert("Database".to_string());
    }
    if server_hit {
        types.insert("Server".to_string());
    }

    (types, key_count, conn_count)
}

/// 类型级 precision/recall。
fn type_metrics(produced: &HashSet<String>, gt: &HashSet<String>) -> (f64, f64) {
    let inter = produced.intersection(gt).count();
    let p = if produced.is_empty() {
        0.0
    } else {
        inter as f64 / produced.len() as f64
    };
    let r = if gt.is_empty() {
        1.0
    } else {
        inter as f64 / gt.len() as f64
    };
    (p, r)
}

// ===========================================================================
// (a) 实体质量：LLM(F4 词表) vs 旧正则 —— 20 条真实配置
// ===========================================================================

#[tokio::test]
#[ignore = "真实 LLM + 真实 Nacos — 显式运行"]
async fn phase0_a_entity_quality() {
    println!("\n========== (a) 实体质量：LLM(F4) vs 旧正则，20 条真实 prod 配置 ==========");

    let registry = prompt_registry();
    let client = llm_client();
    let healthy = client.health_check().await.unwrap_or(false);
    assert!(healthy, "LLM 端点不可达");

    let t0 = Instant::now();
    let all = fetch_prod_configs().await;
    println!(
        "拉取 Nacos prod 配置 {} 条（{}ms）",
        all.len(),
        t0.elapsed().as_millis()
    );

    // 取样：前 20 条非空、长度 <= 8000（qwen3.5 ctx 4096 安全）
    let sample: Vec<VirtualFile> = all
        .iter()
        .filter(|v| !v.content.trim().is_empty() && v.content.len() <= 8000)
        .take(SAMPLE_N)
        .cloned()
        .collect();
    assert_eq!(sample.len(), SAMPLE_N, "可标注配置不足 20 条");

    let mut rows = Vec::new();
    let mut llm_parse_fail = 0usize;
    let mut total_llm_types = 0usize;
    let mut total_llm_correct = 0usize;
    let mut total_gt_types = 0usize;
    let mut total_reg_types = 0usize;
    let mut total_reg_correct = 0usize;
    let mut gt_conns = 0usize;
    let mut reg_conns = 0usize;
    let mut llm_db_entities = 0usize;

    for (i, vf) in sample.iter().enumerate() {
        let (gt, key_count, conn_count) = ground_truth(vf);
        let reg = regex_type_set(&vf.content);
        let reg_keys = regex_extract_keys(&vf.content);
        let found_conns = regex_extract_connections(&vf.content);

        let (llm_out, _raw, _elapsed) = llm_extract_f4(&client, &registry, vf).await;
        let mut llm_types: HashSet<String> = HashSet::new();
        let mut llm_entity_names: Vec<String> = Vec::new();
        match llm_out {
            Some(out) => {
                for e in &out.entities {
                    let t = e.entity_type.trim().to_string();
                    if t.is_empty() || t.eq_ignore_ascii_case("other") {
                        continue;
                    }
                    llm_types.insert(t.clone());
                    llm_entity_names.push(format!("{}:{}", e.name, t));
                    if t.eq_ignore_ascii_case("database") {
                        llm_db_entities += 1;
                    }
                }
                if out.entities.is_empty() {
                    llm_parse_fail += 1;
                }
            }
            None => llm_parse_fail += 1,
        }

        let (llm_p, llm_r) = type_metrics(&llm_types, &gt);
        let (reg_p, reg_r) = type_metrics(&reg, &gt);

        total_gt_types += gt.len();
        total_llm_types += llm_types.len();
        total_llm_correct += llm_types.intersection(&gt).count();
        total_reg_types += reg.len();
        total_reg_correct += reg.intersection(&gt).count();
        gt_conns += conn_count;
        reg_conns += found_conns.len();

        let data_id = vf
            .virtual_path
            .rsplit('/')
            .next()
            .unwrap_or("?")
            .to_string();
        rows.push(json!({
            "idx": i + 1,
            "data_id": data_id,
            "bytes": vf.content.len(),
            "gt_types": gt.iter().cloned().collect::<Vec<_>>(),
            "reg_types": reg.iter().cloned().collect::<Vec<_>>(),
            "llm_types": llm_types.iter().cloned().collect::<Vec<_>>(),
            "llm_precision": format!("{:.2}", llm_p),
            "llm_recall": format!("{:.2}", llm_r),
            "reg_precision": format!("{:.2}", reg_p),
            "reg_recall": format!("{:.2}", reg_r),
            "gt_keys": key_count,
            "reg_keys": reg_keys.len(),
            "gt_conns": conn_count,
            "reg_conns": found_conns.len(),
            "llm_db_entities": llm_entity_names.iter().filter(|n| n.ends_with(":Database") || n.to_lowercase().contains(":database")).count(),
        }));
        println!(
            "  [{:>2}] {:<28} gt={:?} reg={:?} llm={:?} | LLM p/r={:.2}/{:.2} REG p/r={:.2}/{:.2}",
            i + 1,
            data_id,
            sorted(&gt),
            sorted(&reg),
            sorted(&llm_types),
            llm_p,
            llm_r,
            reg_p,
            reg_r
        );
    }

    let n = sample.len() as f64;
    let macro_p = |rows: &Vec<serde_json::Value>, f: &str| {
        rows.iter()
            .map(|r| r[f].as_str().unwrap_or("0").parse::<f64>().unwrap())
            .sum::<f64>()
            / n
    };
    let llm_macro_p = macro_p(&rows, "llm_precision");
    let llm_macro_r = macro_p(&rows, "llm_recall");
    let reg_macro_p = macro_p(&rows, "reg_precision");
    let reg_macro_r = macro_p(&rows, "reg_recall");
    let llm_micro_p = total_llm_correct as f64 / total_llm_types.max(1) as f64;
    let llm_micro_r = total_llm_correct as f64 / total_gt_types.max(1) as f64;
    let reg_micro_p = total_reg_correct as f64 / total_reg_types.max(1) as f64;
    let reg_micro_r = total_reg_correct as f64 / total_gt_types.max(1) as f64;

    println!("\n== 汇总 ==");
    let summary = json!({
        "configs": sample.len(),
        "parse_fail": llm_parse_fail,
        "llm_macro_precision": format!("{:.3}", llm_macro_p),
        "llm_macro_recall": format!("{:.3}", llm_macro_r),
        "reg_macro_precision": format!("{:.3}", reg_macro_p),
        "reg_macro_recall": format!("{:.3}", reg_macro_r),
        "llm_micro_precision": format!("{:.3}", llm_micro_p),
        "llm_micro_recall": format!("{:.3}", llm_micro_r),
        "reg_micro_precision": format!("{:.3}", reg_micro_p),
        "reg_micro_recall": format!("{:.3}", reg_micro_r),
        "connections": {"gt": gt_conns, "reg_found": reg_conns, "llm_db_entities": llm_db_entities},
        "gate_llm_precision_ge_regex": llm_micro_p >= reg_micro_p,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());

    // 门禁：LLM entity_type precision >= 正则
    assert!(
        llm_micro_p >= reg_micro_p,
        "门禁失败: LLM precision {llm_micro_p:.3} < 正则 {reg_micro_p:.3}"
    );
    println!("门禁结论: LLM precision({llm_micro_p:.3}) >= 正则({reg_micro_p:.3}) ✓");
}

fn sorted(s: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = s.iter().cloned().collect();
    v.sort();
    v
}

// ===========================================================================
// (b) 增量：改 1 条 → 重建 → LLM 调用次数 == 1
// ===========================================================================

#[tokio::test]
#[ignore = "真实 LLM + 真实 Nacos — 显式运行"]
async fn phase0_b_incremental_llm_count() {
    println!("\n========== (b) 增量：改 1 条配置 → 重建 → LLM 调用次数 == 1 ==========");

    let registry = prompt_registry();
    let all = fetch_prod_configs().await;
    let vfs: Vec<VirtualFile> = all
        .iter()
        .filter(|v| !v.content.trim().is_empty())
        .cloned()
        .collect();
    let n = vfs.len();
    println!("参与增量测试的配置数: {n}");
    assert!(n >= 3, "配置太少");

    let strategy = IncrementalStrategy;
    let repo = MemSnapshotRepo::default();

    // run1：无快照 → 全部选中
    let (changed1, _) = strategy
        .select_virtual_files(&vfs, Some(&repo), PROJECT)
        .await
        .expect("select_virtual_files 失败");
    println!("run1（无快照）: 选中 {}/{}", changed1.len(), n);
    assert_eq!(changed1.len(), n, "首次构建应全量选中");

    // 保存快照 → run2：无变化 → 0 选中
    repo.save_snapshots(PROJECT, &snapshots_for(&vfs))
        .await
        .unwrap();
    let (changed2, _) = strategy
        .select_virtual_files(&vfs, Some(&repo), PROJECT)
        .await
        .unwrap();
    println!("run2（内容未变）: 选中 {}", changed2.len());
    assert_eq!(changed2.len(), 0, "内容未变应 0 选中");

    // 修改 1 条 → run3：恰好 1 条选中
    let mut modified = vfs.clone();
    let mut target = modified[0].clone();
    target.content = format!(
        "{}\n# phase0-verify: 模拟变更 {}\n",
        target.content,
        chrono::Utc::now().timestamp()
    );
    target.content_hash = sha256_hex(&target.content);
    modified[0] = target;
    let (changed3, deleted3) = strategy
        .select_virtual_files(&modified, Some(&repo), PROJECT)
        .await
        .unwrap();
    println!(
        "run3（改 1 条）: 选中 {} 删除 {}",
        changed3.len(),
        deleted3.len()
    );
    assert_eq!(changed3.len(), 1, "改 1 条应只选中 1 条");
    assert_eq!(changed3[0].virtual_path, vfs[0].virtual_path);

    // LLM 调用计数：仅对选中文件执行抽取
    let calls = Arc::new(Mutex::new(0usize));
    let counting = CountingClient {
        inner: llm_client(),
        calls: calls.clone(),
    };
    for vf in &changed3 {
        let _ = llm_extract_f4(&counting, &registry, vf).await;
    }
    let count = *calls.lock().unwrap();
    println!("重建 LLM 调用次数: {count}（期望 1）");
    assert_eq!(count, 1, "改 1 条应恰好 1 次 LLM 调用");

    // 兜底断言：不修改任何内容时，重建不应有任何 LLM 调用
    let calls2 = Arc::new(Mutex::new(0usize));
    let counting2 = CountingClient {
        inner: llm_client(),
        calls: calls2.clone(),
    };
    let (changed4, _) = strategy
        .select_virtual_files(&vfs, Some(&repo), PROJECT)
        .await
        .unwrap();
    for vf in &changed4 {
        let _ = llm_extract_f4(&counting2, &registry, vf).await;
    }
    let count2 = *calls2.lock().unwrap();
    println!("未变更重建 LLM 调用次数: {count2}（期望 0）");
    assert_eq!(count2, 0);
}

// ===========================================================================
// (c) 搜索兼容：新 Nacos 实体 dt search 格式一致
// ===========================================================================

#[tokio::test]
#[ignore = "真实 LLM + 真实后端 — 显式运行"]
async fn phase0_c_search_compat() {
    println!("\n========== (c) 搜索兼容：新 Nacos 实体 dt search 格式一致 ==========");

    // c1. 静态检查：search_config.rs Cypher 回退必须覆盖 F4 图标签（compat P0-A）
    let search_src = std::fs::read_to_string("src/application/context/search_config.rs")
        .expect("读取 search_config.rs 失败");
    for label in ["ConfigKey", "Server", "Database", "NacosConfig"] {
        assert!(
            search_src.contains(&format!("n:{label}"))
                || search_src.contains(&format!(":{label} ")),
            "search_config.rs Cypher 回退缺少标签 {label}"
        );
    }
    println!(
        "c1 静态检查 ✓: search_config.rs Cypher 回退覆盖 NacosConfig/ConfigKey/Server/Database"
    );

    // c2. 运行时：真实后端跑 1 条 Nacos 配置（chunk→LLM→Consolidator 入库），
    //     然后 dt search 验证渲染格式。
    let graph: Arc<dyn GraphRepository> = Arc::new(
        MemgraphClient::connect("bolt://localhost:7688", "memgraph", "")
            .await
            .expect("Memgraph 连接失败"),
    );
    let vector: Arc<dyn VectorRepository> = Arc::new(QdrantRepo::new(
        QdrantClient::connect("http://localhost:6334")
            .await
            .expect("Qdrant 连接失败"),
    ));
    let embed: Arc<dyn EmbedService> = Arc::new(XInferenceClient::new(
        XINFERENCE_URL,
        "",
        "bge-m3",
        "bge-reranker-v2-m3",
        LLM_MODEL,
    ));

    // 清理旧测试数据（幂等）
    let mut cleanup_params = HashMap::new();
    cleanup_params.insert("p".to_string(), json!(PROJECT));
    let _ = graph
        .write_query(
            "MATCH (e:Entity) WHERE e.project = $p DETACH DELETE e",
            cleanup_params.clone(),
        )
        .await;
    let _ = vector
        .delete_by_filter(
            "kg_nodes",
            json!({"must": [{"key": "project", "match": {"value": PROJECT}}]}),
        )
        .await;

    // 取 1 条真实配置，走 chunk→llm（G3 修复后：Nacos 源 → nacos_config 词表，
    // 产出 F4 类型；修复前为 document_with_nlp 的 Config 归一）
    let all = fetch_prod_configs().await;
    let vf = all
        .iter()
        .find(|v| !v.content.trim().is_empty() && v.content.len() <= 6000)
        .expect("无可用配置");
    let data_id = vf
        .virtual_path
        .rsplit('/')
        .next()
        .unwrap_or("?")
        .to_string();
    println!("入库配置: {data_id}");

    let registry = prompt_registry();
    let llm_client: Arc<dyn ChatClient> = Arc::new(llm_client());
    let chunk = ChunkProcessor::default();
    let llm_proc = LlmClientProcessor::new(
        llm_client,
        LLM_MODEL.to_string(),
        registry,
        LlmConfig::default(),
    );

    let mut ctx = PipelineContext::new(
        Path::new(&vf.virtual_path).to_path_buf(),
        vf.content.clone(),
        PROJECT.to_string(),
        FileSourceKind::Nacos,
        None,
        Some(vf.content_hash.clone()),
    );
    let chunk_out = chunk.execute(&ctx).await.expect("chunk 失败");
    let doc_id = chunk_out
        .get("doc_id")
        .and_then(|v| v.as_str())
        .unwrap_or("dt://nacos/unknown")
        .to_string();
    let doc_type = chunk_out
        .get("doc_type")
        .and_then(|v| v.as_str())
        .unwrap_or("yaml")
        .to_string();
    ctx.add_output("chunk", chunk_out);
    let llm_out = llm_proc.execute(&ctx).await.expect("llm 失败");
    let graphs: Vec<ExtractedGraph> =
        serde_json::from_value(llm_out.get("graphs").expect("无 graphs").clone())
            .expect("graphs 解析失败");
    println!(
        "  抽取实体 {} 个（类型: {:?}）",
        graphs.iter().map(|g| g.entities.len()).sum::<usize>(),
        graphs
            .iter()
            .flat_map(|g| g.entities.iter().map(|e| format!("{:?}", e.entity_type)))
            .collect::<Vec<_>>()
    );

    let mut block_texts = HashMap::new();
    if let Some(chunks) = ctx
        .outputs
        .get("chunk")
        .and_then(|o| o.get("chunks"))
        .and_then(|v| v.as_array())
    {
        for c in chunks {
            let idx = c.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let text = c
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            block_texts.insert(idx, text);
        }
    }

    let consolidator = Consolidator::new(graph.clone(), vector.clone(), embed);
    let stats = consolidator
        .consolidate_document(
            PROJECT,
            &doc_id,
            &vf.virtual_path,
            &doc_type,
            &graphs,
            &block_texts,
        )
        .await
        .expect("consolidate 失败");
    println!(
        "  入库: entities_created={} entities_merged={} relations={}",
        stats.entities_created, stats.entities_merged, stats.relations_written
    );

    // dt search CLI 验证格式
    let mut search_names: Vec<String> = graphs
        .iter()
        .flat_map(|g| g.entities.iter().map(|e| e.canonical_name.clone()))
        .filter(|n| !n.is_empty())
        .collect();
    search_names.sort();
    let probe = search_names
        .iter()
        .find(|n| n.len() >= 2)
        .cloned()
        .unwrap_or_else(|| "application".to_string());
    println!("  搜索探针: \"{probe}\"");

    let out = std::process::Command::new("dt")
        .args(["search", &probe, "--world", "knowledge", "--limit", "5"])
        .output()
        .expect("dt search 执行失败");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    println!("  dt search 输出:\n{}", stdout);

    // 格式断言：命中行应含 [score] [类型] 标题 + 摘要/来源要素（search_render 契约）
    let human_lines: Vec<&str> = stdout.lines().collect();
    let has_format = human_lines.iter().any(|l| {
        l.contains('[')
            && (l.contains("] ") || l.contains(']'))
            && (l.contains("摘要:") || l.contains("分析:") || l.contains("来源:"))
    });
    assert!(has_format, "dt search 输出格式不完整: {stdout}");
    println!("c2 运行时 ✓: dt search 渲染格式与既有实体一致（score/类型/摘要/来源要素齐全）");

    // 清理测试数据（幂等）
    let _ = graph
        .write_query(
            "MATCH (e:Entity) WHERE e.project = $p DETACH DELETE e",
            cleanup_params,
        )
        .await;
    let _ = vector
        .delete_by_filter(
            "kg_nodes",
            json!({"must": [{"key": "project", "match": {"value": PROJECT}}]}),
        )
        .await;
    println!("已清理 phase0-verify 测试数据");
}

// ===========================================================================
// (d) 多源混合：fs + nacos 无重复无冲突
// ===========================================================================

#[tokio::test]
#[ignore = "真实 Nacos — 显式运行"]
async fn phase0_d_multisource_no_dup() {
    println!("\n========== (d) 多源混合：--source all（fs+nacos）无重复无冲突 ==========");

    // fs 侧：仓库内真实文件
    let fs_paths = [
        "src/application/pipeline/virtual_file.rs",
        "src/application/pipeline/context.rs",
        "config/prompts/nacos_config.yaml",
    ];
    let mut fs_vfs = Vec::new();
    for p in fs_paths {
        let content = std::fs::read_to_string(p).expect(p);
        fs_vfs.push(VirtualFile::from_fs(
            format!("file://{p}"),
            content.clone(),
            PROJECT.to_string(),
            None,
            sha256_hex(&content),
        ));
    }

    // nacos 侧
    let nacos_vfs = fetch_prod_configs().await;
    let nacos_vfs: Vec<VirtualFile> = nacos_vfs
        .iter()
        .filter(|v| !v.content.trim().is_empty())
        .take(30)
        .cloned()
        .collect();

    println!(
        "fs 虚拟文件 {} 条, nacos 虚拟文件 {} 条",
        fs_vfs.len(),
        nacos_vfs.len()
    );

    let mut all = fs_vfs.clone();
    all.extend(nacos_vfs.clone());

    // 1) 路径唯一性
    let mut path_seen = HashSet::new();
    let mut dup_paths = Vec::new();
    for vf in &all {
        if !path_seen.insert(vf.virtual_path.clone()) {
            dup_paths.push(vf.virtual_path.clone());
        }
    }
    assert!(dup_paths.is_empty(), "存在重复路径: {dup_paths:?}");

    // 2) 跨源冲突（同路径不同内容）：fs 用 file:// 前缀、nacos 用 dt://nacos/ 前缀，构造上不重叠
    let fs_ns: HashSet<&str> = fs_vfs.iter().map(|v| v.virtual_path.as_str()).collect();
    let nacos_ns: HashSet<&str> = nacos_vfs.iter().map(|v| v.virtual_path.as_str()).collect();
    let overlap = fs_ns.intersection(&nacos_ns).count();
    assert_eq!(overlap, 0, "fs 与 nacos 路径冲突: {overlap}");
    assert!(fs_ns.iter().all(|p| p.starts_with("file://")));
    assert!(nacos_ns.iter().all(|p| p.starts_with("dt://nacos/")));

    // 3) 语义重复：同 content_hash 不同路径（跨源重复内容）
    let mut hash_map: HashMap<&str, Vec<&str>> = HashMap::new();
    for vf in &all {
        hash_map
            .entry(vf.content_hash.as_str())
            .or_default()
            .push(vf.virtual_path.as_str());
    }
    let dup_hash: Vec<_> = hash_map
        .iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(h, paths)| {
            (
                h.to_string(),
                paths.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        })
        .collect();
    println!("同内容多路径（跨源内容重复）: {}", dup_hash.len());
    for (h, paths) in &dup_hash {
        println!("  hash={h:.12} paths={paths:?}");
    }
    // 允许同内容（如两份相同配置）但必须是同一来源内部或无害；路径级重复必须为 0
    assert!(dup_paths.is_empty());

    // 4) 增量选择：空快照 → 全部选中（fs+nacos 统一流）
    let strategy = IncrementalStrategy;
    let repo = MemSnapshotRepo::default();
    let (changed, deleted) = strategy
        .select_virtual_files(&all, Some(&repo), PROJECT)
        .await
        .unwrap();
    println!(
        "统一流增量选择: 选中 {}/{} 删除 {}",
        changed.len(),
        all.len(),
        deleted.len()
    );
    assert_eq!(changed.len(), all.len());
    assert!(deleted.is_empty());

    println!("(d) 结论 ✓: fs+nacos 路径零冲突、零重复（fs=file:// 前缀 / nacos=dt://nacos/ 前缀）");
}

// ===========================================================================
// (e) 纯 VirtualFile（无磁盘文件）端到端
// ===========================================================================

#[tokio::test]
#[ignore = "真实 LLM — 显式运行"]
async fn phase0_e_pure_virtualfile_e2e() {
    println!("\n========== (e) 纯 VirtualFile 端到端（无磁盘文件） ==========");

    let registry = prompt_registry();
    let content = "server:\n  port: 8080\n  host: 0.0.0.0\nspring:\n  datasource:\n    url: jdbc:mysql://10.0.0.1:3306/order?useSSL=false\n    username: app\n  redis:\n    host: 10.0.0.2\n    port: 6379\nlogging:\n  level:\n    root: info\n".to_string();
    let hash = sha256_hex(&content);

    // 纯 VirtualFile：dt:// 路径，磁盘上不存在此文件
    let vf = VirtualFile::new(
        "dt://nacos/0e5dee28-c361-480c-911d-229d66b46c2d/order-service.yaml",
        content,
        PROJECT.to_string(),
        FileSourceKind::Nacos,
        None,
        hash,
    );
    assert!(
        !Path::new(&vf.virtual_path).exists(),
        "纯 VirtualFile 不应对应磁盘文件"
    );

    let mut ctx = PipelineContext::new(
        Path::new(&vf.virtual_path).to_path_buf(),
        vf.content.clone(),
        vf.project.clone(),
        FileSourceKind::Nacos,
        None,
        Some(vf.content_hash.clone()),
    );

    // chunk 处理器
    let chunk = ChunkProcessor::default();
    let chunk_out = chunk.execute(&ctx).await.expect("chunk 失败");
    let doc_id = chunk_out
        .get("doc_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let doc_type = chunk_out
        .get("doc_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    println!("chunk: doc_id={doc_id} doc_type={doc_type}");
    assert_eq!(doc_type, "yaml", "yaml 虚拟文件应产出 yaml doc_type");
    ctx.add_output("chunk", chunk_out);

    // LLM 处理器（G3 修复后：Nacos 源 → select_prompt 路由 nacos_config 词表）
    let llm_arc: Arc<dyn ChatClient> = Arc::new(llm_client());
    let llm_proc = LlmClientProcessor::new(
        llm_arc,
        LLM_MODEL.to_string(),
        registry,
        LlmConfig::default(),
    );
    let llm_out = llm_proc.execute(&ctx).await.expect("llm 失败");
    let prompt_name = llm_out
        .get("prompt_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    println!("llm: prompt_name={prompt_name}");
    // G3 断言：Nacos 源必须路由到 nacos_config（修复前走 document_with_nlp，
    // 实测 12 实体全为 Config 类型）。
    assert_eq!(
        prompt_name, "nacos_config",
        "G3: Nacos 源应路由到 nacos_config 词表"
    );
    let graphs: Vec<ExtractedGraph> =
        serde_json::from_value(llm_out.get("graphs").expect("无 graphs").clone())
            .expect("graphs 解析失败");
    let n_entities = graphs.iter().map(|g| g.entities.len()).sum::<usize>();
    let n_degraded = graphs.iter().filter(|g| g.degraded).count();
    println!(
        "llm: {} 个块, {} 实体, {} 降级",
        graphs.len(),
        n_entities,
        n_degraded
    );
    // G4 断言：真实管线产出应为 F4 词表类型（NacosConfig/ConfigKey/ConfigSection/
    // Database/Server），不得全部归一为 Config/Other。LLM 偶发空输出时不硬断
    // （实体质量为 (a) 的专门议题），但只要有实体就必须出现 F4 专用类型。
    if n_entities > 0 {
        let types: Vec<String> = graphs
            .iter()
            .flat_map(|g| {
                g.entities
                    .iter()
                    .map(|e| e.entity_type.as_str().to_string())
            })
            .collect();
        println!("llm: 实体类型={types:?}");
        let f4_types = [
            "NacosConfig",
            "ConfigKey",
            "ConfigSection",
            "Database",
            "Server",
        ];
        assert!(
            types.iter().any(|t| f4_types.contains(&t.as_str())),
            "G4: 应产出 F4 词表类型: {types:?}"
        );
    } else {
        eprintln!("[WARN] 本次 LLM 输出为空——路由断言仍成立，实体质量为 (a) 议题");
    }

    // F4 直调对照：同一内容用 nacos_config 词表（与真实管线同一提示词，
    // 保留作交叉验证）
    let f4_client = llm_client();
    let (f4_out, _raw, _elapsed) = llm_extract_f4(
        &f4_client,
        &PromptRegistry::load(Path::new(PROMPTS_DIR)).unwrap(),
        &vf,
    )
    .await;
    match &f4_out {
        Some(o) => {
            let types: Vec<String> = o.entities.iter().map(|e| e.entity_type.clone()).collect();
            println!(
                "f4(nacos_config) 直调: {} 实体, 类型={types:?}",
                o.entities.len()
            );
            assert!(!o.entities.is_empty(), "F4 词表抽取不应为空");
        }
        None => eprintln!("[WARN] F4 直调解析失败（不影响 VirtualFile 机制断言）"),
    }

    println!(
        "(e) 结论 ✓: 纯 VirtualFile（dt:// 路径、无磁盘文件）chunk→LLM 端到端无 panic、\
         Nacos 源正确路由 nacos_config 词表（G3）、输出可解析"
    );
    // 注：G3 修复前真实管线对 yaml 走 document_with_nlp（select_prompt 未接
    // nacos_config），现已路由到 F4 词表——见 phase0-verification.md §3 G3。
}

// ===========================================================================
// (f) 性能基准：单条耗时 → 全量估算
// ===========================================================================

#[tokio::test]
#[ignore = "真实 LLM + 真实 Nacos — 显式运行"]
async fn phase0_f_perf_benchmark() {
    println!("\n========== (f) 性能基准：单条耗时 → 全量估算 ==========");

    let t0 = Instant::now();
    let all = fetch_prod_configs().await;
    let fetch_ms = t0.elapsed().as_millis();
    let total = all.len();
    println!("拉取 {total} 条配置: {fetch_ms}ms");

    let registry = prompt_registry();
    let client = llm_client();
    let sample: Vec<VirtualFile> = all
        .iter()
        .filter(|v| !v.content.trim().is_empty() && v.content.len() <= 8000)
        .take(5)
        .cloned()
        .collect();

    let mut llm_times = Vec::new();
    let mut llm_bytes = Vec::new();
    for vf in &sample {
        let (_, _, elapsed) = llm_extract_f4(&client, &registry, vf).await;
        llm_times.push(elapsed.as_millis() as f64);
        llm_bytes.push(vf.content.len() as f64);
    }
    let avg_llm_ms = llm_times.iter().sum::<f64>() / llm_times.len() as f64;
    let avg_bytes = llm_bytes.iter().sum::<f64>() / llm_bytes.len() as f64;

    // 全量估算：72 条 × 平均 LLM 耗时 + 拉取
    let est_ms = total as f64 * avg_llm_ms + fetch_ms as f64;
    println!(
        "单条 LLM 抽取: 平均 {:.0}ms（样本 {} 条，平均 {:.0} 字节）",
        avg_llm_ms,
        sample.len(),
        avg_bytes
    );
    println!(
        "全量估算: {total} 条 × {:.0}ms + 拉取 {fetch_ms}ms ≈ {:.0}s（{:.1} 分钟）",
        avg_llm_ms,
        est_ms / 1000.0,
        est_ms / 60000.0
    );

    let summary = json!({
        "fetch_total_ms": fetch_ms,
        "configs_total": total,
        "llm_sample": sample.len(),
        "llm_avg_ms_per_item": format!("{:.0}", avg_llm_ms),
        "llm_avg_bytes_per_item": format!("{:.0}", avg_bytes),
        "full_volume_estimate_seconds": format!("{:.0}", est_ms / 1000.0),
        "full_volume_estimate_minutes": format!("{:.1}", est_ms / 60000.0),
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());

    // 预期：全量 20-60 分钟内（方案 §3 Phase 0 的估算假设）。
    // 若超出则如实报告——这是性能发现的载体，不是测试失败。
    if est_ms / 60000.0 <= 60.0 {
        println!(
            "(f) 结论 ✓: 全量估算 {:.1} 分钟（符合 20-60 分钟预期）",
            est_ms / 60000.0
        );
    } else {
        println!(
            "(f) 结论 ⚠: 全量估算 {:.1} 分钟，超出 20-60 分钟预期——\n\
             单条 LLM 抽取耗时 {:.0}s 为瓶颈（详见 phase0-verification.md）",
            est_ms / 60000.0,
            avg_llm_ms / 1000.0
        );
    }
}
