//! Processor registry — holds all registered processors.
//!
//! The registry stores a list of processor instances and provides methods
//! to query them by file path or to retrieve all processors.

use crate::application::pipeline::processor::Processor;
use std::path::Path;

/// Registry of available pipeline processors.
///
/// The registry holds all processor instances and provides methods
/// to query, filter, and sort them for execution.
pub struct ProcessorRegistry {
    processors: Vec<Box<dyn Processor>>,
}

impl ProcessorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Register a processor.
    pub fn register(&mut self, processor: Box<dyn Processor>) {
        self.processors.push(processor);
    }

    /// Return all registered processors sorted by priority (ascending).
    /// Lower-priority values run first.
    pub fn all(&self) -> &[Box<dyn Processor>] {
        &self.processors
    }

    /// Return a sorted vector of processors that match the given file path.
    ///
    /// Processors are returned in priority order (lowest first).
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

    /// Return the number of registered processors.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Return `true` when no processors are registered.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
