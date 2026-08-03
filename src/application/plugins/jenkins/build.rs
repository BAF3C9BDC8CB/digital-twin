//! 构建流式输出 / 日志拉取逻辑（占位实现）。
//!
//! 后续将实现 `Build`（服务端流式）和 `GetBuildLog` RPC。
//! 通过 Jenkins API 实时流式输出构建控制台日志。

/// 构建流式输出的占位实现。
pub struct BuildStream;

impl BuildStream {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuildStream {
    fn default() -> Self {
        Self::new()
    }
}
