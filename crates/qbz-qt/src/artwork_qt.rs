//! Artwork pipeline — SOURCE-AWARE resolution for every QBZ art source.
//!
//! Every surface (Home rails, search, playlists, the queue panel, the
//! now-playing bar, the ambient triad) hands this module an `art_url` and
//! gets back a `file://` path QML can decode. Those urls are NOT all HTTP,
//! and treating them as if they were is what left local and Plex covers
//! blank. The taxonomy is the same one the Slint frontend routes on
//! (`crates/qbz/src/queue.rs` `load_artwork`, `artwork.rs`
//! `fetch_and_decode_ref`):
//!
//! | shape                          | class              | resolution              |
//! |--------------------------------|--------------------|-------------------------|
//! | `http://` / `https://`         | [`ArtUrl::Http`]   | disk cache, else download|
//! | `file:///…`                    | [`ArtUrl::LocalFile`] | strip + stat, NO fetch |
//! | `/abs/path/cover.jpg`          | [`ArtUrl::LocalFile`] | stat, NO fetch        |
//! | `/library/…` `/photo/…`        | [`ArtUrl::Plex`]   | tokenize, then as HTTP  |
//! | Plex path, no server/token     | [`ArtUrl::PlexUnconfigured`] | unresolvable  |
//!
//! A `file://` url can never be handed to reqwest ("builder error for url"),
//! and a bare `/library/...` path has no scheme at all — both are resolved
//! WITHOUT the network here, and [`download_missing`] drops them instead of
//! trying to GET them.
//!
//! Plex credentials come from `local_plex` (the same store the Local Library
//! grid reads) and the thumb url is built by `local_plex::thumb_url`, so a
//! cover fetched for the Albums grid and the same cover in the queue share
//! ONE cache entry ([`PLEX_THUMB_PX`] is the single transcode size).
//!
//! ## Repeat visits resolve from RAM
//!
//! `ImageCacheService::get` is not a read: it stats the file AND runs an
//! `UPDATE … SET last_accessed` on the shared SQLite connection, behind the
//! process-wide cache mutex that the 16 concurrent downloaders also hold to
//! `store`. The window dispatches call it once per visible card, on the Qt
//! GUI thread, on EVERY window pass — so a second visit to a view queued
//! dozens of write transactions behind the downloaders instead of resolving
//! the already-on-disk covers straight away (the "grey squares that fill in
//! unevenly" report). [`RESOLVED`] memoizes url -> cached file path so a
//! repeat lookup is a read-lock + one stat, with no SQLite and no contention
//! with the download pool. Entries are dropped when the file is gone (another
//! process may evict the shared cache), so the memo can never serve a dead
//! path.
//!
//! POC-NOTE: on download completion the WHOLE section list is republished
//! (a `homeSectionsJson` swap) — per-row QAbstractListModel updates are
//! the documented follow-up. No RGBA decode in Rust: QML `Image` decodes
//! `file://` asynchronously and natively.
//!
//! ## Cache eviction — both directories, once per run
//!
//! [`housekeeping`] runs on the blocking pool at boot and bounds BOTH caches:
//! the SHARED `~/.cache/qbz/images` (the full-size originals `store` writes;
//! `qbz_cache::ImageCacheService`, SQLite `last_accessed` LRU, budget
//! [`shared_budget_bytes`] (a Settings preference, 200 MB by default), trimmed
//! at boot and again every [`TRIM_EVERY_BYTES`] of new originals, in
//! [`EVICT_BATCH_ENTRIES`]-row batches so the
//! GUI thread is never parked behind a long unlink loop) and this module's
//! OWN derivative directory `images/scaled` ([`MAX_SCALED_BYTES`], LRU by
//! mtime, plus the one-time `.jpg` orphan sweep). The shared trim was the
//! Slint app's `spawn_evict` (v2.0.2); it was retired with that frontend
//! (69ed61207) and 2.1.0/2.1.1 shipped with NO caller of `evict` at all — the
//! directory grew without bound.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};

use qbz_cache::ImageCacheService;
use tokio::sync::Semaphore;

use crate::home_qt::HomeSection;

/// Matches artwork.rs `MAX_CONCURRENT`.
const MAX_CONCURRENT: usize = 16;
/// Matches artwork.rs `HTTP_TIMEOUT` (a hung request on a captive portal
/// must not pin a semaphore permit indefinitely).
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Server-side transcode size for EVERY Plex thumb this process fetches.
/// One size app-wide is deliberate: the cache key is the final tokenized url,
/// so a second size would mean a second download of the same cover. 256 px is
/// the Local Library grid's card size and is ample for the queue rows / the
/// now-playing bar / the sidebar dock, which all render well under it.
pub const PLEX_THUMB_PX: u32 = 256;

/// The ONE larger Plex transcode tier (contract 04 §3): only the big slots
/// (immersive main art, lightbox-class surfaces) tokenize at 1024; every list
/// stays at [`PLEX_THUMB_PX`]. The transcode url carries `width=`, so the two
/// tiers are separate cache entries by construction. (The Slint app passes
/// per-surface sizes — `qbz/src/artwork.rs:555-566` — so this is parity, not
/// invention.)
pub const PLEX_THUMB_PX_LARGE: u32 = 1024;

/// Memo ceiling. The memo is an accelerator, not a source of truth, so it is
/// cleared wholesale rather than carrying LRU bookkeeping.
const MEMO_CAP: usize = 8192;

/// Shared client (artwork.rs: "instead of reqwest::get building a fresh
/// client + TLS state per image").
static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .expect("apply proxy configuration")
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("reqwest client")
});

/// The process-wide image cache (`~/.cache/qbz/images`, shared with the
/// Slint/Tauri apps). `Connection` is not Sync, hence the Mutex; `get` /
/// `store` are short blocking calls, fine under it.
static CACHE: OnceLock<Mutex<ImageCacheService>> = OnceLock::new();

/// Raw art url -> its resolved file in the disk cache. See the module docs:
/// this is what makes a repeat window pass synchronous.
static RESOLVED: LazyLock<RwLock<HashMap<String, PathBuf>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn cache() -> Option<&'static Mutex<ImageCacheService>> {
    if CACHE.get().is_none() {
        match ImageCacheService::new() {
            Ok(service) => {
                let _ = CACHE.set(Mutex::new(service));
            }
            Err(e) => {
                log::error!("[qbz-qt] image cache open failed: {e}");
                return None;
            }
        }
    }
    CACHE.get()
}

// ---------------------------------------------------------------------------
// URL taxonomy
// ---------------------------------------------------------------------------

/// What an `art_url` actually is. See the table in the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtUrl {
    /// Nothing to resolve (empty, or a shape we do not understand).
    Empty,
    /// A fetchable http(s) url — Qobuz CDN covers, and Plex thumbs that have
    /// already been tokenized by [`classify`].
    Http(String),
    /// A file ALREADY ON DISK: the RAW filesystem path (no `file://`, no
    /// percent-encoding). Local Library covers, `qbz-library` thumbnails and
    /// offline-download covers. Never downloaded.
    LocalFile(String),
    /// A Plex server-relative thumb resolved against the configured server:
    /// the tokenized fetch url, handled exactly like [`ArtUrl::Http`] from
    /// here on (so its cache entry is shared with the Local Library grid).
    Plex(String),
    /// A Plex thumb with no usable server / token — unresolvable until the
    /// user connects Plex. Distinct from [`ArtUrl::Empty`] so the miss is
    /// logged for what it is instead of looking like a dead download.
    PlexUnconfigured,
}

/// Classify an `art_url`. Cheap for every class except a Plex path, which
/// reads the Plex settings row to build the tokenized url (memoized by
/// [`cached_path`] afterwards, so this happens once per url).
pub fn classify(url: &str) -> ArtUrl {
    let url = url.trim();
    if url.is_empty() {
        return ArtUrl::Empty;
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return ArtUrl::Http(url.to_string());
    }
    // A `file://` url is ALREADY on disk — reqwest cannot even build a
    // request for it ("builder error for url"), so it must never be fetched.
    if url.starts_with("file://") {
        // `local_queue_track` escapes `%`, `#` and `?` before this value
        // reaches QML. The filesystem probe needs the inverse, otherwise a
        // real `Disc 01 - Soundtrack #01/cover.jpg` is stat'ed as a literal
        // `...%2301/cover.jpg` and every queue-owned surface gets a miss.
        return ArtUrl::LocalFile(local_path(url));
    }
    // A SOURCE-TAGGED token (`jellyfin:<albumId>/<tag>`,
    // `subsonic:al-<id>_<hash>`), stamped by `local_queue_track` for a media
    // server row.
    //
    // This is the ONE arm that does not guess. Every other arm above reads the
    // SHAPE of the string, which works for a filesystem path and a CDN url and
    // is exactly what fails for an opaque token: a Jellyfin image tag looks
    // like nothing in particular, and a Subsonic coverArt id looks like a
    // relative path. So the tag names the owner and the owner is asked —
    // `registry().artwork_token` is the same call the Local Library grid makes,
    // so a cover fetched for a queue row and the same cover in the grid share
    // one cache entry.
    //
    // It is here rather than at the 111 call sites of `cached_path` for the
    // same reason `classify` exists at all: one taxonomy, asked from
    // everywhere. The difference is that this arm delegates instead of
    // sniffing.
    if let Some((word, token)) = url.split_once(':') {
        if let Some(id) = qbz_source::SourceId::from_word(word) {
            if matches!(
                id,
                qbz_source::SourceId::JELLYFIN | qbz_source::SourceId::SUBSONIC
            ) {
                return match qbz_source::registry().artwork_token(
                    id,
                    token,
                    qbz_source::ArtSize::Card,
                ) {
                    qbz_source::ArtRef::Fetch { url, .. } => ArtUrl::Http(url),
                    // The server is not connected. Reuse the Plex arm's answer:
                    // "the art exists, it cannot be resolved right now", so the
                    // miss is logged for what it is instead of looking like a
                    // dead download.
                    qbz_source::ArtRef::Unavailable(_) => ArtUrl::PlexUnconfigured,
                    _ => ArtUrl::Empty,
                };
            }
        }
    }
    // A Plex thumb is server-relative; it needs base url + token to exist.
    // `local_plex::thumb_url` is the SAME builder the Local Library grid
    // uses, so both surfaces produce identical cache keys.
    if crate::local_plex::is_thumb_path(url) {
        let fetch = crate::local_plex::thumb_url(url, Some(PLEX_THUMB_PX));
        return if fetch.is_empty() {
            ArtUrl::PlexUnconfigured
        } else {
            ArtUrl::Plex(fetch)
        };
    }
    if qbz_models::fs_url::is_local_abs_path(url) {
        return ArtUrl::LocalFile(url.to_string());
    }
    ArtUrl::Empty
}

/// `file://` URI for a raw filesystem path.
///
/// Qt's `QUrl` parses `#` as a fragment and `?` as a query, so a cover under
/// `…/Album #1/cover.jpg` resolves to nothing when the path is concatenated
/// raw. Percent-encode exactly those two plus `%` itself (first, or the
/// escapes we add would be double-decoded). Anything already carrying the
/// scheme is passed through untouched.
/// The inverse of [`file_url`]: a `file://` URI back to a filesystem path.
///
/// `trim_start_matches("file://")` is NOT the inverse — it leaves the three
/// percent-escapes in place, so any path holding `%`, `#` or `?` comes back as
/// a name that does not exist on disk. That silently broke whatever opened the
/// result (a tint decode, a copy) for exactly the files the escaping exists to
/// support.
pub fn local_path(url: &str) -> String {
    qbz_models::fs_url::local_path(url)
}

pub fn file_url(path: &str) -> String {
    qbz_models::fs_url::file_url(path)
}

// ---------------------------------------------------------------------------
// Memoized disk-cache lookup
// ---------------------------------------------------------------------------

fn memo_get(key: &str) -> Option<PathBuf> {
    let hit = RESOLVED.read().ok()?.get(key).cloned()?;
    if hit.is_file() {
        return Some(hit);
    }
    // Evicted underneath us (the cache dir is shared with the other
    // frontends) — forget it and let the caller re-resolve / re-download.
    if let Ok(mut memo) = RESOLVED.write() {
        memo.remove(key);
    }
    None
}

fn memo_put(key: &str, path: PathBuf) {
    if let Ok(mut memo) = RESOLVED.write() {
        if memo.len() >= MEMO_CAP {
            memo.clear();
        }
        memo.insert(key.to_string(), path);
    }
}

/// The cached file for `fetch`, memoized under the caller's raw `key`.
/// `key` is what repeats across window passes (a `/library/...` path stays
/// the same while its tokenized url is rebuilt every time), so it is the
/// memo key; `fetch` is the http url the image cache is keyed by.
fn disk_path(key: &str, fetch: &str) -> Option<PathBuf> {
    if let Some(path) = memo_get(key) {
        return Some(path);
    }
    let path = cache().and_then(|c| c.lock().ok()?.get(fetch))?;
    // A ZERO-BYTE entry is a failed download that still landed a file, and the
    // cache counts it as a hit FOREVER — that cover is then permanently broken
    // and QML logs `Error decoding: ....img` on every paint. Treat it as a
    // miss (and do not memo it), so the next window pass re-fetches it. The
    // local arm of `cached_path` already stats for the same class of problem;
    // this is the remote arm's half. Found live: one 0-byte file in the image
    // cache, one undecodable sidebar cover (2026-07-31).
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > 0 => {}
        Ok(_) => {
            log::debug!("[qbz-qt] artwork: empty cache entry for {fetch}, re-fetching");
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Err(_) => return None,
    }
    memo_put(key, path.clone());
    Some(path)
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolve any art url to a `file://` path QML can decode ("" when it cannot
/// be resolved without the network, or at all). SYNCHRONOUS — no download,
/// and on a repeat call no SQLite either.
pub fn cached_path(url: &str) -> String {
    // A custom cover registered for this artwork's hash wins over every
    // cached/remote variant (cover_artwork_qt::override_for_url), so cards,
    // the NPB, the queue and mosaics all render the user's art.
    let custom = crate::cover_artwork_qt::override_for_url(url);
    if !custom.is_empty() {
        return file_url(&custom);
    }
    match classify(url) {
        ArtUrl::Empty | ArtUrl::PlexUnconfigured => String::new(),
        // Already on disk: this is the whole fix for local covers. Stat it so
        // a stale library row yields "" (placeholder) instead of a broken
        // Image source.
        ArtUrl::LocalFile(path) => {
            if Path::new(&path).is_file() {
                file_url(&path)
            } else {
                String::new()
            }
        }
        ArtUrl::Http(fetch) | ArtUrl::Plex(fetch) => disk_path(url, &fetch)
            .map(|p| file_url(&p.to_string_lossy()))
            .unwrap_or_default(),
    }
}

/// [`cached_path`] for a cover whose `(cache_key, fetch url)` pair is ALREADY
/// resolved — i.e. one that arrived as a `qbz_source::ArtRef::Fetch`.
///
/// The difference is only that it does not call [`classify`], because it does
/// not have to: the source that owns the row already said what its token means
/// (design 02 §9 stage 4). `classify` remains for the surfaces that still hand
/// this module a bare string.
///
/// The `cache_key` keeps doing both jobs it did before — the custom-cover
/// override is looked up under it, and it is the memo key `disk_path` stores
/// beside the fetch url — so a cover cached by the old code path is still a
/// hit.
pub fn cached_path_for(cache_key: &str, fetch: &str) -> String {
    let custom = crate::cover_artwork_qt::override_for_url(cache_key);
    if !custom.is_empty() {
        return file_url(&custom);
    }
    disk_path(cache_key, fetch)
        .map(|p| file_url(&p.to_string_lossy()))
        .unwrap_or_default()
}

/// [`download_missing`] for pairs that need no classification: each job is
/// `(cache_key, fetch url)`, deduped by the FETCH url so two rows sharing a
/// cover download it once.
pub async fn download_pairs(pairs: Vec<(String, String)>) {
    let mut jobs: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (key, fetch) in pairs {
        // Overridden art never needs a fetch — `cached_path_for` answers it.
        if !crate::cover_artwork_qt::override_for_url(&key).is_empty() {
            continue;
        }
        if fetch.is_empty() {
            continue;
        }
        if seen.insert(fetch.clone()) {
            jobs.push((key, fetch));
        }
    }
    fetch_jobs(jobs).await;
}

/// The cached file for a FETCHABLE cover (http/https, and Plex thumbs once
/// tokenized) as a RAW filesystem path — `None` when it is not on disk.
///
/// The remote arm of [`cached_path`] without its `file_url` step. It exists
/// because a consumer that needs a DIFFERENT URI form cannot un-escape what
/// `file_url` produced: `media_controls_qt` builds `mpris:artUrl` through
/// `qbz_models::ArtworkRef::to_mpris_url` (full percent-encoding via
/// `url::Url::from_file_path`), and `file_url` escapes only `%`, `#` and `?`.
/// Same synchronous, no-download contract as [`cached_path`]: memo first, then
/// one SQLite hit, and a zero-byte entry counts as a miss.
///
/// Local classes deliberately return `None` — they were never in the disk
/// cache to begin with, and their caller already has the path.
pub fn cached_raw_path(url: &str) -> Option<PathBuf> {
    // The custom-cover override answers here too (MPRIS art and the other
    // raw-path consumers must not show the Qobuz art the UI replaced).
    let custom = crate::cover_artwork_qt::override_for_url(url);
    if !custom.is_empty() {
        return Some(PathBuf::from(custom));
    }
    match classify(url) {
        ArtUrl::Http(fetch) | ArtUrl::Plex(fetch) => disk_path(url, &fetch),
        ArtUrl::Empty | ArtUrl::PlexUnconfigured | ArtUrl::LocalFile(_) => None,
    }
}

/// Fill `art_path` from the disk cache for every card that already has one.
/// Returns the urls still missing (deduped, non-empty).
pub fn attach_cached(sections: &mut [HomeSection]) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Apply the user's custom cover / portrait overrides before resolving art,
    // so every Home surface (all tabs, recommendations, pinned/recent rails)
    // shows them on cards — not just the album/artist pages. ONE store read for
    // the whole pass; a profile with no overrides skips it entirely. Album
    // rails key by id, artist rails ("artist"/"artists") by display name (the
    // card title). Other kinds pass through the album lookup, a harmless no-op
    // (a playlist/track/radio id is never a registered album key).
    let overrides = crate::cover_artwork_qt::override_snapshot();
    let has_overrides = !overrides.is_empty();
    for section in sections.iter_mut() {
        let is_artist = section.kind == "artist" || section.kind == "artists";
        for card in section.items.iter_mut() {
            if has_overrides {
                let fallback = std::mem::take(&mut card.art_url);
                card.art_url = if is_artist {
                    overrides.artist(&card.title, fallback)
                } else {
                    overrides.album(&card.id, fallback)
                };
            }
            if card.art_url.is_empty() {
                continue;
            }
            let hit = cached_path(&card.art_url);
            if !hit.is_empty() {
                card.art_path = hit;
            } else if seen.insert(card.art_url.clone()) {
                missing.push(card.art_url.clone());
            }
        }
    }
    missing
}

/// Download whatever in `urls` is actually fetchable, storing each into the
/// image cache. Local files are skipped (they are already on disk) and Plex
/// paths are tokenized first — neither is ever handed to reqwest. Returns
/// when all downloads settled (failures are logged and skipped — the cards
/// keep their placeholder).
type ArtworkReady = Arc<dyn Fn(String, String) + Send + Sync>;

pub async fn download_missing(urls: Vec<String>) {
    download_missing_inner(urls, None).await;
}

/// Publish each resolved cover without waiting for the slowest sibling.
/// Uses the same jobs and concurrency limit as the batch-only caller.
pub async fn download_missing_progressive(
    urls: Vec<String>,
    ready: impl Fn(String, String) + Send + Sync + 'static,
) {
    download_missing_inner(urls, Some(Arc::new(ready))).await;
}

async fn download_missing_inner(urls: Vec<String>, ready: Option<ArtworkReady>) {
    // (memo key = the caller's raw url, http url to GET), deduped by the
    // fetch url so two rows sharing a cover download it once.
    let mut jobs: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut primary_by_fetch = std::collections::HashMap::<String, String>::new();
    let mut aliases = std::collections::HashMap::<String, Vec<String>>::new();
    for url in urls {
        // Overridden art never needs a fetch — cached_path answers it.
        if !crate::cover_artwork_qt::override_for_url(&url).is_empty() {
            continue;
        }
        match classify(&url) {
            ArtUrl::Http(fetch) | ArtUrl::Plex(fetch) => {
                if ready.is_some() {
                    let primary = primary_by_fetch
                        .entry(fetch.clone())
                        .or_insert_with(|| url.clone());
                    let group = aliases.entry(primary.clone()).or_default();
                    if !group.contains(&url) {
                        group.push(url.clone());
                    }
                }
                if seen.insert(fetch.clone()) {
                    jobs.push((url, fetch));
                }
            }
            // Already on disk — `cached_path` resolved it synchronously.
            ArtUrl::LocalFile(_) => {}
            ArtUrl::PlexUnconfigured => {
                log::debug!("[qbz-qt] plex artwork unresolvable (no server/token): {url}");
            }
            ArtUrl::Empty => {}
        }
    }
    let ready = ready.map(|callback| {
        Arc::new(move |key: String, path: String| {
            if let Some(keys) = aliases.get(&key) {
                for alias in keys {
                    callback(alias.clone(), path.clone());
                }
            }
        }) as ArtworkReady
    });
    fetch_jobs_with_ready(jobs, ready).await;
}

/// The shared download loop: `(cache_key, fetch url)` jobs, already deduped.
///
/// Split out of [`download_missing`] so [`download_pairs`] — the
/// classification-free entry the `ArtRef::Fetch` pipeline uses — reuses the
/// bounded pool, the error-page guard and the cache store rather than growing
/// a second copy of them.
async fn fetch_jobs(jobs: Vec<(String, String)>) {
    fetch_jobs_with_ready(jobs, None).await;
}

async fn fetch_jobs_with_ready(jobs: Vec<(String, String)>, ready: Option<ArtworkReady>) {
    if jobs.is_empty() {
        return;
    }
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let mut handles = Vec::with_capacity(jobs.len());
    for (key, url) in jobs {
        let sem = Arc::clone(&semaphore);
        let ready = ready.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore open");
            // A concurrent pass may have stored it while we queued.
            if let Some(path) = disk_path(&key, &url) {
                if let Some(ready) = &ready {
                    ready(key, file_url(&path.to_string_lossy()));
                }
                return;
            }
            let response = match HTTP.get(&url).send().await {
                Ok(resp) => resp,
                Err(e) => {
                    log::warn!("[qbz-qt] artwork download failed ({url}): {e}");
                    return;
                }
            };
            // Without this an error page (a 401 from Plex, a 404 from the
            // CDN) is stored AS the cover and the card stays grey forever.
            let response = match response.error_for_status() {
                Ok(resp) => resp,
                Err(e) => {
                    log::warn!("[qbz-qt] artwork download rejected ({url}): {e}");
                    return;
                }
            };
            let bytes = match response.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("[qbz-qt] artwork read failed ({url}): {e}");
                    return;
                }
            };
            if bytes.is_empty() {
                log::warn!("[qbz-qt] artwork empty body ({url})");
                return;
            }
            let Some(cache) = cache() else {
                return;
            };
            let stored = cache
                .lock()
                .ok()
                .and_then(|guard| match guard.store(&url, &bytes) {
                    Ok(path) => Some(path),
                    Err(e) => {
                        log::warn!("[qbz-qt] artwork store failed ({url}): {e}");
                        None
                    }
                });
            if let Some(path) = stored {
                note_stored(bytes.len());
                // Seed the memo so the republish that follows resolves from
                // RAM instead of re-querying SQLite for every card.
                memo_put(&key, path.clone());
                if let Some(ready) = &ready {
                    ready(key, file_url(&path.to_string_lossy()));
                }
            }
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

// ---------------------------------------------------------------------------
// Now-playing artwork (phase 4)
// ---------------------------------------------------------------------------

/// The pixel tier the now-playing feed (`npArtworkPath`) resolves Qobuz
/// covers at. The queue persists WHATEVER variant the building surface
/// picked — a restored session can carry the 50px `small` (measured in the
/// owner's image cache 2026-08-15: 50x50 JPEGs, pixelated art on the NPB,
/// MPRIS and immersive). 600 covers every consumer of this feed (NPB large,
/// the preview overlay, the sidebar dock, queue rows through the derivative
/// layer); bigger immersive slots are served by the large feed below.
const NP_FEED_PX: u32 = 600;

/// The url the now-playing feed actually resolves: a sized Qobuz cover is
/// rewritten to [`NP_FEED_PX`] (`qbz_models::qobuz_cover_at_px`); everything
/// else (local files, Plex thumbs, unsized urls) passes through. A custom
/// cover override is keyed on the url the surfaces carry, so its presence
/// pins the original — the rewrite must not route around it.
fn np_feed_url(url: &str) -> String {
    if !crate::cover_artwork_qt::override_for_url(url).is_empty() {
        return url.to_string();
    }
    qbz_models::qobuz_cover_at_px(url, NP_FEED_PX).unwrap_or_else(|| url.to_string())
}

/// Attach the current track's artwork to the NowPlayingModel: a disk hit (or
/// a local file, which is always a hit) applies immediately; a miss downloads
/// in the background and then republishes both Qt and MPRIS metadata.
pub fn attach_now_playing(track: &qbz_models::QueueTrack, title: &str, album: &str) {
    // D2 track edge: the size-aware large feed belongs to the track that just
    // ended. Clear it, and re-resolve at the pending bucket when a panel has
    // one outstanding (the QML side only calls `requestNpArtworkSize` on
    // bucket/visibility changes, so without this kick a track change with a
    // settled window would leave the immersive art on the small feed).
    restart_now_playing_large();
    let Some(url) = track.artwork_url.as_deref().filter(|url| !url.is_empty()) else {
        crate::now_playing::set_artwork_path(String::new());
        return;
    };
    let feed = np_feed_url(url);
    let hit = cached_path(&feed);
    if !hit.is_empty() {
        crate::now_playing::set_artwork_path(hit);
        return;
    }
    // Nothing fetchable (a missing local cover, Plex not connected): clear
    // the bar instead of leaving the previous track's cover behind.
    if matches!(
        classify(&feed),
        ArtUrl::Empty | ArtUrl::LocalFile(_) | ArtUrl::PlexUnconfigured
    ) {
        crate::now_playing::set_artwork_path(String::new());
        return;
    }
    let expected_track_id = track.id;
    let track = track.clone();
    let title = title.to_string();
    let album = album.to_string();
    crate::spawn(async move {
        download_missing(vec![feed.clone()]).await;
        let path = cached_path(&feed);
        // Downloads can outlive their track. Publishing an old completion
        // would paint the next song with the previous cover and would also
        // overwrite its MPRIS metadata, so both consumers share this guard.
        if path.is_empty() || crate::now_playing::art_seed().track_id != expected_track_id {
            return;
        }
        crate::now_playing::set_artwork_path(path);
        crate::media_controls_qt::refresh_now_playing_artwork(&track, &title, &album);
    });
}

// ---------------------------------------------------------------------------
// Size-aware LARGE now-playing art (contract 2026-08-15-immersive-completion
// 04 §4 — workstream D2)
// ---------------------------------------------------------------------------
//
// `npArtworkPath` stays the fallback feed — the queue-build sites' best()
// pick, with sized Qobuz covers rewritten UP to [`NP_FEED_PX`] (a restored
// queue can carry the 50px `small`; owner smoke 2026-08-15). The immersive
// panels (Static / AlbumReactive /
// SPLIT) compute their slot size, call `QbzPlayer.requestNpArtworkSize(px)`,
// and bind `npArtworkPathLarge` with a fallback to `npArtworkPath` (the Slint
// `artwork-large.width > 0 ? artwork-large : artwork` pattern). Re-resolution
// is BUCKETED on `qbz_models::ImageSet::bucket_for_px` — one landing (and one
// notify) per size tier crossed, never per resize pixel (pulse law), and a
// SHRINK never re-fetches: the already-cached larger file downscales through
// the RoundedImage derivative layer.

/// album id -> the full Qobuz variant set, registered at queue-build time
/// (the only place the `ImageSet` still exists — `QueueTrack` flattens it to
/// one url). Capped, cleared wholesale: an accelerator, not a source of
/// truth; a miss just keeps the large feed on the small-feed url.
static NP_VARIANTS: LazyLock<RwLock<HashMap<String, qbz_models::ImageSet>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

const NP_VARIANTS_CAP: usize = 512;

/// Queue-build sites call this with the album's full `ImageSet` so the large
/// feed can re-pick the variant per size bucket later.
pub fn note_np_variants(album_id: &str, image: &qbz_models::ImageSet) {
    if album_id.is_empty() || image.best().is_none() {
        return;
    }
    if let Ok(mut map) = NP_VARIANTS.write() {
        if map.len() >= NP_VARIANTS_CAP {
            map.clear();
        }
        map.insert(album_id.to_string(), image.clone());
    }
}

fn np_variants(album_id: &str) -> Option<qbz_models::ImageSet> {
    NP_VARIANTS.read().ok()?.get(album_id).cloned()
}

/// The last large-art request: which track it serves and the biggest bucket
/// requested for it. `bucket == 0` = no panel has asked for large art this
/// session (the common case: immersive never opened), so track edges cost
/// nothing.
struct LargeReq {
    track_id: u64,
    bucket: u32,
}

static NP_LARGE: Mutex<LargeReq> = Mutex::new(LargeReq {
    track_id: 0,
    bucket: 0,
});

/// Registry-miss rewrites that already failed this session (a 404 = the
/// album predates the mega tier; a network failure pins the same way, and
/// self-heals on the next launch). One miss per url EVER: without this a
/// pre-mega album would cost a 404 on EVERY track change — the reason the
/// rewrite used to be capped at [`NP_FEED_PX`]. The cap is what left
/// restored-queue art soft on the big slots: a restored session carries no
/// `ImageSet`, every smoke of the owner starts from one, and 600px into a
/// ~1000px split slot reads as pixelation (owner smoke 2026-08-15 night).
static NP_LARGE_NEG: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// `QbzPlayer.requestNpArtworkSize(px)` — a panel reports its art slot size
/// (CSS px; the same expression as its artProbe cap, minus the native clamp,
/// which stays the final safety). Bucketed + deduped: a resize drag costs at
/// most one resolve per tier crossed, and requests at or below the tier
/// already resolved for this track are no-ops.
pub fn request_now_playing_art(px: i32) {
    if px <= 0 {
        return;
    }
    let bucket = qbz_models::ImageSet::bucket_for_px(px as u32);
    let seed = crate::now_playing::art_seed();
    if seed.url.is_empty() {
        return;
    }
    {
        let mut lr = NP_LARGE.lock().unwrap();
        if lr.track_id == seed.track_id && bucket <= lr.bucket {
            return;
        }
        lr.track_id = seed.track_id;
        lr.bucket = bucket;
    }
    resolve_np_large(seed, bucket);
}

/// Track edge (called from [`attach_now_playing`]): drop the previous track's
/// large path and re-resolve at the pending bucket, if any.
fn restart_now_playing_large() {
    crate::now_playing::set_artwork_path_large(String::new());
    let bucket = NP_LARGE.lock().unwrap().bucket;
    if bucket == 0 {
        return;
    }
    let seed = crate::now_playing::art_seed();
    if seed.url.is_empty() {
        return;
    }
    NP_LARGE.lock().unwrap().track_id = seed.track_id;
    resolve_np_large(seed, bucket);
}

/// Kick the async resolve; the landing publishes ONLY if the track is still
/// current (a slow Plex transcode must not stamp the previous track's art
/// onto the new one).
fn resolve_np_large(seed: crate::now_playing::ArtSeed, bucket: u32) {
    crate::spawn(async move {
        let path = resolve_large(&seed, bucket).await;
        if crate::now_playing::art_seed().track_id == seed.track_id {
            // "" on failure: the QML fallback keeps showing npArtworkPath.
            crate::now_playing::set_artwork_path_large(path);
        }
    });
}

/// Where the large art for `seed` at size `bucket` comes from:
///  - Qobuz (registry hit): the `ImageSet::for_px` variant url, through the
///    shared disk cache like any remote cover;
///  - Qobuz (registry miss — a restored queue keeps no `ImageSet`): the
///    seed url's size suffix rewritten to `min(bucket, 600)`
///    (`qbz_models::qobuz_cover_at_px`);
///  - Plex: the 1024px transcode tier ([`PLEX_THUMB_PX_LARGE`]);
///  - Local: the on-demand 1600px tier (`local_artwork::large_art_blocking`);
///  - anything else: "" — the fallback shows the small feed.
async fn resolve_large(seed: &crate::now_playing::ArtSeed, bucket: u32) -> String {
    if !matches!(seed.source.as_str(), "local" | "plex") {
        if let Some(set) = np_variants(&seed.album_id) {
            if let Some(url) = set.for_px(bucket).cloned() {
                let hit = cached_path(&url);
                if !hit.is_empty() {
                    return hit;
                }
                download_missing(vec![url.clone()]).await;
                return cached_path(&url);
            }
        }
        // Registry miss (a restored queue flattens tracks to ONE url, so no
        // ImageSet survives): a sized Qobuz cover still upgrades by suffix
        // rewrite, now to the FULL requested bucket — the 600 cap this used
        // to have left restored-queue art soft on every big slot (see
        // NP_LARGE_NEG). A 404 (a pre-mega album) is paid ONCE per url per
        // session thanks to the negative set, never per track change. A
        // custom cover override answers the small feed already, so the large
        // feed stays empty and the QML fallback shows it.
        if !crate::cover_artwork_qt::override_for_url(&seed.url).is_empty() {
            return String::new();
        }
        if let Some(upgraded) = qbz_models::qobuz_cover_at_px(&seed.url, bucket) {
            if NP_LARGE_NEG
                .read()
                .ok()
                .is_some_and(|n| n.contains(&upgraded))
            {
                return String::new();
            }
            let hit = cached_path(&upgraded);
            if !hit.is_empty() {
                return hit;
            }
            download_missing(vec![upgraded.clone()]).await;
            let got = cached_path(&upgraded);
            if got.is_empty() {
                if let Ok(mut n) = NP_LARGE_NEG.write() {
                    n.insert(upgraded);
                }
            }
            return got;
        }
        return String::new();
    }
    // Plex: classify() hardcodes the 256 tier, so the large tier tokenizes
    // apart under a size-suffixed memo key (the transcode url itself carries
    // `width=`, so cache entries separate naturally).
    if crate::local_plex::is_thumb_path(&seed.url) {
        let key = format!("{}@{}", seed.url, PLEX_THUMB_PX_LARGE);
        let fetch = crate::local_plex::thumb_url(&seed.url, Some(PLEX_THUMB_PX_LARGE));
        if fetch.is_empty() {
            return String::new();
        }
        if let Some(p) = disk_path(&key, &fetch) {
            return file_url(&p.to_string_lossy());
        }
        download_one(key.clone(), fetch.clone()).await;
        return disk_path(&key, &fetch)
            .map(|p| file_url(&p.to_string_lossy()))
            .unwrap_or_default();
    }
    // Local / offline file: the on-demand large tier (bounded decode, native
    // when smaller). Blocking — off the async executor.
    if let ArtUrl::LocalFile(path) = classify(&seed.url) {
        let track_id = (seed.source == "local").then_some(seed.track_id);
        let resolved = tokio::task::spawn_blocking(move || {
            crate::local_artwork::large_art_blocking(&path, track_id)
        })
        .await
        .ok()
        .flatten();
        return resolved
            .map(|p| file_url(&p.to_string_lossy()))
            .unwrap_or_default();
    }
    String::new()
}

/// Single-url download into the shared cache — the large feed's fetch (the
/// batched [`download_missing`] re-classifies urls, which would tokenize a
/// Plex path at the WRONG tier; this takes the final fetch url directly).
/// Same discipline as the batched body: status check, empty-body reject,
/// zero-byte entries count as misses on the next read.
async fn download_one(key: String, fetch: String) {
    if disk_path(&key, &fetch).is_some() {
        return;
    }
    let response = match HTTP.get(&fetch).send().await {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("[qbz-qt] artwork download failed ({fetch}): {e}");
            return;
        }
    };
    let response = match response.error_for_status() {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("[qbz-qt] artwork download rejected ({fetch}): {e}");
            return;
        }
    };
    let bytes = match response.bytes().await {
        Ok(b) if !b.is_empty() => b,
        _ => return,
    };
    let Some(cache) = cache() else {
        return;
    };
    let stored = cache
        .lock()
        .ok()
        .and_then(|guard| match guard.store(&fetch, &bytes) {
            Ok(path) => Some(path),
            Err(e) => {
                log::warn!("[qbz-qt] artwork store failed ({fetch}): {e}");
                None
            }
        });
    if let Some(path) = stored {
        note_stored(bytes.len());
        memo_put(&key, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with a temporary XDG_CACHE_HOME and QBZ_ARTWORK_TEST_ISOLATED=1.
    /// The image cache is process-global, so this deliberately runs alone.
    #[tokio::test]
    #[ignore = "requires an isolated cache profile; run with --ignored --test-threads=1"]
    async fn progressive_cover_is_published_before_slow_sibling() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        assert_eq!(
            std::env::var("QBZ_ARTWORK_TEST_ISOLATED").as_deref(),
            Ok("1")
        );
        qbz_app::ensure_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let release_slow = Arc::new(tokio::sync::Notify::new());
        let gate = release_slow.clone();
        let server = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let gate = gate.clone();
                peers.spawn(async move {
                    let mut request = [0; 2048];
                    let count = socket.read(&mut request).await.unwrap();
                    if String::from_utf8_lossy(&request[..count]).contains("/slow ") {
                        gate.notified().await;
                    }
                    let image = b"P6\n1 1\n255\n\xff\x00\x00";
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        image.len()
                    );
                    socket.write_all(headers.as_bytes()).await.unwrap();
                    socket.write_all(image).await.unwrap();
                });
            }
            while let Some(result) = peers.join_next().await {
                result.unwrap();
            }
        });
        let fast = format!("{base}/fast");
        let slow = format!("{base}/slow");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let urls = vec![slow.clone(), fast.clone(), fast.clone()];
        let download = tokio::spawn(download_missing_progressive(urls, move |key, path| {
            tx.send((key, path)).unwrap();
        }));
        let first = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("fast cover must not wait for slow cover")
            .unwrap();
        assert_eq!(first.0, fast);
        assert!(!first.1.is_empty());
        assert!(!download.is_finished());
        release_slow.notify_one();
        let second = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.0, slow);
        download.await.unwrap();
        server.await.unwrap();
        assert!(rx.recv().await.is_none(), "duplicate URLs must emit once");
    }

    #[test]
    fn a_trim_is_due_once_the_stored_bytes_cross_the_step() {
        let counter = AtomicU64::new(0);
        assert!(!trim_due_after(&counter, TRIM_EVERY_BYTES / 2));
        assert!(!trim_due_after(&counter, TRIM_EVERY_BYTES / 2 - 1));
        assert!(trim_due_after(&counter, 1));
    }

    #[test]
    fn the_shared_budget_defaults_to_200_mb() {
        assert_eq!(DEFAULT_SHARED_BUDGET_MB * 1024 * 1024, 200 * 1024 * 1024);
    }

    #[test]
    fn http_urls_are_fetchable() {
        assert_eq!(
            classify("https://static.qobuz.com/images/covers/a.jpg"),
            ArtUrl::Http("https://static.qobuz.com/images/covers/a.jpg".into())
        );
    }

    #[test]
    fn file_urls_resolve_to_a_raw_path_and_are_never_fetched() {
        assert_eq!(
            classify("file:///home/u/.local/share/qbz/thumbnails/deadbeef.jpg"),
            ArtUrl::LocalFile("/home/u/.local/share/qbz/thumbnails/deadbeef.jpg".into())
        );
        assert_eq!(
            classify("/home/u/Music/Album/cover.jpg"),
            ArtUrl::LocalFile("/home/u/Music/Album/cover.jpg".into())
        );
        let raw = "/mnt/music/Disc 01 - Soundtrack #01/100%? cover.jpg";
        assert_eq!(classify(&file_url(raw)), ArtUrl::LocalFile(raw.into()));
    }

    #[test]
    fn relative_and_empty_urls_are_empty() {
        // A SOURCE-TAGGED media token is DELEGATED, never sniffed. Untagged, a
        // Jellyfin `<albumId>/<tag>` and a Subsonic `al-x_y` are both shapes
        // this function would have called `Empty` or a filesystem path — which
        // is why the now-playing bar, the queue and MPRIS showed no cover for a
        // media-server track even though the grid did.
        //
        // Disconnected answers `PlexUnconfigured` rather than `Empty` for the
        // same reason the Plex arm does: the art EXISTS, it cannot be resolved
        // right now, and the two must stay distinguishable. Nothing is
        // connected in a unit test, so that is what this asserts — the
        // CONNECTED shape is covered where the credentials live
        // (`JellyfinSource::artwork_token`).
        assert_eq!(
            classify("jellyfin:alb-1/deadbeef"),
            ArtUrl::PlexUnconfigured
        );
        assert_eq!(
            classify("subsonic:al-abc_59fec8ff"),
            ArtUrl::PlexUnconfigured
        );
        // A brand spelling folds the same way the source words do.
        assert_eq!(classify("navidrome:al-abc"), ArtUrl::PlexUnconfigured);
        // A Subsonic track cover id contains its own colon; only the FIRST
        // separator may be consumed.
        assert_eq!(classify("subsonic:dc-abc:1_0"), ArtUrl::PlexUnconfigured);
        // And the arm must not swallow anything that merely contains a colon.
        assert_eq!(
            classify("https://static.qobuz.com/images/covers/a.jpg"),
            ArtUrl::Http("https://static.qobuz.com/images/covers/a.jpg".into())
        );
        assert_eq!(classify("plex:whatever"), ArtUrl::Empty);
        assert_eq!(classify(""), ArtUrl::Empty);
        assert_eq!(classify("   "), ArtUrl::Empty);
        assert_eq!(classify("covers/a.jpg"), ArtUrl::Empty);
    }

    #[test]
    fn file_url_escapes_what_qurl_would_eat() {
        assert_eq!(file_url("/m/Album #1/c.jpg"), "file:///m/Album %231/c.jpg");
        assert_eq!(file_url("/m/a?b/c.jpg"), "file:///m/a%3Fb/c.jpg");
        assert_eq!(file_url("/m/100%/c.jpg"), "file:///m/100%25/c.jpg");
        assert_eq!(file_url("file:///m/a.jpg"), "file:///m/a.jpg");
    }

    #[test]
    fn missing_local_file_resolves_to_placeholder() {
        assert_eq!(cached_path("/nonexistent/qbz/cover.jpg"), "");
    }

    #[test]
    fn np_variant_registry_round_trips_and_skips_empty_sets() {
        let set = qbz_models::ImageSet {
            thumbnail: Some("t150".into()),
            mega: Some("m3000".into()),
            ..Default::default()
        };
        note_np_variants("alb-1", &set);
        let got = np_variants("alb-1").expect("registered");
        assert_eq!(got.for_px(660).map(String::as_str), Some("m3000"));
        assert_eq!(got.for_px(72).map(String::as_str), Some("t150"));
        // Empty ids and url-less sets are not registered.
        note_np_variants("", &set);
        note_np_variants("alb-2", &qbz_models::ImageSet::default());
        assert!(np_variants("").is_none());
        assert!(np_variants("alb-2").is_none());
    }
}

// ---------------------------------------------------------------------------
// Scaled derivatives
// ---------------------------------------------------------------------------

/// The deterministic derivative path for one already-computed draw size.
///
/// Kept separate from [`scaled_path`] so the Qt GUI thread can perform the
/// cheap warm-cache probe below without decoding the original, creating the
/// directory, or touching the derivative's mtime.  The expensive/mutating arm
/// remains on `spawn_blocking` through `QbzSession.artScaled()`.
fn scaled_output_path(path: &str, w: u32, h: u32) -> Option<(String, PathBuf)> {
    if w == 0 || h == 0 {
        return None;
    }
    // NOT strip_prefix: leaves the `/` before a Windows drive letter, and the
    // three percent-escapes, in a string used BOTH as the cache key and as
    // the path to open.
    let src = qbz_models::fs_url::local_path(path);
    let dir = dirs::cache_dir()?.join("qbz").join("images").join("scaled");

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&src, &mut hasher);
    let out = dir.join(format!(
        "{:016x}_{w}x{h}.png",
        std::hash::Hasher::finish(&hasher)
    ));
    Some((src, out))
}

/// Return an existing derivative without doing any image work.
///
/// `RoundedImage` calls this after its dimension probe computes the exact
/// aspect-preserving draw size.  On a warm disk cache the answer is available
/// in the SAME GUI event that made the original probe Ready, so the visible
/// `Image` switches to the small derivative before the next scene-graph frame
/// instead of painting/uploading the original and replacing that texture one
/// or more event-loop turns later.  A miss is deliberately just one local
/// `stat`; generation still goes through [`scaled_path`] off-thread.
pub fn cached_scaled_path(path: &str, w: u32, h: u32) -> Option<PathBuf> {
    let (_, out) = scaled_output_path(path, w, h)?;
    out.is_file().then_some(out)
}

/// A cover re-encoded to fit inside `w`x`h`, ASPECT PRESERVED. Canvas
/// `drawImage` does not filter when it scales — a 600px cover drawn into a
/// 200px cell throws away 8 of every 9 pixels, which is the aliasing the
/// owner sees as poor render quality (measured: the card raster is
/// bit-for-bit identical to an ImageMagick `-filter Point` reference,
/// RMSE 0.000). Handing the Canvas a file that is ALREADY the drawn size
/// makes the draw 1:1 and removes the resample entirely.
///
/// Three properties are load-bearing, and the first version of this function
/// had none of them:
///
///  * **Aspect is preserved** (`DynamicImage::thumbnail`, not
///    `imageops::thumbnail`, which resizes to the exact box and distorts).
///    The old body squashed every non-square request — 46 files on disk are
///    square covers forced into a 32x28 box — and squashed every LOGO into
///    the square `fit: "contain"` box of `LabelCard`. The caller asks for the
///    box the art is DRAWN in; fitting inside it is what makes the draw 1:1
///    for `contain` and for a square cell, and merely soft (never distorted)
///    in the one remaining case, a non-square crop cell.
///  * **Alpha survives.** The old body went through `to_rgb8()`, so a label
///    logo shipped as a transparent PNG came back matted onto black. PNG out,
///    same colour type in as out.
///  * **The re-encode is LOSSLESS.** The old body wrote JPEG at the crate
///    default (quality 75 — verified against the quantisation tables of the
///    files already in `~/.cache/qbz/images/scaled`), which costs more than
///    the resample it was meant to fix: measured against a Lanczos reference
///    of the same cover at 200px, the q75 derivative lands at RMSE 4.222 and
///    the lossless one at 1.303, for 25 KB instead of 6 KB per card cover.
///    At ~800 card-sized derivatives that is ~20 MB of cache — cheap for the
///    only artefact-free option, and it is what makes alpha possible at all.
///
/// Blocking: decodes and resizes. Call from `spawn_blocking`.
pub fn scaled_path(path: &str, w: u32, h: u32) -> Option<std::path::PathBuf> {
    let (src, out) = scaled_output_path(path, w, h)?;
    let dir = out.parent()?;
    std::fs::create_dir_all(&dir).ok()?;
    if out.is_file() {
        // Mark it used so `evict_scaled`'s mtime order is access order, not
        // creation order. Best-effort: a failure only degrades eviction to
        // FIFO, which costs one regeneration. (`File::set_modified` is stable
        // since Rust 1.75; if `cargo check` disagrees, that is the ONE thing
        // to re-check here.)
        let _ = std::fs::File::options()
            .write(true)
            .open(&out)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        return Some(out);
    }

    // The artwork cache stores files as `.img` — `image::open` guesses the
    // format from the EXTENSION and fails on it, which is why not one
    // derivative was ever produced. Sniff the content instead.
    let img = image::ImageReader::open(&src)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    // Upscaling would only cost bytes — hand back the source instead.
    if img.width() <= w && img.height() <= h {
        return Some(std::path::PathBuf::from(src));
    }
    // `DynamicImage::thumbnail` = fit inside (w, h) keeping the aspect ratio,
    // with the same fast box filter the Slint frontend decodes covers through
    // (`artwork.rs` `decode_rgba`). Measured at 200px it is within RMSE 1.3 of
    // a Lanczos reference and carries the same edge energy (Laplacian variance
    // 334.5 vs 337.3), i.e. no visible softening and no ringing.
    // ATOMIC: encode into a unique sibling, then rename. Writing straight to
    // `out` publishes the path the instant the file is created, so a QML
    // `Image` that resolves it mid-encode opens a TRUNCATED png and reports
    // "Unsupported image format" — and Qt caches that failure against the URL,
    // so the cover stays dead for the rest of the session even though the file
    // on disk ends up perfectly valid. Observed in the offscreen smoke
    // (2026-07-31): two covers failed to decode while `file(1)` called the very
    // same paths valid 200x200 PNGs seconds later. `rename` within one
    // directory is atomic, so a reader sees either nothing or the whole image.
    // Same discipline `local_state.rs` already uses for the prefs JSON.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = out.with_extension(format!(
        "part{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if img
        .thumbnail(w, h)
        .save_with_format(&tmp, image::ImageFormat::Png)
        .is_err()
        || std::fs::rename(&tmp, &out).is_err()
    {
        // Never leave a partial behind: `prune` bills the directory by total
        // bytes, so an orphan would eat the budget without ever being a hit.
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(out)
}

/// Byte ceiling for the scaled-derivative cache. Measured 2026-07-29: a warm
/// cache is 261 files / 13.7 MB at 52.6 KB average, and a fully browsed
/// session projects to ~45 MB. 64 MB leaves headroom for ~1200 derivatives
/// (every card size of ~400 distinct covers) while keeping the directory an
/// order of magnitude under the 260 MB the parent image cache already holds.
const MAX_SCALED_BYTES: u64 = 64 * 1024 * 1024;

/// Default byte budget for the SHARED cache (`~/.cache/qbz/images`): the
/// Tauri/Slint default, now a preference (`ui_prefs` `image_cache_max_mb`,
/// Settings > Offline > Artwork cache).
pub(crate) const DEFAULT_SHARED_BUDGET_MB: u64 = 200;

/// The budget in bytes, from the preference (0 / absent = the default).
pub(crate) fn shared_budget_bytes() -> u64 {
    crate::settings_qt::pref_json("image_cache_max_mb")
        .and_then(|v| v.as_u64())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_SHARED_BUDGET_MB)
        * 1024
        * 1024
}

/// Rows deleted per `evict` call; the cache mutex is released between calls.
const EVICT_BATCH_ENTRIES: usize = 64;

/// Bytes stored since the last trim. The Tauri app evicted after EVERY
/// store; here a trim runs on the blocking pool once this many new bytes
/// have landed, so the directory can never sit more than this far above
/// the budget between the boot trim and the next one (an early release
/// grew the folder to 1 GB — never again).
const TRIM_EVERY_BYTES: u64 = 16 * 1024 * 1024;
static STORED_SINCE_TRIM: AtomicU64 = AtomicU64::new(0);
static TRIM_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Account for a freshly stored original. Returns true when a trim is due
/// (the counter is reset by the caller that takes the trim).
fn trim_due_after(counter: &AtomicU64, stored: u64) -> bool {
    counter.fetch_add(stored, Ordering::Relaxed) + stored >= TRIM_EVERY_BYTES
}

fn note_stored(stored: usize) {
    if !trim_due_after(&STORED_SINCE_TRIM, stored as u64) {
        return;
    }
    STORED_SINCE_TRIM.store(0, Ordering::Relaxed);
    trim_shared_now();
}

/// Trim the shared cache to its budget on the blocking pool (a budget
/// change, or the running counter above); one trim in flight at a time.
pub(crate) fn trim_shared_now() {
    if TRIM_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::task::spawn_blocking(|| {
        if let Some(freed) = evict_shared(shared_budget_bytes()) {
            if freed > 0 {
                log::info!("[qbz-qt][perf] image cache trimmed {freed} B (budget {} B)", shared_budget_bytes());
            }
        }
        TRIM_IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

/// (files, bytes) held by the shared cache, for the Settings stats line.
pub(crate) fn shared_stats() -> Option<(u64, u64)> {
    let guard = cache()?.lock().ok()?;
    let stats = guard.stats().ok()?;
    Some((stats.file_count, stats.total_bytes))
}

/// Delete every original in the shared cache (Settings > Clear). Blocking —
/// call from `spawn_blocking`. The memo is dropped too so no row can keep
/// serving a path that is gone.
pub(crate) fn clear_shared() -> Result<u64, String> {
    let cache = cache().ok_or_else(|| "image cache unavailable".to_string())?;
    let freed = cache
        .lock()
        .map_err(|_| "image cache lock poisoned".to_string())?
        .clear()?;
    if let Ok(mut memo) = RESOLVED.write() {
        memo.clear();
    }
    Ok(freed)
}

/// Trim the shared cache to `max_bytes`, LRU by `last_accessed`, in batches.
/// Blocking (SQLite + unlinks) — call from `spawn_blocking`. Returns the
/// bytes freed, `None` when the cache could not be opened (already logged).
fn evict_shared(max_bytes: u64) -> Option<u64> {
    let cache = cache()?;
    let mut freed_total = 0u64;
    loop {
        let batch = {
            let guard = cache.lock().ok()?;
            match guard.evict(max_bytes, EVICT_BATCH_ENTRIES) {
                Ok(batch) => batch,
                Err(e) => {
                    log::warn!("[qbz-qt] image cache eviction failed: {e}");
                    return Some(freed_total);
                }
            }
        };
        freed_total += batch.freed_bytes;
        if batch.evicted_entries == 0 {
            return Some(freed_total);
        }
        std::thread::yield_now();
    }
}

/// Keep `scaled/` bounded: drop the oldest derivatives past
/// [`MAX_SCALED_BYTES`]. Mtime order, oldest first — the same shape as
/// `icon_tint_qt::prune`, with one deliberate difference: `prune` caps on a
/// directory COUNT, this caps on BYTES, because a derivative's size varies 20x
/// with the drawn cell size and a file count says nothing about the footprint.
///
/// Eviction is free of consequence: a re-request regenerates the file, so a
/// wrong victim costs one `thumbnail()` call and never a wrong pixel. The one
/// case that is NOT free is a live `RoundedImage` whose `_scaled` still names
/// a file this just unlinked — that is handled where it can be, on the QML
/// side, by RoundedImage's `Image.Error` -> clear `_scaled` + `_reqKey` arm.
/// Do not land this without that arm.
///
/// Returns (files removed, bytes reclaimed) for the log line the verification
/// counts.
fn evict_scaled(dir: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= MAX_SCALED_BYTES {
        return (0, 0);
    }
    files.sort_by_key(|(t, _, _)| *t);
    let mut over = total - MAX_SCALED_BYTES;
    let (mut n, mut freed) = (0usize, 0u64);
    for (_, len, path) in files {
        if over == 0 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            over = over.saturating_sub(len);
            freed += len;
            n += 1;
        }
    }
    (n, freed)
}

/// ONE-TIME orphan sweep. `scaled_path` wrote `.jpg` before the lossless
/// change to this module's `{hash}_{w}x{h}.png` key; the extension IS the
/// cache key, so every `.jpg` here is unreachable by construction — 1259
/// files / 10.05 MB measured 2026-07-29. Nothing can regenerate them and
/// nothing can serve them, so this is a delete, not an eviction policy.
fn sweep_orphan_scaled(dir: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let (mut n, mut freed) = (0usize, 0u64);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jpg") {
            continue;
        }
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            n += 1;
            freed += len;
        }
    }
    (n, freed)
}

/// Boot-time housekeeping for both image caches — see the module doc
/// "Cache eviction". Blocking; call from `spawn_blocking`.
pub fn housekeeping() {
    // 1. The SHARED cache (full-size originals): LRU trim to the budget.
    let budget = shared_budget_bytes();
    if let Some(freed) = evict_shared(budget) {
        if freed > 0 {
            log::info!("[qbz-qt][perf] image cache evicted {freed} B (budget {budget} B)");
        }
    }
    // 1b. Files the database does not know (a crash mid-store, a lost db):
    //     never evicted by the LRU, so they are swept here — anything under
    //     an hour old is left alone in case its store is still in flight.
    if let Some(cache) = cache() {
        if let Ok(guard) = cache.lock() {
            match guard.sweep_orphans(std::time::Duration::from_secs(3_600)) {
                Ok((files, bytes)) if files > 0 => {
                    log::info!("[qbz-qt][perf] image cache swept {files} orphan files ({bytes} B)");
                }
                Ok(_) => {}
                Err(e) => log::warn!("[qbz-qt] image cache orphan sweep failed: {e}"),
            }
        }
    }
    // 2. This module's OWN derivative directory: orphan sweep, then byte cap.
    let Some(dir) = dirs::cache_dir().map(|d| d.join("qbz").join("images").join("scaled")) else {
        return;
    };
    if !dir.is_dir() {
        return;
    }
    let (orphans, o_bytes) = sweep_orphan_scaled(&dir);
    let (evicted, e_bytes) = evict_scaled(&dir);
    log::info!(
        "[qbz-qt][perf] scaled cache housekeeping: {orphans} orphans ({o_bytes} B), \
         {evicted} evicted ({e_bytes} B)"
    );
}
