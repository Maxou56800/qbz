//! Loudness cache — persists loudness measurements in SQLite.
//!
//! Follows the `AudioSettingsStore` pattern: database lives in
//! `dirs::data_dir()/qbz/loudness_cache.db`.
//!
//! A row holds the track's ABSOLUTE integrated loudness (LUFS), its peak and
//! a ranked source, so the start gain for ANY target is derived at read time
//! (`resolve_gain`) — changing the target never invalidates a row. Rows
//! written before the `lufs` column existed hold a target-relative `gain_db`
//! instead; they keep resolving from it until a fresh measurement lands.
//!
//! Thread-safe via `Mutex<Connection>`.

use rusqlite::{params, Connection};
use std::sync::Mutex;

use crate::loudness::db_to_linear;

/// Where a row came from, best last. `store` never lets a lower rank
/// overwrite a higher one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoudnessSource {
    /// The live analyser's window (≥ 30 s of the track, not all of it).
    Ebur128Partial = 1,
    /// ReplayGain: the catalog's own value or a file tag — a full-track figure.
    ReplayGain = 2,
    /// QBZ's own full-track EBU R128 measurement (prefetch pre-analysis or a
    /// track played to its end).
    Ebur128Full = 3,
}

impl LoudnessSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LoudnessSource::Ebur128Partial => "ebur128",
            LoudnessSource::ReplayGain => "replaygain",
            LoudnessSource::Ebur128Full => "ebur128-full",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ebur128-full" => LoudnessSource::Ebur128Full,
            "replaygain" => LoudnessSource::ReplayGain,
            _ => LoudnessSource::Ebur128Partial,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredLoudness {
    /// Absolute integrated loudness. `None` on rows written before this
    /// column existed (they hold a target-relative `gain_db` instead).
    pub lufs: Option<f32>,
    pub legacy_gain_db: f32,
    /// Linear peak (true peak when measured here, sample peak from ReplayGain).
    pub peak: Option<f32>,
    pub source: LoudnessSource,
}

/// ReplayGain 2.0 reference: a track gain of `g` dB means the track sits at
/// `-18 - g` LUFS.
pub const REPLAYGAIN_REFERENCE_LUFS: f32 = -18.0;
/// Maximum boost. Attenuation is never capped.
pub const MAX_BOOST_DB: f32 = 6.0;
/// Headroom kept under full scale when a peak is known and clipping is guarded.
const PEAK_HEADROOM_DB: f32 = -1.0;

pub fn lufs_from_replaygain(gain_db: f32) -> f32 {
    REPLAYGAIN_REFERENCE_LUFS - gain_db
}

/// Linear gain that moves a track at `lufs` to `target_lufs`: boost capped at
/// [`MAX_BOOST_DB`], and with `prevent_clipping` never above what keeps the
/// known peak [`PEAK_HEADROOM_DB`] under full scale.
pub fn gain_for(lufs: f32, peak: Option<f32>, target_lufs: f32, prevent_clipping: bool) -> f32 {
    let db = (target_lufs - lufs).min(MAX_BOOST_DB);
    let mut gain = db_to_linear(db);
    if prevent_clipping {
        if let Some(p) = peak.filter(|p| *p > 0.0) {
            let ceiling = db_to_linear(PEAK_HEADROOM_DB) / p;
            if gain > ceiling {
                gain = ceiling;
            }
        }
    }
    gain
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

    /// No cache at all: lookups always miss and stores are no-ops. Last resort.
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
        // the analysis. TABLE ONLY, no reader yet. Additive, idempotent (the
        // duplicate-column error is the "already there" case).
        let _ = conn.execute_batch("ALTER TABLE track_loudness ADD COLUMN content_key TEXT;");
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_track_loudness_content_key
               ON track_loudness(content_key) WHERE content_key IS NOT NULL;",
        );
        // Absolute loudness (2026-09): rows used to hold a target-relative
        // gain, which a target change silently invalidated. Additive.
        let _ = conn.execute_batch("ALTER TABLE track_loudness ADD COLUMN lufs REAL;");

        Ok(Self {
            conn: Some(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, Connection>> {
        self.conn.as_ref()?.lock().ok()
    }

    /// Rank-guarded write: a lower-ranked source never overwrites a higher
    /// one; the same rank refreshes the row. Returns whether it wrote.
    pub fn store(&self, track_id: u64, lufs: f32, peak: Option<f32>, source: LoudnessSource) -> bool {
        let Some(conn) = self.lock() else {
            return false;
        };
        let existing: Option<String> = conn
            .query_row(
                "SELECT source FROM track_loudness WHERE track_id = ?1",
                params![track_id as i64],
                |row| row.get(0),
            )
            .ok();
        if let Some(existing) = existing {
            if LoudnessSource::parse(&existing) > source {
                return false;
            }
        }
        let result = conn.execute(
            "INSERT INTO track_loudness (track_id, gain_db, peak, source, lufs, created_at)
             VALUES (?1, 0.0, ?2, ?3, ?4, strftime('%s', 'now'))
             ON CONFLICT(track_id) DO UPDATE SET
                gain_db = 0.0, peak = excluded.peak, source = excluded.source,
                lufs = excluded.lufs, created_at = excluded.created_at",
            params![
                track_id as i64,
                peak.map(|p| p as f64).unwrap_or(0.0),
                source.as_str(),
                lufs as f64
            ],
        );
        match result {
            Ok(_) => true,
            Err(e) => {
                log::warn!("[LoudnessCache] Failed to store loudness for track {track_id}: {e}");
                false
            }
        }
    }

    pub fn lookup(&self, track_id: u64) -> Option<StoredLoudness> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT gain_db, peak, source, lufs FROM track_loudness WHERE track_id = ?1",
            params![track_id as i64],
            |row| {
                Ok(StoredLoudness {
                    legacy_gain_db: row.get::<_, f64>(0)? as f32,
                    peak: row
                        .get::<_, Option<f64>>(1)?
                        .map(|p| p as f32)
                        .filter(|p| *p > 0.0),
                    source: LoudnessSource::parse(&row.get::<_, String>(2)?),
                    lufs: row.get::<_, Option<f64>>(3)?.map(|l| l as f32),
                })
            },
        )
        .ok()
    }

    /// The linear start gain for `target_lufs`, or `None` for an unknown track.
    pub fn resolve_gain(
        &self,
        track_id: u64,
        target_lufs: f32,
        prevent_clipping: bool,
    ) -> Option<(f32, LoudnessSource)> {
        let row = self.lookup(track_id)?;
        let gain = match row.lufs {
            Some(lufs) => gain_for(lufs, row.peak, target_lufs, prevent_clipping),
            // Pre-column row: a target-relative adjustment measured against
            // whatever the target was then. Honoured as-is until refreshed.
            None => db_to_linear(row.legacy_gain_db.min(MAX_BOOST_DB)),
        };
        Some((gain, row.source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_cache_stores_absolute_loudness_and_resolves_gain_for_any_target() {
        let cache = LoudnessCache::in_memory().unwrap();
        assert!(cache.lookup(7).is_none());
        assert!(cache.store(7, -12.0, Some(0.9), LoudnessSource::Ebur128Partial));
        let row = cache.lookup(7).unwrap();
        assert_eq!(
            (row.lufs, row.peak, row.source),
            (Some(-12.0), Some(0.9), LoudnessSource::Ebur128Partial)
        );
        let (g, _) = cache.resolve_gain(7, -14.0, true).unwrap();
        assert!((g - db_to_linear(-2.0)).abs() < 1e-4, "-14 target on a -12 track is -2 dB, got {g}");
        // Changing the target re-derives the gain from the same row.
        let (g, _) = cache.resolve_gain(7, -18.0, true).unwrap();
        assert!((g - db_to_linear(-6.0)).abs() < 1e-4);
    }

    #[test]
    fn store_never_downgrades_a_better_source() {
        let cache = LoudnessCache::in_memory().unwrap();
        assert!(cache.store(7, -12.0, None, LoudnessSource::ReplayGain));
        assert!(
            !cache.store(7, -9.0, None, LoudnessSource::Ebur128Partial),
            "partial must not overwrite replaygain"
        );
        assert_eq!(cache.lookup(7).unwrap().lufs, Some(-12.0));
        assert!(cache.store(7, -11.0, Some(0.98), LoudnessSource::Ebur128Full));
        assert_eq!(cache.lookup(7).unwrap().source, LoudnessSource::Ebur128Full);
        // Same rank refreshes.
        assert!(cache.store(7, -11.5, Some(0.97), LoudnessSource::Ebur128Full));
        assert_eq!(cache.lookup(7).unwrap().lufs, Some(-11.5));
    }

    #[test]
    fn gain_math_caps_the_boost_and_respects_the_peak_ceiling() {
        assert!(
            (gain_for(-30.0, None, -14.0, true) - db_to_linear(6.0)).abs() < 1e-4,
            "quiet track, no peak: +6 dB cap"
        );
        let ceiling = db_to_linear(-1.0) / 0.9;
        assert!((gain_for(-20.0, Some(0.9), -14.0, true) - ceiling).abs() < 1e-4, "-1 dBTP over the peak");
        assert!(
            (gain_for(-20.0, Some(0.9), -14.0, false) - db_to_linear(6.0)).abs() < 1e-4,
            "no clipping guard: full boost"
        );
        assert!(
            (gain_for(-5.0, Some(1.0), -14.0, true) - db_to_linear(-9.0)).abs() < 1e-4,
            "attenuation is never capped"
        );
        assert!((lufs_from_replaygain(-4.0) - (-14.0)).abs() < 1e-6);
    }

    #[test]
    fn legacy_rows_without_lufs_still_resolve_from_their_stored_gain() {
        let cache = LoudnessCache::in_memory().unwrap();
        cache
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO track_loudness (track_id, gain_db, peak, source) VALUES (9, -3.0, 0.0, 'ebur128')",
                [],
            )
            .unwrap();
        let (g, src) = cache.resolve_gain(9, -14.0, true).unwrap();
        assert!((g - db_to_linear(-3.0)).abs() < 1e-4);
        assert_eq!(src, LoudnessSource::Ebur128Partial);
    }

    #[test]
    fn disabled_cache_misses_and_swallows_writes() {
        let cache = LoudnessCache::disabled();
        assert!(!cache.store(7, -12.0, None, LoudnessSource::ReplayGain));
        assert!(cache.lookup(7).is_none());
        assert!(cache.resolve_gain(7, -14.0, true).is_none());
    }
}
