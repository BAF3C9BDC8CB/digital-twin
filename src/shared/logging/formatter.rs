//! 日志格式化器模块。
//!
//! 2026-08-22 起文件层由 JSON 行改为紧凑纯文本（`init.rs` 中直接使用
//! `tracing_subscriber::fmt::layer().compact()` 构建），JSON 结构化格式已废弃。
//!
//! 输出示例（文本层，一行一事件）：
//! ```text
//! 2026-08-22 07:47:25.571 INFO digital_twin::interfaces::cli::build: 搜索: query=prefetch 日志 分词=["prefetch", "日志"] world=code limit=3 json=false project=None show_content=false
//! ```
//!
//! 保留本模块仅为历史说明；如需自定义精确格式以后再在此实现。
