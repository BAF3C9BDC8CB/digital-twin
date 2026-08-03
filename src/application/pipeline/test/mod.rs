//! 流水线测试验证——Digital Twin 构建流水线的独立集成测试。
//!
//! # 架构
//!
//! [`verify_test_data`] 函数清理旧的 test- 前缀数据，对每个实体类型
//! （classes、methods、Nacos 配置、pods、Jenkins 任务、知识条目）执行
//! 验证检查，并返回一个 [`TestReport`]。
//!
//! ```text
//!     verify_test_data()
//!     → cleanup_test_data()
//!     → 10 条 Cypher 查询 + Qdrant 检查
//!     → TestReport
//! ```

pub mod cleanup;
pub mod report;
pub mod runner;

pub use report::TestReport;
pub use runner::verify_test_data;
