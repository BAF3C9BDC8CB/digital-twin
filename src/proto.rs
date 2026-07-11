//! Auto-generated protobuf code from proto/*.proto
//!
//! Compiled by tonic-build in build.rs.  Each `include!()` pulls the
//! generated file from the cargo OUT_DIR.

/// Common shared types (HealthStatus, Error, Empty, KeyValue).
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
