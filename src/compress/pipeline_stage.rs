use crate::compress::Compressor;
use crate::error::AppError;
use crate::pipeline::PipelineStage;
use serde_json::Value;
use std::sync::Arc;

/// Pipeline stage that compresses large `tool_result` content blocks
/// in the latest user message (the "live zone" in headroom terminology).
///
/// This captures the biggest token savings: tool outputs in agent loops
/// often contain massive JSON arrays (search results, file listings, logs).
pub struct CompressionStage {
    compressor: Arc<Compressor>,
}

impl CompressionStage {
    pub fn new(compressor: Arc<Compressor>) -> Self {
        CompressionStage { compressor }
    }
}

impl PipelineStage for CompressionStage {
    fn name(&self) -> &'static str {
        "compression"
    }

    /// Pipeline orchestration:
    /// 1. Detect live zone (latest user message)
    /// 2. Cache-safety: hash frozen prefix + check cache_control
    /// 3. Content-detect → compress each tool_result block
    /// 4. Token-validate: reject if no savings
    /// 5. Re-serialize + verify prefix integrity
    fn process(&self, body: &mut Value) -> Result<(), AppError> {
        let Value::Object(ref mut map) = body else {
            return Ok(());
        };

        let Some(messages) = map.get_mut("messages").and_then(|v| v.as_array_mut()) else {
            return Ok(());
        };

        // ── Step 1: Find the live zone ────────────────────────────
        let last_user_idx = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, msg)| msg.get("role").and_then(|r| r.as_str()) == Some("user"))
            .map(|(i, _)| i);

        let Some(user_idx) = last_user_idx else {
            return Ok(());
        };

        // ── Step 2: Cache safety — hash frozen prefix ────────────
        let user_msg = &messages[user_idx];
        let _prefix_hash = crate::compress::live_zone::hash_messages(&messages[..user_idx]);
        let _frozen_count = crate::compress::cache_aware::frozen_message_count(messages);

        if !crate::compress::cache_aware::is_live_zone_safe_to_compress(user_msg) {
            tracing::debug!("Compression: skipped (cache_control on live zone)");
            return Ok(());
        }

        // ── Step 3: Compress tool_result blocks ───────────────────
        let user_msg = &mut messages[user_idx];
        let Some(content) = user_msg.get_mut("content") else {
            return Ok(());
        };

        let Some(blocks) = content.as_array_mut() else {
            return Ok(());
        };

        let mut compressed_count = 0usize;
        let mut bytes_saved = 0u64;

        for block in blocks.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }

            // Get the content — could be a string or array of blocks
            let tool_content = match block.get_mut("content") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(Value::Array(inner_blocks)) => {
                    // For array content, concatenate text blocks
                    let text: String = inner_blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() {
                        None
                    } else {
                        Some(text)
                    }
                }
                _ => None,
            };

            let Some(content_str) = tool_content else {
                continue;
            };

            // Try to compress
            let result = self.compressor.compress_string(&content_str)?;

            match result {
                crate::compress::CompressionResult::Compressed {
                    replacement,
                    original_bytes,
                    compressed_bytes,
                    ..
                } => {
                    bytes_saved += (original_bytes as u64).saturating_sub(compressed_bytes as u64);
                    compressed_count += 1;

                    // Replace the content
                    match block.get_mut("content") {
                        Some(Value::String(s)) => *s = replacement,
                        Some(Value::Array(inner_blocks)) => {
                            // Replace with a single text block
                            *inner_blocks = vec![serde_json::json!({
                                "type": "text",
                                "text": replacement,
                            })];
                        }
                        _ => {}
                    }

                    tracing::debug!(
                        "Compression: {}→{} bytes saved={}",
                        original_bytes,
                        compressed_bytes,
                        original_bytes.saturating_sub(compressed_bytes)
                    );
                }
                crate::compress::CompressionResult::Unchanged => {
                    tracing::trace!("Compression: block unchanged (below threshold)");
                }
                crate::compress::CompressionResult::Skipped => {
                    tracing::trace!("Compression: block skipped (unsupported format)");
                }
            }
        }

        // ── Step 4: Verify prefix integrity ──────────────────────
        let new_prefix_hash = crate::compress::live_zone::hash_messages(&messages[..user_idx]);
        if new_prefix_hash != _prefix_hash {
            tracing::warn!(
                "Compression: PREFIX HASH MISMATCH — frozen zone modified! \
                 Compressed data preserved but prompt cache may be invalidated."
            );
        }

        if compressed_count > 0 {
            tracing::info!(
                "Pipeline: {compressed_count} blocks compressed, ~{bytes_saved} bytes saved, \
                 frozen={_frozen_count} live_start={user_idx} prefix_ok={}",
                new_prefix_hash == _prefix_hash
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Compressor;

    fn make_compressor() -> Arc<Compressor> {
        Arc::new(Compressor::for_test(10, 5))
    }

    #[test]
    fn test_compresses_large_tool_result() {
        let stage = CompressionStage::new(make_compressor());

        // Use verbose items so compression passes the token validator gate.
        let items: Vec<Value> = (0..500)
            .map(|i| serde_json::json!({
                "index": i,
                "title": format!("Item number {i} with a very verbose title to consume many tokens"),
                "body": "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".repeat(5),
                "active": i % 7 != 0,
            }))
            .collect();
        let large_json = serde_json::to_string(&items).unwrap();

        let mut body = serde_json::json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": "Find files"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "search", "input": {}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": large_json}
                ]}
            ]
        });

        stage.process(&mut body).unwrap();

        // Check that the tool_result content was replaced
        let blocks = body["messages"][2]["content"].as_array().unwrap();
        let tool_block = &blocks[0];
        // String content stays as string after compression
        let content_str = tool_block["content"].as_str().unwrap();
        assert!(content_str.contains("<<ccr:"), "should contain CCR marker");
        assert!(content_str.contains("bytes"), "should contain size info");
    }

    #[test]
    fn test_small_tool_result_unchanged() {
        let stage = CompressionStage::new(make_compressor());

        let mut body = serde_json::json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "small result"}
                ]}
            ]
        });

        let original = body.clone();
        stage.process(&mut body).unwrap();

        // Should be unchanged (below threshold)
        assert_eq!(body, original);
    }

    #[test]
    fn test_string_content_compressed() {
        let stage = CompressionStage::new(make_compressor());

        let items: Vec<Value> = (0..50).map(|i| serde_json::json!({"id": i})).collect();
        let large_json = serde_json::to_string(&items).unwrap();

        let mut body = serde_json::json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": large_json}
                ]}
            ]
        });

        stage.process(&mut body).unwrap();

        let blocks = body["messages"][0]["content"].as_array().unwrap();
        let content = &blocks[0]["content"];
        // String content should be replaced with a string containing CCR
        let s = content.as_str().unwrap();
        assert!(s.contains("<<ccr:"));
    }

    #[test]
    fn test_preserves_non_user_messages() {
        let stage = CompressionStage::new(make_compressor());

        let mut body = serde_json::json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "assistant", "content": "I'll help."}
            ]
        });

        let original = body.clone();
        stage.process(&mut body).unwrap();
        assert_eq!(body, original);
    }
}
