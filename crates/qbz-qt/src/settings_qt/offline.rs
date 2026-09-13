//! Settings > Offline — the offline-MODE rows and the lyrics-cache row.
//!
//! ADR-006: the mode itself is the shared `qbz_app::offline_mode` engine
//! (`crate::offline_fwd::engine()`), including the #279 stream-first snapshot
//! it takes when induced offline is entered; the lyrics stats/clear go through
//! `qbz_lyrics::LyricsCacheDb` opened at the SAME per-user path
//! `lyrics_qt.rs` binds the service to.
//!
//! The offline-cache group is live too (manager, open folder and clear-all),
//! but its controller lives in `offline_manager_qt`; this module only owns the
//! mode and lyrics rows serialized into the Settings document.

use serde::Serialize;

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    /// The persisted induced-offline flag (the "Enable Offline Mode" toggle).
    #[serde(rename = "modeEnabled")]
    pub mode_enabled: bool,
    /// Tauri-era manual-offline network submission preference.
    #[serde(rename = "allowImmediateScrobbling")]
    pub allow_immediate_scrobbling: bool,
    /// Tauri-era physical-offline retention preference.
    #[serde(rename = "allowAccumulatedScrobbling")]
    pub allow_accumulated_scrobbling: bool,
    /// False when there is no per-user lyrics cache yet — the stats line hides.
    #[serde(rename = "lyricsLoaded")]
    pub lyrics_loaded: bool,
    #[serde(rename = "lyricsEntries")]
    pub lyrics_entries: i64,
    #[serde(rename = "lyricsSize")]
    pub lyrics_size: String,
    /// Shared artwork cache (`~/.cache/qbz/images`): stats line + budget.
    #[serde(rename = "imageCacheLoaded")]
    pub image_cache_loaded: bool,
    #[serde(rename = "imageCacheFiles")]
    pub image_cache_files: i64,
    #[serde(rename = "imageCacheSize")]
    pub image_cache_size: String,
    #[serde(rename = "imageCacheSizes")]
    pub image_cache_sizes: Vec<String>,
    #[serde(rename = "imageCacheSizeIndex")]
    pub image_cache_size_index: i32,
}

/// Budget choices for the artwork cache, in MB (index 1 = the default).
pub(crate) const IMAGE_CACHE_MB: &[u64] = &[100, 200, 500, 1000];
const IMAGE_CACHE_LABELS: &[&str] = &["100 MB", "200 MB", "500 MB", "1 GB"];

fn lyrics_db_path() -> Option<std::path::PathBuf> {
    let uid = qbz_app::user_data::UserDataPaths::load_last_user_id()?;
    Some(
        dirs::cache_dir()?
            .join("qbz")
            .join("users")
            .join(uid.to_string())
            .join("lyrics")
            .join("lyrics.db"),
    )
}

/// `offline_manager::human_size` equivalent (B / KB / MB / GB, one decimal).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn snapshot() -> Snapshot {
    let engine = crate::offline_fwd::engine();
    let settings = engine.settings().unwrap_or_default();

    let (lyrics_loaded, lyrics_entries, lyrics_size) = match lyrics_db_path() {
        Some(path) if path.exists() => match qbz_lyrics::LyricsCacheDb::new(&path) {
            Ok(db) => match db.stats() {
                Ok(stats) => (true, stats.entries as i64, human_size(stats.size_bytes)),
                Err(e) => {
                    log::warn!("[qbz-qt] lyrics cache stats failed: {e}");
                    (false, 0, String::new())
                }
            },
            Err(e) => {
                log::warn!("[qbz-qt] lyrics cache open failed: {e}");
                (false, 0, String::new())
            }
        },
        _ => (false, 0, String::new()),
    };

    let (image_cache_loaded, image_cache_files, image_cache_size) =
        match crate::artwork_qt::shared_stats() {
            Some((files, bytes)) => (true, files as i64, human_size(bytes)),
            None => (false, 0, String::new()),
        };
    let budget_mb = crate::artwork_qt::shared_budget_bytes() / (1024 * 1024);
    let image_cache_size_index = IMAGE_CACHE_MB
        .iter()
        .position(|mb| *mb == budget_mb)
        .unwrap_or(1) as i32;

    Snapshot {
        mode_enabled: settings.manual_offline_mode,
        allow_immediate_scrobbling: settings.allow_immediate_scrobbling,
        allow_accumulated_scrobbling: settings.allow_accumulated_scrobbling,
        lyrics_loaded,
        lyrics_entries,
        lyrics_size,
        image_cache_loaded,
        image_cache_files,
        image_cache_size,
        image_cache_sizes: IMAGE_CACHE_LABELS.iter().map(|l| l.to_string()).collect(),
        image_cache_size_index,
    }
}

/// Settings > Offline > "Clear" on the artwork cache.
pub async fn clear_image_cache() {
    let _ = tokio::task::spawn_blocking(|| match crate::artwork_qt::clear_shared() {
        Ok(freed) => log::info!("[qbz-qt] image cache cleared ({freed} B)"),
        Err(e) => log::error!("[qbz-qt] image cache clear failed: {e}"),
    })
    .await;
}

pub fn set_allow_immediate_scrobbling(value: bool) -> Result<(), String> {
    let result = crate::offline_fwd::engine().set_allow_immediate_scrobbling(value);
    if result.is_ok() {
        crate::integrations_qt::scrobble_policy_changed();
    }
    result
}

pub fn set_allow_accumulated_scrobbling(value: bool) -> Result<(), String> {
    let result = crate::offline_fwd::engine().set_allow_accumulated_scrobbling(value);
    if result.is_ok() {
        crate::integrations_qt::scrobble_policy_changed();
    }
    result
}

/// "Enable Offline Mode" — the persisted induced flag. `set_induced` also
/// runs the #279 stream-first snapshot/restore against the audio store, so
/// the store handle is passed in (best-effort, exactly like the Slint).
pub fn set_mode_enabled(value: bool) -> Result<(), String> {
    let engine = crate::offline_fwd::engine();
    super::with_audio(|store| {
        engine
            .set_induced(value, Some(store))
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .or_else(|_| {
        // No audio store open (pre-login): flip the flag anyway.
        crate::offline_fwd::engine()
            .set_induced(value, None)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// "Clear lyrics cache".
pub async fn clear_lyrics_cache() {
    let _ = tokio::task::spawn_blocking(|| {
        let Some(path) = lyrics_db_path() else {
            return;
        };
        if !path.exists() {
            return;
        }
        match qbz_lyrics::LyricsCacheDb::new(&path).and_then(|db| db.clear()) {
            Ok(()) => log::info!("[qbz-qt] lyrics cache cleared"),
            Err(e) => log::error!("[qbz-qt] lyrics cache clear failed: {e}"),
        }
    })
    .await;
}
