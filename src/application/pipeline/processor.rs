//! [`Processor`] trait —— 每个流水线阶段都必须实现的契约。
//!
//! 一个处理器负责文件分析的一个阶段（例如语言检测、tree-sitter 解析、
//! NLP 分析、embedding）。处理器通过 [`Processor::matches`] 声明自己能处理
//! 哪些文件（依据 [`PipelineContext`] 中的路径或来源类型），并产生一个合并到共享
//! [`PipelineContext`](super::context::PipelineContext) 中的 [`ProcessorOutput`] 结构。

use async_trait::async_trait;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::domain::error::DtError;

/// 流水线处理链中的单个阶段。
///
/// # 排序
///
/// 处理器按 [`priority`](Processor::priority) 排序（数值越低越先执行），
/// 使得廉价的"守门"检查（语言检测、文件类型过滤）先于昂贵的分析执行。
///
/// # 线程安全
///
/// 所有 trait 方法均为 `&self` 且要求 `Send + Sync`，这样在无数据依赖时
/// 流水线运行器可以并行执行独立的处理器。
#[async_trait]
pub trait Processor: Send + Sync {
    /// 该处理器的人类可读名称（例如 `"tree_sitter"`、`"chunk"`、
    /// `"code_embedder"`）。
    ///
    /// 该名称用作处理器在 [`PipelineContext`] 中存储其输出时的键。
    fn name(&self) -> &str;

    /// 相对执行优先级。数值越低越先执行。
    ///
    /// 典型约定：
    /// - `0–99`   —— 文件类型 / 语言检测
    /// - `100–199` —— 结构解析器（tree-sitter）
    /// - `200–299` —— NLP / 语义分析器
    /// - `300+`   —— embedding / 向量化（通常依赖前置阶段）
    fn priority(&self) -> i32;

    /// 如果该处理器可以处理给定上下文对应的文件则返回 `true`。
    ///
    /// 该检查在 [`execute`](Processor::execute) **之前**进行，以便流水线
    /// 廉价地跳过不合适的处理器。实现可通过 `ctx.file_path` 或
    /// `ctx.source_kind` 判定。
    fn matches(&self, ctx: &PipelineContext) -> bool;

    /// 针对共享的 [`PipelineContext`] 执行该处理器。
    ///
    /// 上下文携带原始文件内容以及前置阶段的输出，使下游处理器能够构建在
    /// 上游结果之上。
    ///
    /// # 错误
    ///
    /// 失败时返回 [`DtError`]。流水线运行器可根据错误严重程度决定跳过或中止。
    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError>;
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::context::PipelineContext;
    use crate::application::pipeline::virtual_file::FileSourceKind;
    use std::path::PathBuf;

    struct DummyProcessor;

    #[async_trait]
    impl Processor for DummyProcessor {
        fn name(&self) -> &str {
            "dummy"
        }

        fn priority(&self) -> i32 {
            100
        }

        fn matches(&self, ctx: &PipelineContext) -> bool {
            ctx.file_path
                .extension()
                .and_then(|e| e.to_str())
                == Some("rs")
        }

        async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
            let mut out = ProcessorOutput::new();
            out.set("file_name", ctx.file_path.to_string_lossy().to_string());
            out.set("project", ctx.project_name.clone());
            Ok(out)
        }
    }

    #[test]
    fn processor_basics() {
        let p = DummyProcessor;
        assert_eq!(p.name(), "dummy");
        assert_eq!(p.priority(), 100);

        let ctx_rs = PipelineContext::new(
            PathBuf::from("main.rs"),
            "fn main() {}".to_string(),
            "my_project".to_string(),
            FileSourceKind::Fs,
            None,
            None,
        );
        assert!(p.matches(&ctx_rs));

        let ctx_py = PipelineContext::new(
            PathBuf::from("main.py"),
            "print(1)".to_string(),
            "my_project".to_string(),
            FileSourceKind::Fs,
            None,
            None,
        );
        assert!(!p.matches(&ctx_py));
    }

    #[tokio::test]
    async fn processor_execute() {
        let p = DummyProcessor;
        let ctx = PipelineContext::new(
            PathBuf::from("main.rs"),
            "fn main() {}".to_string(),
            "my_project".to_string(),
            FileSourceKind::Fs,
            None,
            None,
        );
        let out = p.execute(&ctx).await.unwrap();
        let file_name = out.get("file_name").and_then(|v| v.as_str()).unwrap();
        assert!(file_name.ends_with("main.rs"));
        assert_eq!(
            out.get("project").and_then(|v| v.as_str()),
            Some("my_project")
        );
    }
}
