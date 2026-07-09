mod config;
mod models;
mod parser;
mod scanner;
mod event;
mod knowledge;
mod search;
mod health;
mod validate;
mod common;
mod client;
mod index;
mod sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dt", version = "3.1.0", about = "Digital Twin CLI - knowledge graph & vector index management")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Incremental build: scan project, hash-compare files, index changes only.
    /// Use --file to auto-resolve project from config.yaml.
    Build {
        /// Project path (auto-resolved if --file given)
        #[arg(long)]
        path: Option<String>,
        /// Project name (auto-resolved if --file given)
        #[arg(long)]
        name: Option<String>,
        /// Any file in project for auto-resolution
        #[arg(long)]
        file: Option<String>,
    },
    /// Full rebuild of a project (auto-resolves from config.yaml)
    Index {
        /// Project path (auto-resolved if --file given)
        #[arg(long)]
        path: Option<String>,
        /// Project name (auto-resolved if --file given)
        #[arg(long)]
        name: Option<String>,
        /// Any file in project for auto-resolution
        #[arg(long)]
        file: Option<String>,
    },
    /// Remove methods for a file or entire project from Neo4j + Qdrant
    Remove {
        /// File path for auto-resolution (project + file)
        #[arg(long)]
        file: Option<String>,
        /// Project name override
        #[arg(long)]
        project: Option<String>,
        /// File path within project (used with --project override)
        #[arg(long)]
        rel_file: Option<String>,
        /// Remove entire project
        #[arg(long, default_value = "false")]
        all: bool,
    },
    /// Write an Event node to Neo4j
    Event {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        entity_id: String,
        #[arg(long)]
        entity_type: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        details: Option<String>,
    },
    /// Write a Knowledge node to Neo4j
    Memorize {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        entity_id: String,
        #[arg(long)]
        entity_type: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        details: Option<String>,
    },
    /// Semantic code search
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        /// Search all projects under a directory path (auto-matches from config.yaml)
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long, default_value = "false")]
        all: bool,
        #[arg(long, default_value = "false")]
        json: bool,
        /// Expand query into multiple variants for higher recall
        #[arg(long, default_value = "false")]
        expand: bool,
    },
    /// Search knowledge graph nodes via vector similarity (KG→Qdrant bridge)
    SearchKg {
        query: String,
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Check connectivity of all backend services
    Health,
    /// Build Neo4j CALLS relationships
    BuildCallGraph {
        #[arg(long)]
        name: String,
    },
    /// Validate extraction quality (dry-run)
    Validate {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Sync Nacos configurations into the knowledge graph
    NacosSync {
        #[arg(long, default_value = "all")]
        env: String,
    },
    /// Sync Kubernetes resources into the knowledge graph
    K8sSync {
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Sync KG nodes to Qdrant vector index (KG→Qdrant bridge)
    KgSync {
        /// Labels to sync (default: all business labels)
        #[arg(long)]
        labels: Vec<String>,
        /// Only sync nodes not yet synced
        #[arg(long, default_value = "false")]
        incremental: bool,
        /// Preview mode, no writes
        #[arg(long, default_value = "false")]
        dry_run: bool,
    },
    /// Build all projects defined in config.yaml
    BuildAll {
        /// Path to config.yaml (default: ~/.config/opencode/skills/digital-twin/config.yaml)
        #[arg(long)]
        config: Option<String>,
        /// Full rebuild instead of incremental
        #[arg(long, default_value = "false")]
        full: bool,
        /// Only build specified project names (comma-separated)
        #[arg(long)]
        filter: Option<String>,
    },
    /// List all projects from config.yaml with indexing status
    List {
        /// Filter by project name (substring match)
        #[arg(long)]
        filter: Option<String>,
        /// Show detailed status (queries Neo4j + Qdrant)
        #[arg(long, default_value = "false")]
        all: bool,
    },
    /// Parse a single file to JSON (no DB writes)
    Parse {
        #[arg(long)]
        file: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        root: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { path, name, file } => {
            let (name, path) = resolve_project(file.as_deref(), name.clone(), path.clone())?;
            index::build::run_build(&path, &name).await?;
        }
        Commands::Index { path, name, file } => {
            let (name, path) = resolve_project(file.as_deref(), name.clone(), path.clone())?;
            index::full::run_index(&path, &name).await?;
        }
        Commands::Remove { file, project, rel_file, all } => {
            let (project, rel) = if let Some(f) = file {
                let cfg = config::load();
                match cfg.resolve_file(&f) {
                    Some((n, _, r)) if all => (n, String::new()),
                    Some((n, _, r)) => (n, r),
                    None => anyhow::bail!("无法解析文件所属项目: {}", f),
                }
            } else if let (Some(p), Some(r)) = (&project, &rel_file) {
                (p.clone(), r.clone())
            } else if let Some(p) = &project {
                (p.clone(), String::new())
            } else {
                anyhow::bail!("请指定 --file（自动解析）或 --project")
            };
            index::remove::run_remove(&project, if rel.is_empty() { None } else { Some(&rel) }, all).await?;
        }
        Commands::Event { r#type, entity_id, entity_type, project, details } => {
            event::write_event(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Memorize { r#type, entity_id, entity_type, project, details } => {
            knowledge::write_knowledge(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Search { query, project, path, limit, all, json, expand } => {
            search::run_search(&query, project.as_deref(), path.as_deref(), limit, all, json, expand).await?;
        }
        Commands::SearchKg { query, limit } => {
            search::run_search_kg(&query, limit).await?;
        }
        Commands::NacosSync { env } => {
            sync::nacos::run_sync(&env).await?;
        }
        Commands::K8sSync { limit } => {
            sync::k8s::run_sync(limit).await?;
        }
        Commands::KgSync { labels, incremental, dry_run } => {
            let labels_opt = if labels.is_empty() { None } else { Some(labels) };
            sync::kg::run_kg_sync(labels_opt, incremental, dry_run).await?;
        }
        Commands::Health => {
            health::run_health().await?;
        }
        Commands::BuildCallGraph { name } => {
            client::neo4j::ensure_schema().await?;
            let count = client::neo4j::create_call_relationships(&name).await?;
            println!("[done] created {} CALLS relationships for project {}", count, name);
        }
        Commands::Validate { path, name } => {
            validate::run_validate(&path, &name).await?;
        }
        Commands::BuildAll { config, full, filter } => {
            index::build_all::run_build_all(config.as_deref(), full, filter.as_deref()).await?;
        }
        Commands::List { filter, all } => {
            index::list::run_list(filter.as_deref(), all).await?;
        }
        Commands::Parse { file, project, root } => {
            let mut p = parser::Parser::new()?;
            let parsed = p.parse_file(&file, &project, &root)?;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "methods": parsed.methods.iter().map(|m| serde_json::json!({
                    "method_id": m.method_id, "name": m.name, "class_name": m.class_name,
                    "signature": m.signature, "start_line": m.start_line,
                    "end_line": m.end_line, "language": m.language, "calls": m.calls,
                })).collect::<Vec<_>>(),
                "classes": parsed.classes.iter().map(|c| serde_json::json!({
                    "class_id": c.class_id, "name": c.name,
                    "kind": format!("{:?}", c.kind),
                })).collect::<Vec<_>>(),
            }))?);
        }
    }
    Ok(())
}

/// 从 config.yaml 或显式参数解析 (name, path)
fn resolve_project(file: Option<&str>, name: Option<String>, path: Option<String>) -> anyhow::Result<(String, String)> {
    if let (Some(n), Some(p)) = (name, path) {
        return Ok((n, p));
    }
    if let Some(f) = file {
        let cfg = config::load();
        if let Some((n, p, _)) = cfg.resolve_file(f) {
            return Ok((n, p));
        }
    }
    anyhow::bail!("无法解析项目: 请用 --name/--path 指定，或确保文件路径在 config.yaml 的 projects 段内")
}
