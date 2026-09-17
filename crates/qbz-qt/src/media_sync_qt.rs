//! The library sweep: a media server's catalog into the shared cache.
//!
//! Lives here rather than in `qbz-source` because it needs the tokio runtime
//! and a channel to the progress UI, and that crate deliberately has neither
//! (design 02 §8). It writes through the cache handle the SOURCE owns, so the
//! sweep and every read agree about which user's mirror they are touching.
//!
//! # The two protocols cost very different amounts, and the code says so
//!
//! Measured on 2026-08-20:
//!
//! | | rows | wall | per track |
//! |---|---|---|---|
//! | Jellyfin | 4924 | **45.8 s** | 9.3 ms |
//! | Subsonic | 6678 | **0.81 s** | 0.12 ms |
//!
//! Jellyfin's cost is server-side media-info hydration, demanded by
//! `Fields=MediaSources` — the only way to get `BitDepth` / `SampleRate`.
//! `Fields=MediaStreams` trims 29 % of the bytes and saves nothing. Subsonic
//! ships the same facts as ordinary OpenSubsonic song fields, for free.
//!
//! Jellyfin therefore publishes a cheap essential-metadata pass first, without
//! `MediaSources`, and hydrates quality afterwards in bounded batches. Visible,
//! queued, playing, and recent rows get priority over the catalog-wide fill.
//! Subsonic ships complete rows in its ordinary paged sweep.
//!
//! # Two rules the prune depends on
//!
//! 1. **A generation prunes only after a sweep that COMPLETED.** It deletes
//!    rows the sweep did not observe, which is how a track deleted on the
//!    server disappears here. A connection dropped halfway would otherwise
//!    read as "the server deleted everything the sweep never got to".
//! 2. **A delta sweep never prunes**, for the same reason with the sign
//!    flipped: it deliberately does not see unchanged rows, so every one of
//!    them looks stale.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use qbz_app::settings::media_servers::{MediaServerKind, MediaServerSettings};
use qbz_media_cache::{CachedLibrary, CachedTrack, RemoteSource};

/// One sweep at a time, per source. A second one would fight the first for the
/// cache's write lock and double the server's load to produce the same rows.
static JELLYFIN_BUSY: AtomicBool = AtomicBool::new(false);
static SUBSONIC_BUSY: AtomicBool = AtomicBool::new(false);
static JELLYFIN_CANCEL: AtomicBool = AtomicBool::new(false);
static JELLYFIN_QUALITY_EPOCH: AtomicU64 = AtomicU64::new(1);
static SUBSONIC_EPOCH: AtomicU64 = AtomicU64::new(1);
static JELLYFIN_QUALITY_RUNNING: AtomicBool = AtomicBool::new(false);
static JELLYFIN_BULK_QUALITY: AtomicBool = AtomicBool::new(false);
static JELLYFIN_QUALITY_QUEUE: LazyLock<Mutex<QualityQueue>> =
    LazyLock::new(|| Mutex::new(QualityQueue::default()));
static JELLYFIN_STATE_GATE: Mutex<()> = Mutex::new(());
static SUBSONIC_STATE_GATE: Mutex<()> = Mutex::new(());

const QUALITY_QUEUE_MAX: usize = 2_000;
const QUALITY_RETRY_SECS: i64 = 60;

#[derive(Default)]
struct QualityQueue {
    urgent: VecDeque<String>,
    visible: VecDeque<String>,
    present: HashSet<String>,
}

impl QualityQueue {
    fn push(&mut self, item_ids: impl IntoIterator<Item = String>, urgent: bool) {
        for item_id in item_ids {
            if item_id.is_empty() || self.present.contains(&item_id) {
                continue;
            }
            while self.present.len() >= QUALITY_QUEUE_MAX {
                let removed = if urgent {
                    self.visible.pop_back().or_else(|| self.urgent.pop_back())
                } else {
                    // A merely visible row never displaces an urgent
                    // playing/queued row when the bounded queue is full.
                    self.visible.pop_front()
                };
                if let Some(removed) = removed {
                    self.present.remove(&removed);
                } else {
                    break;
                }
            }
            if self.present.len() >= QUALITY_QUEUE_MAX {
                continue;
            }
            if self.present.insert(item_id.clone()) {
                if urgent {
                    self.urgent.push_back(item_id);
                } else {
                    self.visible.push_back(item_id);
                }
            }
        }
    }

    fn pop_batch(&mut self, limit: usize) -> Vec<String> {
        let mut rows = Vec::with_capacity(limit);
        while rows.len() < limit {
            let Some(item_id) = self.urgent.pop_front().or_else(|| self.visible.pop_front()) else {
                break;
            };
            self.present.remove(&item_id);
            rows.push(item_id);
        }
        rows
    }

    fn is_empty(&self) -> bool {
        self.present.is_empty()
    }

    fn clear(&mut self) {
        self.urgent.clear();
        self.visible.clear();
        self.present.clear();
    }
}

fn busy_flag(kind: MediaServerKind) -> &'static AtomicBool {
    match kind {
        MediaServerKind::Jellyfin => &JELLYFIN_BUSY,
        MediaServerKind::Subsonic => &SUBSONIC_BUSY,
    }
}

/// Is a sweep running for this server right now?
pub fn is_syncing(kind: MediaServerKind) -> bool {
    busy_flag(kind).load(Ordering::Relaxed)
}

/// Cancel source work without waiting for an in-flight request. Every cache
/// write checks the epoch/cancel latch after the await, so a late response is
/// discarded and can never authorize prune or publish stale quality.
pub fn cancel(kind: MediaServerKind) {
    match kind {
        MediaServerKind::Jellyfin => {
            JELLYFIN_CANCEL.store(true, Ordering::Release);
            invalidate_jellyfin_quality();
        }
        MediaServerKind::Subsonic => {
            SUBSONIC_EPOCH.fetch_add(1, Ordering::AcqRel);
        }
    }
}

pub fn cancel_all() {
    for kind in MediaServerKind::ALL {
        cancel(kind);
    }
}

pub(crate) fn media_server_state_guard(
    kind: MediaServerKind,
) -> std::sync::MutexGuard<'static, ()> {
    let gate = match kind {
        MediaServerKind::Jellyfin => &JELLYFIN_STATE_GATE,
        MediaServerKind::Subsonic => &SUBSONIC_STATE_GATE,
    };
    gate.lock().unwrap_or_else(|error| error.into_inner())
}

/// Visible rows enter the secondary quality queue behind playing/queued rows
/// but ahead of the catalog-wide background fill.
pub fn prioritize_jellyfin_quality(item_ids: Vec<String>, urgent: bool) {
    if item_ids.is_empty() {
        return;
    }
    JELLYFIN_QUALITY_QUEUE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push(item_ids, urgent);
    start_jellyfin_quality_worker();
}

/// Resume an interrupted catalog-wide hydration after an online session bind.
/// Pending state lives in the cache, so this does not need an in-memory
/// checkpoint and exits immediately when every row is already hydrated.
pub fn resume_jellyfin_quality() {
    prioritize_recent_jellyfin_quality();
    JELLYFIN_BULK_QUALITY.store(true, Ordering::Release);
    start_jellyfin_quality_worker();
}

fn invalidate_jellyfin_quality() {
    JELLYFIN_QUALITY_EPOCH.fetch_add(1, Ordering::AcqRel);
    JELLYFIN_BULK_QUALITY.store(false, Ordering::Release);
    JELLYFIN_QUALITY_QUEUE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// RAII guard so an early return — or a `?` — cannot leave the flag stuck on.
/// A stuck flag means the sync button never works again until a restart.
struct BusyGuard(&'static AtomicBool, MediaServerKind);

impl BusyGuard {
    fn acquire(kind: MediaServerKind) -> Option<Self> {
        let flag = busy_flag(kind);
        if flag.load(Ordering::Acquire) || !crate::media_connection_qt::begin_sync(kind) {
            return None;
        }
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| BusyGuard(flag, kind))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
        crate::media_connection_qt::syncing(self.1, false);
    }
}

/// What a finished sweep reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncReport {
    /// Rows written or refreshed.
    pub saved: usize,
    /// Rows deleted because the server no longer has them. Always 0 for a
    /// delta sweep — see the module header.
    pub pruned: usize,
    /// Tracks in the cache afterwards.
    pub total: u64,
}

/// A media-cache sweep failure that the UI can present at the right severity.
/// A server-side library refresh is not a broken connection: Navidrome keeps
/// answering requests while its own catalog is incomplete, and exposes that
/// state through `getScanStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Failed(String),
    ServerRefreshing { server: String, scanned: u64 },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::ServerRefreshing { server, scanned } => {
                write!(
                    f,
                    "{server} is refreshing its library ({scanned} items scanned)"
                )
            }
        }
    }
}

/// Push a progress line to the UI. Cheap enough to call once per bounded page.
fn report(kind: MediaServerKind, done: u64, total: u64) {
    log::info!("[qbz-qt] {} sync: {done}/{total}", kind.as_str());
    let text = format!("{done}/{total}");
    crate::media_connection_qt::progress(kind, text.clone());
}

// ---------------------------------------------------------------------------
// Jellyfin
// ---------------------------------------------------------------------------

/// Sweep a Jellyfin server into the cache.
///
/// `full` forces a complete pass; otherwise a server that has been swept before
/// gets a DELTA (`minDateLastSaved`), which Jellyfin honours — verified: a
/// future-dated delta returns zero rows. The essential pass deliberately omits
/// expensive `MediaSources`; quality is retained while a secondary worker
/// refreshes changed rows after this function publishes the catalog.
pub async fn sync_jellyfin(full: bool) -> Result<SyncReport, String> {
    let kind = MediaServerKind::Jellyfin;
    let Some(_guard) = BusyGuard::acquire(kind) else {
        return Err("a jellyfin sync is already running".into());
    };
    // A new authoritative pass supersedes an older hydration response. The
    // request may still finish, but its epoch can no longer write.
    invalidate_jellyfin_quality();
    JELLYFIN_CANCEL.store(false, Ordering::Release);
    let cfg = crate::media_servers_qt::get(kind);
    if !cfg.is_configured(kind) {
        return Err("jellyfin is not configured".into());
    }
    let out = sync_jellyfin_inner(cfg, full).await;
    out
}

async fn sync_jellyfin_inner(cfg: MediaServerSettings, full: bool) -> Result<SyncReport, String> {
    let kind = MediaServerKind::Jellyfin;

    let client = qbz_jellyfin::JellyfinClient::new(&cfg.base_url, &cfg.token, &cfg.username, &cfg.device_id)
        .map_err(|e| e.to_string())?;
    // `username` holds the Jellyfin USER ID for the API's purposes — see
    // `connect_jellyfin`, which stores the id the auth response returned rather
    // than the typed name, because every `/Items` call keys on the id.
    let libraries = client.music_libraries().await.map_err(|e| e.to_string())?;
    if libraries.is_empty() {
        return Err("this jellyfin server exposes no music library".into());
    }
    let selected: Vec<&qbz_jellyfin::MusicLibrary> = libraries
        .iter()
        .filter(|library| cfg.selected_libraries.contains(&library.id))
        .collect();
    let wanted: Vec<&qbz_jellyfin::MusicLibrary> =
        if cfg.selected_libraries.is_empty() || selected.is_empty() {
            // Never chosen: take them all. Matching the Plex flow, where the first
            // fetch (and a completely stale selection) defaults to everything
            // rather than authorizing an accidental empty prune.
            libraries.iter().collect()
        } else {
            selected
        };

    let delta = (!full && cfg.last_sync_at > 0).then(|| iso8601(cfg.last_sync_at));
    let sync_epoch = JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire);
    let generation = begin_source_sync(RemoteSource::Jellyfin)?.generation;
    log::info!(
        "[media-sync] source=jellyfin phase=essential generation={generation} mode={} libraries={}",
        if delta.is_some() { "delta" } else { "full" },
        wanted.len(),
    );
    let mut saved = 0usize;
    let pass = async {
        for lib in &wanted {
            let mut offset = 0u64;
            let mut declared_total = None;
            loop {
                if JELLYFIN_CANCEL.load(Ordering::Acquire) {
                    return Err("jellyfin sync cancelled".to_string());
                }
                let (page, page_total) = client
                    .essential_tracks_page(Some(&lib.id), offset, delta.as_deref())
                    .await
                    .map_err(|error| error.to_string())?;
                if JELLYFIN_CANCEL.load(Ordering::Acquire) {
                    return Err("jellyfin sync cancelled".to_string());
                }
                if declared_total.is_some_and(|total| total != page_total) {
                    return Err("jellyfin page total changed during sync".to_string());
                }
                declared_total = Some(page_total);
                if page.is_empty() {
                    if offset != page_total {
                        return Err("jellyfin page ended before its declared total".to_string());
                    }
                    break;
                }
                let rows: Vec<CachedTrack> = page
                    .iter()
                    .map(|track| jellyfin_row(track, &cfg.server_id, &lib.id))
                    .collect();
                saved += write_essential_rows(
                    RemoteSource::Jellyfin,
                    generation,
                    sync_epoch,
                    &rows,
                )?;
                offset = offset.saturating_add(page.len() as u64);
                if offset > page_total {
                    return Err("jellyfin page exceeded its declared total".to_string());
                }
                report(kind, offset, page_total);
                log::info!(
                    "[media-sync] source=jellyfin phase=essential-page generation={generation} rows={} checkpoint={offset} total={page_total} prune_authorized=false",
                    page.len(),
                );
                if offset == page_total {
                    break;
                }
                if page.len() < qbz_jellyfin::PAGE_SIZE as usize {
                    return Err("jellyfin short page cannot reach its declared total".to_string());
                }
            }
        }

        write_libraries_for_epoch(
            RemoteSource::Jellyfin,
            sync_epoch,
            &libraries
                .iter()
                .map(|library| CachedLibrary {
                    source: "jellyfin".into(),
                    library_id: library.id.clone(),
                    name: library.name.clone(),
                    server_id: cfg.server_id.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        complete_source_sync(
            RemoteSource::Jellyfin,
            generation,
            sync_epoch,
            delta.is_none(),
        )
    }
    .await;

    let pruned = match pass {
        Ok(pruned) => pruned,
        Err(error) => {
            let _ = interrupt_source_sync(RemoteSource::Jellyfin, generation, sync_epoch);
            return Err(error);
        }
    };
    let report = finish_jellyfin(cfg, saved, pruned, sync_epoch)?;
    log::info!(
        "[media-sync] source=jellyfin phase=essential-complete generation={generation} rows={saved} pruned={pruned} prune_authorized={}",
        delta.is_none(),
    );
    prioritize_recent_jellyfin_quality();
    JELLYFIN_BULK_QUALITY.store(true, Ordering::Release);
    start_jellyfin_quality_worker();
    Ok(report)
}

fn jellyfin_row(t: &qbz_jellyfin::JellyfinTrack, server_id: &str, library_id: &str) -> CachedTrack {
    // Preserve the addressable ITEM together with the optional cache-busting
    // tag. Per-item art is the disc/track layer and therefore outranks the
    // MusicAlbum image. A missing album tag is not a missing cover: Jellyfin
    // serves `/Items/{albumId}/Images/Primary` without one.
    let artwork_token = t
        .item_image_tag
        .as_ref()
        .filter(|_| !t.id.is_empty())
        .map(|tag| format!("{}/{}", t.id, tag));
    let collection_artwork_token = (!t.album_id.is_empty()).then(|| {
        format!(
            "{}/{}",
            t.album_id,
            t.album_image_tag.as_deref().unwrap_or_default()
        )
    });
    CachedTrack {
        id: 0,
        source: "jellyfin".into(),
        item_id: t.id.clone(),
        server_id: server_id.to_string(),
        library_id: library_id.to_string(),
        title: t.title.clone(),
        artist: t.artist.clone(),
        album_artist: t.album_artist.clone(),
        album: t.album.clone(),
        album_id: t.album_id.clone(),
        track_number: t.track_number,
        disc_number: t.disc_number,
        duration_ms: t.duration_ms,
        year: t.year,
        genres: t.genres.clone(),
        genre: t.genre.clone(),
        container: t.container.clone(),
        codec: t.codec.clone(),
        bit_depth: t.bit_depth,
        sample_rate_hz: t.sample_rate_hz,
        channels: t.channels,
        bitrate_kbps: t.bitrate_bps.map(|b| b / 1000),
        artwork_token,
        collection_artwork_token,
        isrc: None,
        recording_mbid: t.recording_mbid.clone(),
        size_bytes: None,
    }
}

fn jellyfin_quality_row(
    quality: qbz_jellyfin::JellyfinTrackQuality,
) -> qbz_media_cache::CachedTrackQuality {
    qbz_media_cache::CachedTrackQuality {
        item_id: quality.id,
        container: quality.container,
        codec: quality.codec,
        bit_depth: quality.bit_depth,
        sample_rate_hz: quality.sample_rate_hz,
        channels: quality.channels,
        bitrate_kbps: quality.bitrate_bps.map(|value| value / 1000),
    }
}

fn prioritize_recent_jellyfin_quality() {
    let row_ids = crate::recently_qt::jellyfin_recent_track_ids();
    if row_ids.is_empty() {
        return;
    }
    let item_ids = handle(RemoteSource::Jellyfin)
        .with(|cache| {
            row_ids
                .iter()
                .filter_map(|row_id| {
                    qbz_media_cache::track_by_id(cache, *row_id)
                        .ok()
                        .flatten()
                        .map(|track| track.item_id)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    prioritize_jellyfin_quality(item_ids, true);
}

fn quality_queue_has_work() -> bool {
    JELLYFIN_BULK_QUALITY.load(Ordering::Acquire)
        || !JELLYFIN_QUALITY_QUEUE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
}

fn start_jellyfin_quality_worker() {
    if !crate::media_servers_qt::get(MediaServerKind::Jellyfin)
        .is_configured(MediaServerKind::Jellyfin)
    {
        return;
    }
    if JELLYFIN_QUALITY_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let epoch = JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire);
    crate::spawn(async move {
        if let Err(error) = jellyfin_quality_worker(epoch).await {
            log::warn!("[jellyfin-quality] phase=paused reason={error}");
            if epoch == JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
                JELLYFIN_BULK_QUALITY.store(false, Ordering::Release);
                JELLYFIN_QUALITY_QUEUE
                    .lock()
                    .unwrap_or_else(|lock_error| lock_error.into_inner())
                    .clear();
            }
        }
        JELLYFIN_QUALITY_RUNNING.store(false, Ordering::Release);
        // A new essential sync may have advanced the epoch and queued a fresh
        // bulk pass while this old request was still in flight. Cancellation
        // clears all work, so any work present here belongs to the current
        // epoch and must get a new worker.
        if quality_queue_has_work() {
            start_jellyfin_quality_worker();
        }
    });
}

async fn jellyfin_quality_worker(epoch: u64) -> Result<(), String> {
    let cfg = crate::media_servers_qt::get(MediaServerKind::Jellyfin);
    if !cfg.is_configured(MediaServerKind::Jellyfin) {
        return Ok(());
    }
    let client = qbz_jellyfin::JellyfinClient::new(&cfg.base_url, &cfg.token, &cfg.username, &cfg.device_id)
        .map_err(|error| error.to_string())?;
    let mut hydrated = 0usize;
    let mut batches = 0usize;
    let mut priority_published = false;
    loop {
        if epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
            return Ok(());
        }
        let priority = JELLYFIN_QUALITY_QUEUE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_batch(qbz_jellyfin::QUALITY_BATCH_SIZE);
        let was_priority = !priority.is_empty();
        let candidates = if was_priority {
            pending_quality_async(priority).await?
        } else if JELLYFIN_BULK_QUALITY.load(Ordering::Acquire) {
            quality_candidates_async(qbz_jellyfin::QUALITY_BATCH_SIZE).await?
        } else {
            Vec::new()
        };
        if candidates.is_empty() {
            if was_priority {
                continue;
            }
            JELLYFIN_BULK_QUALITY.store(false, Ordering::Release);
            break;
        }

        let started = std::time::Instant::now();
        let response = match client.track_quality(&candidates).await {
            Ok(response) => response,
            Err(error) => {
                if epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
                    return Ok(());
                }
                defer_quality_async(candidates, QUALITY_RETRY_SECS, epoch).await?;
                JELLYFIN_BULK_QUALITY.store(false, Ordering::Release);
                schedule_jellyfin_quality_retry(epoch);
                return Err(error.to_string());
            }
        };
        if epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
            return Ok(());
        }
        let returned = response
            .iter()
            .map(|quality| quality.id.as_str())
            .collect::<HashSet<_>>();
        let missing = candidates
            .iter()
            .filter(|item_id| !returned.contains(item_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let updates = response
            .into_iter()
            .map(jellyfin_quality_row)
            .collect::<Vec<_>>();
        let updated = update_quality_async(updates, epoch).await?;
        if !missing.is_empty() {
            defer_quality_async(missing, QUALITY_RETRY_SECS, epoch).await?;
        }
        hydrated = hydrated.saturating_add(updated);
        batches = batches.saturating_add(1);
        log::info!(
            "[jellyfin-quality] phase=hydrate batch={} requested={} updated={} missing={} priority={} elapsed={:?}",
            batches,
            candidates.len(),
            updated,
            candidates.len().saturating_sub(updated),
            was_priority,
            started.elapsed(),
        );
        if was_priority && updated > 0 && !priority_published {
            // Coalesced by the catalog worker. This updates a bounded visible
            // page promptly while the remaining bulk fill stays silent.
            crate::local_catalog_qt::request_catch_up();
            priority_published = true;
        }
    }
    if hydrated > 0 {
        crate::local_catalog_qt::request_catch_up();
    }
    log::info!("[jellyfin-quality] phase=complete batches={batches} hydrated={hydrated}");
    Ok(())
}

fn schedule_jellyfin_quality_retry(epoch: u64) {
    crate::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(QUALITY_RETRY_SECS as u64)).await;
        if epoch == JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
            JELLYFIN_BULK_QUALITY.store(true, Ordering::Release);
            start_jellyfin_quality_worker();
        }
    });
}

async fn pending_quality_async(item_ids: Vec<String>) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        handle(RemoteSource::Jellyfin)
            .with(|cache| {
                qbz_media_cache::pending_quality_ids(cache, RemoteSource::Jellyfin, &item_ids)
            })
            .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
    })
    .await
    .map_err(|error| format!("jellyfin quality cache worker failed: {error}"))?
}

async fn quality_candidates_async(limit: usize) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        handle(RemoteSource::Jellyfin)
            .with(|cache| qbz_media_cache::quality_candidates(cache, RemoteSource::Jellyfin, limit))
            .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
    })
    .await
    .map_err(|error| format!("jellyfin quality candidate worker failed: {error}"))?
}

async fn update_quality_async(
    updates: Vec<qbz_media_cache::CachedTrackQuality>,
    epoch: u64,
) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || {
        handle(RemoteSource::Jellyfin)
            .with_mut(|cache| {
                if epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
                    return Ok(0);
                }
                qbz_media_cache::update_track_quality(cache, RemoteSource::Jellyfin, &updates)
            })
            .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
    })
    .await
    .map_err(|error| format!("jellyfin quality write worker failed: {error}"))?
}

async fn defer_quality_async(
    item_ids: Vec<String>,
    retry_secs: i64,
    epoch: u64,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        handle(RemoteSource::Jellyfin)
            .with_mut(|cache| {
                if epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire) {
                    return Ok(0);
                }
                qbz_media_cache::defer_track_quality(
                    cache,
                    RemoteSource::Jellyfin,
                    &item_ids,
                    retry_secs,
                )
            })
            .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
    })
    .await
    .map_err(|error| format!("jellyfin quality defer worker failed: {error}"))??;
    Ok(())
}

/// Jellyfin wants `minDateLastSaved` as ISO-8601 UTC.
///
/// Hand-rolled from a Unix timestamp rather than pulling in `chrono`: this is
/// the only date this crate formats, the civil-calendar arithmetic below is
/// fixed-rule (proleptic Gregorian, no zones, no leap seconds), and the value
/// is only ever compared by the server against its own stamps.
fn iso8601(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // Days since 1970-01-01 -> civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mth <= 2 { y + 1 } else { y };
    format!("{year:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// ---------------------------------------------------------------------------
// Subsonic
// ---------------------------------------------------------------------------

/// Sweep a Subsonic-compatible server into the cache.
///
/// No delta — the protocol offers none — and none is needed: the whole library
/// costs under a second because quality rides along with every song.
///
/// The sweep MODE is detected rather than assumed. `search3` with an empty
/// query enumerated 6678 tracks in 14 requests on Navidrome, but that is the
/// behaviour most likely to differ on a server that was never on the bench, so
/// a probe decides and the portable `getAlbumList2` + `getAlbum` walk is the
/// fallback.
pub async fn sync_subsonic(_full: bool) -> Result<SyncReport, SyncError> {
    let kind = MediaServerKind::Subsonic;
    let Some(_guard) = BusyGuard::acquire(kind) else {
        return Err(SyncError::Failed(
            "a subsonic sync is already running".into(),
        ));
    };
    let cfg = crate::media_servers_qt::get(kind);
    let Some((base, creds)) = crate::media_servers_qt::subsonic_credentials() else {
        return Err(SyncError::Failed("subsonic is not configured".into()));
    };
    let client = qbz_subsonic::SubsonicClient::new(&base, creds)
        .map_err(|error| SyncError::Failed(error.to_string()))?;
    // This is the SERVER'S scan, not QBZ's cache sweep. Navidrome may keep
    // answering search/list calls with an incomplete catalog while it runs;
    // importing that intermediate view would look like a broken integration
    // and could authorize a misleading empty generation. The endpoint has no
    // total, only a processed count, so the UI reports exactly that.
    match client.scan_status().await {
        Ok(status) if status.scanning => {
            let raw_server = cfg
                .server_name
                .split_whitespace()
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("Subsonic");
            let server = match raw_server.to_ascii_lowercase().as_str() {
                "navidrome" => "Navidrome".to_string(),
                "gonic" => "Gonic".to_string(),
                "airsonic" => "Airsonic".to_string(),
                _ => raw_server.to_string(),
            };
            return Err(SyncError::ServerRefreshing {
                server,
                scanned: status.count,
            });
        }
        Ok(_) => {}
        Err(error) => {
            // Some older/partial Subsonic servers omit this standard endpoint.
            // That cannot disable an otherwise working integration; continue
            // with QBZ's guarded generation and retain its failure semantics.
            log::warn!("[media-sync] source=subsonic server scan status unavailable: {error}");
        }
    }
    let sync_epoch = SUBSONIC_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    let out = sync_subsonic_inner(cfg, client, sync_epoch)
        .await
        .map_err(SyncError::Failed);
    out
}

async fn sync_subsonic_inner(
    cfg: MediaServerSettings,
    client: qbz_subsonic::SubsonicClient,
    sync_epoch: u64,
) -> Result<SyncReport, String> {
    let kind = MediaServerKind::Subsonic;
    let folders = client.music_folders().await.map_err(|e| e.to_string())?;
    if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
        return Err("subsonic sync cancelled".to_string());
    }
    // Keep the successful probe page: refetching offset zero both wastes one
    // request and opens a window where a transient empty response could turn a
    // proven non-empty Search3 library into an empty authoritative generation.
    let (mode, mut first_search_page) = match client.search_page(0).await {
        Ok(page) if !page.is_empty() => (qbz_subsonic::SweepMode::Search3, Some(page)),
        _ => (qbz_subsonic::SweepMode::PerAlbum, None),
    };
    if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
        return Err("subsonic sync cancelled".to_string());
    }
    let generation = begin_source_sync(RemoteSource::Subsonic)?.generation;
    log::info!(
        "[media-sync] source=subsonic phase=enumerate generation={generation} mode={mode:?} prune_authorized=false"
    );
    let mut saved = 0usize;

    let pass = async {
        match mode {
            qbz_subsonic::SweepMode::Search3 => {
                // `search3` returns song/disc coverArt tokens. Fetch the small
                // album listing as enrichment so collection art survives as a
                // separate fallback instead of being guessed from disc one.
                let mut collection_art = HashMap::<String, String>::new();
                let mut album_offset = 0u32;
                loop {
                    match client.album_page(album_offset).await {
                        Ok(albums) => {
                            let page_len = albums.len() as u32;
                            for album in albums {
                                if let Some(token) = album.cover_art {
                                    collection_art.insert(album.id, token);
                                }
                            }
                            album_offset = album_offset.saturating_add(page_len);
                            if page_len < qbz_subsonic::PAGE_SIZE {
                                break;
                            }
                        }
                        Err(error) => {
                            // Collection art is enrichment; a server that can
                            // enumerate songs must not lose its usable catalog
                            // merely because getAlbumList2 is unavailable.
                            log::warn!(
                                "[media-sync] source=subsonic collection artwork unavailable: {error}"
                            );
                            collection_art.clear();
                            break;
                        }
                    }
                }
                let mut offset = 0u32;
                let mut page_number = 0u64;
                loop {
                    if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
                        return Err("subsonic sync cancelled".to_string());
                    }
                    let page = if offset == 0 {
                        first_search_page
                            .take()
                            .ok_or_else(|| "subsonic search probe page is missing".to_string())?
                    } else {
                        client
                            .search_page(offset)
                            .await
                            .map_err(|e| e.to_string())?
                    };
                    if page.is_empty() {
                        break;
                    }
                    let page_len = u32::try_from(page.len())
                        .map_err(|_| "subsonic page length overflow".to_string())?;
                    let rows: Vec<CachedTrack> = page
                        .iter()
                        .map(|track| {
                            subsonic_row(
                                track,
                                collection_art.get(&track.album_id).map(String::as_str),
                            )
                        })
                        .collect();
                    saved += write_generation_rows(
                        RemoteSource::Subsonic,
                        generation,
                        sync_epoch,
                        &rows,
                    )?;
                    offset = offset
                        .checked_add(page_len)
                        .ok_or_else(|| "subsonic search offset overflow".to_string())?;
                    page_number = page_number.saturating_add(1);
                    report(kind, offset as u64, offset as u64);
                    log::info!(
                        "[media-sync] source=subsonic phase=search-page generation={generation} page={page_number} rows={page_len} checkpoint={offset} prune_authorized=false"
                    );
                    if page_len < qbz_subsonic::PAGE_SIZE {
                        break;
                    }
                }
            }
            qbz_subsonic::SweepMode::PerAlbum => {
                let mut album_offset = 0u32;
                let mut albums_seen = 0u64;
                let mut page_number = 0u64;
                loop {
                    if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
                        return Err("subsonic sync cancelled".to_string());
                    }
                    let album_page = client
                        .album_page(album_offset)
                        .await
                        .map_err(|e| e.to_string())?;
                    if album_page.is_empty() {
                        break;
                    }
                    let page_len = u32::try_from(album_page.len())
                        .map_err(|_| "subsonic album page length overflow".to_string())?;
                    for album in album_page {
                        if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
                            return Err("subsonic sync cancelled".to_string());
                        }
                        // Any failed album makes this generation incomplete.
                        // Skipping it and pruning would reinterpret a server
                        // outage as deletion of every track in that album.
                        let tracks = client
                            .album_tracks(&album.id)
                            .await
                            .map_err(|e| e.to_string())?;
                        let rows: Vec<CachedTrack> = tracks
                            .iter()
                            .map(|track| subsonic_row(track, album.cover_art.as_deref()))
                            .collect();
                        saved += write_generation_rows(
                            RemoteSource::Subsonic,
                            generation,
                            sync_epoch,
                            &rows,
                        )?;
                        albums_seen = albums_seen.saturating_add(1);
                        if albums_seen % 25 == 0 {
                            report(kind, albums_seen, albums_seen);
                        }
                    }
                    album_offset = album_offset
                        .checked_add(page_len)
                        .ok_or_else(|| "subsonic album offset overflow".to_string())?;
                    page_number = page_number.saturating_add(1);
                    log::info!(
                        "[media-sync] source=subsonic phase=album-page generation={generation} page={page_number} albums={page_len} checkpoint={album_offset} tracks={saved} prune_authorized=false"
                    );
                    if page_len < qbz_subsonic::PAGE_SIZE {
                        break;
                    }
                }
            }
        }

        write_libraries_for_epoch(
            RemoteSource::Subsonic,
            sync_epoch,
            &folders
                .iter()
                .map(|folder| CachedLibrary {
                    source: "subsonic".into(),
                    library_id: folder.id.clone(),
                    name: folder.name.clone(),
                    server_id: String::new(),
                })
                .collect::<Vec<_>>(),
        )?;
        complete_source_sync(RemoteSource::Subsonic, generation, sync_epoch, true)
    }
    .await;

    let pruned = match pass {
        Ok(pruned) => pruned,
        Err(error) => {
            let _ = interrupt_source_sync(RemoteSource::Subsonic, generation, sync_epoch);
            return Err(error);
        }
    };
    log::info!(
        "[media-sync] source=subsonic phase=complete generation={generation} rows={saved} pruned={pruned} prune_authorized=true"
    );
    finish_subsonic(cfg, saved, pruned, sync_epoch)
}

fn subsonic_row(
    t: &qbz_subsonic::SubsonicTrack,
    collection_artwork_token: Option<&str>,
) -> CachedTrack {
    CachedTrack {
        id: 0,
        source: "subsonic".into(),
        item_id: t.id.clone(),
        server_id: String::new(),
        library_id: String::new(),
        title: t.title.clone(),
        artist: t.artist.clone(),
        album_artist: t.album_artist.clone(),
        album: t.album.clone(),
        album_id: t.album_id.clone(),
        track_number: t.track_number,
        disc_number: t.disc_number,
        duration_ms: t.duration_ms,
        year: t.year,
        genres: t.genres.clone(),
        genre: t.genre.clone(),
        container: t.suffix.clone(),
        codec: t.content_type.clone(),
        bit_depth: t.bit_depth,
        sample_rate_hz: t.sample_rate_hz,
        channels: t.channels,
        bitrate_kbps: t.bitrate_kbps,
        // The OPAQUE coverArt id, verbatim. Never parsed, never built.
        artwork_token: t.cover_art.clone(),
        collection_artwork_token: collection_artwork_token.map(str::to_string),
        isrc: t.isrc.clone(),
        recording_mbid: t.recording_mbid.clone(),
        size_bytes: t.size,
    }
}

// ---------------------------------------------------------------------------
// Cache plumbing — through the SOURCE's handle, never a second connection
// ---------------------------------------------------------------------------

/// The cache handle for a source. Going through the registry rather than
/// opening our own connection is what keeps the sweep and every read pointed at
/// the same user's mirror: `bind_user` moves them together.
fn handle(source: RemoteSource) -> &'static qbz_source::CacheHandle {
    match source {
        RemoteSource::Jellyfin => qbz_source::registry().jellyfin().cache(),
        RemoteSource::Subsonic => qbz_source::registry().subsonic().cache(),
    }
}

fn begin_source_sync(
    source: RemoteSource,
) -> Result<qbz_media_cache::SourceSyncGeneration, String> {
    handle(source)
        .with_mut(|cache| qbz_media_cache::begin_source_sync(cache, source))
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn source_epoch_is_current(source: RemoteSource, sync_epoch: u64) -> bool {
    let current = match source {
        RemoteSource::Jellyfin => JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire),
        RemoteSource::Subsonic => SUBSONIC_EPOCH.load(Ordering::Acquire),
    };
    sync_epoch == current
}

fn write_essential_rows(
    source: RemoteSource,
    generation: u64,
    sync_epoch: u64,
    rows: &[CachedTrack],
) -> Result<usize, String> {
    handle(source)
        .with_mut(|cache| {
            if !source_epoch_is_current(source, sync_epoch) {
                return Err(format!("{} sync cancelled", source.as_str()));
            }
            qbz_media_cache::save_essential_tracks(cache, source, generation, rows)
        })
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn write_generation_rows(
    source: RemoteSource,
    generation: u64,
    sync_epoch: u64,
    rows: &[CachedTrack],
) -> Result<usize, String> {
    handle(source)
        .with_mut(|cache| {
            if !source_epoch_is_current(source, sync_epoch) {
                return Err(format!("{} sync cancelled", source.as_str()));
            }
            qbz_media_cache::save_generation_tracks(cache, source, generation, rows)
        })
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn complete_source_sync(
    source: RemoteSource,
    generation: u64,
    sync_epoch: u64,
    prune_old: bool,
) -> Result<usize, String> {
    handle(source)
        .with_mut(|cache| {
            if !source_epoch_is_current(source, sync_epoch) {
                return Err(format!("{} sync cancelled", source.as_str()));
            }
            qbz_media_cache::complete_source_sync(cache, source, generation, prune_old)
        })
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn interrupt_source_sync(
    source: RemoteSource,
    generation: u64,
    sync_epoch: u64,
) -> Result<(), String> {
    handle(source)
        .with(|cache| {
            if !source_epoch_is_current(source, sync_epoch) {
                return Ok(());
            }
            qbz_media_cache::interrupt_source_sync(cache, source, generation)
        })
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn write_libraries_for_epoch(
    source: RemoteSource,
    sync_epoch: u64,
    libraries: &[CachedLibrary],
) -> Result<(), String> {
    handle(source)
        .with_mut(|cache| {
            if !source_epoch_is_current(source, sync_epoch) {
                return Err(format!("{} sync cancelled", source.as_str()));
            }
            qbz_media_cache::save_libraries(cache, source, libraries)
        })
        .ok_or_else(|| "the remote cache is not bound to a user".to_string())?
}

fn count(source: RemoteSource) -> u64 {
    handle(source)
        .with(|c| qbz_media_cache::count(c, source).unwrap_or(0))
        .unwrap_or(0)
}

fn finish_jellyfin(
    cfg: MediaServerSettings,
    saved: usize,
    pruned: usize,
    sync_epoch: u64,
) -> Result<SyncReport, String> {
    let _gate = media_server_state_guard(MediaServerKind::Jellyfin);
    if JELLYFIN_CANCEL.load(Ordering::Acquire)
        || sync_epoch != JELLYFIN_QUALITY_EPOCH.load(Ordering::Acquire)
    {
        return Err("jellyfin sync cancelled".to_string());
    }
    finish(
        MediaServerKind::Jellyfin,
        cfg,
        saved,
        pruned,
        RemoteSource::Jellyfin,
    )
}

fn finish_subsonic(
    cfg: MediaServerSettings,
    saved: usize,
    pruned: usize,
    sync_epoch: u64,
) -> Result<SyncReport, String> {
    let _gate = media_server_state_guard(MediaServerKind::Subsonic);
    if !source_epoch_is_current(RemoteSource::Subsonic, sync_epoch) {
        return Err("subsonic sync cancelled".to_string());
    }
    finish(
        MediaServerKind::Subsonic,
        cfg,
        saved,
        pruned,
        RemoteSource::Subsonic,
    )
}

/// Stamp the sweep and report.
///
/// `last_sync_at` is written ONLY here, at the end of a run that finished —
/// every failure path above returns early without touching it. That is what
/// makes the next delta sound: a stamp written after a partial sweep would tell
/// the server "I have everything up to now", and the rows the interrupted run
/// never saw would never be asked for again.
fn finish(
    kind: MediaServerKind,
    mut cfg: MediaServerSettings,
    saved: usize,
    pruned: usize,
    source: RemoteSource,
) -> Result<SyncReport, String> {
    let total = count(source);
    cfg.last_sync_at = qbz_media_cache::sweep_start();
    cfg.last_sync_tracks = total as i64;
    crate::media_servers_qt::put(kind, &cfg);
    log::info!(
        "[qbz-qt] {} sync finished: {saved} saved, {pruned} pruned, {total} cached",
        kind.as_str()
    );
    crate::local_catalog_qt::request_catch_up();
    Ok(SyncReport {
        saved,
        pruned,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The delta parameter Jellyfin is handed. A wrong date is not a crash —
    /// it is a sweep that silently returns the wrong set, so the arithmetic is
    /// pinned against known instants.
    #[test]
    fn the_delta_timestamp_is_iso8601_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which is where a hand-rolled civil calendar goes wrong.
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(iso8601(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(iso8601(1_709_251_200), "2024-03-01T00:00:00Z");
        // A non-leap century, the other classic off-by-one.
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }

    /// The busy flag must survive an early return. A stuck flag means the sync
    /// button never works again until the app restarts.
    #[test]
    fn the_busy_guard_releases_on_drop() {
        let kind = MediaServerKind::Jellyfin;
        assert!(!is_syncing(kind));
        {
            let _g = BusyGuard::acquire(kind).expect("first acquire");
            assert!(is_syncing(kind));
            // A second sweep is refused rather than queued.
            assert!(BusyGuard::acquire(kind).is_none());
        }
        assert!(!is_syncing(kind), "the flag stuck after the guard dropped");
        // ...and the other source was never blocked by it.
        assert!(!is_syncing(MediaServerKind::Subsonic));
    }

    #[test]
    fn subsonic_cancellation_invalidates_late_page_writes() {
        let epoch = SUBSONIC_EPOCH.load(Ordering::Acquire);
        assert!(source_epoch_is_current(RemoteSource::Subsonic, epoch));
        cancel(MediaServerKind::Subsonic);
        assert!(!source_epoch_is_current(RemoteSource::Subsonic, epoch));
    }

    /// An item image wins over album art. Album art remains addressable by id
    /// when Jellyfin omits its optional cache-busting tag.
    #[test]
    fn jellyfin_art_tokens_keep_item_and_tagless_album_fallbacks() {
        let mut t = qbz_jellyfin::JellyfinTrack {
            id: "i".into(),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            album_id: "alb".into(),
            track_number: None,
            disc_number: None,
            duration_ms: 0,
            year: None,
            genres: Vec::new(),
            genre: None,
            container: String::new(),
            codec: None,
            bit_depth: None,
            sample_rate_hz: None,
            channels: None,
            bitrate_bps: None,
            album_image_tag: Some("tag".into()),
            item_image_tag: None,
            recording_mbid: None,
            server_path: None,
        };
        let row = jellyfin_row(&t, "srv", "lib");
        assert_eq!(row.artwork_token, None);
        assert_eq!(row.collection_artwork_token.as_deref(), Some("alb/tag"));
        t.album_id = String::new();
        let row = jellyfin_row(&t, "srv", "lib");
        assert_eq!(row.artwork_token, None);
        assert_eq!(row.collection_artwork_token, None);
        t.album_id = "alb".into();
        t.album_image_tag = None;
        assert_eq!(
            jellyfin_row(&t, "srv", "lib")
                .collection_artwork_token
                .as_deref(),
            Some("alb/")
        );
        t.item_image_tag = Some("disc-tag".into());
        let row = jellyfin_row(&t, "srv", "lib");
        assert_eq!(row.artwork_token.as_deref(), Some("i/disc-tag"));
        assert_eq!(row.collection_artwork_token.as_deref(), Some("alb/"));
    }

    /// Bitrate crosses the boundary in different units: Jellyfin reports bits
    /// per second, Subsonic kilobits. The cache stores kbps.
    #[test]
    fn bitrate_is_normalised_to_kbps_from_both_wires() {
        let jf = qbz_jellyfin::JellyfinTrack {
            id: "i".into(),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            album_id: "a".into(),
            track_number: None,
            disc_number: None,
            duration_ms: 0,
            year: None,
            genres: Vec::new(),
            genre: None,
            container: String::new(),
            codec: None,
            bit_depth: None,
            sample_rate_hz: None,
            channels: None,
            bitrate_bps: Some(3_120_281),
            album_image_tag: None,
            item_image_tag: None,
            recording_mbid: None,
            server_path: None,
        };
        assert_eq!(jellyfin_row(&jf, "s", "l").bitrate_kbps, Some(3120));
    }

    #[test]
    fn quality_queue_deduplicates_and_puts_urgent_rows_first() {
        let mut queue = QualityQueue::default();
        queue.push(vec!["visible-a".into(), "same".into()], false);
        queue.push(vec!["urgent".into(), "same".into()], true);
        assert_eq!(
            queue.pop_batch(3),
            vec![
                "urgent".to_string(),
                "visible-a".to_string(),
                "same".to_string()
            ]
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn visible_quality_never_evicts_urgent_rows_at_the_memory_cap() {
        let mut queue = QualityQueue::default();
        queue.push(
            (0..QUALITY_QUEUE_MAX).map(|index| format!("urgent-{index}")),
            true,
        );
        queue.push(vec!["visible".into()], false);
        assert_eq!(queue.present.len(), QUALITY_QUEUE_MAX);
        assert!(!queue.present.contains("visible"));
        assert_eq!(queue.pop_batch(1), vec!["urgent-0".to_string()]);
    }

    #[test]
    fn jellyfin_quality_mapping_keeps_lossy_nulls_and_normalises_bitrate() {
        let mapped = jellyfin_quality_row(qbz_jellyfin::JellyfinTrackQuality {
            id: "item".into(),
            container: "mp3".into(),
            codec: Some("mp3".into()),
            bit_depth: None,
            sample_rate_hz: Some(44_100),
            channels: Some(2),
            bitrate_bps: Some(320_999),
        });
        assert_eq!(mapped.item_id, "item");
        assert_eq!(mapped.bit_depth, None);
        assert_eq!(mapped.sample_rate_hz, Some(44_100));
        assert_eq!(mapped.bitrate_kbps, Some(320));
    }
}
