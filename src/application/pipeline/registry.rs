//! 处理器注册表——保存所有已注册的处理器。
//!
//! 注册表存储处理器实例列表，并提供按文件路径查询或获取全部处理器的方法。

use crate::application::pipeline::processor::Processor;
use std::path::Path;

/// 可用流水线处理器的注册表。
///
/// 注册表保存全部处理器实例，并提供查询、过滤与排序以用于执行的方法。
pub struct ProcessorRegistry {
    processors: Vec<Box<dyn Processor>>,
}

impl ProcessorRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// 注册一个处理器。
    pub fn register(&mut self, processor: Box<dyn Processor>) {
        self.processors.push(processor);
    }

    /// 返回按优先级升序排列的全部已注册处理器。
    /// 优先级越低的处理器越先执行。
    pub fn all(&self) -> &[Box<dyn Processor>] {
        &self.processors
    }

    /// 返回与给定文件路径匹配的处理器排序向量。
    ///
    /// 处理器按优先级顺序返回（最低优先）。
    pub fn matching(&self, file_path: &Path) -> Vec<&dyn Processor> {
        let mut matching: Vec<&dyn Processor> = self
            .processors
            .iter()
            .filter(|p| p.matches(file_path))
            .map(|p| p.as_ref())
            .collect();
        matching.sort_by_key(|p| p.priority());
        matching
    }

    /// 返回已注册处理器的数量。
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// 当没有注册任何处理器时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
