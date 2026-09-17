//! SQLite database for offline cache index

use rusqlite::{params, Connection};
use std::path::Path;

use crate::types::{
    CachedTrackInfo, OfflineCacheStats, OfflineCacheStatus, ReadyTrackForSync, TrackCacheInfo,
};

/// Maps a `cached_tracks` row (with the canonical 17-column SELECT used by
/// `get_track`, `get_all_tracks`, and `get_album_tracks`) into a `CachedTrackInfo`.
///
/// SELECT must be:
/// `track_id, title, artist, album, album_id, duration_secs, file_size_bytes,
///  quality, bit_depth, sample_rate, status, progress_percent, error_message,
///  created_at, last_accessed_at, artwork_path, file_path`
fn row_to_cached_track_info(row: &rusqlite::Row) -> rusqlite::Result<CachedTrackInfo> {
    Ok(CachedTrackInfo {
        track_id: row.get::<_, i64>(0)? as u64,
        title: row.get(1)?,
        artist: row.get(2)?,
        album: row.get(3)?,
        album_id: row.get(4)?,
        duration_secs: row.get::<_, i64>(5)? as u64,
        file_size_bytes: row.get::<_, i64>(6)? as u64,
        quality: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        bit_depth: row.get::<_, Option<i64>>(8)?.map(|v| v as u32),
        sample_rate: row.get(9)?,
        status: OfflineCacheStatus::from_str(&row.get::<_, String>(10)?),
        progress_percent: row.get::<_, i64>(11)? as u8,
        error_message: row.get(12)?,
        created_at: row.get(13)?,
        last_accessed_at: row.get(14)?,
        artwork_path: row.get::<_, Option<String>>(15)?,
        file_path: row.get(16)?,
    })
}

/// Database wrapper for cached tracks index
pub struct OfflineCacheDb {
    conn: Connection,
}

impl OfflineCacheDb {
    /// Open or create the database
    pub fn new(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path)
            .map_err(|e| format!("Failed to open offline cache database: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to enable WAL for offline cache database: {}", e))?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Get reference to the connection (for direct queries)
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Initialize database schema
    fn init_schema(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS cached_tracks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                track_id INTEGER UNIQUE NOT NULL,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                album TEXT,
                album_id TEXT,
                duration_secs INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                file_size_bytes INTEGER NOT NULL DEFAULT 0,
                format TEXT NOT NULL DEFAULT 'flac',
                quality TEXT,
                bit_depth INTEGER,
                sample_rate REAL,
                artwork_path TEXT,
                status TEXT NOT NULL DEFAULT 'queued',
                progress_percent INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_accessed_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_track_id ON cached_tracks(track_id);
            CREATE INDEX IF NOT EXISTS idx_status ON cached_tracks(status);
            CREATE INDEX IF NOT EXISTS idx_last_accessed ON cached_tracks(last_accessed_at);
            ",
            )
            .map_err(|e| format!("Failed to initialize database schema: {}", e))?;

        self.migrate_v2_cmaf_columns()?;

        Ok(())
    }

    /// Additive migration for the v2 offline format.
    ///
    /// Adds columns for bit-identical CMAF storage:
    /// - `cache_format`: 1 = legacy plain FLAC, 2 = raw CMAF bundle
    /// - `init_path`: path to the init.mp4 (contains FLAC header + table)
    /// - `content_key_wrapped`: AES content key wrapped with qbz-secrets
    /// - `infos_wrapped`: session infos salt wrapped with qbz-secrets
    /// - `format_id`: Qobuz format id (e.g. 5/6/7/27)
    /// - `n_segments`: number of audio segments (s=1..=n)
    ///
    /// Existing rows keep `cache_format=1` so playback continues to read
    /// the legacy plain-FLAC `file_path` for them. New downloads go to
    /// `cache_format=2`. We never rewrite v1 rows into v2 — the two
    /// formats coexist until the v1 rows naturally expire via the
    /// subscription-lapse cache wipe or user-triggered re-download.
    fn migrate_v2_cmaf_columns(&self) -> Result<(), String> {
        let existing = self.existing_columns("cached_tracks")?;
        let add = |col: &str, ddl: &str| -> Result<(), String> {
            if !existing.iter().any(|c| c == col) {
                let sql = format!("ALTER TABLE cached_tracks ADD COLUMN {}", ddl);
                self.conn
                    .execute(&sql, [])
                    .map_err(|e| format!("Failed to add column {}: {}", col, e))?;
                log::info!(
                    "[OfflineCache/MIGRATE] Added column {} to cached_tracks",
                    col
                );
            }
            Ok(())
        };
        add("cache_format", "cache_format INTEGER NOT NULL DEFAULT 1")?;
        add("init_path", "init_path TEXT")?;
        add("content_key_wrapped", "content_key_wrapped BLOB")?;
        add("infos_wrapped", "infos_wrapped BLOB")?;
        add("format_id", "format_id INTEGER")?;
        add("n_segments", "n_segments INTEGER")?;
        Ok(())
    }

    fn existing_columns(&self, table: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .map_err(|e| format!("Failed to prepare PRAGMA: {}", e))?;
        let cols = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("Failed to read PRAGMA: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to iterate PRAGMA: {}", e))?;
        Ok(cols)
    }

    /// Insert a new track to cache for offline
    pub fn insert_track(&self, info: &TrackCacheInfo, file_path: &str) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO cached_tracks
             (track_id, title, artist, album, album_id, duration_secs, file_path, quality, bit_depth, sample_rate, status, progress_percent, created_at, last_accessed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', 0, datetime('now'), datetime('now'))",
            params![
                info.track_id as i64,
                info.title,
                info.artist,
                info.album,
                info.album_id,
                info.duration_secs as i64,
                file_path,
                info.quality,
                info.bit_depth.map(|v| v as i64),
                info.sample_rate,
            ],
        ).map_err(|e| format!("Failed to insert track: {}", e))?;

        Ok(())
    }

    /// Insert multiple tracks in a single transaction (batch queuing)
    pub fn insert_tracks_batch(&self, tracks: &[(&TrackCacheInfo, String)]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;

        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO cached_tracks
                 (track_id, title, artist, album, album_id, duration_secs, file_path, quality, bit_depth, sample_rate, status, progress_percent, created_at, last_accessed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', 0, datetime('now'), datetime('now'))"
            ).map_err(|e| format!("Failed to prepare batch insert: {}", e))?;

            for (info, file_path) in tracks {
                stmt.execute(params![
                    info.track_id as i64,
                    info.title,
                    info.artist,
                    info.album,
                    info.album_id,
                    info.duration_secs as i64,
                    file_path,
                    info.quality,
                    info.bit_depth.map(|v| v as i64),
                    info.sample_rate,
                ])
                .map_err(|e| format!("Failed to insert track {}: {}", info.track_id, e))?;
            }
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit batch insert: {}", e))?;

        Ok(())
    }

    /// Update track status
    pub fn update_status(
        &self,
        track_id: u64,
        status: OfflineCacheStatus,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE cached_tracks SET status = ?1, error_message = ?2 WHERE track_id = ?3",
                params![status.as_str(), error, track_id as i64],
            )
            .map_err(|e| format!("Failed to update status: {}", e))?;

        Ok(())
    }

    /// Update caching progress
    pub fn update_progress(
        &self,
        track_id: u64,
        progress: u8,
        size_bytes: u64,
    ) -> Result<(), String> {
        self.conn.execute(
            "UPDATE cached_tracks SET progress_percent = ?1, file_size_bytes = ?2 WHERE track_id = ?3",
            params![progress as i64, size_bytes as i64, track_id as i64],
        ).map_err(|e| format!("Failed to update progress: {}", e))?;

        Ok(())
    }

    /// Mark caching as complete
    pub fn mark_complete(&self, track_id: u64, file_size: u64) -> Result<(), String> {
        self.conn.execute(
            "UPDATE cached_tracks SET status = 'ready', progress_percent = 100, file_size_bytes = ?1, last_accessed_at = datetime('now') WHERE track_id = ?2",
            params![file_size as i64, track_id as i64],
        ).map_err(|e| format!("Failed to mark complete: {}", e))?;

        Ok(())
    }

    /// Update last accessed time (for LRU)
    pub fn touch(&self, track_id: u64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE cached_tracks SET last_accessed_at = datetime('now') WHERE track_id = ?1",
                params![track_id as i64],
            )
            .map_err(|e| format!("Failed to update access time: {}", e))?;

        Ok(())
    }

    /// Check if a track is cached and ready
    pub fn is_cached(&self, track_id: u64) -> Result<bool, String> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_tracks WHERE track_id = ?1 AND status = 'ready'",
                params![track_id as i64],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check cache: {}", e))?;

        Ok(count > 0)
    }

    /// Get file path for a cached track
    pub fn get_file_path(&self, track_id: u64) -> Result<Option<String>, String> {
        let result = self.conn.query_row(
            "SELECT file_path FROM cached_tracks WHERE track_id = ?1 AND status = 'ready'",
            params![track_id as i64],
            |row| row.get(0),
        );

        match result {
            Ok(path) => Ok(Some(path)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get file path: {}", e)),
        }
    }

    /// Get all ready (cached) tracks with their file paths for syncing to library
    pub fn get_ready_tracks_for_sync(&self) -> Result<Vec<ReadyTrackForSync>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT track_id, title, artist, album, duration_secs, file_path, bit_depth, sample_rate
             FROM cached_tracks WHERE status = 'ready'"
        ).map_err(|e| format!("Failed to prepare query: {}", e))?;

        let tracks = stmt
            .query_map([], |row| {
                Ok(ReadyTrackForSync {
                    track_id: row.get::<_, i64>(0)? as u64,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    album: row.get(3)?,
                    duration_secs: row.get::<_, i64>(4)? as u64,
                    file_path: row.get(5)?,
                    bit_depth: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    sample_rate: row.get(7)?,
                })
            })
            .map_err(|e| format!("Failed to query tracks: {}", e))?;

        let mut result = Vec::new();
        for track in tracks {
            result.push(track.map_err(|e| format!("Failed to read track: {}", e))?);
        }

        Ok(result)
    }

    /// Get track info
    pub fn get_track(&self, track_id: u64) -> Result<Option<CachedTrackInfo>, String> {
        let result = self.conn.query_row(
            "SELECT track_id, title, artist, album, album_id, duration_secs, file_size_bytes, quality, bit_depth, sample_rate, status, progress_percent, error_message, created_at, last_accessed_at, artwork_path, file_path
             FROM cached_tracks WHERE track_id = ?1",
            params![track_id as i64],
            row_to_cached_track_info,
        );

        match result {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get track: {}", e)),
        }
    }

    /// Get all cached tracks
    pub fn get_all_tracks(&self) -> Result<Vec<CachedTrackInfo>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT track_id, title, artist, album, album_id, duration_secs, file_size_bytes, quality, bit_depth, sample_rate, status, progress_percent, error_message, created_at, last_accessed_at, artwork_path, file_path
             FROM cached_tracks ORDER BY last_accessed_at DESC"
        ).map_err(|e| format!("Failed to prepare query: {}", e))?;

        let tracks = stmt
            .query_map([], row_to_cached_track_info)
            .map_err(|e| format!("Failed to query tracks: {}", e))?;

        let mut result = Vec::new();
        for track in tracks {
            result.push(track.map_err(|e| format!("Failed to read track: {}", e))?);
        }

        Ok(result)
    }

    /// Returns all cached track rows for a given album_id (any status).
    pub fn get_album_tracks(&self, album_id: &str) -> Result<Vec<CachedTrackInfo>, String> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT track_id, title, artist, album, album_id, duration_secs,
                        file_size_bytes, quality, bit_depth, sample_rate, status,
                        progress_percent, error_message, created_at, last_accessed_at,
                        artwork_path, file_path
                 FROM cached_tracks WHERE album_id = ?1",
            )
            .map_err(|e| format!("Prepare failed: {}", e))?;
        let rows = stmt
            .query_map([album_id], row_to_cached_track_info)
            .map_err(|e| format!("Query failed: {}", e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Row decode failed: {}", e))
    }

    /// Resets a track row to Pending state for re-download.
    /// Clears progress_percent and error_message.
    pub fn reset_track_for_redownload(&self, track_id: u64) -> Result<(), String> {
        self.conn()
            .execute(
                "UPDATE cached_tracks
                 SET status = 'queued', progress_percent = 0, error_message = NULL
                 WHERE track_id = ?1",
                [track_id as i64],
            )
            .map_err(|e| format!("Update failed: {}", e))?;
        Ok(())
    }

    /// Delete a track from cache
    pub fn delete_track(&self, track_id: u64) -> Result<Option<String>, String> {
        // Get file path before deleting
        let file_path: Option<String> = self
            .conn
            .query_row(
                "SELECT file_path FROM cached_tracks WHERE track_id = ?1",
                params![track_id as i64],
                |row| row.get(0),
            )
            .ok();

        self.conn
            .execute(
                "DELETE FROM cached_tracks WHERE track_id = ?1",
                params![track_id as i64],
            )
            .map_err(|e| format!("Failed to delete track: {}", e))?;

        Ok(file_path)
    }

    /// Get statistics
    pub fn get_stats(
        &self,
        cache_path: &str,
        limit_bytes: Option<u64>,
    ) -> Result<OfflineCacheStats, String> {
        let total_tracks: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cached_tracks", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count tracks: {}", e))?;

        let ready_tracks: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_tracks WHERE status = 'ready'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count ready tracks: {}", e))?;

        let downloading_tracks: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM cached_tracks WHERE status = 'downloading' OR status = 'queued'",
            [],
            |row| row.get(0),
        ).map_err(|e| format!("Failed to count downloading tracks: {}", e))?;

        let failed_tracks: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cached_tracks WHERE status = 'failed'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count failed tracks: {}", e))?;

        let total_size: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(file_size_bytes), 0) FROM cached_tracks WHERE status = 'ready'",
            [],
            |row| row.get(0),
        ).map_err(|e| format!("Failed to sum sizes: {}", e))?;

        Ok(OfflineCacheStats {
            total_tracks: total_tracks as usize,
            ready_tracks: ready_tracks as usize,
            downloading_tracks: downloading_tracks as usize,
            failed_tracks: failed_tracks as usize,
            total_size_bytes: total_size as u64,
            limit_bytes,
            cache_path: cache_path.to_string(),
        })
    }

    /// Clear all entries
    pub fn clear_all(&self) -> Result<Vec<String>, String> {
        // Get all file paths first
        let mut stmt = self
            .conn
            .prepare("SELECT file_path FROM cached_tracks")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let paths: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query paths: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        self.conn
            .execute("DELETE FROM cached_tracks", [])
            .map_err(|e| format!("Failed to clear database: {}", e))?;

        Ok(paths)
    }

    /// Update file path for a track (after organizing)
    pub fn update_file_path(&self, track_id: u64, new_path: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE cached_tracks SET file_path = ?1 WHERE track_id = ?2",
                params![new_path, track_id as i64],
            )
            .map_err(|e| format!("Failed to update file path: {}", e))?;
        Ok(())
    }

    /// Update artwork path for a track
    pub fn update_artwork_path(&self, track_id: u64, artwork_path: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE cached_tracks SET artwork_path = ?1 WHERE track_id = ?2",
                params![artwork_path, track_id as i64],
            )
            .map_err(|e| format!("Failed to update artwork path: {}", e))?;
        Ok(())
    }

    // ==================== v2 CMAF bundle fields ====================

    /// Persist the CMAF-specific columns for a track after it was
    /// successfully downloaded as a raw encrypted bundle.
    ///
    /// `file_path` here is the concatenated-segments file (or primary
    /// segment file, depending on how the caller lays out the bundle on
    /// disk). `init_path` is the init.mp4. Both keys are already wrapped
    /// by the caller via `qbz-secrets::SecretBox::wrap`.
    #[allow(clippy::too_many_arguments)]
    pub fn set_cmaf_bundle(
        &self,
        track_id: u64,
        segments_path: &str,
        init_path: &str,
        content_key_wrapped: &[u8],
        infos_wrapped: &[u8],
        format_id: u32,
        n_segments: u32,
        total_bytes: u64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE cached_tracks
                    SET cache_format = 2,
                        file_path = ?1,
                        init_path = ?2,
                        content_key_wrapped = ?3,
                        infos_wrapped = ?4,
                        format_id = ?5,
                        n_segments = ?6,
                        file_size_bytes = ?7
                    WHERE track_id = ?8",
                params![
                    segments_path,
                    init_path,
                    content_key_wrapped,
                    infos_wrapped,
                    format_id as i64,
                    n_segments as i64,
                    total_bytes as i64,
                    track_id as i64,
                ],
            )
            .map_err(|e| format!("Failed to write CMAF bundle fields: {}", e))?;
        Ok(())
    }

    /// Read back the bundle fields for a track, for offline playback.
    pub fn get_cmaf_bundle(&self, track_id: u64) -> Result<Option<CmafBundleRow>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT cache_format, file_path, init_path, content_key_wrapped,
                        infos_wrapped, format_id, n_segments
                   FROM cached_tracks
                  WHERE track_id = ?1",
            )
            .map_err(|e| format!("Failed to prepare CMAF bundle select: {}", e))?;
        let row: Result<CmafBundleRow, _> = stmt.query_row(params![track_id as i64], |row| {
            Ok(CmafBundleRow {
                cache_format: row.get::<_, i64>(0)? as u8,
                segments_path: row.get(1)?,
                init_path: row.get::<_, Option<String>>(2)?,
                content_key_wrapped: row.get::<_, Option<Vec<u8>>>(3)?,
                infos_wrapped: row.get::<_, Option<Vec<u8>>>(4)?,
                format_id: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                n_segments: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
            })
        });
        match row {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to read CMAF bundle: {}", e)),
        }
    }

    /// Deletes all rows for the given album_id in a single transaction.
    /// Returns (deleted track_ids, total file_size_bytes freed).
    pub fn delete_album_tracks(&self, album_id: &str) -> Result<(Vec<u64>, u64), String> {
        let tx = self
            .conn()
            .unchecked_transaction()
            .map_err(|e| format!("Failed to begin tx: {}", e))?;

        let ids: Vec<u64> = {
            let mut stmt = tx
                .prepare("SELECT track_id FROM cached_tracks WHERE album_id = ?1")
                .map_err(|e| format!("Prepare failed: {}", e))?;
            let rows = stmt
                .query_map([album_id], |row| row.get::<_, i64>(0).map(|v| v as u64))
                .map_err(|e| format!("Query failed: {}", e))?;
            rows.collect::<rusqlite::Result<Vec<u64>>>()
                .map_err(|e| format!("Row decode failed: {}", e))?
        };

        let bytes: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(file_size_bytes), 0) FROM cached_tracks WHERE album_id = ?1",
                [album_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Sum failed: {}", e))?;

        tx.execute("DELETE FROM cached_tracks WHERE album_id = ?1", [album_id])
            .map_err(|e| format!("Delete failed: {}", e))?;

        tx.commit()
            .map_err(|e| format!("Commit failed: {}", e))?;

        Ok((ids, bytes as u64))
    }
}

/// Raw snapshot of the v2 bundle columns for a cached track.
///
/// `cache_format` tells the caller how to interpret the rest:
/// - `1` — legacy plain FLAC at `segments_path`; other fields empty.
/// - `2` — raw CMAF bundle; all fields populated.
#[derive(Debug, Clone)]
pub struct CmafBundleRow {
    pub cache_format: u8,
    pub segments_path: String,
    pub init_path: Option<String>,
    pub content_key_wrapped: Option<Vec<u8>>,
    pub infos_wrapped: Option<Vec<u8>>,
    pub format_id: Option<u32>,
    pub n_segments: Option<u32>,
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_db() -> (TempDir, OfflineCacheDb) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("idx.db");
        let db = OfflineCacheDb::new(&path).unwrap();
        (tmp, db)
    }

    fn sample_track(id: u64, album_id: Option<&str>) -> TrackCacheInfo {
        TrackCacheInfo {
            track_id: id,
            title: format!("t{}", id),
            artist: "A".into(),
            album: Some("Alb".into()),
            album_id: album_id.map(String::from),
            duration_secs: 100,
            quality: "lossless".into(),
            bit_depth: Some(16),
            sample_rate: Some(44100.0),
        }
    }

    #[test]
    fn delete_album_tracks_returns_deleted_ids_and_freed_bytes() {
        let (_tmp, db) = fresh_db();
        db.insert_track(&sample_track(1, Some("alb1")), "/p/1").unwrap();
        db.insert_track(&sample_track(2, Some("alb1")), "/p/2").unwrap();
        db.insert_track(&sample_track(3, Some("alb2")), "/p/3").unwrap();
        db.mark_complete(1, 1000).unwrap();
        db.mark_complete(2, 2000).unwrap();
        db.mark_complete(3, 9999).unwrap();

        let (ids, bytes) = db.delete_album_tracks("alb1").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&1) && ids.contains(&2));
        assert_eq!(bytes, 3000);

        // alb2 untouched
        let remaining = db.get_all_tracks().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].track_id, 3);
    }

    #[test]
    fn get_album_tracks_returns_only_matching_album() {
        let (_tmp, db) = fresh_db();
        db.insert_track(&sample_track(1, Some("alb1")), "/p/1").unwrap();
        db.insert_track(&sample_track(2, Some("alb2")), "/p/2").unwrap();

        let alb1 = db.get_album_tracks("alb1").unwrap();
        assert_eq!(alb1.len(), 1);
        assert_eq!(alb1[0].track_id, 1);
    }

    #[test]
    fn reset_track_for_redownload_clears_progress_and_error() {
        let (_tmp, db) = fresh_db();
        db.insert_track(&sample_track(1, Some("alb1")), "/p/1").unwrap();
        db.update_status(1, OfflineCacheStatus::Failed, Some("boom")).unwrap();

        db.reset_track_for_redownload(1).unwrap();

        let track = db.get_track(1).unwrap().unwrap();
        assert!(matches!(track.status, OfflineCacheStatus::Queued));
        assert_eq!(track.progress_percent, 0);
        assert!(track.error_message.is_none());
    }
}
