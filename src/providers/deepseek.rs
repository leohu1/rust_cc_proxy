use crate::config::ProviderConfig;
use crate::error::AppError;
use crate::protocol::models::ModelInfo;
use crate::providers::{Provider, ProviderKind};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

/// Provider for DeepSeek's API.
///
/// Uses DeepSeek's `/anthropic` endpoint which is mostly Anthropic-compatible
/// but needs a few fixes applied to the request body.
pub struct DeepSeekProvider {
    config: ProviderConfig,
}

impl DeepSeekProvider {
    pub fn new(config: &ProviderConfig) -> Self {
        DeepSeekProvider {
            config: config.clone(),
        }
    }
}

impl Provider for DeepSeekProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::DeepSeek
    }

    fn upstream_url(&self) -> &str {
        &self.config.upstream_url
    }

    fn prepare_headers(&self, _incoming: &HeaderMap) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(ref api_key) = self.config.api_key {
            if let Ok(val) = HeaderValue::from_str(api_key) {
                headers.insert(
                    HeaderName::from_static("authorization"),
                    HeaderValue::from_str(&format!("Bearer {api_key}")).unwrap_or(val),
                );
            }
        }
        headers
    }

    fn model_list(&self) -> Vec<ModelInfo> {
        // Prefix with "claude-" for CC Switch: Claude Code only shows
        // models whose IDs start with "claude" or "anthropic" in its picker.
        vec![
            ModelInfo::new("claude-deepseek-v4-pro", "DeepSeek V4 Pro").with_max_tokens(65536),
            ModelInfo::new("claude-deepseek-v4-flash", "DeepSeek V4 Flash").with_max_tokens(65536),
        ]
    }

    fn resolve_model(&self, client_model: &str) -> String {
        // Map claude- prefixed model names back to DeepSeek model names
        let stripped = client_model
            .strip_prefix("claude-")
            .unwrap_or(client_model);

        match stripped {
            "deepseek-v4-pro" | "deepseek-v4-flash" => stripped.to_string(),
            _ => {
                // Check model_map for custom mappings
                if let Some(mapped) = self.config.model_map.get(stripped) {
                    return mapped.clone();
                }
                // Fallback
                self.config.default_model.clone()
            }
        }
    }

    fn default_model(&self) -> &str {
        &self.config.default_model
    }

    fn requires_sse_translation(&self) -> bool {
        // DeepSeek's /anthropic endpoint returns Anthropic-compatible SSE
        false
    }

    fn transform_request(&self, body: &mut Value) -> Result<(), AppError> {
        // Fix 1: Normalize thinking configuration
        normalize_thinking(body);

        // Fix 2: Inject empty thinking blocks before tool_use blocks
        if let Some(thinking) = body.get("thinking") {
            if thinking.get("type").and_then(|t| t.as_str()) == Some("enabled") {
                if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
                    if model.starts_with("deepseek-v") {
                        inject_thinking_blocks(body);
                    }
                }
            }
        }

        Ok(())
    }
}

/// Fix 1: Normalize thinking configuration for DeepSeek compatibility.
///
/// - Converts `adaptive` / `auto` thinking types to `enabled`
///   (the only values DeepSeek's /anthropic endpoint accepts).
/// - Strips `reasoning_effort` and `output_config` fields
///   (not recognized by DeepSeek).
/// - When thinking is mapped to `disabled`, removes historical
///   `thinking` / `redacted_thinking` blocks from messages.
fn normalize_thinking(body: &mut Value) {
    let Value::Object(ref mut map) = body else {
        return;
    };

    // Fix thinking.type
    let thinking_type = map
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());

    if let Some(t) = thinking_type {
        let normalized = match t.as_str() {
            "adaptive" | "auto" => "enabled",
            other => other,
        };

        if let Some(thinking) = map.get_mut("thinking") {
            if let Some(obj) = thinking.as_object_mut() {
                obj.insert("type".to_string(), Value::String(normalized.to_string()));
            }
        }

        // Strip unrecognized fields
        map.remove("reasoning_effort");
        map.remove("output_config");

        // If thinking is disabled, strip thinking/redacted_thinking blocks
        if normalized == "disabled" {
            if let Some(messages) = map.get_mut("messages").and_then(|m| m.as_array_mut()) {
                for msg in messages.iter_mut() {
                    if let Some(content) = msg.get_mut("content") {
                        if let Some(blocks) = content.as_array_mut() {
                            blocks.retain(|block| {
                                let block_type = block
                                    .get("type")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("");
                                block_type != "thinking"
                                    && block_type != "redacted_thinking"
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Fix 2: Inject empty `thinking` blocks before `tool_use` blocks
/// in assistant messages.
///
/// DeepSeek's /anthropic endpoint requires a `thinking` block before
/// `tool_use` blocks in assistant messages. Without it, the API returns
/// HTTP 400. We inject an empty `{"type":"thinking","thinking":""}` block
/// immediately before the first `tool_use` block when one is missing.
fn inject_thinking_blocks(body: &mut Value) {
    let Value::Object(ref mut map) = body else {
        return;
    };

    let Some(messages) = map.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    for msg in messages.iter_mut() {
        // Only process assistant messages
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }

        let Some(content) = msg.get_mut("content") else {
            continue;
        };

        let Some(blocks) = content.as_array_mut() else {
            continue;
        };

        // Check if this message has tool_use blocks but no thinking blocks
        let has_tool_use = blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
        let has_thinking = blocks.iter().any(|b| {
            let t = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
            t == "thinking" || t == "redacted_thinking"
        });

        if has_tool_use && !has_thinking {
            // Find the first tool_use block and insert thinking before it
            if let Some(pos) = blocks.iter().position(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            }) {
                blocks.insert(
                    pos,
                    serde_json::json!({
                        "type": "thinking",
                        "thinking": ""
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_adaptive_thinking() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "adaptive", "budget_tokens": 4096},
            "reasoning_effort": "high",
            "messages": []
        });

        normalize_thinking(&mut body);

        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_inject_thinking_before_tool_use() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me check."},
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}}
                    ]
                }
            ]
        });

        inject_thinking_blocks(&mut body);

        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "thinking");
        assert_eq!(blocks[2]["type"], "tool_use");
    }

    #[test]
    fn test_no_injection_when_thinking_exists() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me check."},
                        {"type": "thinking", "thinking": "reasoning..."},
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}}
                    ]
                }
            ]
        });

        let original = body.clone();
        inject_thinking_blocks(&mut body);

        // Should be unchanged — thinking block already present
        assert_eq!(body, original);
    }

    #[test]
    fn test_inject_before_multiple_tool_uses() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me check."},
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}},
                        {"type": "tool_use", "id": "toolu_2", "name": "search", "input": {}}
                    ]
                }
            ]
        });

        inject_thinking_blocks(&mut body);

        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "thinking");
        assert_eq!(blocks[2]["type"], "tool_use");
        assert_eq!(blocks[3]["type"], "tool_use");
        // Only one thinking block injected
        assert_eq!(blocks.len(), 4);
    }

    #[test]
    fn test_normalize_auto_thinking() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "auto", "budget_tokens": 2048},
            "messages": []
        });

        normalize_thinking(&mut body);

        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn test_normalize_disabled_strips_thinking_blocks() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "disabled"},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "old reasoning..."},
                        {"type": "text", "text": "Normal text."},
                        {"type": "redacted_thinking", "data": "redacted"}
                    ]
                }
            ]
        });

        normalize_thinking(&mut body);

        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn test_normalize_strips_reasoning_effort() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "reasoning_effort": "xhigh",
            "output_config": {"some": "config"},
            "messages": []
        });

        normalize_thinking(&mut body);

        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn test_inject_no_tool_use_no_change() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Plain text only."}
                    ]
                }
            ]
        });

        let original = body.clone();
        inject_thinking_blocks(&mut body);
        assert_eq!(body, original);
    }

    #[test]
    fn test_inject_skips_user_messages() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "enabled"},
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {}}
                    ]
                }
            ]
        });

        let original = body.clone();
        inject_thinking_blocks(&mut body);
        assert_eq!(body, original);
    }

    #[test]
    fn test_transform_request_full_flow() {
        let mut body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "thinking": {"type": "adaptive", "budget_tokens": 4096},
            "reasoning_effort": "xhigh",
            "output_config": {},
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "I'll help."},
                        {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}
                    ]
                },
                {
                    "role": "user",
                    "content": "What's the weather?"
                }
            ]
        });

        let config = ProviderConfig {
            upstream_url: "https://api.deepseek.com/anthropic".into(),
            api_key: Some("sk-test".into()),
            default_model: "deepseek-v4-flash".into(),
            model_map: Default::default(),
        };
        let provider = DeepSeekProvider::new(&config);

        provider.transform_request(&mut body).unwrap();

        // thinking normalized
        assert_eq!(body["thinking"]["type"], "enabled");
        // reasoning_effort stripped
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("output_config").is_none());
        // thinking block injected before tool_use
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "thinking");
        assert_eq!(blocks[2]["type"], "tool_use");
    }

    #[test]
    fn test_model_resolution() {
        let config = ProviderConfig {
            upstream_url: "https://api.deepseek.com/anthropic".into(),
            api_key: None,
            default_model: "deepseek-v4-flash".into(),
            model_map: [("sonnet".into(), "deepseek-v4-pro".into())].into(),
        };
        let provider = DeepSeekProvider::new(&config);

        // Direct deepseek model
        assert_eq!(provider.resolve_model("deepseek-v4-pro"), "deepseek-v4-pro");
        // Claude-prefixed
        assert_eq!(
            provider.resolve_model("claude-deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
        // Model map match
        assert_eq!(provider.resolve_model("sonnet"), "deepseek-v4-pro");
        // Unknown → fallback
        assert_eq!(provider.resolve_model("unknown-model"), "deepseek-v4-flash");
    }

    #[test]
    fn test_prepare_headers_with_api_key() {
        let config = ProviderConfig {
            upstream_url: "https://api.deepseek.com/anthropic".into(),
            api_key: Some("sk-ds-secret".into()),
            default_model: "deepseek-v4-flash".into(),
            model_map: Default::default(),
        };
        let provider = DeepSeekProvider::new(&config);

        let incoming = HeaderMap::new();
        let headers = provider.prepare_headers(&incoming);

        let auth = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(auth, "Bearer sk-ds-secret");
    }

    #[test]
    fn test_prepare_headers_no_api_key() {
        let config = ProviderConfig {
            upstream_url: "https://api.deepseek.com/anthropic".into(),
            api_key: None,
            default_model: "deepseek-v4-flash".into(),
            model_map: Default::default(),
        };
        let provider = DeepSeekProvider::new(&config);

        let incoming = HeaderMap::new();
        let headers = provider.prepare_headers(&incoming);

        assert!(headers.get("authorization").is_none());
    }
}
