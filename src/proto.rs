//! 由 proto/*.proto 自动生成的 protobuf 代码
//!
//! 由 build.rs 中的 tonic-build 编译。每个 `include!()` 从 cargo 的
//! OUT_DIR 引入生成的文件。

/// 通用共享类型（HealthStatus、Error、Empty、KeyValue）。
pub mod dt {
    pub mod common {
        include!(concat!(env!("OUT_DIR"), "/dt.common.rs"));
    }
    pub mod embed {
        include!(concat!(env!("OUT_DIR"), "/dt.embed.rs"));
    }
    pub mod core {
        include!(concat!(env!("OUT_DIR"), "/dt.core.rs"));
    }
    pub mod metrics {
        include!(concat!(env!("OUT_DIR"), "/dt.metrics.rs"));
    }
    pub mod log {
        include!(concat!(env!("OUT_DIR"), "/dt.log.rs"));
    }
}
