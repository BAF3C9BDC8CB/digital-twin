// Batch build all projects from config.yaml

use std::path::Path;
use std::time::Instant;

use anyhow::Result;

use crate::config;
use crate::index;

pub async fn run_build_all(
    config_path: Option<&str>,
    full: bool,
    filter: Option<&str>,
) -> Result<()> {
    let owned_cfg;
    let cfg: &config::DtConfig = if let Some(p) = config_path {
        owned_cfg = config::load_from(p)?;
        &owned_cfg
    } else {
        config::load()
    };

    let all_projects = cfg.projects();
    if all_projects.is_empty() {
        println!("config.yaml 中没有配置项目");
        return Ok(());
    }

    // 过滤器
    let filters: Vec<&str> = filter
        .map(|f| f.split(',').map(|s| s.trim()).collect())
        .unwrap_or_default();

    let projects: Vec<(String, String)> = if filters.is_empty() {
        all_projects
    } else {
        all_projects
            .into_iter()
            .filter(|(name, _)| filters.contains(&name.as_str()))
            .collect()
    };

    if projects.is_empty() {
        println!("没有匹配的项目");
        return Ok(());
    }

    println!("═══ dt build-all ═══");
    println!(
        "配置文件: {}",
        config_path.unwrap_or("~/.config/opencode/skills/digital-twin/config.yaml")
    );
    println!("模式: {}", if full { "全量重建 (index)" } else { "增量构建 (build)" });
    println!("项目数: {}", projects.len());
    println!();

    let wall_t0 = Instant::now();
    let mut ok = 0usize;
    let mut fail = 0usize;

    for (i, (name, path)) in projects.iter().enumerate() {
        if !Path::new(path).is_dir() {
            println!(
                "[{}/{}] ⚠️ {} — 路径不存在: {}",
                i + 1,
                projects.len(),
                name,
                path
            );
            fail += 1;
            continue;
        }

        println!(
            "[{}/{}] {} @ {}",
            i + 1,
            projects.len(),
            name,
            path
        );

        let result = if full {
            index::full::run_index(path, name).await
        } else {
            index::build::run_build(path, name).await
        };

        match result {
            Ok(()) => ok += 1,
            Err(e) => {
                eprintln!("  ❌ 失败: {}", e);
                fail += 1;
            }
        }
    }

    println!();
    println!(
        "═══ 完成: {} 成功, {} 失败, 总耗时 {:.1}s ═══",
        ok,
        fail,
        wall_t0.elapsed().as_secs_f64()
    );
    Ok(())
}
