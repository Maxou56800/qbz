//! Shared offline-cache → bytes resolver (frontend-agnostic).
//!
//! This is the OFFLINE tier of the playback tier-walk. Both frontends slot
//! it in BEFORE the network tier in their play path; `qbz-player` (which
//! already owns L1/L2 + the network/CMAF pipeline) stays untouched.
//!
//! - Tauri's `v2_play_track` / `v2_play_next_gapless` resolve offline bytes
//!   through this helper (keeping their hardware-compatibility / quality
//!   gating around the returned bytes).
//! - Slint's play path calls it before `Player::play_track`.

use std::path::Path;

use qbz_offline_cache::{CacheEventSink, OfflineCacheState};

/// Resolve a track's offline-cached bytes by its **Qobuz track id**.
///
/// - `cache_format = 2` (CMAF) → read the bundle, unwrap the content key via
///   the secret vault, and decrypt to a complete FLAC (off the async thread).
///   When `sink` is `Some`, `UnlockStart`/`UnlockEnd` are emitted around the
///   decrypt so a frontend can show an "unlocking" hint on the row.
/// - `cache_format = 1` (legacy plain FLAC) → read the file directly.
/// - no ready row / any failure → `None` (the caller treats it as a cache
///   miss and continues to the next tier / the network).
///
/// Offline copies are always downloaded at the top quality tier, so there is
/// no quality gate here; a frontend that must honor a hardware limit (e.g.
/// the Tauri ALSA path) applies its own check to the returned bytes.
pub async fn resolve_offline_bytes(
    track_id: u64,
    offline: &OfflineCacheState,
    sink: Option<&CacheEventSink>,
) -> Option<Vec<u8>> {
    let row = {
        let guard = offline.db.lock().await;
        let db = guard.as_ref()?;
        let row = db.get_cmaf_bundle(track_id).ok().flatten()?;
        // A play IS an access: keep `last_accessed_at` truthful (before this
        // only the download wrote it). Nothing evicts on it today —
        // `maintenance::check_cache_limit` only REFUSES new downloads at the
        // limit, and downloaded albums are user data kept until removed in
        // the offline manager — but it is the stamp any future space-reclaim
        // policy must order by. This is the one place every offline play
        // funnels through (both frontends and qbzd resolve here).
        if let Err(e) = db.touch(track_id) {
            log::debug!("[OfflineResolve] touch({track_id}) failed: {e}");
        }
        row
    };

    match row.cache_format {
        2 => {
            let cache_path = offline.get_cache_path();
            match sink {
                Some(sink) => {
                    qbz_offline_cache::load_cmaf_bundle_with_ui_events(
                        sink, track_id, track_id, row, cache_path,
                    )
                    .await
                }
                None => tokio::task::spawn_blocking(move || {
                    qbz_offline_cache::load_cmaf_bundle(track_id, &row, Path::new(&cache_path))
                })
                .await
                .ok()
                .flatten(),
            }
        }
        _ => {
            let path = Path::new(&row.segments_path);
            if path.exists() {
                std::fs::read(path).ok()
            } else {
                log::warn!(
                    "[OfflineResolve] track {} v1 row points to a missing file {:?}",
                    track_id,
                    path
                );
                None
            }
        }
    }
}
