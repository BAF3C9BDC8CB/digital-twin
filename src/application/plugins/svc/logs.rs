//! 本地服务日志尾随（占位实现）。
//!
//! 后续将实现 `GetLogs` 服务端流式 RPC，尾随读取本地
//! 服务日志文件。

/// 日志流式输出的占位实现。
pub struct ServiceLogs;

impl ServiceLogs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServiceLogs {
    fn default() -> Self {
        Self::new()
    }
}
