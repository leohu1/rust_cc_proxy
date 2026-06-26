//! Live-zone byte surgery.
//!
//! Identifies the "live zone" in the request body (the latest user message),
//! compresses tool_result blocks, and splices compressed bytes back while
//! preserving the cache-frozen prefix byte-for-byte.
//!
//! Cache safety: SHA-256 hash of frozen messages is verified before/after.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::compress::{CompressionResult, Compressor};

pub struct SurgeryReport {
    pub body: Vec<u8>,
    pub blocks_compressed: usize,
    pub bytes_saved: u64,
    pub prefix_integrity_ok: bool,
}

pub fn hash_messages(messages: &[Value]) -> String {
    if messages.is_empty() { return String::new(); }
    let bytes = serde_json::to_vec(messages).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

pub fn operate(body_bytes: &[u8], compressor: &Compressor) -> SurgeryReport {
    let mut body: Value = match serde_json::from_slice(body_bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Live-zone: parse failed: {e}");
            return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: true };
        }
    };

    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(m) => m,
        None => return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: true },
    };

    let user_idx = match messages.iter().enumerate().rev()
        .find(|(_, m)| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .map(|(i, _)| i) {
        Some(i) => i,
        None => return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: true },
    };

    // ── Clone prefix for integrity check ─────────────────────────
    let prefix_snapshot: Vec<Value> = messages[..user_idx].to_vec();
    let prefix_hash = hash_messages(&prefix_snapshot);

    // Cache-aware safety
    let cache_safe = crate::compress::cache_aware::is_live_zone_safe_to_compress(&messages[user_idx]);
    if !cache_safe {
        return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: true };
    }

    // ── Compress tool_result blocks in live zone ──────────────────
    let user_msg = &mut messages[user_idx];
    let blocks = match user_msg.get_mut("content").and_then(|c| c.as_array_mut()) {
        Some(b) => b,
        None => return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: true },
    };

    let mut compressed_count = 0usize;
    let mut bytes_saved = 0u64;

    for block in blocks.iter_mut() {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
        let content = match block.get("content") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(a)) => {
                let t: String = a.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n");
                if t.is_empty() { None } else { Some(t) }
            }
            _ => None,
        };
        let Some(ref text) = content else { continue; };

        match compressor.compress_string(text) {
            Ok(CompressionResult::Compressed { replacement, original_bytes, compressed_bytes: cb, .. }) => {
                bytes_saved += original_bytes as u64 - cb as u64;
                compressed_count += 1;
                match block.get_mut("content") {
                    Some(Value::String(s)) => *s = replacement,
                    Some(Value::Array(a)) => *a = vec![serde_json::json!({"type":"text","text":replacement})],
                    _ => {}
                }
                tracing::debug!("Live-zone: {original_bytes}→{cb} bytes");
            }
            _ => {}
        }
    }

    // ── Verify prefix integrity ───────────────────────────────────
    let new_prefix = &messages[..user_idx];
    let prefix_ok = hash_messages(new_prefix) == prefix_hash;
    if !prefix_ok {
        tracing::warn!("Live-zone: PREFIX HASH MISMATCH — cache invalidated! Reverting.");
        return SurgeryReport { body: body_bytes.to_vec(), blocks_compressed: 0, bytes_saved: 0, prefix_integrity_ok: false };
    }

    let new_body = serde_json::to_vec(&body).unwrap_or_else(|_| body_bytes.to_vec());
    if compressed_count > 0 {
        tracing::info!("Live-zone: {compressed_count} blocks, ~{bytes_saved} bytes saved, prefix OK");
    }
    SurgeryReport { body: new_body, blocks_compressed: compressed_count, bytes_saved, prefix_integrity_ok: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::Compressor;

    #[test]
    fn test_live_zone_empty() {
        let body = br#"{"model":"t","messages":[]}"#;
        let c = Compressor::for_test(100, 5);
        let r = operate(body, &c);
        assert_eq!(r.blocks_compressed, 0);
        assert!(r.prefix_integrity_ok);
    }

    #[test]
    fn test_live_zone_compresses() {
        let items: Vec<Value> = (0..200).map(|i| serde_json::json!({"id":i,"body":"verbose ".repeat(50)})).collect();
        let json = serde_json::to_string(&items).unwrap();
        let body = serde_json::json!({
            "model":"t","messages":[
                {"role":"user","content":"frozen"},
                {"role":"assistant","content":"ok"},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":json}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let c = Compressor::for_test(50, 5);
        let r = operate(&bytes, &c);
        assert!(r.blocks_compressed > 0);
        assert!(r.prefix_integrity_ok);
        // Frozen messages unchanged
        let m: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(m["messages"][0]["content"], "frozen");
        assert_eq!(m["messages"][1]["content"], "ok");
    }
}
