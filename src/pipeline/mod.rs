pub mod system_normalizer;

use crate::error::AppError;
use serde_json::Value;
use std::sync::Arc;

/// A processing stage in the request pipeline.
///
/// Each stage can inspect and mutate the request body before it is
/// forwarded to the upstream provider.
pub trait PipelineStage: Send + Sync {
    /// Process the request body in-place.
    fn process(&self, body: &mut Value) -> Result<(), AppError>;

    /// Human-readable name for logging.
    fn name(&self) -> &'static str;
}

/// An ordered sequence of pipeline stages.
pub struct Pipeline {
    stages: Vec<Arc<dyn PipelineStage>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline { stages: Vec::new() }
    }

    /// Append a stage to the pipeline.
    pub fn push(&mut self, stage: Arc<dyn PipelineStage>) {
        self.stages.push(stage);
    }

    /// Run all stages in order over the request body.
    pub fn run(&self, body: &mut Value) -> Result<(), AppError> {
        for stage in &self.stages {
            stage.process(body)?;
        }
        Ok(())
    }

    /// Returns the number of stages.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}
