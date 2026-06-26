//! Compress-Cache-Retrieve (CCR) — reversible compression storage.
//!
//! Large content is hashed (BLAKE3, 24-char hex), the original bytes are
//! stored locally, and a `<<ccr:HASH>>` marker is embedded in the compressed
//! output. The LLM can later retrieve the full content via a tool call or
//! this proxy can expand markers when proxying `headroom_retrieve` calls.

use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory CCR store with a capacity limit.
///
/// Thread-safe via `Mutex`. For persistent storage (SQLite, Redis),
/// replace with an alternative backend implementing the same interface.
pub struct CcrStore {
    entries: Mutex<HashMap<String, Vec<u8>>>,
    max_entries: usize,
    total_stored: Mutex<usize>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl CcrStore {
    /// Create a new store with the given maximum number of entries.
    pub fn new(max_entries: usize) -> Self {
        CcrStore {
            entries: Mutex::new(HashMap::new()),
            max_entries,
            total_stored: Mutex::new(0),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }

    /// Store content and return its CCR hash (24-char hex).
    pub fn store(&self, content: &str) -> String {
        let hash = ccr_hash(content);
        let bytes = content.as_bytes().to_vec();

        let mut entries = self.entries.lock().unwrap();
        // Evict oldest entries if at capacity
        if entries.len() >= self.max_entries {
            // Simple eviction: remove first key we find
            if let Some(key) = entries.keys().next().cloned() {
                entries.remove(&key);
            }
        }
        entries.insert(hash.clone(), bytes);
        *self.total_stored.lock().unwrap() += 1;
        hash
    }

    /// Retrieve content by hash. Returns `None` if not found.
    pub fn get(&self, hash: &str) -> Option<String> {
        let entries = self.entries.lock().unwrap();
        match entries.get(hash) {
            Some(bytes) => {
                *self.hits.lock().unwrap() += 1;
                String::from_utf8(bytes.clone()).ok()
            }
            None => {
                *self.misses.lock().unwrap() += 1;
                None
            }
        }
    }

    /// Check if a hash exists in the store.
    pub fn contains(&self, hash: &str) -> bool {
        self.entries.lock().unwrap().contains_key(hash)
    }

    /// Return current statistics.
    pub fn stats(&self) -> CcrStats {
        CcrStats {
            entries: self.entries.lock().unwrap().len(),
            max_entries: self.max_entries,
            total_stored: *self.total_stored.lock().unwrap(),
            hits: *self.hits.lock().unwrap(),
            misses: *self.misses.lock().unwrap(),
        }
    }
}

/// Compute a BLAKE3 hash, return the first 24 hex characters (96 bits).
pub fn ccr_hash(content: &str) -> String {
    let hash = blake3::hash(content.as_bytes());
    hash.to_hex()[..24].to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CcrStats {
    pub entries: usize,
    pub max_entries: usize,
    pub total_stored: usize,
    pub hits: u64,
    pub misses: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_retrieve() {
        let store = CcrStore::new(100);
        let hash = store.store("hello world");
        assert_eq!(hash.len(), 24);

        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, "hello world");
    }

    #[test]
    fn test_different_content_different_hash() {
        let store = CcrStore::new(100);
        let h1 = store.store("content-a");
        let h2 = store.store("content-b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_missing_key_returns_none() {
        let store = CcrStore::new(100);
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn test_eviction_at_capacity() {
        let store = CcrStore::new(3);
        store.store("a");
        store.store("b");
        store.store("c");
        // This should evict one
        store.store("d");

        // h3 may or may not still be there depending on eviction order
        let stats = store.stats();
        assert!(stats.entries <= 3);
    }
}
