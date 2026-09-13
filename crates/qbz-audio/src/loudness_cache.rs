//! Loudness cache — persists EBU R128 measurements in SQLite.
//!
//! Follows the `AudioSettingsStore` pattern: database lives in
//! `dirs::data_dir()/qbz/loudness_cache.db`.
//!
//! Thread-safe via `Mutex<Connection>`.

use rusqlite::{params, Connection};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct CachedLoudness {
    pub gain_db: f32,
    pub peak: f32,
    /// Source of the measurement: "ebur128" or "replaygain"
    pub source: String,
}

pub struct LoudnessCache {
    /// `None` = disabled: every lookup misses, every store is dropped.
    conn: Option<Mutex<Connection>>,
}

impl LoudnessCache {
    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "Could not determine data directory".to_string())?
            .join("qbz");

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;

        let db_path = data_dir.join("loudness_cache.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open loudness cache database: {}", e))?;
        let cache = Self::with_connection(conn)?;
        log::info!("[LoudnessCache] Opened at {}", db_path.display());
        Ok(cache)
    }

    /// Session-only cache (SQLite in memory): the fallback when the on-disk
    /// database cannot be opened. Analyses are reused within the run and lost
    /// at exit.
    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("Failed to open in-memory loudness cache: {}", e))?;
        Self::with_connection(conn)
    }

    /// No cache at all: `get*` always miss and `set*` are no-ops. Last resort.
    pub fn disabled() -> Self {
        Self { conn: None }
    }

    fn with_connection(conn: Connection) -> Result<Self, String> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to enable WAL for loudness cache database: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS track_loudness (
                track_id INTEGER PRIMARY KEY,
                gain_db REAL NOT NULL,
                peak REAL NOT NULL DEFAULT 0.0,
                source TEXT NOT NULL DEFAULT 'ebur128',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
        )
        .map_err(|e| format!("Failed to create loudness table: {}", e))?;

        // Identity beyond the Qobuz id (2026-08-28): an ISRC or a content
        // fingerprint, so a local/Plex copy of the same recording can reuse
        // the analysis. TABLE ONLY — the analyser and every audio path are
        // untouched; `set`/`get` keep their track_id contract. Additive,
        // idempotent (the duplicate-column error is the "already there" case).
        let _ = conn.execute_batch("ALTER TABLE track_loudness ADD COLUMN content_key TEXT;");
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_track_loudness_content_key
               ON track_loudness(content_key) WHERE content_key IS NOT NULL;",
        );

        Ok(Self {
            conn: Some(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, Connection>> {
        self.conn.as_ref()?.lock().ok()
    }

    /// Look up cached loudness for a track.
    pub fn get(&self, track_id: u64) -> Option<CachedLoudness> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT gain_db, peak, source FROM track_loudness WHERE track_id = ?1",
            params![track_id as i64],
            |row| {
                Ok(CachedLoudness {
                    gain_db: row.get::<_, f64>(0)? as f32,
                    peak: row.get::<_, f64>(1)? as f32,
                    source: row.get(2)?,
                })
            },
        )
        .ok()
    }

    /// Attach a cross-source content key (ISRC or fingerprint) to an
    /// existing row. No-op when the track has no analysis yet.
    pub fn set_content_key(&self, track_id: u64, content_key: &str) {
        if content_key.trim().is_empty() {
            return;
        }
        if let Some(conn) = self.lock() {
            let _ = conn.execute(
                "UPDATE track_loudness SET content_key = ?1 WHERE track_id = ?2",
                params![content_key.trim(), track_id as i64],
            );
        }
    }

    /// Look up cached loudness by content key (any source's copy of the
    /// same recording). Newest analysis wins.
    pub fn get_by_content_key(&self, content_key: &str) -> Option<CachedLoudness> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT gain_db, peak, source FROM track_loudness
              WHERE content_key = ?1 ORDER BY created_at DESC LIMIT 1",
            params![content_key.trim()],
            |row| {
                Ok(CachedLoudness {
                    gain_db: row.get::<_, f64>(0)? as f32,
                    peak: row.get::<_, f64>(1)? as f32,
                    source: row.get(2)?,
                })
            },
        )
        .ok()
    }

    /// Store or update loudness data for a track.
    pub fn set(&self, track_id: u64, gain_db: f32, peak: f32, source: &str) {
        if let Some(conn) = self.lock() {
            let result = conn.execute(
                "INSERT OR REPLACE INTO track_loudness (track_id, gain_db, peak, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, strftime('%s', 'now'))",
                params![track_id as i64, gain_db as f64, peak as f64, source],
            );
            if let Err(e) = result {
                log::warn!(
                    "[LoudnessCache] Failed to store loudness for track {}: {}",
                    track_id,
                    e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_cache_round_trips_by_track_and_content_key() {
        let cache = LoudnessCache::in_memory().unwrap();
        assert!(cache.get(7).is_none());
        cache.set(7, -3.5, 0.9, "ebur128");
        let hit = cache.get(7).unwrap();
        assert_eq!((hit.gain_db, hit.peak, hit.source.as_str()), (-3.5, 0.9, "ebur128"));
        cache.set_content_key(7, "USRC17607839");
        assert_eq!(cache.get_by_content_key("USRC17607839").unwrap().gain_db, -3.5);
    }

    #[test]
    fn disabled_cache_misses_and_swallows_writes() {
        let cache = LoudnessCache::disabled();
        cache.set(7, -3.5, 0.9, "ebur128");
        cache.set_content_key(7, "USRC17607839");
        assert!(cache.get(7).is_none());
        assert!(cache.get_by_content_key("USRC17607839").is_none());
    }
}
