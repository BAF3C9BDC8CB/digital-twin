use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

pub struct SideEffectRunner {
    graph: Arc<dyn GraphRepository>,
}

impl SideEffectRunner {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    pub async fn run(
        &self,
        effects: &[String],
        params: &HashMap<String, Value>,
    ) -> Result<(), DtError> {
        for cypher in effects {
            if cypher.trim().is_empty() {
                continue;
            }
            self.graph.write_query(cypher, params.clone()).await?;
        }
        Ok(())
    }
}
