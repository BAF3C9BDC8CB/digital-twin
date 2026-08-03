//! 用于 tracing-subscriber 的 JSON 结构化日志格式化器。
//!
//! 使用 tracing-subscriber 内置的 `.json()` layer，事件为扁平化结构。
//!
//! 输出示例：
//! ```json
//! {"timestamp":"2026-07-09T14:30:00.123456Z","level":"INFO","target":"crate::interfaces::server",
//!  "message":"Starting project build","trace_id":"a1b2c3","plugin":"k8s"}
//! ```
//!
//! 注意：内置格式使用 `timestamp`（而非 `ts`），精度为微秒。
//! 若生产环境需要完全符合规范的格式，可在以后启用
//! `formatter_exact.rs` 中的自定义格式化器。

// 本模块提供再导出与配置辅助函数。
// 实际的 JSON layer 通过 `tracing_subscriber::fmt::layer().json()` 创建。
// 保留本模块以供将来实现精确格式的格式化器。
