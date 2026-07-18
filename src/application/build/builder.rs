//! Build command — CLI entry point for `dt build`.
//!
//! This module defines the `BuildCommand` struct with clap arguments
//! and provides the `run()` method that executes the build pipeline.

use clap::Parser;
use crate::domain::error::DtError;
use crate::domain::traits::{BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use std::path::PathBuf;
use std::sync::Arc;

use crate::infrastructure::parser::ParserRegistry;
use super::service::BuildServiceImpl;

/// Build command — index a project's source code into the knowledge graph.
///
/// Usage:
/// ```bash
/// dt build /path/to/project --name my-project
/// dt build /path/to/project --name my-project --full
/// ```
#[derive(Parser, Debug, Clone)]
#[command(name = "build", about = "Index project source code into the knowledge graph")]
pub struct BuildCommand {
    /// Path to the project root directory.
    #[arg(value_name = "PATH")]
    pub project_path: PathBuf,

    /// Project name (used for entity IDs and graph grouping).
    #[arg(short = 'n', long = "name")]
    pub project_name: String,

    /// Perform a full rebuild (ignoring incremental snapshots).
    #[arg(long = "full")]
    pub full: bool,

    /// Show verbose output.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

/// Dependencies needed to run a build.
pub struct BuildDependencies {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
}

impl BuildCommand {
    /// Execute the build command with the provided dependencies.
    pub async fn run(&self, deps: BuildDependencies) -> Result<(), DtError> {
        if self.verbose {
            tracing::info!(
                "Starting build: project={}, path={}, full={}",
                self.project_name,
                self.project_path.display(),
                self.full
            );
        }

        let registry = Arc::new(ParserRegistry::new());
        let service = BuildServiceImpl::new(
            registry,
            deps.graph,
            deps.vector,
            deps.snapshot,
            deps.embed,
            self.full,
        );

        let report = service.build(&self.project_name, &self.project_path).await?;

        if self.verbose {
            tracing::info!(
                "Build complete: {} files scanned, {} changed, {} methods, {}ms",
                report.files_scanned,
                report.files_changed,
                report.methods_total,
                report.elapsed_ms
            );
        } else {
            println!(
                "Build report for {}: {} files scanned, {} changed, {} methods indexed, {}ms",
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
            "--name", "test",
            "--full",
        ]);
        assert!(cmd.is_ok());
        let cmd = cmd.unwrap();
        assert_eq!(cmd.project_path, PathBuf::from("/tmp/test-project"));
        assert_eq!(cmd.project_name, "test");
        assert!(cmd.full);
    }

    #[test]
    fn command_help_contains_info() {
        let cmd = BuildCommand::try_parse_from(["build", "--help"]);
        // --help returns an error from clap (it's how clap prints help)
        let err = cmd.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("build") || msg.contains("Index"));
    }
}
