//! K8s 日志流式输出 / 下载逻辑（占位实现）。
//!
//! 后续将实现 `GetLogs`（服务端流式）和 `DownloadLogs` RPC。
//! 底层使用 Kuboard WebSocket API 或与 kubectl 等价的 K8s 客户端。

/// 日志流式输出逻辑的占位实现。
pub struct K8sLogStream;

impl K8sLogStream {
    pub fn new() -> Self {
        Self
    }
}

impl Default for K8sLogStream {
    fn default() -> Self {
        Self::new()
    }
}
