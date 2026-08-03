//! K8s 状态操作（占位实现）。
//!
//! 后续将实现 `GetPods`、`GetDeployments`、`GetServices`
//! 和 `GetStatus` RPC。

/// K8s 状态查询的占位实现。
pub struct K8sStatus;

impl K8sStatus {
    pub fn new() -> Self {
        Self
    }
}

impl Default for K8sStatus {
    fn default() -> Self {
        Self::new()
    }
}
