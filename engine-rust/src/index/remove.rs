use anyhow::Result;
use crate::client::{neo4j, qdrant};
use crate::config;

pub async fn run_remove(project: &str, file: Option<&str>, all: bool) -> Result<()> {
    let collection = format!("{}_methods", project);

    if all {
        println!("[remove] clearing all data for project {}...", project);
        if let Err(e) = qdrant::delete_collection(&collection).await {
            eprintln!("[warn] qdrant delete_collection: {}", e);
        }
        neo4j::delete_all_methods(project).await?;
        println!("[done] project {} data removed", project);

        // 同步：从 config.yaml 中移除该项目
        let _ = config::remove_project_from_config(project);
        return Ok(());
    }

    if let Some(file_path) = file {
        println!("[remove] removing file {}...", file_path);
        if let Err(e) = qdrant::delete_points_by_filter(&collection, file_path).await {
            eprintln!("[warn] qdrant delete {}: {}", file_path, e);
        }
        neo4j::delete_methods_by_file(project, file_path).await?;
        println!("[done] removed methods for file {}", file_path);
        return Ok(());
    }

    eprintln!("[error] specify --file <path> or --all");
    Ok(())
}
