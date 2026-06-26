use crate::config::ProviderConfig;
use crate::error::AppError;
use crate::protocol::models::ModelInfo;
use crate::providers::{Provider, ProviderKind};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

/// Passthrough provider for Anthropic's native API.
///
/// No request transformation is needed — the proxy forwards
/// requests as-is and streams SSE events directly.
pub struct AnthropicProvider {
    config: ProviderConfig,
}

impl AnthropicProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        AnthropicProvider {
            config: config.clone(),
        }
    }
}

impl Provider for AnthropicProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn upstream_url(&self) -> &str {
        &self.config.upstream_url
    }

    fn prepare_headers(&self, _incoming: &HeaderMap) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ref api_key) = self.config.api_key {
            if let Ok(val) = HeaderValue::from_str(api_key) {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    val,
                );
            }
        }
        headers
    }

    fn model_list(&self) -> Vec<ModelInfo> {
        // Return models with claude- prefix for CC Switch compatibility.
        // These appear in Claude Code's model picker.
        let models = [
            ("claude-sonnet-4-20250514", "Claude Sonnet 4"),
            ("claude-sonnet-4-6-20251001", "Claude Sonnet 4.6"),
            ("claude-opus-4-8-20251001", "Claude Opus 4.8"),
            ("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
            ("claude-fable-5-20251001", "Claude Fable 5"),
        ];

        models
            .iter()
            .map(|(id, name)| ModelInfo::new(*id, *name))
            .collect()
    }

    fn resolve_model(&self, client_model: &str) -> String {
        // Passthrough: use the model name as-is
        client_model.to_string()
    }

    fn default_model(&self) -> &str {
        &self.config.default_model
    }

    fn requires_sse_translation(&self) -> bool {
        false
    }

    fn transform_request(&self, _body: &mut Value) -> Result<(), AppError> {
        // No transformation needed for Anthropic passthrough
        Ok(())
    }
}
