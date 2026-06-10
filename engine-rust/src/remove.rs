use anyhow::Result;

use crate::{config, neo4j, qdrant};

pub async fn run_remove(project: &str, file: Option<&str>, all: bool) -> Result<()> {
    let collection = format!("{}_methods", project);

    if all {
        // Remove entire project
        println!("[删除] 清除项目 {} 所有数据...", project);
        let _ = qdrant::delete_collection(&collection).await;
        neo4j::delete_all_methods(project).await?;
        println!("[完成] 项目 {} 数据已全部删除", project);
        return Ok(());
    }

    if let Some(file_path) = file {
        println!("[删除] 移除文件 {}...", file_path);
        let _ = qdrant::delete_points_by_filter(&collection, file_path).await;
        neo4j::delete_methods_by_file(project, file_path).await?;
        println!("[完成] 已移除文件 {} 的方法", file_path);
        return Ok(());
    }

    println!("请指定 --file <path> 或 --all");
    Ok(())
}
