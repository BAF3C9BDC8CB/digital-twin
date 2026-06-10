use anyhow::Result;
use crate::{scanner, parser::Parser};

pub async fn run_validate(root: &str, project: &str) -> Result<()> {
    let files = scanner::collect_files(root);
    println!("[扫描] 发现 {} 个文件", files.len());

    let mut p = Parser::new()?;
    let mut all_methods = Vec::new();
    let mut file_count = 0;
    let mut skip_count = 0;

    for f in &files {
        let _content = match std::fs::read_to_string(f) { Ok(c) => c, Err(_) => { skip_count += 1; continue; } };
        let fpath = f.to_string_lossy();
        match p.parse_file(&fpath, project, root) {
            Ok(parsed) => { all_methods.extend(parsed.methods); file_count += 1; }
            Err(_) => { skip_count += 1; }
        }
    }

    println!("[提取] {} 文件处理, {} 跳过, 共 {} 个方法", file_count, skip_count, all_methods.len());

    let mut empty_name = 0;
    for m in &all_methods {
        if m.name.is_empty() { empty_name += 1; }
    }

    println!("\n========== 验证结果: {} ==========", project);
    println!("  总方法数:      {}", all_methods.len());
    println!("  空方法名:      {} ❌", empty_name);
    println!("  错误:          {} 文件", skip_count);

    if empty_name == 0 && skip_count == 0 {
        println!("✅ 验证通过！");
    } else {
        println!("⚠️  发现异常");
    }
    Ok(())
}
