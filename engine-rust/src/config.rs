use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

static CFG: OnceLock<DtConfig> = OnceLock::new();

// ── 顶层配置 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DtConfig {
    pub server: ServerConfig,
    pub services: ServicesConfig,
    #[serde(default)]
    pub snapshot_dir: String,
    /// 项目注册表
    /// 格式: 列表，每项包含 base + items
    #[serde(default)]
    pub projects: Vec<ProjectGroup>,
    /// 文档目录（文档索引使用）
    #[serde(default)]
    pub document_dirs: Vec<String>,
    /// 扫描器配置（忽略规则）
    #[serde(default)]
    pub scanner: ScannerConfig,
}

// ── 扫描器配置 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScannerConfig {
    /// 额外忽略的目录名（追加到内置默认列表）
    #[serde(default)]
    pub ignore_dirs: Vec<String>,
    /// 额外忽略的文件扩展名（追加到内置默认列表）
    #[serde(default)]
    pub ignore_ext: Vec<String>,
    /// 额外忽略的文件名（追加到内置默认列表）
    #[serde(default)]
    pub ignore_files: Vec<String>,
    /// 单文件大小上限（字节），默认 500KB
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
}

fn default_max_file_size() -> u64 { 500 * 1024 }

impl Default for ScannerConfig {
    fn default() -> Self {
        ScannerConfig {
            ignore_dirs: vec![],
            ignore_ext: vec![],
            ignore_files: vec![],
            max_file_size: 500 * 1024,
        }
    }
}

// ── 项目配置 ──────────────────────────────────────────────────────────────
//
// projects 段格式为 key-value 映射：
//   projects:
//     uvp-user-center: /data/.../uvp-user-center
//     GoDingtalk: /data/.../GoDingtalk

// ── 项目组 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectGroup {
    /// 项目根路径
    pub base: String,
    /// 项目列表
    /// - "name"   → base/name 即完整路径
    /// - "name: rel_path" → base/rel_path 即完整路径
    #[serde(default)]
    pub items: Vec<serde_yaml::Value>,
}

// ── 基础设施配置 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerConfig {
    pub hostname: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServicesConfig {
    pub neo4j: Neo4jConfig,
    pub qdrant: QdrantConfig,
    pub embed_server: EmbedConfig,
    #[serde(default)]
    pub k8s: Option<K8sConfig>,
    #[serde(default)]
    pub nacos: Option<NacosServerConfig>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct K8sConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub cluster_id: String,
    #[serde(default)]
    pub skip_tls_verify: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NacosServerConfig {
    pub test: String,
    pub prod: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Neo4jConfig {
    pub url: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QdrantConfig {
    pub url: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbedConfig {
    #[serde(default)]
    pub url: String,
    pub dim: usize,
    pub model: String,
}

// ── 配置加载 ──────────────────────────────────────────────────────────────

fn find_config() -> Option<String> {
    if let Ok(p) = std::env::var("DT_CONFIG") {
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    let candidates = [
        "./config.yaml",
        &format!(
            "{}/.config/opencode/skills/digital-twin/config.yaml",
            std::env::var("HOME").unwrap_or_default()
        ),
        &format!(
            "{}/.config/digital-twin/config.yaml",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];
    for p in &candidates {
        if Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

fn defaults() -> DtConfig {
    DtConfig {
        server: ServerConfig {
            hostname: "localhost".into(),
        },
        services: ServicesConfig {
            neo4j: Neo4jConfig {
                url: "http://localhost:7474".into(),
                user: "neo4j".into(),
                password: String::new(),
            },
            qdrant: QdrantConfig {
                url: "http://localhost:6333".into(),
            },
            embed_server: EmbedConfig {
                url: String::new(),
                dim: 1024,
                model: "BAAI/bge-m3".into(),
            },
            k8s: None,
            nacos: None,
        },
        snapshot_dir: "/var/lib/digital-twin/snapshots".into(),
        projects: vec![],
        document_dirs: vec![],
        scanner: ScannerConfig::default(),
    }
}

pub fn load() -> &'static DtConfig {
    CFG.get_or_init(|| {
        if let Some(p) = find_config() {
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            if let Ok(cfg) = serde_yaml::from_str::<DtConfig>(&content) {
                return cfg;
            }
        }
        defaults()
    })
}

/// 从指定路径加载配置（不缓存）
pub fn load_from(path: &str) -> anyhow::Result<DtConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("无法读取配置文件 {}: {}", path, e))?;
    serde_yaml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("配置文件解析失败: {}", e))
}

impl DtConfig {
    /// 返回展开后的项目列表 (name, full_path)，按名排序。
     pub fn projects(&self) -> Vec<(String, String)> {
         let mut result = Vec::new();
         for group in &self.projects {
             let base = group.base.trim_end_matches('/');
             for item in &group.items {
                 let (name, rel) = parse_item(item);
                 let path = format!("{}/{}", base, rel);
                 result.push((name, path));
             }
         }
         result.sort_by(|a, b| a.0.cmp(&b.0));
         result
     }

    /// 根据文件路径反向查找所属项目 → (项目名, 项目路径, 项目内相对路径)
    pub fn resolve_file(&self, file_path: &str) -> Option<(String, String, String)> {
        use std::path::Path;
        // 尝试 canonicalize；若文件不存在则用原始路径
        let abs = Path::new(file_path).canonicalize()
            .unwrap_or_else(|_| Path::new(file_path).to_path_buf());
        let abs_str = abs.to_string_lossy().to_string();

        let mut projs: Vec<_> = self.projects().into_iter().collect();
        projs.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

        for (name, proj_path) in &projs {
            let proj_abs = Path::new(proj_path).canonicalize().ok();
            let proj_str = proj_abs
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| proj_path.clone());

            if abs_str.starts_with(&proj_str) {
                let rel = abs_str
                    .strip_prefix(&proj_str)?
                    .strip_prefix('/')
                    .unwrap_or("");
                return Some((name.clone(), proj_str, rel.to_string()));
            }
        }
        None
    }
 }

/// 解析单个项目条目 → (项目名, 相对路径)
fn parse_item(v: &serde_yaml::Value) -> (String, String) {
    match v {
        serde_yaml::Value::String(s) => (s.clone(), s.clone()),
        serde_yaml::Value::Mapping(m) => {
            if let Some((k, v)) = m.iter().next() {
                let name = k.as_str().unwrap_or("").to_string();
                let rel = v.as_str().unwrap_or("").to_string();
                (name, rel)
            } else {
                (String::new(), String::new())
            }
        }
        _ => (String::new(), String::new()),
    }
}

// ── 扫描器忽略规则 ─────────────────────────────────────────────────────────
//
// 内置默认值覆盖常见构建产物和依赖目录。
// 配置文件中 scanner.ignore_* 会追加到默认列表，不会替换。

pub const MAX_FILE_SIZE: u64 = 500 * 1024;
pub const SQLITE_PATH: &str = "/var/lib/digital-twin/lazy.db";

/// 内置忽略目录 + 配置文件追加
pub fn ignore_dirs() -> HashSet<String> {
    let mut s: HashSet<String> = [
        "node_modules", ".git", "__pycache__", "venv", ".venv",
        "dist", "build", ".next", "target", "bin", "obj",
        "release", "debug", ".vscode", ".idea", "coverage",
        "vendor", "third_party", "third-party", ".husky", ".cache",
        "log", "logs", "nacos_config", ".gradle", "cmake-build-debug",
        "runtime", "temp", "uploads", "download", "wxapp",
        "charts", "charts-dev", "charts-test", "sql",
        ".mvn", "thinkphp",
    ].iter().map(|&s| s.to_string()).collect();
    for d in &load().scanner.ignore_dirs {
        if !d.is_empty() {
            s.insert(d.clone());
        }
    }
    s
}

/// 内置忽略扩展名 + 配置文件追加
pub fn ignore_ext() -> HashSet<String> {
    let mut s: HashSet<String> = [
        ".log", ".out", ".class", ".jar", ".war", ".ear",
        ".pyc", ".o", ".a", ".lib", ".dll", ".so", ".dylib", ".exe",
        ".zip", ".tar", ".gz", ".bz2", ".rar", ".7z", ".apk",
        ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".svg",
        ".mp3", ".mp4", ".avi", ".flv", ".wav",
        ".ttf", ".woff", ".eot", ".db", ".sqlite", ".db-journal",
        ".dex", ".bin",
    ].iter().map(|&s| s.to_string()).collect();
    for e in &load().scanner.ignore_ext {
        if !e.is_empty() {
            s.insert(e.clone());
        }
    }
    s
}

/// 内置忽略文件名 + 配置文件追加
pub fn ignore_files() -> HashSet<String> {
    let mut s: HashSet<String> = [
        "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
        ".DS_Store",
    ].iter().map(|&s| s.to_string()).collect();
    for f in &load().scanner.ignore_files {
        if !f.is_empty() {
            s.insert(f.clone());
        }
    }
    s
}

/// 单文件大小上限
pub fn max_file_size() -> u64 {
    let v = load().scanner.max_file_size;
    if v > 0 { v } else { 500 * 1024 }
}

// ── 配置文件同步 ───────────────────────────────────────────────────────────
//
// dt build / dt index / dt remove 成功后自动同步 config.yaml，
// 每次修改前自动备份，并 diff 对比新旧差异。

/// 安全写入配置：先备份，再写入，输出 diff。
fn safe_write_config(cfg_path: &str, old_content: &str, new_cfg: &DtConfig) -> anyhow::Result<()> {
    let new_content = serde_yaml::to_string(new_cfg)?;

    if old_content == new_content {
        return Ok(());
    }

    // 备份
    let backup_dir = format!("{}/backups", config_backup_dir());
    std::fs::create_dir_all(&backup_dir)?;
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_path = format!("{}/config_{}.yaml", backup_dir, ts);
    std::fs::write(&backup_path, old_content)?;

    // 输出差异
    println!("  📋 config.yaml 变更:");
    let (old_projects, new_projects) = (extract_project_names(old_content), extract_project_names(&new_content));
    let added: Vec<_> = new_projects.iter().filter(|n| !old_projects.contains(*n)).collect();
    let removed: Vec<_> = old_projects.iter().filter(|n| !new_projects.contains(*n)).collect();
    if !added.is_empty() {
        println!("    ➕ 新增: {}", added.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !removed.is_empty() {
        println!("    ➖ 删除: {}", removed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    println!("  💾 备份: {}", backup_path);

    // 写入
    std::fs::write(cfg_path, &new_content)?;
    Ok(())
}

/// 从 YAML 内容中提取项目名列表（简单行匹配）
fn extract_project_names(content: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- ") && !trimmed.starts_with("- base:") && !trimmed.starts_with("- items:") {
            let entry = &trimmed[2..];
            // "name" or "name: rel_path"
            let name = if let Some((n, _)) = entry.split_once(':') {
                n.trim().to_string()
            } else {
                entry.to_string()
            };
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

fn config_backup_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{}/.config/opencode/skills/digital-twin", home)
}

/// 将项目追加到 config.yaml。
/// 自动归类到匹配的 base 下。
pub fn sync_project_to_config(name: &str, path: &str) -> anyhow::Result<()> {
    let cfg_path = find_config().unwrap_or_else(|| {
        format!(
            "{}/.config/opencode/skills/digital-twin/config.yaml",
            std::env::var("HOME").unwrap_or_default()
        )
    });

    if let Some(parent) = std::path::Path::new(&cfg_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let content = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let mut cfg: DtConfig = if content.is_empty() {
        defaults()
    } else {
        serde_yaml::from_str(&content).unwrap_or_else(|_| defaults())
    };

    // 检查是否已存在
    if cfg.projects.iter().any(|g| g.items.iter().any(|v| entry_name(v) == name)) {
        return Ok(());
    }

    // 找到最佳匹配的 group 或新建
    let best_base = find_best_base(path, &cfg);
    let group = cfg.projects.iter_mut().find(|g| g.base == best_base);

    let rel = if let Some(stripped) = path.strip_prefix(&format!("{}/", best_base)) {
        stripped.to_string()
    } else {
        path.to_string()
    };

    let new_item = if rel == name {
        serde_yaml::Value::String(name.to_string())
    } else {
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String(name.to_string()),
            serde_yaml::Value::String(rel),
        );
        serde_yaml::Value::Mapping(m)
    };

    if let Some(g) = group {
        if !g.items.contains(&new_item) {
            g.items.push(new_item);
            g.items.sort_by(|a, b| entry_name(a).cmp(entry_name(b)));
        }
    } else {
        cfg.projects.push(ProjectGroup {
            base: best_base,
            items: vec![new_item],
        });
    }

    safe_write_config(&cfg_path, &content, &cfg)?;
    println!("  📝 已同步到 config.yaml: {}", name);
    Ok(())
}

/// 从 config.yaml 删除项目。
pub fn remove_project_from_config(name: &str) -> anyhow::Result<()> {
    let cfg_path = match find_config() {
        Some(p) => p,
        None => return Ok(()),
    };

    let content = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if content.is_empty() {
        return Ok(());
    }

    let mut cfg: DtConfig = serde_yaml::from_str(&content)?;

    let mut removed = false;
    for group in &mut cfg.projects {
        let before = group.items.len();
        group.items.retain(|v| entry_name(v) != name);
        if group.items.len() < before {
            removed = true;
        }
    }

    // 清理空 group
    cfg.projects.retain(|g| !g.items.is_empty());

    if removed {
        safe_write_config(&cfg_path, &content, &cfg)?;
        println!("  📝 已从 config.yaml 移除: {}", name);
    }
    Ok(())
}

/// 从 YAML Value 提取项目名
fn entry_name(v: &serde_yaml::Value) -> &str {
    match v {
        serde_yaml::Value::String(s) => s.as_str(),
        serde_yaml::Value::Mapping(m) => {
            m.keys().next().and_then(|k| k.as_str()).unwrap_or("")
        }
        _ => "",
    }
}

/// 在已有 groups 中找最佳匹配的 base 路径
fn find_best_base(path: &str, cfg: &DtConfig) -> String {
    let mut best = String::new();
    let mut best_len = 0;
    for group in &cfg.projects {
        let base = group.base.trim_end_matches('/');
        if path.starts_with(base) && base.len() > best_len {
            best = base.to_string();
            best_len = base.len();
        }
    }
    if best.is_empty() {
        // 无匹配 → 使用 path 的父目录
        std::path::Path::new(path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(path)
            .to_string()
    } else {
        best
    }
}
