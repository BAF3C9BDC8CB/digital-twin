//! Digital Twin 系统的同步 trait 与类型。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;

// ---------------------------------------------------------------------------
// SyncReport
// ---------------------------------------------------------------------------

/// 针对一种资源类型的单次同步操作的结果。
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// 人类可读的来源名（例如 "nacos/test"）。
    pub source: String,

    // -- Nacos 专用 --
    /// 已处理的命名空间数。
    pub namespaces: usize,
    /// 已 upsert 的配置数。
    pub configs: usize,
    /// 已同步的服务数。
    pub services: usize,
    /// 已创建/更新的关系数。
    pub links_created: usize,

    // -- K8s 专用 --
    /// 从外部 API 获取的条目数。
    pub items_fetched: usize,
    /// 在图数据库中新建的条目数。
    pub items_created: usize,
    /// 已存在并被更新的条目数。
    pub items_updated: usize,
    /// 因冲突或去重而跳过的条目数。
    pub items_skipped: usize,
    /// 写入失败的条目数。
    pub items_failed: usize,
    /// 同步过程中收集的错误消息（非致命）。
    pub errors: Vec<String>,

    /// 墙钟耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 同步被跳过时为 `true`（WriteCoordinator 冲突）。
    pub skipped: bool,
}

impl SyncReport {
    /// 创建 "skipped" 报告。
    pub fn skipped(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            namespaces: 0,
            configs: 0,
            services: 0,
            links_created: 0,
            items_fetched: 0,
            items_created: 0,
            items_updated: 0,
            items_skipped: 0,
            items_failed: 0,
            errors: vec![],
            elapsed_ms: 0,
            skipped: true,
        }
    }

    /// 创建带 nacos 友好字段的成功完成报告。
    pub fn completed(
        source: impl Into<String>,
        namespaces: usize,
        configs: usize,
        services: usize,
        links_created: usize,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            source: source.into(),
            namespaces,
            configs,
            services,
            links_created,
            items_fetched: configs + services,
            items_created: configs + services,
            items_updated: 0,
            items_skipped: 0,
            items_failed: 0,
            errors: vec![],
            elapsed_ms,
            skipped: false,
        }
    }

    /// 若未发生写入错误则返回 `true`。
    pub fn is_success(&self) -> bool {
        self.items_failed == 0 && self.errors.is_empty()
    }

    /// 已写入的条目总数（新建 + 更新）。
    pub fn items_written(&self) -> usize {
        self.items_created + self.items_updated
    }

    /// 总操作数（用于汇总展示）。
    pub fn total_ops(&self) -> usize {
        self.configs + self.services + self.links_created
    }

    /// 向报告追加一条错误消息。
    pub fn add_error(&mut self, msg: impl Into<String>) {
        self.items_failed += 1;
        self.errors.push(msg.into());
    }
}

// ---------------------------------------------------------------------------
// SyncSource trait
// ---------------------------------------------------------------------------

/// 可同步到知识图谱的外部系统数据来源。
#[async_trait]
pub trait SyncSource: Send + Sync {
    /// 该来源的人类可读名称（例如 "nacos/config"）。
    fn name(&self) -> &str;

    /// 执行同步。
    async fn sync(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError>;
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_report_skipped() {
        let r = SyncReport::skipped("test-src");
        assert!(r.skipped);
        assert_eq!(r.source, "test-src");
        assert_eq!(r.configs, 0);
        assert_eq!(r.elapsed_ms, 0);
    }

    #[test]
    fn sync_report_completed() {
        let r = SyncReport::completed("api", 3, 42, 5, 10, 1234);
        assert!(!r.skipped);
        assert_eq!(r.namespaces, 3);
        assert_eq!(r.configs, 42);
        assert_eq!(r.services, 5);
        assert_eq!(r.links_created, 10);
        assert_eq!(r.elapsed_ms, 1234);
    }

    #[test]
    fn sync_report_total_ops() {
        let r = SyncReport::completed("api", 1, 10, 5, 3, 100);
        assert_eq!(r.total_ops(), 18);
    }
}
