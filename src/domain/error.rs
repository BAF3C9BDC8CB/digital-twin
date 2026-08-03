//! 数字孪生系统的错误类型。

/// 应用最顶层的错误枚举。
#[derive(Debug, thiserror::Error)]
pub enum DtError {
    /// 带消息的通用错误。
    #[error("{0}")]
    General(String),
    /// I/O 错误。
    #[error("I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    /// JSON 序列化/反序列化错误。
    #[error("JSON 错误：{0}")]
    Json(#[from] serde_json::Error),
    /// 仓库错误——包装底层存储错误。
    #[error("存储层错误：{0}")]
    Repository(String),
    /// 配置错误。
    #[error("配置错误：{0}")]
    Config(String),
    /// gRPC / 网络错误。
    #[error("gRPC 错误：{0}")]
    Grpc(String),
    /// 未找到。
    #[error("未找到：{0}")]
    NotFound(String),
    /// 超时。
    #[error("超时：{0}")]
    Timeout(String),
    /// 网络 / HTTP 错误。
    #[error("网络错误：{0}")]
    Network(String),
}

impl From<&str> for DtError {
    fn from(s: &str) -> Self {
        DtError::General(s.to_string())
    }
}

impl From<anyhow::Error> for DtError {
    fn from(e: anyhow::Error) -> Self {
        DtError::General(e.to_string())
    }
}
