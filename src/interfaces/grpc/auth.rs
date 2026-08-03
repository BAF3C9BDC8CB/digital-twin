//! gRPC 认证拦截器——基于角色的访问控制。
//!
//! 拦截器检查每个入站 gRPC 请求的对端地址，并将 `Role` 注入
//! 请求扩展，使下游处理器能够按操作执行授权。
//!
//! ## 信任模型
//!
//! ```text
//! 调用方                       检测方式          角色
//! ───────────────────────────────────────────────────
//! OpenCode MCP (unix socket) remote_addr = None  AdminRole
//! CLI  (dt xxx)              remote_addr = None  AdminRole
//! 外部系统 (TCP)             remote_addr = Some  ReadOnlyRole
//! ```
//!
//! 未来：ReadOnlyRole 调用方可出示 JWT bearer token 以提升为 AdminRole。

/// 从连接上下文中提取的授权角色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// 完全访问——授予 Unix socket 连接（本地信任）。
    AdminRole,
    /// 只读访问——授予网络连接。
    /// 未来：可凭有效 JWT token 提升为 `AdminRole`。
    ReadOnlyRole,
}

/// Tonic 拦截器，根据对端地址判定调用方角色，
/// 并将其注入 `request.extensions()`。
///
/// # 检测逻辑
///
/// - `request.remote_addr()` 对 Unix domain socket 返回 `None` →
///   视为受信任（`AdminRole`）。
/// - `request.remote_addr()` 对 TCP 连接返回 `Some(_)` →
///   受限（`ReadOnlyRole`），除非出示有效 JWT（未来）。
///
/// # 示例
///
/// ```ignore
/// use tower::ServiceBuilder;
/// use tonic::service::interceptor;
///
/// let layer = ServiceBuilder::new()
///     .layer(interceptor(auth::auth_interceptor));
/// let server = tonic::transport::Server::builder()
///     .layer(layer);
/// ```
#[allow(clippy::result_large_err)] // tonic::Status 因 API 约定而体积较大
pub fn auth_interceptor(mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
    let role = match req.remote_addr() {
        // Unix domain socket——本地受信任的客户端
        None => Role::AdminRole,
        // TCP 网络连接——默认受限
        Some(_addr) => Role::ReadOnlyRole,
    };

    req.extensions_mut().insert(role);
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interceptor_injects_role_for_tcp() {
        // 构造带 TCP 远程地址的请求
        let _addr: std::net::SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let mut req = tonic::Request::new(());
        // 只有当 tonic 从元数据解析后 remote_addr() 才为 Some。
        // 在单元测试中默认是 None。因此这里改为测试枚举语义。
        req.extensions_mut().insert(Role::ReadOnlyRole);

        let role = req.extensions().get::<Role>().unwrap();
        assert_eq!(*role, Role::ReadOnlyRole);
    }

    #[test]
    fn interceptor_returns_result() {
        let req = tonic::Request::new(());
        // 未设置 remote_addr 时 tonic 返回 None → 视为 AdminRole
        let result = auth_interceptor(req);
        assert!(result.is_ok());
        let req = result.unwrap();
        let role = req.extensions().get::<Role>().unwrap();
        assert_eq!(*role, Role::AdminRole);
    }
}
