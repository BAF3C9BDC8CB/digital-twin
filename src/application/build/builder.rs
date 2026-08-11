//! Build 命令 — `dt build` 的 CLI 入口。
//! Hook 验证：单文件修改应触发增量构建。
//!
//! 本模块定义带 clap 参数的 `BuildCommand` 结构体，
//! 并提供执行构建流水线的 `run()` 方法。

use crate::domain::error::DtError;
use crate::domain::traits::{
    BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use crate::domain::types::BatchConfig;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use super::service::BuildServiceImpl;
use crate::application::pipeline::infer_client::ChatClient;
use crate::infrastructure::parser::ParserRegistry;

/// Build 命令 — 将项目源代码索引到知识图谱。
///
/// 用法：
/// ```bash
/// dt build /path/to/project --name my-project
/// dt build /path/to/project --name my-project --full
/// ```
#[derive(Parser, Debug, Clone)]
#[command(name = "build", about = "将项目源代码索引到知识图谱")]
pub struct BuildCommand {
    /// 项目根目录路径。
    #[arg(value_name = "PATH")]
    pub project_path: PathBuf,

    /// 项目名称（用于实体 ID 与图谱分组）。
    #[arg(short = 'n', long = "name")]
    pub project_name: String,

    /// 执行全量重建（忽略增量快照）。
    #[arg(long = "full")]
    pub full: bool,

    /// 显示详细输出。
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// 跳过向量嵌入（processors.embed=false）。
    #[arg(long = "skip-embed")]
    pub skip_embed: bool,

    /// 关闭构建末尾的 LLM 缺口补偿自愈（默认开启）。
    /// 语义为"关"：带此 flag 时 llm_backfill=false。
    #[arg(long = "no-llm-backfill", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub llm_backfill: bool,
}

/// 运行构建所需的依赖。
pub struct BuildDependencies {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
    pub llm_client: Option<Arc<dyn ChatClient>>,
    pub llm_model: String,
    pub target_file: Option<PathBuf>,
    pub batch_config: Option<BatchConfig>,
    /// 跳过向量嵌入（保留 Qdrant 中的已有向量）。
    pub skip_embed: bool,
    /// 扫描忽略规则（来自 config.yaml scanner 段）；默认使用内置规则。
    pub scan_config: crate::domain::types::ScanConfig,
    /// Phase 2（方法级 LLM 分析）并发——CLI 从 provider max_concurrent 读取。
    pub llm_concurrency: usize,
    /// Phase 2 单次 LLM 回复最大 token 数——CLI 从 provider max_tokens 读取
    /// （与模型上下文长度相关，可按模型配置调整）。
    pub llm_max_tokens: u32,
}

impl BuildCommand {
    /// 使用提供的依赖执行 build 命令。
    pub async fn run(&self, deps: BuildDependencies) -> Result<(), DtError> {
        if self.verbose {
            tracing::info!(
                "开始构建: project={}, path={}, full={}",
                self.project_name,
                self.project_path.display(),
                self.full
            );
        }

        let registry = Arc::new(ParserRegistry::new());
        let batch = deps.batch_config.unwrap_or_default();
        let service = BuildServiceImpl::new(
            registry,
            deps.graph,
            deps.vector,
            deps.snapshot,
            deps.embed,
            deps.llm_client,
            deps.llm_model,
            deps.target_file,
            self.full,
            batch,
            deps.skip_embed || self.skip_embed,
            deps.llm_concurrency,
            deps.llm_max_tokens,
        )
        .with_llm_backfill(self.llm_backfill);

        let report = service
            .with_scan_config(deps.scan_config)
            .build(&self.project_name, &self.project_path)
            .await?;

        if self.verbose {
            tracing::info!(
                "构建完成: 扫描 {} 个文件, 变更 {} 个, 共 {} 个方法, {}ms",
                report.files_scanned,
                report.files_changed,
                report.methods_total,
                report.elapsed_ms
            );
        } else {
            println!(
                "{} 的构建报告: 扫描 {} 个文件, 变更 {} 个, 索引 {} 个方法, {}ms",
                report.project,
                report.files_scanned,
                report.files_changed,
                report.methods_total,
                report.elapsed_ms
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parses_args() {
        let cmd = BuildCommand::try_parse_from([
            "build",
            "/tmp/test-project",
            "--name",
            "test",
            "--full",
        ]);
        assert!(cmd.is_ok());
        let cmd = cmd.unwrap();
        assert_eq!(cmd.project_path, PathBuf::from("/tmp/test-project"));
        assert_eq!(cmd.project_name, "test");
        assert!(cmd.full);
        // 补偿自愈默认开启
        assert!(cmd.llm_backfill);
    }

    #[test]
    fn no_llm_backfill_flag_disables() {
        let cmd = BuildCommand::try_parse_from([
            "build",
            "/tmp/test-project",
            "--name",
            "test",
            "--no-llm-backfill",
        ]);
        assert!(cmd.is_ok());
        assert!(!cmd.unwrap().llm_backfill);
    }

    #[test]
    fn command_help_contains_info() {
        let cmd = BuildCommand::try_parse_from(["build", "--help"]);
        // --help 会让 clap 返回错误（这是 clap 打印帮助信息的方式）
        let err = cmd.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("build") || msg.contains("Index"));
    }
}
