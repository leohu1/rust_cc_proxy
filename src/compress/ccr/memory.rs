//! In-memory CCR backend backed by an `LruCache`.
//!
//! Fast but volatile — all entries are lost on restart.
//! Uses LRU eviction: when at capacity, the least-recently-used entry
//! is evicted first. Entries also track their creation time so the
//! `CcrStore` can report age statistics.
//!
//! Best for development, testing, and single-instance deployments.

use std::time::{SystemTime, UNIX_EPOCH};

use lru::LruCache;
use std::sync::Mutex;

use super::CcrBackend;

pub struct InMemoryBackend {
    entries: Mutex<LruCache<String, CacheEntry>>,
    max_entries: usize,
}

struct CacheEntry {
    data: Vec<u8>,
    #[allow(dead_code)]
    created_at: u64,
}

impl InMemoryBackend {
    pub fn new(max_entries: usize) -> Self {
        InMemoryBackend {
            entries: Mutex::new(LruCache::unbounded()),
            max_entries,
        }
    }
}

impl CcrBackend for InMemoryBackend {
    fn put(&self, hash: &str, payload: &[u8]) {
        let mut entries = self.entries.lock().unwrap();

        // If at capacity and this is a new key, evict LRU entry
        if entries.len() >= self.max_entries && !entries.contains(hash) {
            entries.pop_lru();
        }

        let now = epoch_secs();
        entries.put(
            hash.to_string(),
            CacheEntry {
                data: payload.to_vec(),
                created_at: now,
            },
        );
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let mut entries = self.entries.lock().unwrap();
        // `.get()` updates LRU order
        entries.get(hash).map(|e| e.data.clone())
    }

    fn contains(&self, hash: &str) -> bool {
        self.entries.lock().unwrap().contains(hash)
    }

    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_put_and_get() {
        let backend = InMemoryBackend::new(100);
        backend.put("abc", b"hello world");
        assert_eq!(backend.get("abc").unwrap(), b"hello world");
    }

    #[test]
    fn test_lru_eviction() {
        let backend = InMemoryBackend::new(3);
        backend.put("a", b"1");
        backend.put("b", b"2");
        backend.put("c", b"3");
        backend.put("d", b"4"); // evicts "a" (oldest)
        assert!(backend.len() <= 3);
        assert!(backend.get("a").is_none(), "LRU should evict 'a' first");
        assert_eq!(backend.get("b").unwrap(), b"2");
    }

    #[test]
    fn test_lru_access_updates_order() {
        let backend = InMemoryBackend::new(3);
        backend.put("a", b"1");
        backend.put("b", b"2");
        backend.put("c", b"3");
        // Access "a" — makes it recently used
        let _ = backend.get("a");
        backend.put("d", b"4"); // evicts "b" (now oldest)
        assert!(backend.get("a").is_some(), "'a' was accessed and should survive");
        assert!(backend.get("b").is_none(), "'b' should be evicted");
    }

    #[test]
    fn test_contains() {
        let backend = InMemoryBackend::new(10);
        backend.put("key1", b"val");
        assert!(backend.contains("key1"));
        assert!(!backend.contains("key2"));
    }

    #[test]
    fn test_is_empty() {
        let backend = InMemoryBackend::new(10);
        assert!(backend.is_empty());
        backend.put("k", b"v");
        assert!(!backend.is_empty());
    }

    #[test]
    fn test_replace_existing_no_eviction() {
        let backend = InMemoryBackend::new(2);
        backend.put("a", b"1");
        backend.put("b", b"2");
        backend.put("a", b"updated"); // update, no eviction
        assert_eq!(backend.len(), 2);
        assert_eq!(backend.get("a").unwrap(), b"updated");
    }
}
