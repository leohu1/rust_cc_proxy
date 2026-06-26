//! Cache-aware compression.
//!
//! Anthropic and some providers support prompt caching via `cache_control`
//! markers on content blocks and messages. The "frozen zone" (cached prefix)
//! must NOT be modified — any change invalidates the cache.
//!
//! This module detects the boundary between the frozen (cached) and live
//! (uncached) zones in the messages array, so compression only touches the
//! live zone. The key insight: Claude Code places `cache_control` markers
//! on system blocks and early conversation turns; the latest user message
//! is always in the live zone.
//!
//! Our compression already targets only the latest user message, which is
//! always in the live zone. This module adds an explicit safety check:
//! verify that no `cache_control` marker exists on the latest user message's
//! content blocks before compressing them.

use serde_json::Value;

/// Check if a content block has a `cache_control` marker.
pub fn has_cache_control(block: &Value) -> bool {
    block
        .get("cache_control")
        .and_then(|cc| cc.get("type"))
        .map(|t| t == "ephemeral")
        .unwrap_or(false)
}

/// Find the frozen message count — the number of messages from the start
/// that are in the prompt cache (have `cache_control` on at least one content
/// block). Messages at or above this index are in the live zone.
pub fn frozen_message_count(messages: &[Value]) -> usize {
    let mut count = 0;
    for msg in messages {
        let has_cache = match msg.get("content") {
            Some(Value::Array(blocks)) => blocks.iter().any(has_cache_control),
            _ => false,
        };
        if has_cache {
            count += 1;
        } else {
            // Cache is contiguous from the start; first message without
            // cache_control breaks the frozen zone.
            break;
        }
    }
    count
}

/// Check if the latest user message is safe to compress (no cache_control
/// on its tool_result blocks). Returns true if compression is safe.
pub fn is_live_zone_safe_to_compress(user_msg: &Value) -> bool {
    let blocks = match user_msg.get("content").and_then(|c| c.as_array()) {
        Some(b) => b,
        None => return true, // string content — always safe
    };

    // If any tool_result block has cache_control, skip compression
    !blocks.iter().any(|b| {
        b.get("type").and_then(|t| t.as_str()) == Some("tool_result") && has_cache_control(b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_cache_control() {
        let block = serde_json::json!({"type": "text", "text": "hello"});
        assert!(!has_cache_control(&block));
    }

    #[test]
    fn test_has_cache_control() {
        let block = serde_json::json!({
            "type": "text",
            "text": "hello",
            "cache_control": {"type": "ephemeral"}
        });
        assert!(has_cache_control(&block));
    }

    #[test]
    fn test_frozen_message_count() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}}]}),
            serde_json::json!({"role": "assistant", "content": "b"}),
            serde_json::json!({"role": "user", "content": "c"}),
        ];
        assert_eq!(frozen_message_count(&messages), 1);
    }

    #[test]
    fn test_no_frozen_messages() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "a"}),
            serde_json::json!({"role": "assistant", "content": "b"}),
        ];
        assert_eq!(frozen_message_count(&messages), 0);
    }

    #[test]
    fn test_live_zone_safe() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "data"}]
        });
        assert!(is_live_zone_safe_to_compress(&msg));
    }

    #[test]
    fn test_live_zone_unsafe_with_cache() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "data", "cache_control": {"type": "ephemeral"}}]
        });
        assert!(!is_live_zone_safe_to_compress(&msg));
    }
}
