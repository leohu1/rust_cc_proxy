pub mod anthropic;
pub mod deepseek;

use crate::config::ProviderConfig;
use crate::error::AppError;
use crate::protocol::models::ModelInfo;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Identifies a provider backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Anthropic,
    DeepSeek,
}

/// Core abstraction for an LLM provider backend.
///
/// Each supported provider implements this trait to handle
/// protocol differences (request/response transformation,
/// authentication, model resolution, etc.).
pub trait Provider: Send + Sync {
    /// Which provider this is.
    fn kind(&self) -> ProviderKind;

    /// Base URL for the upstream API.
    fn upstream_url(&self) -> &str;

    /// Extra HTTP headers to include when forwarding to this provider
    /// (e.g., auth headers, custom headers).
    fn prepare_headers(&self, _incoming: &HeaderMap) -> HeaderMap {
        HeaderMap::new()
    }

    /// Transform the request body before sending upstream.
    /// Default: no transformation (passthrough).
    fn transform_request(&self, _body: &mut Value) -> Result<(), AppError> {
        Ok(())
    }

    /// List available models for `GET /v1/models`.
    fn model_list(&self) -> Vec<ModelInfo>;

    /// Resolve a client-requested model name to the actual upstream model.
    fn resolve_model(&self, client_model: &str) -> String;

    /// Whether SSE events from this provider need translation into
    /// Anthropic-format SSE. Passthrough providers (Anthropic-native,
    /// DeepSeek /anthropic endpoint) return false.
    fn requires_sse_translation(&self) -> bool {
        false
    }

    /// The provider's default model.
    fn default_model(&self) -> &str;
}

/// Registry of all configured providers, keyed by ProviderKind.
pub struct ProviderRegistry {
    providers: HashMap<ProviderKind, Arc<dyn Provider>>,
    default_kind: ProviderKind,
}

impl ProviderRegistry {
    pub fn new(default_kind: ProviderKind) -> Self {
        ProviderRegistry {
            providers: HashMap::new(),
            default_kind,
        }
    }

    /// Register a provider.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.kind(), provider);
    }

    /// Resolve a client-requested model name to (provider, upstream_model).
    ///
    /// Resolution order:
    /// 1. Model starts with provider prefix → route to that provider
    /// 2. Otherwise → use default provider
    pub fn resolve(&self, requested_model: &str) -> (&Arc<dyn Provider>, String) {
        // Check for known prefixes
        if requested_model.starts_with("deepseek-v") || requested_model.starts_with("deepseek-") {
            if let Some(provider) = self.providers.get(&ProviderKind::DeepSeek) {
                let upstream = provider.resolve_model(requested_model);
                return (provider, upstream);
            }
        }

        // Fallback to default provider
        let provider = self
            .providers
            .get(&self.default_kind)
            .or_else(|| self.providers.values().next())
            .expect("ProviderRegistry must have at least one provider");

        let upstream = provider.resolve_model(requested_model);
        (provider, upstream)
    }

    /// Get a specific provider by kind.
    pub fn get(&self, kind: &ProviderKind) -> Option<&Arc<dyn Provider>> {
        self.providers.get(kind)
    }

    /// Collect model lists from all providers.
    pub fn all_models(&self) -> Vec<ModelInfo> {
        let mut models = Vec::new();
        for provider in self.providers.values() {
            models.extend(provider.model_list());
        }
        models
    }
}

/// Build a provider from its configuration.
pub fn create_provider(
    kind: ProviderKind,
    config: &ProviderConfig,
) -> Arc<dyn Provider> {
    match kind {
        ProviderKind::Anthropic => Arc::new(anthropic::AnthropicProvider::new(config)),
        ProviderKind::DeepSeek => Arc::new(deepseek::DeepSeekProvider::new(config)),
    }
}
