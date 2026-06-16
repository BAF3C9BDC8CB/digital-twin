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
    /// Incremental build: scan project, hash-compare files, index changes only
    Build {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Index a single file (for instant incremental update)
    Update {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        file: String,
    },
    /// Full rebuild of a project
    Index {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Remove methods for a file or entire project from Neo4j + Qdrant
    Remove {
        #[arg(long)]
        project: String,
        #[arg(long)]
        file: Option<String>,
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
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long, default_value = "false")]
        all: bool,
        #[arg(long, default_value = "false")]
        json: bool,
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
        Commands::Build { path, name } => {
            index::build::run_build(&path, &name).await?;
        }
        Commands::Update { path, name, file } => {
            index::update::run_update(&path, &name, &file).await?;
        }
        Commands::Index { path, name } => {
            index::full::run_index(&path, &name).await?;
        }
        Commands::Remove { project, file, all } => {
            index::remove::run_remove(&project, file.as_deref(), all).await?;
        }
        Commands::Event { r#type, entity_id, entity_type, project, details } => {
            event::write_event(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Memorize { r#type, entity_id, entity_type, project, details } => {
            knowledge::write_knowledge(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Search { query, project, limit, all, json } => {
            search::run_search(&query, project.as_deref(), limit, all, json).await?;
        }
        Commands::NacosSync { env } => {
            sync::nacos::run_sync(&env).await?;
        }
        Commands::K8sSync { limit } => {
            sync::k8s::run_sync(limit).await?;
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
