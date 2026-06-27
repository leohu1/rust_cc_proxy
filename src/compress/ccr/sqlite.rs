//! SQLite CCR backend — persistent, WAL-mode, TTL-based expiry.
//!
//! Survives proxy restarts. Suitable for production single-instance
//! deployments. For multi-worker setups, consider a shared Redis backend
//! (future).
//!
//! ## Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS ccr_entries (
//!     hash         TEXT PRIMARY KEY,
//!     original     BLOB NOT NULL,
//!     created_at   INTEGER NOT NULL,
//!     ttl_seconds  INTEGER NOT NULL
//! );
//! ```

use super::CcrBackend;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct SqliteBackend {
    conn: Mutex<Connection>,
    default_ttl_seconds: u64,
    #[allow(dead_code)]
    path: PathBuf,
}

impl SqliteBackend {
    /// Open or create the SQLite database at `path`.
    ///
    /// Enables WAL mode for concurrent reads and sets `synchronous=NORMAL`
    /// for a good balance of safety and performance.
    pub fn open(path: impl AsRef<Path>, default_ttl_seconds: u64) -> rusqlite::Result<Self> {
        let conn = Connection::open(path.as_ref())?;

        // Performance pragmas
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;

        // Schema
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ccr_entries (
                hash         TEXT PRIMARY KEY,
                original     BLOB NOT NULL,
                created_at   INTEGER NOT NULL,
                ttl_seconds  INTEGER NOT NULL
            )",
            [],
        )?;

        // Clean up expired entries on startup
        let now = epoch_secs();
        conn.execute(
            "DELETE FROM ccr_entries WHERE created_at + ttl_seconds <= ?1",
            params![now as i64],
        )?;

        tracing::info!(
            "CCR SQLite: {} (WAL mode, TTL={}s)",
            path.as_ref().display(),
            default_ttl_seconds
        );

        Ok(SqliteBackend {
            conn: Mutex::new(conn),
            default_ttl_seconds,
            path: path.as_ref().to_path_buf(),
        })
    }

    /// Purge all expired entries. Returns the number of rows deleted.
    pub fn purge_expired(&self) -> usize {
        let now = epoch_secs() as i64;
        let conn = self.conn.lock().unwrap();
        match conn.execute(
            "DELETE FROM ccr_entries WHERE created_at + ttl_seconds <= ?1",
            params![now],
        ) {
            Ok(n) => {
                if n > 0 {
                    tracing::debug!("CCR SQLite: purged {n} expired entries");
                }
                n
            }
            Err(e) => {
                tracing::warn!("CCR SQLite purge failed: {e}");
                0
            }
        }
    }
}

impl CcrBackend for SqliteBackend {
    fn put(&self, hash: &str, payload: &[u8]) {
        let now = epoch_secs() as i64;
        let conn = self.conn.lock().unwrap();
        if let Err(e) = conn.execute(
            "INSERT INTO ccr_entries (hash, original, created_at, ttl_seconds)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(hash) DO UPDATE SET
                 original    = excluded.original,
                 created_at  = excluded.created_at,
                 ttl_seconds = excluded.ttl_seconds",
            params![hash, payload, now, self.default_ttl_seconds as i64],
        ) {
            tracing::warn!("CCR SQLite put({hash}) failed: {e}");
        }
    }

    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let conn = self.conn.lock().unwrap();
        let now = epoch_secs() as i64;

        // Lazy-purge this one expired entry
        let _ = conn.execute(
            "DELETE FROM ccr_entries WHERE hash = ?1 AND created_at + ttl_seconds <= ?2",
            params![hash, now],
        );

        match conn.query_row(
            "SELECT original FROM ccr_entries WHERE hash = ?1 AND created_at + ttl_seconds > ?2",
            params![hash, now],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(bytes) => Some(bytes),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                tracing::warn!("CCR SQLite get({hash}) failed: {e}");
                None
            }
        }
    }

    fn contains(&self, hash: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let now = epoch_secs() as i64;
        matches!(
            conn.query_row(
                "SELECT 1 FROM ccr_entries WHERE hash = ?1 AND created_at + ttl_seconds > ?2",
                params![hash, now],
                |_| Ok(()),
            ),
            Ok(())
        )
    }

    fn len(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        let now = epoch_secs() as i64;
        // Count non-expired
        conn.query_row(
            "SELECT COUNT(*) FROM ccr_entries WHERE created_at + ttl_seconds > ?1",
            params![now],
            |row| row.get::<_, usize>(0),
        )
        .unwrap_or(0)
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
    fn test_sqlite_store_and_retrieve() {
        let backend = SqliteBackend::open(":memory:", 3600).unwrap();
        backend.put("abc123", b"hello sqlite");
        assert_eq!(backend.get("abc123").unwrap(), b"hello sqlite");
        assert!(backend.contains("abc123"));
        assert_eq!(backend.len(), 1);
    }

    #[test]
    fn test_sqlite_missing_key() {
        let backend = SqliteBackend::open(":memory:", 3600).unwrap();
        assert!(backend.get("nope").is_none());
        assert!(!backend.contains("nope"));
    }

    #[test]
    fn test_sqlite_upsert() {
        let backend = SqliteBackend::open(":memory:", 3600).unwrap();
        backend.put("key", b"v1");
        backend.put("key", b"v2"); // upsert
        assert_eq!(backend.get("key").unwrap(), b"v2");
        assert_eq!(backend.len(), 1); // still one row
    }

    #[test]
    fn test_sqlite_ttl_expiry() {
        let backend = SqliteBackend::open(":memory:", 0).unwrap(); // TTL=0 → expire immediately
        backend.put("ephemeral", b"data");
        // With TTL=0, created_at + 0 <= now, so it should be expired
        assert!(backend.get("ephemeral").is_none());
    }
}
