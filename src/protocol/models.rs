use serde::Serialize;

/// Response for `GET /v1/models` — OpenAI-compatible model list format.
#[derive(Debug, Serialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelInfo>,
}

/// A single model entry.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl ModelListResponse {
    pub fn new(models: Vec<ModelInfo>) -> Self {
        ModelListResponse {
            object: "list".to_string(),
            data: models,
        }
    }
}

impl ModelInfo {
    /// Create a model entry. For CC Switch compatibility, the `id` should start with
    /// `claude-` or `anthropic-` so Claude Code recognizes it in its model picker.
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        ModelInfo {
            id: id.into(),
            object: "model".to_string(),
            created: 0,
            owned_by: None,
            display_name: Some(display_name.into()),
            max_tokens: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}
