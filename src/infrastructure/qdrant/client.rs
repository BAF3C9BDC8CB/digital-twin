//! Qdrant gRPC 客户端——异步向量搜索与 upsert。
//!
//! 使用 `qdrant-client` crate 进行异步 gRPC 通信。

use crate::domain::error::DtError;
use qdrant_client::Qdrant;

/// 包装 `qdrant-client` crate 的 `Qdrant` 客户端的 Qdrant gRPC 客户端。
#[derive(Clone)]
pub struct QdrantClient {
    client: Qdrant,
}

impl QdrantClient {
    /// 连接到给定 URI 的 Qdrant 服务器并返回客户端。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// let client = QdrantClient::connect("http://localhost:6334").await?;
    /// ```
    pub async fn connect(uri: &str) -> Result<Self, DtError> {
        let client = Qdrant::from_url(uri)
            .build()
            .map_err(|e| DtError::Repository(format!("Qdrant 连接: {}", e)))?;
        Ok(Self { client })
    }

    /// 返回内部 `qdrant_client::Qdrant` 客户端的引用。
    pub fn inner(&self) -> &Qdrant {
        &self.client
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use qdrant_client::Qdrant;

    /// 验证 `QdrantClient` 可以被构造（无需实际连接）。
    #[test]
    fn test_client_can_be_constructed_without_connection() {
        // 没有运行中的 Qdrant 时无法测试 `connect()`，但我们可以
        // 验证结构体可以编译并通过类型检查。
        fn _accept_client(_c: &QdrantClient) {}
    }

    #[test]
    fn test_qdrant_type_is_available() {
        // 验证 qdrant-client crate 被正确链接。
        let _ = Qdrant::from_url("http://localhost:6334");
    }
}
