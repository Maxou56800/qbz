//! Image cache service
//!
//! LRU disk cache for Qobuz album/artist images.
//! - Stores images keyed by MD5 hash of URL
//! - Tracks last-access time for LRU eviction
//! - Respects a configurable max size ([`ImageCacheService::evict`], batched)
//! - Framework-agnostic: the Qt shell owns the policy (`artwork_qt.rs`), this
//!   crate owns the store (`~/.cache/qbz/images`).

use md5::{Digest, Md5};
use rusqlite::{params, Connection};
use std::path::PathBuf;

/// Cache statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImageCacheStats {
    pub total_bytes: u64,
    pub file_count: u64,
}

/// Outcome of one [`ImageCacheService::evict`] batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictBatch {
    pub freed_bytes: u64,
    pub evicted_entries: usize,
}

pub struct ImageCacheService {
    cache_dir: PathBuf,
    conn: Connection,
}

impl ImageCacheService {
    pub fn new() -> Result<Self, String> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| "Could not find cache directory".to_string())?
            .join("qbz")
            .join("images");
        Self::open_at(cache_dir)
    }

    /// Open (or create) the cache at an explicit directory. `new` is this on
    /// `~/.cache/qbz/images`; tests use a temp dir.
    pub fn open_at(cache_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed to create image cache dir: {}", e))?;

        let db_path = cache_dir.join("image_cache.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open image cache database: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to enable WAL: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cached_images (
                hash TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                file_size INTEGER NOT NULL DEFAULT 0,
                last_accessed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_last_accessed ON cached_images (last_accessed);",
        )
        .map_err(|e| format!("Failed to create image cache table: {}", e))?;

        Ok(Self { cache_dir, conn })
    }

    /// Test hook: rewrite an entry's `last_accessed` stamp.
    #[cfg(test)]
    fn backdate(&self, url: &str, last_accessed: i64) {
        let hash = Self::url_hash(url);
        self.conn
            .execute(
                "UPDATE cached_images SET last_accessed = ?1 WHERE hash = ?2",
                params![last_accessed, hash],
            )
            .expect("backdate");
    }

    fn url_hash(url: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(url.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn cache_path(&self, hash: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.img", hash))
    }

    /// Get a cached image path, updating last-access time.
    /// Returns None if not cached.
    pub fn get(&self, url: &str) -> Option<PathBuf> {
        let hash = Self::url_hash(url);
        let path = self.cache_path(&hash);

        if !path.exists() {
            // File missing — clean up stale DB entry
            let _ = self
                .conn
                .execute("DELETE FROM cached_images WHERE hash = ?1", params![hash]);
            return None;
        }

        // Update last-accessed time
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let _ = self.conn.execute(
            "UPDATE cached_images SET last_accessed = ?1 WHERE hash = ?2",
            params![now, hash],
        );

        Some(path)
    }

    /// Store image bytes in the cache.
    /// Returns the local file path on success.
    pub fn store(&self, url: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let hash = Self::url_hash(url);
        let path = self.cache_path(&hash);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        std::fs::write(&path, bytes).map_err(|e| format!("Failed to write cached image: {}", e))?;

        let file_size = bytes.len() as i64;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO cached_images (hash, url, file_size, last_accessed)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash, url, file_size, now],
            )
            .map_err(|e| format!("Failed to insert image cache entry: {}", e))?;

        Ok(path)
    }

    /// Evict least-recently-accessed entries until the total size is under
    /// `max_bytes`, deleting at most `max_entries` rows per call.
    ///
    /// Batched on purpose: the Qt shell holds a process-wide mutex around
    /// this service and its GUI thread takes that mutex to resolve covers,
    /// so a caller trims in small batches and releases the lock in between.
    /// Loop until `evicted_entries == 0`.
    pub fn evict(&self, max_bytes: u64, max_entries: usize) -> Result<EvictBatch, String> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(file_size), 0) FROM cached_images",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to query cache size: {}", e))?;

        let mut batch = EvictBatch {
            freed_bytes: 0,
            evicted_entries: 0,
        };
        if (total as u64) <= max_bytes {
            return Ok(batch);
        }
        let mut to_free = (total as u64) - max_bytes;

        // LRU entries (oldest access first), bounded to this batch.
        let mut stmt = self
            .conn
            .prepare("SELECT hash, file_size FROM cached_images ORDER BY last_accessed ASC LIMIT ?1")
            .map_err(|e| format!("Failed to prepare eviction query: {}", e))?;
        let limit = i64::try_from(max_entries).unwrap_or(i64::MAX);
        let entries: Vec<(String, i64)> = stmt
            .query_map(params![limit], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Failed to query LRU entries: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        for (hash, file_size) in entries {
            if to_free == 0 {
                break;
            }
            let path = self.cache_path(&hash);
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            let _ = self
                .conn
                .execute("DELETE FROM cached_images WHERE hash = ?1", params![hash]);
            let size = file_size as u64;
            batch.freed_bytes += size;
            batch.evicted_entries += 1;
            to_free = to_free.saturating_sub(size);
        }

        Ok(batch)
    }

    /// Get cache statistics.
    pub fn stats(&self) -> Result<ImageCacheStats, String> {
        let (total_bytes, file_count): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(file_size), 0), COUNT(*) FROM cached_images",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Failed to query image cache stats: {}", e))?;

        Ok(ImageCacheStats {
            total_bytes: total_bytes as u64,
            file_count: file_count as u64,
        })
    }

    /// Clear the entire cache.
    pub fn clear(&self) -> Result<u64, String> {
        let stats = self.stats()?;

        // Delete all files
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "img").unwrap_or(false) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }

        // Clear database
        self.conn
            .execute("DELETE FROM cached_images", [])
            .map_err(|e| format!("Failed to clear image cache table: {}", e))?;

        Ok(stats.total_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qbz-image-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn evict_drops_least_recently_accessed_first_and_unlinks_files() {
        let dir = fresh_dir("lru");
        let cache = ImageCacheService::open_at(dir.clone()).unwrap();
        let oldest = cache.store("https://img/oldest", &[0u8; 100]).unwrap();
        let middle = cache.store("https://img/middle", &[0u8; 100]).unwrap();
        let newest = cache.store("https://img/newest", &[0u8; 100]).unwrap();
        cache.backdate("https://img/oldest", 1_000);
        cache.backdate("https://img/middle", 2_000);
        cache.backdate("https://img/newest", 3_000);

        let batch = cache.evict(150, usize::MAX).unwrap();

        assert_eq!(batch, EvictBatch { freed_bytes: 200, evicted_entries: 2 });
        assert!(!oldest.exists());
        assert!(!middle.exists());
        assert!(newest.exists());
        let stats = cache.stats().unwrap();
        assert_eq!((stats.total_bytes, stats.file_count), (100, 1));
        assert!(cache.get("https://img/oldest").is_none());
        assert!(cache.get("https://img/newest").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_is_a_no_op_at_or_under_budget() {
        let dir = fresh_dir("noop");
        let cache = ImageCacheService::open_at(dir.clone()).unwrap();
        let kept = cache.store("https://img/a", &[0u8; 100]).unwrap();
        assert_eq!(cache.evict(100, usize::MAX).unwrap(), EvictBatch { freed_bytes: 0, evicted_entries: 0 });
        assert_eq!(cache.evict(u64::MAX, usize::MAX).unwrap().evicted_entries, 0);
        assert!(kept.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_honours_the_batch_size_so_a_caller_can_release_its_lock_between_batches() {
        let dir = fresh_dir("batch");
        let cache = ImageCacheService::open_at(dir.clone()).unwrap();
        for i in 0..3 {
            cache.store(&format!("https://img/{i}"), &[0u8; 100]).unwrap();
            cache.backdate(&format!("https://img/{i}"), 1_000 + i);
        }
        assert_eq!(cache.evict(0, 1).unwrap(), EvictBatch { freed_bytes: 100, evicted_entries: 1 });
        assert_eq!(cache.evict(0, 1).unwrap(), EvictBatch { freed_bytes: 100, evicted_entries: 1 });
        assert_eq!(cache.evict(0, 1).unwrap(), EvictBatch { freed_bytes: 100, evicted_entries: 1 });
        assert_eq!(cache.evict(0, 1).unwrap(), EvictBatch { freed_bytes: 0, evicted_entries: 0 });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_refreshes_last_accessed_so_a_touched_entry_survives_eviction() {
        let dir = fresh_dir("touch");
        let cache = ImageCacheService::open_at(dir.clone()).unwrap();
        let touched = cache.store("https://img/touched", &[0u8; 100]).unwrap();
        let untouched = cache.store("https://img/untouched", &[0u8; 100]).unwrap();
        cache.backdate("https://img/touched", 1_000);
        cache.backdate("https://img/untouched", 2_000);
        // A read is an access: it moves `touched` to the newest slot.
        assert!(cache.get("https://img/touched").is_some());
        assert_eq!(cache.evict(100, usize::MAX).unwrap().freed_bytes, 100);
        assert!(touched.exists());
        assert!(!untouched.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
