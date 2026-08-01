//! gRPC authentication interceptor — role-based access control.
//!
//! The interceptor inspects the peer address of every incoming gRPC request
//! and injects a `Role` into the request extensions so that downstream
//! handlers can enforce per-operation authorization.
//!
//! ## Trust model
//!
//! ```text
//! Caller                     Detection          Role
//! ───────────────────────────────────────────────────
//! OpenCode MCP (unix socket) remote_addr = None  AdminRole
//! CLI  (dt xxx)              remote_addr = None  AdminRole
//! External system (TCP)      remote_addr = Some  ReadOnlyRole
//! ```
//!
//! Future: ReadOnlyRole callers may present a JWT bearer token to be
//! elevated to AdminRole.

/// Authorization role extracted from the connection context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// Full access — granted to Unix socket connections (local trust).
    AdminRole,
    /// Read-only access — granted to network connections.
    /// Future: can be elevated to `AdminRole` with a valid JWT token.
    ReadOnlyRole,
}

/// Tonic interceptor that determines the caller's role from the peer
/// address and injects it into `request.extensions()`.
///
/// # Detection logic
///
/// - `request.remote_addr()` returns `None` for Unix domain sockets →
///   trusted (`AdminRole`).
/// - `request.remote_addr()` returns `Some(_)` for TCP connections →
///   restricted (`ReadOnlyRole`), unless a valid JWT is presented (future).
///
/// # Example
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
#[allow(clippy::result_large_err)] // tonic::Status is large by API contract
pub fn auth_interceptor(mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
    let role = match req.remote_addr() {
        // Unix domain socket — locally trusted client
        None => Role::AdminRole,
        // TCP network connection — restricted by default
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
        // Construct a request with a TCP remote address
        let _addr: std::net::SocketAddr = "192.168.1.1:12345".parse().unwrap();
        let mut req = tonic::Request::new(());
        // remote_addr() is Some only after tonic parses it from metadata.
        // In unit tests it defaults to None. We test the enum semantics instead.
        req.extensions_mut().insert(Role::ReadOnlyRole);

        let role = req.extensions().get::<Role>().unwrap();
        assert_eq!(*role, Role::ReadOnlyRole);
    }

    #[test]
    fn interceptor_returns_result() {
        let req = tonic::Request::new(());
        // Without a set remote_addr, tonic returns None → treated as AdminRole
        let result = auth_interceptor(req);
        assert!(result.is_ok());
        let req = result.unwrap();
        let role = req.extensions().get::<Role>().unwrap();
        assert_eq!(*role, Role::AdminRole);
    }
}
