use crate::error::AppError;
use crate::pipeline::PipelineStage;
use serde_json::Value;

/// Extracts `role: "system"` messages from the `messages[]` array and
/// merges them into the top-level `system` field.
///
/// Claude Code v2.1.154+ (Lean System Prompt) injects system instructions
/// as `role: "system"` entries in the messages array. The standard Anthropic
/// Messages API (and DeepSeek's /anthropic endpoint) rejects these — the
/// system prompt must be in the top-level `system` field.
pub struct SystemRoleNormalizer;

impl PipelineStage for SystemRoleNormalizer {
    fn name(&self) -> &'static str {
        "system_role_normalizer"
    }

    fn process(&self, body: &mut Value) -> Result<(), AppError> {
        let Value::Object(ref mut map) = body else {
            return Ok(());
        };

        // Get the messages array
        let Some(messages) = map.get_mut("messages").and_then(|v| v.as_array_mut()) else {
            return Ok(());
        };

        // Collect system messages and their indices
        let mut system_contents: Vec<Value> = Vec::new();
        let mut system_indices: Vec<usize> = Vec::new();

        for (i, msg) in messages.iter().enumerate() {
            if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                if role == "system" {
                    if let Some(content) = msg.get("content").cloned() {
                        system_contents.push(content);
                    }
                    system_indices.push(i);
                }
            }
        }

        if system_contents.is_empty() {
            return Ok(());
        }

        // Remove system messages in reverse order (to keep indices valid)
        for i in system_indices.iter().rev() {
            messages.remove(*i);
        }

        // Merge extracted system content into the top-level `system` field
        let merged_text = system_contents
            .into_iter()
            .map(|content| match content {
                Value::String(s) => s,
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        // Merge into existing system prompt if present
        if let Some(existing) = map.get_mut("system") {
            match existing {
                Value::String(s) => {
                    s.push_str("\n\n");
                    s.push_str(&merged_text);
                }
                Value::Array(blocks) => {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": merged_text,
                    }));
                }
                _ => {}
            }
        } else {
            map.insert("system".to_string(), Value::String(merged_text));
        }

        tracing::debug!("Extracted {} system messages from messages[] array", system_indices.len());

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_system_from_messages() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 4096
        });

        let normalizer = SystemRoleNormalizer;
        normalizer.process(&mut body).unwrap();

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "system message should be removed");
        assert_eq!(messages[0]["role"], "user");

        let system = body["system"].as_str().unwrap();
        assert!(system.contains("You are a helpful assistant"));
    }

    #[test]
    fn test_merges_with_existing_system() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "Existing prompt.",
            "messages": [
                {"role": "system", "content": "Additional instructions."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 4096
        });

        SystemRoleNormalizer.process(&mut body).unwrap();

        let system = body["system"].as_str().unwrap();
        assert!(system.contains("Existing prompt."));
        assert!(system.contains("Additional instructions."));
    }

    #[test]
    fn test_no_system_messages_unchanged() {
        let original = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there"}
            ],
            "max_tokens": 4096
        });

        let mut body = original.clone();
        SystemRoleNormalizer.process(&mut body).unwrap();

        assert_eq!(body, original);
    }

    #[test]
    fn test_multiple_system_messages() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "First system message."},
                {"role": "user", "content": "Hello"},
                {"role": "system", "content": "Second system message."},
                {"role": "assistant", "content": "Hi"}
            ],
            "max_tokens": 4096
        });

        SystemRoleNormalizer.process(&mut body).unwrap();

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "both system messages should be removed");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");

        let system = body["system"].as_str().unwrap();
        assert!(system.contains("First system message."));
        assert!(system.contains("Second system message."));
    }

    #[test]
    fn test_system_with_content_blocks() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {
                    "role": "system",
                    "content": [
                        {"type": "text", "text": "Block one."},
                        {"type": "text", "text": "Block two."}
                    ]
                },
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 4096
        });

        SystemRoleNormalizer.process(&mut body).unwrap();

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);

        let system = body["system"].as_str().unwrap();
        assert!(system.contains("Block one."));
        assert!(system.contains("Block two."));
    }

    #[test]
    fn test_merges_with_existing_system_array() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "Existing block."}
            ],
            "messages": [
                {"role": "system", "content": "Additional."},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 4096
        });

        SystemRoleNormalizer.process(&mut body).unwrap();

        let system_blocks = body["system"].as_array().unwrap();
        assert_eq!(system_blocks.len(), 2);
        assert_eq!(system_blocks[0]["text"], "Existing block.");
        assert_eq!(system_blocks[1]["text"], "Additional.");
    }

    #[test]
    fn test_non_object_body_unchanged() {
        let mut body = serde_json::json!("just a string");
        let original = body.clone();
        SystemRoleNormalizer.process(&mut body).unwrap();
        assert_eq!(body, original);
    }

    #[test]
    fn test_no_messages_field_unchanged() {
        let mut body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 4096
        });
        let original = body.clone();
        SystemRoleNormalizer.process(&mut body).unwrap();
        assert_eq!(body, original);
    }
}
