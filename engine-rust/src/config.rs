use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

static CFG: OnceLock<DtConfig> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DtConfig {
    pub server: ServerConfig,
    pub services: ServicesConfig,
    #[serde(default)]
    pub snapshot_dir: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerConfig {
    pub hostname: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServicesConfig {
    pub neo4j: Neo4jConfig,
    pub qdrant: QdrantConfig,
    pub embed_server: EmbedConfig,
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
    pub url: String,
    pub dim: usize,
    pub model: String,
}

fn find_config() -> Option<String> {
    // Priority: DT_CONFIG env > ./config.yaml > ~/.config/digital-twin/config.yaml
    if let Ok(p) = std::env::var("DT_CONFIG") {
        if Path::new(&p).exists() { return Some(p); }
    }
    let candidates = [
        "./config.yaml",
        &format!("{}/.config/digital-twin/config.yaml", std::env::var("HOME").unwrap_or_default()),
    ];
    for p in &candidates {
        if Path::new(p).exists() { return Some(p.to_string()); }
    }
    None
}

pub fn load() -> &'static DtConfig {
    CFG.get_or_init(|| {
        if let Some(p) = find_config() {
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            if let Ok(cfg) = serde_yaml::from_str::<DtConfig>(&content) {
                return cfg;
            }
        }
        DtConfig {
            server: ServerConfig { hostname: "localhost".into() },
            services: ServicesConfig {
                neo4j: Neo4jConfig {
                    url: "http://localhost:7474".into(),
                    user: "neo4j".into(),
                    password: "neo4j".into(),
                },
                qdrant: QdrantConfig { url: "http://localhost:6333".into() },
                embed_server: EmbedConfig {
                    url: "http://localhost:8001".into(),
                    dim: 768,
                    model: "BAAI/bge-base-zh-v1.5".into(),
                },
            },
            snapshot_dir: "/var/lib/digital-twin/snapshots".into(),
        }
    })
}

pub const MAX_FILE_SIZE: u64 = 500 * 1024;
pub const SQLITE_PATH: &str = "/var/lib/digital-twin/lazy.db";

pub fn ignore_dirs() -> HashSet<&'static str> {
    [
        "node_modules", ".git", "__pycache__", "venv", ".venv",
        "dist", "build", ".next", "target", "bin", "obj",
        "release", "debug", ".vscode", ".idea", "coverage",
        "vendor", "third_party", "third-party", ".husky", ".cache",
        "log", "logs", "nacos_config", ".gradle", "cmake-build-debug",
        "runtime", "temp", "uploads", "download", "wxapp",
        "charts", "charts-dev", "charts-test", "sql",
        ".mvn", "thinkphp",
    ].iter().copied().collect()
}

pub fn ignore_ext() -> HashSet<&'static str> {
    [
        ".log", ".out", ".class", ".jar", ".war", ".ear",
        ".pyc", ".o", ".a", ".lib", ".dll", ".so", ".dylib", ".exe",
        ".zip", ".tar", ".gz", ".bz2", ".rar", ".7z", ".apk",
        ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".svg",
        ".mp3", ".mp4", ".avi", ".flv", ".wav",
        ".ttf", ".woff", ".eot", ".db", ".sqlite", ".db-journal",
        ".dex", ".bin",
    ].iter().copied().collect()
}

pub fn ignore_files() -> HashSet<&'static str> {
    [
        "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
        ".DS_Store",
    ].iter().copied().collect()
}
