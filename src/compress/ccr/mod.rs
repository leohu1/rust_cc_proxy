//! Compress-Cache-Retrieve (CCR) — reversible compression storage.
//!
//! Large content is hashed (BLAKE3, 24-char hex), the original bytes are
//! stored, and a `<<ccr:HASH>>` marker is embedded in the compressed
//! output. The LLM can later retrieve the full content via a tool call or
//! this proxy can expand markers when proxying `headroom_retrieve` calls.
//!
//! ## Backends
//!
//! - **InMemory** (default) — fast, no persistence, lost on restart. LRU eviction.
//! - **SQLite** — persistent across restarts, WAL mode, TTL-based expiry with
//!   background periodic purge.

mod memory;
mod sqlite;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use memory::InMemoryBackend;
pub use sqlite::SqliteBackend;

use crate::error::AppError;

// ── Backend trait ──────────────────────────────────────────────────

/// Pluggable CCR storage backend. Thread-safe (Send + Sync) so it can
/// live behind an `Arc`.
pub trait CcrBackend: Send + Sync {
    /// Store a payload under the given hash key.
    fn put(&self, hash: &str, payload: &[u8]);
    /// Retrieve a payload by hash. Returns `None` if not found or expired.
    fn get(&self, hash: &str) -> Option<Vec<u8>>;
    /// Check whether a hash exists in the store (and hasn't expired).
    fn contains(&self, hash: &str) -> bool;
    /// Number of entries currently stored.
    fn len(&self) -> usize;
    /// Whether the store is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Purge expired entries. Default no-op; SQLite backend overrides this.
    fn purge_expired(&self) -> usize {
        0
    }
}

// ── CcrStore wrapper ───────────────────────────────────────────────

/// High-level CCR store that wraps any [`CcrBackend`] and adds
/// automatic hashing, UTF-8 conversion, and usage statistics.
pub struct CcrStore {
    backend: Box<dyn CcrBackend>,
    total_stored: Mutex<usize>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
    purged: Arc<AtomicU64>,
    /// If `Some`, a background purge task handle.
    #[allow(dead_code)]
    purge_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CcrStore {
    /// Create a store backed by the default in-memory backend.
    pub fn new(capacity: usize) -> Self {
        CcrStore {
            backend: Box::new(InMemoryBackend::new(capacity)),
            total_stored: Mutex::new(0),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
            purged: Arc::new(AtomicU64::new(0)),
            purge_task: Mutex::new(None),
        }
    }

    /// Create a store backed by SQLite at `path`.
    /// Entries expire after `ttl_seconds` (0 = never expire).
    /// Spawns a background tokio task that purges expired entries
    /// every `purge_interval_secs`.
    pub fn with_sqlite(
        path: &str,
        ttl_seconds: u64,
        purge_interval_secs: u64,
    ) -> Result<Self, AppError> {
        let backend = SqliteBackend::open(path, ttl_seconds).map_err(|e| {
            AppError::ConfigError(format!("failed to open CCR SQLite store at {path}: {e}"))
        })?;
        tracing::info!(
            "CCR SQLite backend opened: {path} (TTL={ttl_seconds}s, purge_interval={purge_interval_secs}s)"
        );

        let purged = Arc::new(AtomicU64::new(0));

        // Background purge task — only if interval > 0 and TTL > 0
        if purge_interval_secs > 0 && ttl_seconds > 0 {
            let backend = SqliteBackend::open(path, ttl_seconds).map_err(|e| {
                AppError::ConfigError(format!("failed to open CCR SQLite store for purge task: {e}"))
            })?;
            let purged_clone = purged.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(tokio::time::Duration::from_secs(purge_interval_secs));
                interval.tick().await; // skip first immediate tick
                loop {
                    interval.tick().await;
                    let count = backend.purge_expired();
                    if count > 0 {
                        purged_clone.fetch_add(count as u64, Ordering::Relaxed);
                    }
                }
            });
        }

        let store = CcrStore {
            backend: Box::new(backend),
            total_stored: Mutex::new(0),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
            purged,
            purge_task: Mutex::new(None),
        };

        Ok(store)
    }

    /// Store content and return its CCR hash (24-char hex).
    pub fn store(&self, content: &str) -> String {
        let hash = compute_key(content.as_bytes());
        self.backend.put(&hash, content.as_bytes());
        *self.total_stored.lock().unwrap() += 1;
        hash
    }

    /// Retrieve content by hash. Returns `None` if not found.
    pub fn get(&self, hash: &str) -> Option<String> {
        match self.backend.get(hash) {
            Some(bytes) => {
                *self.hits.lock().unwrap() += 1;
                String::from_utf8(bytes).ok()
            }
            None => {
                *self.misses.lock().unwrap() += 1;
                None
            }
        }
    }

    /// Check if a hash exists in the store.
    pub fn contains(&self, hash: &str) -> bool {
        self.backend.contains(hash)
    }

    /// Return current statistics.
    pub fn stats(&self) -> CcrStats {
        CcrStats {
            entries: self.backend.len(),
            max_entries: 0, // not tracked at this level for SQLite
            total_stored: *self.total_stored.lock().unwrap(),
            hits: *self.hits.lock().unwrap(),
            misses: *self.misses.lock().unwrap(),
            purged: self.purged.load(Ordering::Relaxed),
        }
    }

    /// Manually trigger a purge (for testing or admin use).
    pub fn purge_expired(&self) -> usize {
        let count = self.backend.purge_expired();
        if count > 0 {
            self.purged.fetch_add(count as u64, Ordering::Relaxed);
        }
        count
    }
}

// ── Canonical functions ────────────────────────────────────────────

/// Compute the canonical CCR key from raw bytes.
/// Uses BLAKE3, returns the first 24 hex characters (96 bits).
pub fn compute_key(payload: &[u8]) -> String {
    let hash = blake3::hash(payload);
    hash.to_hex()[..24].to_string()
}

/// Standard `<<ccr:HASH>>` marker injected into compressed output.
pub fn marker_for(hash: &str) -> String {
    format!("<<ccr:{hash}>>")
}

// ── Stats ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct CcrStats {
    pub entries: usize,
    pub max_entries: usize,
    pub total_stored: usize,
    pub hits: u64,
    pub misses: u64,
    pub purged: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_key_deterministic() {
        let k1 = compute_key(b"hello");
        let k2 = compute_key(b"hello");
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 24);
    }

    #[test]
    fn test_compute_key_different() {
        let k1 = compute_key(b"hello");
        let k2 = compute_key(b"world");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_marker_format() {
        let m = marker_for("abc123");
        assert_eq!(m, "<<ccr:abc123>>");
    }

    #[test]
    fn test_store_and_retrieve_memory() {
        let store = CcrStore::new(100);
        let hash = store.store("hello world");
        assert_eq!(hash.len(), 24);

        let retrieved = store.get(&hash).unwrap();
        assert_eq!(retrieved, "hello world");
    }

    #[test]
    fn test_missing_key_returns_none() {
        let store = CcrStore::new(100);
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn test_stats_tracking() {
        let store = CcrStore::new(100);
        store.store("a");
        store.store("b");
        let hash = store.store("c");

        store.get(&hash).unwrap(); // hit
        store.get("nope"); // miss

        let stats = store.stats();
        assert_eq!(stats.total_stored, 3);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_sqlite_purge_expired() {
        let backend = SqliteBackend::open(":memory:", 0).unwrap(); // TTL=0 → immediate expiry
        backend.put("a", b"1");
        backend.put("b", b"2");
        assert_eq!(backend.purge_expired(), 2);
        assert_eq!(backend.len(), 0);
    }
}
