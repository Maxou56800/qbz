//! Queue panel controller — Slint-free port of the `QueueState` assembly
//! side of `crates/qbz/src/queue.rs`: NOW PLAYING card, UP NEXT with the
//! #442 "Next in queue" / "Next up" section markers (from the core's
//! `manual_next_count`), 40-row pagination, live search filter, and the
//! History tab. Mutations go straight to the core queue API; every one
//! ends with a republish.
//!
//! OWED PORTS (what the reference has and this file still does not):
//! - QConnect remote reorder — `reorder_upcoming_if_remote` is offered the
//!   move FIRST in the reference (the cloud is authoritative for queue order
//!   while connected). There is no QConnect service in this port yet, so the
//!   local core path is the only one; when the service lands, `move_track`
//!   grows that arm BEFORE `core().move_track`.
//!
//! The immersive coverflow's flat model (the old second owed port) is now
//! fed by `publish_coverflow` below (immersive-port contract §4.4).
//!
//! Row favorite seeds from the phase-5 library feed (Slint uses fav_cache —
//! same truth).

use std::collections::HashSet;
use std::sync::Mutex;

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::QueueTrack;
use serde::Serialize;
use std::sync::Arc;

/// queue.rs PAGE_SIZE.
pub const PAGE_SIZE: usize = 40;

#[derive(Clone, Default, Serialize)]
pub struct QueueRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    pub album: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    pub duration: String,
    /// Numeric twin used by Listen list for aggregate timing. Keeping the
    /// display string avoids moving per-row formatting into QML.
    #[serde(rename = "durationSecs")]
    pub duration_secs: u64,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    pub explicit: bool,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// LOCAL or Plex row (`QueueTrack::is_local`). The heart is NOT
    /// clickable on these — see `toggle_favorite`: their `id` is a
    /// library.db row id, not a Qobuz catalog id.
    #[serde(rename = "isLocal")]
    pub is_local: bool,
    /// Origin word carried by the queue track. TrackRow uses this to keep
    /// catalog and local id spaces apart in its shared context-menu logic.
    pub source: String,
    /// Qobuz Connect accepts catalog-backed ids only. The extended queue keeps
    /// incompatible rows visible as history/sequence context, but greys and
    /// skips them while Connect is active.
    #[serde(rename = "qconnectCompatible")]
    pub qconnect_compatible: bool,
    /// #442 section header drawn ABOVE this row: "" | "next-in-queue" |
    /// "next-up" (only on the unfiltered list).
    pub section: String,
    /// Ephemeral row (a one-shot file play that leaves no trace in QBZ).
    /// The panel drops every PERSISTENCE affordance on these — the heart,
    /// "Add to playlist", "Track info" — exactly as `QueueSidebar.slint`
    /// gates on `item.is-ephemeral`.
    #[serde(rename = "isEphemeral")]
    pub is_ephemeral: bool,
    /// The API said `streamable: false` for THIS track (#638-adjacent, the
    /// unavailable-tracks port). Distinct from a broken local ref.
    ///
    /// The queue panel is where this matters MOST, not least: both queue seams
    /// filter pulled tracks out, so the only way one reaches this list is the
    /// path that bypasses them BY DESIGN — a session restored from disk
    /// (`playback_driver`'s `set_queue_with_order`). Without this field the
    /// backstop's own shopfront renders a dead row as a perfectly normal one,
    /// which is exactly the "learns by clicking" failure the feature exists to
    /// remove.
    #[serde(rename = "qobuzUnavailable")]
    pub qobuz_unavailable: bool,
    /// Offline-cache state, same encoding as `album_qt::TrackRow::cache_status`
    /// (`3` = downloaded). Paired with the flag above: a pulled track we
    /// already downloaded still plays from disk and must NOT get the dead
    /// treatment.
    #[serde(rename = "cacheStatus")]
    pub cache_status: i32,
}

#[derive(Default, Serialize)]
pub struct QueueDoc {
    #[serde(rename = "hasCurrent")]
    pub has_current: bool,
    pub current: Option<QueueRow>,
    pub upcoming: Vec<QueueRow>,
    #[serde(rename = "upcomingTotal")]
    pub upcoming_total: usize,
    #[serde(rename = "upcomingRemaining")]
    pub upcoming_remaining: usize,
    pub history: Vec<QueueRow>,
    pub page: usize,
    #[serde(rename = "pageCount")]
    pub page_count: usize,
    #[serde(rename = "pageStart")]
    pub page_start: usize,
    #[serde(rename = "pageEnd")]
    pub page_end: usize,
    pub shuffle: bool,
    #[serde(rename = "repeatMode")]
    pub repeat_mode: i32,
    /// "Stop after this song" marker — the decimal-string id of the marked
    /// track, or "" for none. Each row compares its own id to this to draw
    /// the CircleStop in place of its track number (`QueueState.stop-after-id`).
    #[serde(rename = "stopAfterId")]
    pub stop_after_id: String,
    /// Whether InfiniteRadio autoplay is on (the ∞ footer button's accent).
    #[serde(rename = "infinitePlay")]
    pub infinite_play: bool,
    /// The live search query, echoed back so the panel can tell "the queue is
    /// empty" apart from "nothing matched" (`QueueState.search-query`).
    #[serde(rename = "searchQuery")]
    pub search_query: String,
}

/// Panel view state (search + page), session-scope like the Slint view.
#[derive(Default)]
struct ViewState {
    search: String,
    page: usize,
}

/// One row in the full queue projection. `phase_index` is deliberately in the
/// core's own coordinate space: history is most-recent-first there even though
/// this view renders it oldest-first, while upcoming is already play order.
#[derive(Clone, Serialize)]
struct ExtendedQueueRow {
    #[serde(flatten)]
    row: QueueRow,
    phase: &'static str,
    #[serde(rename = "phaseIndex")]
    phase_index: usize,
}

#[derive(Default, Serialize)]
struct ExtendedQueueDoc {
    rows: Vec<ExtendedQueueRow>,
    #[serde(rename = "currentIndex")]
    current_index: i32,
    #[serde(rename = "historyCount")]
    history_count: usize,
    #[serde(rename = "upcomingCount")]
    upcoming_count: usize,
    #[serde(rename = "stopAfterId")]
    stop_after_id: String,
    #[serde(rename = "infinitePlay")]
    infinite_play: bool,
    #[serde(rename = "searchQuery")]
    search_query: String,
}

static VIEW: Mutex<ViewState> = Mutex::new(ViewState {
    search: String::new(),
    page: 0,
});

/// queue.rs `display_title` (version suffix). `pub(crate)` for `mini_qt`'s row
/// projection, which must not grow a private copy of it.
pub(crate) fn display_title(track: &QueueTrack) -> String {
    match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title),
        None => track.title.clone(),
    }
}

/// `pub(crate)` for the same reason as `display_title`.
pub(crate) fn fmt_duration(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Canonical decimal parse — the exact equivalent of the old
/// `feed_id == track.id.to_string()` string compare (rejects the two forms
/// `u64::from_str` accepts but `to_string` never emits: a leading `+` and
/// leading zeros), so the heart state does not change with the lookup shape.
fn canonical_u64(s: &str) -> Option<u64> {
    if s.is_empty() || s.starts_with('+') || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse::<u64>().ok()
}

/// Favorite TRACK ids from the phase-5 library feed — built ONCE per publish.
///
/// PERF (owner report: "the queue lags while a local album is loaded"). This
/// used to be a per-ROW `feed.iter().any(...)`: a full linear scan of the
/// ~10k-row library feed for EVERY row a publish builds (1 current + <=40 page
/// + <=50 history — the core caps history at 50, `qbz-player/src/queue.rs:748`),
/// with a fresh `track.id.to_string()` heap allocation on every compared
/// element and a separate LIBRARY lock per row. That is ~900k allocations and
/// ~91 lock round-trips per publish, and a LOCAL row can never match a Qobuz
/// feed id, so `any()` never short-circuits — every local row paid the FULL
/// scan, which is exactly why the lag shows up with a local album loaded.
///
/// The Slint reference resolves the same truth in O(1) from `fav_cache`'s
/// `HashSet<u64>` (`crates/qbz/src/queue.rs:230`); the port now has that cache
/// (`fav_cache_qt`), so it is the PRIMARY source — the library feed is empty
/// until the user opens the Library view, which is why the now-playing heart
/// used to draw blank on favourited tracks for the whole session. The feed is
/// unioned in because it is the fresher of the two once loaded.
fn favorite_track_ids() -> HashSet<u64> {
    let mut ids = crate::fav_cache_qt::all_tracks();
    if let Some(feed_ids) = crate::library_qt::with_library(|d| {
        d.feed
            .iter()
            .filter(|i| i.is_favorite && i.kind == "track")
            .filter_map(|i| canonical_u64(&i.id))
            .collect::<HashSet<u64>>()
    }) {
        ids.extend(feed_ids);
    }
    ids
}

fn row_from(track: &QueueTrack, favorites: &HashSet<u64>) -> QueueRow {
    // LOCAL/Plex rows carry a library.db row id, not a Qobuz track id, so a
    // numeric collision with the favourite set would light a heart on the
    // wrong row — and `toggle_favorite` below refuses to act on those anyway.
    let is_favorite = !track.is_local && favorites.contains(&track.id);
    let tier = match track.bit_depth {
        Some(d) if d >= 24 => "hires",
        Some(_) => "cd",
        None if track.hires => "hires",
        None => "",
    }
    .to_string();
    QueueRow {
        id: track.id.to_string(),
        title: display_title(track),
        artist: track.artist.clone(),
        artist_id: track.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        album: track.album.clone(),
        album_id: track.album_id.clone().unwrap_or_default(),
        duration: fmt_duration(track.duration_secs),
        duration_secs: track.duration_secs,
        quality_tier: tier,
        quality_detail: crate::home_qt::quality_detail_from_parts(
            track.bit_depth,
            track.sample_rate,
        ),
        explicit: track.parental_warning,
        art_url: track.artwork_url.clone().unwrap_or_default(),
        is_favorite,
        is_local: track.is_local,
        source: track
            .source
            .clone()
            .unwrap_or_else(|| if track.is_local { "local" } else { "qobuz" }.to_string()),
        qconnect_compatible: crate::qconnect_qt::is_qconnect_queue_track(track),
        section: String::new(),
        // Both halves of the reference's test (`queue.rs:148`): the queue
        // track's own source tag, and the id-range check for a row that
        // reached the queue without one.
        is_ephemeral: track.source.as_deref() == Some("ephemeral")
            || crate::local_ephemeral::is_ephemeral_id(track.id as i64),
        // `QueueTrack.streamable` defaults TRUE (unlike catalog `Track`), so a
        // row that never learned its availability reads as available — the
        // permissive direction, deliberately.
        qobuz_unavailable: !track.is_local && !track.streamable,
        cache_status: if crate::offline_qt::is_cached(&track.id.to_string()) {
            3
        } else {
            0
        },
    }
}

/// queue.rs `paginate` (unit-tested upstream; port).
fn paginate(total: usize, requested_page: usize) -> (usize, usize, usize, usize) {
    let page_count = total.div_ceil(PAGE_SIZE).max(1);
    let page = requested_page.min(page_count - 1);
    let start = page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(total);
    (page, page_count, start, end)
}

// ---------------------------------------------------------------------------
// Immersive coverflow (contract §4.4) — the FLAT full-queue model
// ---------------------------------------------------------------------------

/// One coverflow entry. Only the four fields the immersive panels read
/// (`{id, title, artist, artUrl}`) — NOT a full QueueRow: the flat model is
/// rebuilt only on id-sequence change and QML resolves covers via
/// `artwork_qt` from `artUrl`.
#[derive(Clone, Serialize)]
struct CoverflowTrack {
    id: String,
    title: String,
    artist: String,
    #[serde(rename = "artUrl")]
    art_url: String,
}

impl CoverflowTrack {
    fn from_track(t: &QueueTrack) -> Self {
        Self {
            id: t.id.to_string(),
            title: display_title(t),
            artist: t.artist.clone(),
            art_url: t.artwork_url.clone().unwrap_or_default(),
        }
    }
}

/// The flat model + NOW index, 1:1 with the Slint builder
/// (`crates/qbz/src/queue.rs:296-321`, semantics `state.slint:4854-4885`):
/// `[history.reversed (oldest..newest), NOW-PLAYING?, upcoming...]`.
/// `history` arrives MOST-RECENT-FIRST (core order), so it is reversed for
/// the oldest-first flat order. The index is `history.len()` in both arms:
/// with a current track it is NOW's slot (state.slint:4878-4881); without
/// one it points at the first upcoming (Slint queue.rs:315-318).
fn coverflow_flat(
    history: &[QueueTrack],
    current: Option<&QueueTrack>,
    upcoming: &[QueueTrack],
) -> (Vec<CoverflowTrack>, usize) {
    let mut flat: Vec<CoverflowTrack> = Vec::with_capacity(history.len() + 1 + upcoming.len());
    flat.extend(history.iter().rev().map(CoverflowTrack::from_track));
    let index = flat.len();
    if let Some(t) = current {
        flat.push(CoverflowTrack::from_track(t));
    }
    flat.extend(upcoming.iter().map(CoverflowTrack::from_track));
    (flat, index)
}

/// Order-sensitive rolling fingerprint of the flat id-sequence (Slint
/// queue.rs:328-336): equal => pure advance/jump => the tracks array is
/// REUSED and only `index` moves; different => rebuild. Hashing the ordered
/// ids (not a set) catches shuffle/reorder.
fn coverflow_seq_hash(flat: &[CoverflowTrack]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    flat.len().hash(&mut h);
    for t in flat {
        t.id.hash(&mut h);
    }
    h.finish()
}

/// (seq hash, serialized tracks array) — the array is rebuilt ONLY on
/// id-sequence change (state.slint:4854-4885 semantics).
static COVERFLOW_CACHE: Mutex<Option<(u64, String)>> = Mutex::new(None);
/// Last coverflow document handed to Qt — same equality-guarded publish
/// discipline as `LAST_JSON` below (:395-419 precedent).
static LAST_COVERFLOW_JSON: Mutex<String> = Mutex::new(String::new());

/// Build + publish the coverflow document (`{"index":i32,"tracks":[...]}`).
/// Runs on EVERY publish BEFORE the queueJson dedup: a change past the
/// current page can leave queueJson byte-identical (same page rows, same
/// totals) while the flat model moved.
fn publish_coverflow(state: &qbz_models::QueueState) {
    let (flat, index) = coverflow_flat(
        &state.history,
        state.current_track.as_ref(),
        &state.upcoming,
    );
    let seq = coverflow_seq_hash(&flat);
    let tracks_json = {
        let mut cache = COVERFLOW_CACHE.lock().unwrap();
        match cache.as_ref() {
            Some((h, json)) if *h == seq => json.clone(),
            _ => {
                let json = serde_json::to_string(&flat).unwrap_or_else(|_| "[]".into());
                *cache = Some((seq, json.clone()));
                json
            }
        }
    };
    let doc = format!("{{\"index\":{index},\"tracks\":{tracks_json}}}");
    // Identical document -> no Qt hop, no QML re-parse (LAST_JSON precedent).
    let mut last = LAST_COVERFLOW_JSON.lock().unwrap();
    if *last == doc {
        return;
    }
    last.clear();
    last.push_str(&doc);
    crate::queue_bridge::ui(move |mut b| {
        b.as_mut().set_coverflow_json(QString::from(doc.as_str()));
    });
}

/// Last miniplayer queue document handed to Qt — the mini's OWN
/// identical-document guard, same discipline as `LAST_COVERFLOW_JSON` above.
/// The mini queue surface is scrollable, and a post that carries an unchanged
/// document resets its scroll for nothing (miniplayer/tray contract §7-M9).
///
/// **Correcting §7-M9's stated reason, which does not hold in this tree:** it
/// says the mini is fed by "a pump that ticks every second". `publish` is not
/// per-tick — its three call sites inside `start_poll_loop` are all edge-gated
/// (`playback_qt.rs`: the peer-track-id edge, the track-id edge, and the
/// stop-after arm), plus the explicit mutation sites. So the guard earns its
/// place on the mutation path (a reorder or a remove that leaves the mini
/// document identical), not on a 1 Hz republish that does not exist.
///
/// **Known asymmetry with its sibling, stated rather than hidden (§8.1):**
/// `publish_coverflow` has `COVERFLOW_CACHE` and skips the serialize itself
/// when the queue sequence is unchanged; this one serializes first and compares
/// after. Accepted here because `publish` is edge-triggered, so the cost is one
/// build per real queue change, not per frame. If `publish` ever becomes
/// unconditional, this is the function that needs the cache.
static LAST_MINI_QUEUE_JSON: Mutex<String> = Mutex::new(String::new());

/// The full projection is intentionally built only while its view exists. A
/// restored queue may contain thousands of tracks; the sidebar document stays
/// paged at 40 and should not pay to serialize the full sequence in the normal
/// shell path.
static EXTENDED_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_EXTENDED_JSON: Mutex<String> = Mutex::new(String::new());

fn extended_rows(
    state: &qbz_models::QueueState,
    favorites: &HashSet<u64>,
    query: &str,
) -> (Vec<ExtendedQueueRow>, i32) {
    let mut rows = Vec::with_capacity(
        state.history.len() + usize::from(state.current_track.is_some()) + state.upcoming.len(),
    );
    let mut current_index = -1;
    let mut push = |track: &QueueTrack, phase, phase_index, section: &'static str| {
        if query.is_empty() || matches_query(track, query) {
            if phase == "current" {
                current_index = rows.len() as i32;
            }
            let mut row = row_from(track, favorites);
            row.section = section.to_string();
            rows.push(ExtendedQueueRow {
                row,
                phase,
                phase_index,
            });
        }
    };

    // Core history is newest-first; the view reads as one chronological
    // sequence, so the oldest played track is first.
    for (index, track) in state.history.iter().enumerate().rev() {
        push(track, "history", index, "");
    }
    if let Some(track) = state.current_track.as_ref() {
        push(track, "current", 0, "");
    }
    for (index, track) in state.upcoming.iter().enumerate() {
        let section = if query.is_empty() && state.manual_next_count > 0 && index == 0 {
            "next-in-queue"
        } else if query.is_empty() && index == state.manual_next_count {
            "next-up"
        } else {
            ""
        };
        push(track, "upcoming", index, section);
    }
    (rows, current_index)
}

fn publish_extended(
    state: &qbz_models::QueueState,
    favorites: &HashSet<u64>,
    stop_after_id: &str,
    search: &str,
) {
    use std::sync::atomic::Ordering;

    if !EXTENDED_OPEN.load(Ordering::SeqCst) {
        return;
    }
    let query = search.trim().to_lowercase();
    let (rows, current_index) = extended_rows(state, favorites, &query);
    let doc = ExtendedQueueDoc {
        rows,
        current_index,
        history_count: state.history.len(),
        upcoming_count: state.upcoming.len(),
        stop_after_id: stop_after_id.to_string(),
        infinite_play: is_infinite_play(),
        search_query: search.to_string(),
    };
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| {
        r#"{"rows":[],"currentIndex":-1,"historyCount":0,"upcomingCount":0,"stopAfterId":"","infinitePlay":false,"searchQuery":""}"#.into()
    });
    let mut last = LAST_EXTENDED_JSON.lock().unwrap();
    if *last == json {
        return;
    }
    last.clear();
    last.push_str(&json);
    crate::queue_bridge::ui(move |mut b| {
        b.as_mut()
            .set_extended_queue_json(QString::from(json.as_str()));
    });
}

pub fn extended_opened() {
    EXTENDED_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn extended_closed() {
    EXTENDED_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Build + publish the miniplayer's navigable queue document
/// (`{"currentId":"…","rows":[…]}` — `mini_qt::mini_queue_doc`).
///
/// Its own function rather than an inline block, because the guard's early
/// `return` aborts whatever encloses it: inlined into `publish`, an unchanged
/// mini document would skip the panel's own post below and silently freeze the
/// queue panel. Runs on EVERY publish BEFORE the queueJson dedup, for the same
/// reason `publish_coverflow` does — a change past the current page can leave
/// queueJson byte-identical while the mini model moved.
fn publish_mini_queue(state: &qbz_models::QueueState) {
    let doc = crate::mini_qt::mini_queue_doc(state);
    let mut last = LAST_MINI_QUEUE_JSON.lock().unwrap();
    if *last == doc {
        return;
    }
    last.clear();
    last.push_str(&doc);
    crate::mini_bridge::ui(move |mut b| {
        b.as_mut().set_mini_queue_json(QString::from(doc.as_str()));
    });
}

/// Queue-wide upcoming indices of the rows the panel is CURRENTLY showing, in
/// display order (search filter + pagination applied) — the hoisted port of
/// `crates/qbz/src/queue.rs::resolve_upcoming_index`, resolved once per action
/// so one call answers every row on the page.
///
/// WHY IT EXISTS: the QML hands PAGE-LOCAL row numbers, and `page * PAGE_SIZE
/// + row` is the queue index ONLY while the search box is empty. With a query
/// active the page walks the FILTERED list, so that arithmetic names a
/// different track than the one under the pointer — the core indexes the
/// unfiltered upcoming list. Every row mutation (play / remove /
/// remove-all-after / drag reorder) resolves through here, which is also what
/// makes the reorder's insertion slot land on the right gap when the panel is
/// paged or filtered.
///
/// The `paginate` call clamps the page exactly like `publish` does, so the
/// slice is the one on screen even if `VIEW.page` is momentarily past the end.
fn page_row_indices(upcoming: &[QueueTrack], query: &str, page: usize) -> Vec<usize> {
    if query.is_empty() {
        let (_, _, start, end) = paginate(upcoming.len(), page);
        return (start..end).collect();
    }
    let matches: Vec<usize> = upcoming
        .iter()
        .enumerate()
        .filter(|(_, t)| matches_query(t, query))
        .map(|(i, _)| i)
        .collect();
    let (_, _, start, end) = paginate(matches.len(), page);
    matches[start..end].to_vec()
}

/// `page_row_indices` against the live queue + the current view state. ONE
/// `get_queue_state_full` per user action (the mutations below all await a
/// core call anyway); the returned vector is at most PAGE_SIZE long.
async fn current_page_indices(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> Vec<usize> {
    let (search, page) = {
        let view = VIEW.lock().unwrap();
        (view.search.clone(), view.page)
    };
    let query = search.trim().to_lowercase();
    let state = runtime.core().get_queue_state_full().await;
    page_row_indices(&state.upcoming, &query, page)
}

/// Search predicate — same truth as `display_title(t)` + artist, minus the
/// intermediate `display_title` clone in the (dominant) version-less case.
/// Runs over the WHOLE upcoming list on every keystroke, so the saved
/// allocation is one per queue track per keypress.
fn matches_query(track: &QueueTrack, query: &str) -> bool {
    let title_hit = match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title)
            .to_lowercase()
            .contains(query),
        None => track.title.to_lowercase().contains(query),
    };
    title_hit || track.artist.to_lowercase().contains(query)
}

/// Last document handed to Qt — the publish is skipped when the freshly built
/// one is byte-identical (`set_queue_json` emits unconditionally, and a
/// no-op emit is NOT free downstream: THREE QML files re-`JSON.parse` the
/// document (QueuePanel, PlayerBar, NowPlayingBarSmall) and QueuePanel's
/// `onDocChanged` re-dispatches the artwork window for every row — one
/// `stat` + one `libraryArtworkReady` emit + one full `coverMap` object
/// rebuild per row, invalidating every cover binding in the panel AND the
/// sidebar). Several user actions publish twice for one change (e.g.
/// `local_playback::play_rows` -> publish, then the audible step's
/// `play_queue_track` -> publish again), and the poll loop's track edge
/// publishes on top of the transport's own. Same dedup convention as
/// `now_playing::set_effective_stream`.
static LAST_JSON: Mutex<String> = Mutex::new(String::new());

/// Ids of the LOCAL/Plex rows in the document currently on screen (<=91),
/// refreshed by every publish — the guard `toggle_favorite` consults so a
/// local row's library.db id never reaches the Qobuz favorites API.
static LOCAL_ROW_IDS: std::sync::LazyLock<Mutex<HashSet<u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

/// Build + publish the whole panel document.
pub async fn publish(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let t_build = std::time::Instant::now();
    let state = runtime.core().get_queue_state_full().await;
    let has_play_target = state.total_tracks > 0;
    let (search, requested_page) = {
        let view = VIEW.lock().unwrap();
        (view.search.clone(), view.page)
    };
    let query = search.trim().to_lowercase();

    // --- UP NEXT (search-filtered, paged, #442 markers) ------------------
    let filtered: Vec<&QueueTrack> = if query.is_empty() {
        state.upcoming.iter().collect()
    } else {
        state
            .upcoming
            .iter()
            .filter(|t| matches_query(t, &query))
            .collect()
    };
    let upcoming_total = filtered.len();
    let (page, page_count, start, end) = paginate(upcoming_total, requested_page);
    VIEW.lock().unwrap().page = page;

    // ONE feed pass for the whole document (see `favorite_track_ids`).
    let favorites = favorite_track_ids();

    let page_rows: Vec<QueueRow> = filtered[start..end]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut r = row_from(t, &favorites);
            if query.is_empty() {
                let g = start + i;
                if state.manual_next_count > 0 && g == 0 {
                    r.section = "next-in-queue".into();
                } else if g == state.manual_next_count {
                    r.section = "next-up".into();
                }
            }
            r
        })
        .collect();

    let remaining = state
        .current_index
        .map(|idx| state.total_tracks.saturating_sub(idx + 1))
        .unwrap_or(state.total_tracks);

    // Bounded by construction: the core keeps at most 50 history entries
    // (`qbz-player/src/queue.rs:748`), so the document cannot grow without
    // bound — <=91 rows total (1 current + <=40 page + <=50 history).
    let history: Vec<QueueRow> = state
        .history
        .iter()
        .map(|t| row_from(t, &favorites))
        .collect();
    let current = state
        .current_track
        .as_ref()
        .map(|t| row_from(t, &favorites));

    // One core read for the marker (the reference reads it in the same pass,
    // `queue.rs:396`), so a row can compare its id without a second round trip.
    let stop_after_id = runtime
        .core()
        .get_stop_after()
        .await
        .map(|id| id.to_string())
        .unwrap_or_default();
    publish_extended(&state, &favorites, &stop_after_id, &search);

    let doc = QueueDoc {
        has_current: current.is_some(),
        current,
        upcoming: page_rows,
        upcoming_total,
        upcoming_remaining: remaining,
        history,
        page,
        page_count,
        page_start: if upcoming_total == 0 { 0 } else { start + 1 },
        page_end: end,
        shuffle: state.shuffle,
        repeat_mode: match state.repeat {
            qbz_models::RepeatMode::Off => 0,
            qbz_models::RepeatMode::All => 1,
            qbz_models::RepeatMode::One => 2,
        },
        stop_after_id,
        infinite_play: is_infinite_play(),
        // The RAW typed text, not the trimmed/lowercased `query` — this is
        // what the reference's `QueueState.search-query` holds, and the
        // "nothing matched" state gates on it being non-empty.
        search_query: search,
    };
    let rows = doc.upcoming.len() + doc.history.len() + usize::from(doc.has_current);
    *LOCAL_ROW_IDS.lock().unwrap() = doc
        .upcoming
        .iter()
        .chain(doc.history.iter())
        .chain(doc.current.iter())
        .filter(|r| r.is_local)
        .filter_map(|r| canonical_u64(&r.id))
        .collect();
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    // The coverflow doc publishes on EVERY publish, BEFORE the queueJson
    // dedup below — see publish_coverflow for why.
    publish_coverflow(&state);
    // The miniplayer's navigable model rides the same funnel and the same
    // before-the-dedup placement, with its own guard (see publish_mini_queue).
    publish_mini_queue(&state);
    // Identical document -> no Qt hop, no QML re-parse, no artwork re-dispatch.
    // The compare/store and the Qt post stay under ONE lock so two concurrent
    // publishes cannot post out of order and strand a stale document behind an
    // up-to-date `LAST_JSON` (there is no `.await` in here, so the guard never
    // crosses a yield point).
    let mut last = LAST_JSON.lock().unwrap();
    if *last == json {
        log::debug!(
            "[qbz-qt][perf] queue publish: unchanged, skipped ({rows} rows, {:?})",
            t_build.elapsed()
        );
        return;
    }
    last.clear();
    last.push_str(&json);
    log::debug!(
        "[qbz-qt][perf] queue publish: {rows} rows, {} favs, {} bytes, build {:?}",
        favorites.len(),
        json.len(),
        t_build.elapsed()
    );
    crate::queue_bridge::ui(move |mut b| {
        b.as_mut().set_has_play_target(has_play_target);
        b.as_mut().set_queue_json(QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
// View state + mutations (every mutation republishes)
// ---------------------------------------------------------------------------

pub fn set_search(query: &str) {
    {
        let mut view = VIEW.lock().unwrap();
        view.search = query.to_string();
        view.page = 0;
    }
}

pub fn set_page(page: i32) {
    VIEW.lock().unwrap().page = page.max(0) as usize;
}

async fn play_upcoming_on_peer(upcoming_index: usize, track_id: u64) -> bool {
    let Some(service) = crate::qconnect_qt::service() else {
        return false;
    };
    match service
        .play_remote_upcoming_if_active(upcoming_index, track_id)
        .await
    {
        Ok(handled) => handled,
        Err(error) => {
            log::warn!("[qbz-qt] queue: remote upcoming selection failed: {error}");
            crate::toast_qt::error(qbz_i18n::t(
                "Failed to send playback to the selected device",
            ));
            // A peer owns playback. No local cursor mutation or audio fallback.
            true
        }
    }
}

pub async fn play_upcoming(runtime: &Arc<AppRuntime<LoggingAdapter>>, page_index: usize) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let Some(&upcoming_index) = current_page_indices(runtime).await.get(page_index) else {
        log::warn!("[qbz-qt] queue: play_upcoming {page_index} out of range");
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    let Some(candidate) = state.upcoming.get(upcoming_index) else {
        return;
    };
    if play_upcoming_on_peer(upcoming_index, candidate.id).await {
        return;
    }
    if crate::local_playback::preflight_queue_track(candidate)
        .await
        .is_err()
    {
        return;
    }
    if let Some(track) = runtime.core().play_upcoming_at(upcoming_index).await {
        crate::playback_qt::play_queue_track_public(runtime, track.id).await;
    }
}

/// QUEUE-WIDE 0-based index into the UNFILTERED upcoming list (contract
/// §4.4) — the immersive coverflow side cards and up-next rows call this.
/// Unlike `play_upcoming` above it bypasses the page/search VIEW entirely,
/// so it resolves the right track even when the desktop panel was left
/// filtered or paged (state.slint:4896-4900 `play-coverflow-upcoming`).
pub async fn play_upcoming_flat(runtime: &Arc<AppRuntime<LoggingAdapter>>, upcoming_index: usize) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    if let Some(candidate) = state.upcoming.get(upcoming_index) {
        if play_upcoming_on_peer(upcoming_index, candidate.id).await {
            return;
        }
        if crate::local_playback::preflight_queue_track(candidate)
            .await
            .is_err()
        {
            return;
        }
    }
    match runtime.core().play_upcoming_at(upcoming_index).await {
        Some(track) => crate::playback_qt::play_queue_track_public(runtime, track.id).await,
        None => log::warn!("[qbz-qt] queue: play_upcoming_flat {upcoming_index} out of range"),
    }
}

async fn play_extended_upcoming(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    upcoming_index: usize,
    expected_id: u64,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    let Some(track) = state.upcoming.get(upcoming_index).cloned() else {
        return;
    };
    if track.id != expected_id {
        log::warn!("[qbz-qt] queue: upcoming changed during extended activation");
        return;
    }
    if crate::qconnect_qt::queue_admission_enabled()
        && !crate::qconnect_qt::is_qconnect_queue_track(&track)
    {
        log::info!(
            "[qbz-qt] queue: skipped QConnect-incompatible upcoming row {}",
            track.id
        );
        crate::qconnect_qt::toast_unresolvable_tracks(1);
        return;
    }
    if play_upcoming_on_peer(upcoming_index, track.id).await {
        return;
    }
    if crate::local_playback::preflight_queue_track(&track)
        .await
        .is_err()
    {
        return;
    }
    match runtime
        .core()
        .play_upcoming_at_preserving_timeline(upcoming_index, expected_id)
        .await
    {
        Some(track) => crate::playback_qt::play_queue_track_public(runtime, track.id).await,
        None => log::warn!("[qbz-qt] queue: upcoming changed during extended activation"),
    }
}

/// Activate a row from the chronological projection without moving the list's
/// leading edge. Local playback moves only the core cursor; the generic Queue
/// sidebar and Immersive activation paths keep their existing jump semantics.
/// A remote QConnect renderer still receives an insert + play request because
/// its authoritative timeline cannot be rewound through the local core.
pub async fn play_extended(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    phase: &str,
    phase_index: usize,
    expected_id: u64,
) {
    match phase {
        "upcoming" => {
            play_extended_upcoming(runtime, phase_index, expected_id).await;
        }
        "history" => {
            let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
                return;
            };
            let state = runtime.core().get_queue_state_full().await;
            let Some(track) = state.history.get(phase_index).cloned() else {
                return;
            };
            if track.id != expected_id {
                log::warn!("[qbz-qt] queue: history changed during activation");
                return;
            }
            if crate::qconnect_qt::queue_admission_enabled()
                && !crate::qconnect_qt::is_qconnect_queue_track(&track)
            {
                log::info!(
                    "[qbz-qt] queue: skipped QConnect-incompatible history row {}",
                    track.id
                );
                crate::qconnect_qt::toast_unresolvable_tracks(1);
                return;
            }
            if let Some(service) = crate::qconnect_qt::service() {
                match service
                    .insert_at_slot_on_peer_outcome(track.id, track.source.as_deref(), 0)
                    .await
                {
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::Inserted => {
                        // The peer creates the new upcoming occurrence. Retire
                        // the selected local history reference immediately so
                        // its echo cannot leave this occurrence listed twice.
                        runtime
                            .core()
                            .remove_history_entry(phase_index, expected_id)
                            .await;
                        publish(runtime).await;
                        if let Err(error) =
                            service.play_remote_renderer_track_if_active(track.id).await
                        {
                            log::warn!(
                                "[qbz-qt] queue: requeued remote history play failed: {error}"
                            );
                        }
                        return;
                    }
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::HandledFailure => return,
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::Inactive => {}
                }
            }
            if crate::local_playback::preflight_queue_track(&track)
                .await
                .is_err()
            {
                return;
            }
            match runtime
                .core()
                .play_history_at_preserving_timeline(phase_index, expected_id)
                .await
            {
                Some(track) => {
                    crate::playback_qt::play_queue_track_public(runtime, track.id).await;
                }
                None => log::warn!("[qbz-qt] queue: history changed during activation"),
            }
        }
        _ => {}
    }
}

pub async fn remove_upcoming(runtime: &Arc<AppRuntime<LoggingAdapter>>, page_index: usize) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let Some(&upcoming_index) = current_page_indices(runtime).await.get(page_index) else {
        log::warn!("[qbz-qt] queue: remove_upcoming {page_index} out of range");
        return;
    };
    runtime.core().remove_upcoming_track(upcoming_index).await;
    publish(runtime).await;
}

pub async fn remove_upcoming_flat(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    upcoming_index: usize,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    if upcoming_index >= state.upcoming.len() {
        return;
    }
    runtime.core().remove_upcoming_track(upcoming_index).await;
    publish(runtime).await;
}

/// Drop every UPCOMING row that cannot play, and report how many went.
///
/// The last half of "an unavailable track is never blocking": it is skipped,
/// and when there is nowhere left to skip TO, the dead rows are cleaned out
/// rather than left for the user to walk into one at a time. Called only from
/// the dead ends of the skip walk — the budget running out, or the queue edge
/// being reached — never speculatively.
///
/// Walks BACKWARDS so each removal cannot shift an index still to be visited.
///
/// It removes what `queue_track_unavailable` already refuses to play, so this
/// can only delete rows that were unplayable anyway — never a track that is
/// merely on a share this network cannot see, because an unreachable local row
/// is `Unreachable`, not dead, and never enters that predicate.
pub async fn purge_unavailable_upcoming(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> usize {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return 0;
    };
    let doomed: Vec<usize> = {
        let state = runtime.core().get_queue_state_full().await;
        state
            .upcoming
            .iter()
            .enumerate()
            .filter(|(_, t)| crate::playback_qt::queue_track_unavailable(t))
            .map(|(i, _)| i)
            .collect()
    };
    if doomed.is_empty() {
        return 0;
    }
    for index in doomed.iter().rev() {
        runtime.core().remove_upcoming_track(*index).await;
    }
    log::info!(
        "[qbz-qt] queue: purged {} unplayable upcoming row(s)",
        doomed.len()
    );
    publish(runtime).await;
    doomed.len()
}

pub async fn remove_all_after(runtime: &Arc<AppRuntime<LoggingAdapter>>, page_index: usize) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let Some(&upcoming_index) = current_page_indices(runtime).await.get(page_index) else {
        log::warn!("[qbz-qt] queue: remove_all_after {page_index} out of range");
        return;
    };
    runtime.core().remove_upcoming_after(upcoming_index).await;
    publish(runtime).await;
}

pub async fn remove_all_after_flat(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    upcoming_index: usize,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    if upcoming_index >= state.upcoming.len() {
        return;
    }
    runtime.core().remove_upcoming_after(upcoming_index).await;
    publish(runtime).await;
}

/// Drag reorder — move the PAGE-LOCAL row `from_page` to the insertion slot
/// `to_slot` (0..=page_len), the contract of `QueueSidebar.slint`'s
/// `QueueState.reorder`. Both ends resolve to QUEUE-WIDE upcoming indices
/// through `page_row_indices` first: the panel hands page-local numbers like
/// every other row action, and under a search filter (or on page >= 1 of a
/// filtered list) a page-local number is a position in the FILTERED list, not
/// in the queue — passing it straight to the core moved a different track.
///
/// Slot semantics (Slint 1:1): slot k < page_len inserts BEFORE page-local row
/// k; slot == page_len appends after the last visible row (one past it in
/// queue-wide upcoming space). `to == from` and `to == from + 1` are the same
/// gap and are dropped by the panel before they get here; `from_q == to_q` is
/// re-checked below because the two page-local slots can collapse onto one
/// queue index once the filter is resolved.
///
/// POC-NOTE: the Slint arm first offers the move to the QConnect cloud
/// (WS-authoritative when connected) — that seam is now ported below, 1:1
/// with `queue.rs:686-697`.
pub async fn move_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    from_page: usize,
    to_slot: usize,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let page_rows = current_page_indices(runtime).await;
    let Some(&from_q) = page_rows.get(from_page) else {
        log::warn!("[qbz-qt] queue: reorder from {from_page} out of range");
        return;
    };
    let to_q = if to_slot >= page_rows.len() {
        match page_rows.last() {
            Some(&last) => last + 1,
            None => return,
        }
    } else {
        page_rows[to_slot]
    };
    if from_q == to_q {
        return;
    }
    // Connected -> the cloud reorders and echoes a QueueUpdated that
    // materialize_remote_queue applies; do NOT also reorder locally (would
    // diverge). TRAP (§7): routed reorders never touch the local queue, and
    // on ERROR there is NO local fallback (Slint queue.rs:688-697).
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.reorder_upcoming_if_remote(from_q, to_q).await {
            Ok(true) => return,
            Ok(false) => {} // not connected -> local path
            Err(e) => {
                log::warn!("[qbz-qt] queue: reorder handoff failed: {e}");
                return; // connected but errored -> do NOT local-reorder
            }
        }
    }
    runtime.core().move_track(from_q, to_q).await;
    publish(runtime).await;
}

/// Full-view reorder: both coordinates are queue-wide upcoming positions, so
/// no sidebar page/search state participates.
pub async fn move_track_flat(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    from: usize,
    to_slot: usize,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    if from >= state.upcoming.len() || to_slot > state.upcoming.len() {
        return;
    }
    if from == to_slot || from + 1 == to_slot {
        return;
    }
    if let Some(service) = crate::qconnect_qt::service() {
        match service.reorder_upcoming_if_remote(from, to_slot).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                log::warn!("[qbz-qt] queue: extended reorder handoff failed: {error}");
                return;
            }
        }
    }
    runtime.core().move_track(from, to_slot).await;
    publish(runtime).await;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InsertOutcome {
    Local,
    Remote,
}

async fn insert_queue_track_at(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track: QueueTrack,
    to_slot: usize,
) -> Result<InsertOutcome, String> {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return Ok(InsertOutcome::Remote);
    };
    if let Some(service) = crate::qconnect_qt::service() {
        if service
            .insert_at_slot_on_peer_if_active(track.id, track.source.as_deref(), to_slot)
            .await
        {
            return Ok(InsertOutcome::Remote);
        }
    }
    let added_castable =
        crate::playback_qt::batch_all_qconnect_castable(std::slice::from_ref(&track));
    runtime.core().add_track(track).await;
    let upcoming_len = runtime.core().get_queue_state_full().await.upcoming.len();
    if upcoming_len > 0 {
        let from = upcoming_len - 1;
        let slot = to_slot.min(from);
        if from != slot {
            runtime.core().move_track(from, slot).await;
        }
    }
    crate::playback_qt::sync_qconnect_after_add(added_castable).await;
    publish(runtime).await;
    Ok(InsertOutcome::Local)
}

/// Commit a Queue View drag. Upcoming rows reorder only inside Upcoming.
/// History rows may reorder inside History or move their canonical queue
/// occurrence back into Upcoming. The latter never clones by track id, so two
/// separately enqueued copies remain distinct while replaying the same entry
/// keeps only its latest history position.
pub async fn drop_extended(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    phase: &str,
    phase_index: usize,
    expected_id: u64,
    target_phase: &str,
    to_slot: usize,
) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    match phase {
        "upcoming" => {
            if target_phase != "upcoming" {
                return;
            }
            let state = runtime.core().get_queue_state_full().await;
            if state.upcoming.get(phase_index).map(|track| track.id) != Some(expected_id) {
                log::warn!("[qbz-qt] queue: upcoming changed during extended drag");
                return;
            }
            move_track_flat(runtime, phase_index, to_slot).await;
        }
        "history" => {
            if target_phase == "history" {
                if runtime
                    .core()
                    .move_history_entry(phase_index, expected_id, to_slot)
                    .await
                {
                    publish(runtime).await;
                }
                return;
            }
            if target_phase != "upcoming" {
                return;
            }
            let state = runtime.core().get_queue_state_full().await;
            let Some(track) = state.history.get(phase_index).cloned() else {
                return;
            };
            if track.id != expected_id {
                log::warn!("[qbz-qt] queue: history changed during extended drag");
                return;
            }
            // Admission follows enabled intent, including reconnect windows;
            // otherwise a local history row can enter the queue while the
            // transport is down and poison the next cloud sync.
            if crate::qconnect_qt::queue_admission_enabled()
                && !crate::qconnect_qt::is_qconnect_queue_track(&track)
            {
                log::info!(
                    "[qbz-qt] queue: skipped QConnect-incompatible history drag {}",
                    track.id
                );
                crate::qconnect_qt::toast_unresolvable_tracks(1);
                return;
            }
            if let Some(service) = crate::qconnect_qt::service() {
                match service
                    .insert_at_slot_on_peer_outcome(track.id, track.source.as_deref(), to_slot)
                    .await
                {
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::Inserted => {
                        runtime
                            .core()
                            .remove_history_entry(phase_index, expected_id)
                            .await;
                        publish(runtime).await;
                        return;
                    }
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::HandledFailure => return,
                    crate::qconnect_qt::PeerInsertAtSlotOutcome::Inactive => {}
                }
            }
            if runtime
                .core()
                .requeue_history_entry(phase_index, expected_id, to_slot)
                .await
            {
                publish(runtime).await;
            } else {
                log::warn!("[qbz-qt] queue: history changed during extended drag");
            }
        }
        _ => {}
    }
}

/// Ask the panel whether to start playing after an empty-queue drop, or clear
/// the ask. Split from the insert so a MULTI-row drop raises exactly one
/// prompt, after the last row has landed.
pub fn set_drop_play_prompt(on: bool) {
    crate::queue_bridge::ui(move |mut b| b.as_mut().set_drop_play_prompt(on));
}

/// The prompt's two answers. `true` starts the queue from its first upcoming
/// row — the same entry point the row's own click uses, so a dropped-then-
/// played track goes through one path, not two.
pub fn answer_drop_prompt(play: bool) {
    set_drop_play_prompt(false);
    if !play {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move { play_upcoming_flat(&runtime, 0).await });
}

/// Drop a dragged track into the queue AT A POSITION (owner ask 2026-08-10:
/// "poder draggear hacia el queue, definiendo la posición desde el comienzo").
///
/// `to_slot` is a slot in the VISIBLE page of upcoming — the same coordinate
/// [`move_track`] takes — so search and pagination are honoured for free.
///
/// ## Why append-then-move rather than an insert
///
/// `QueueManager` has no insert-at-index, and the three adds it does have are
/// not interchangeable here:
///   `add_track_next` / `add_track_later` both bump `manual_next_count` and
///   place the row inside the manual block. Moving it afterwards would leave
///   that counter describing a track that is no longer there.
///   `add_track` is the clean one: a plain push onto `tracks` (plus the
///   shuffle-order push), no counters touched.
/// So: `add_track` to land at the END of upcoming, then one `move_track` into
/// place. Both are existing public API — nothing is added to the shared
/// `qbz-player` crate for this.
///
/// `move_track` takes UPCOMING-relative indices in both modes (the shuffle
/// branch maps them into `shuffle_order`; the plain branch adds
/// `current_index + 1`), which is why the page mapping below can be handed to
/// it directly. Moving UP (append -> earlier slot) takes its `Up` arm, so the
/// row lands exactly at `to_slot`, i.e. before the row that was there.
pub async fn insert_dragged_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    to_slot: usize,
) -> Result<(), String> {
    // NOT `.expect("one track in, one track out")` any more: the enqueue seam
    // filters now (F2 + D5), so one row in can legitimately be ZERO rows out —
    // and a DRAG of a pulled track into the queue would have PANICKED. The seam
    // has already toasted the reason.
    let qt = crate::playback_qt::queue_track_for(runtime, track_id).await?;
    insert_dragged_queue_track(runtime, qt, to_slot).await
}

/// The same drop, from an already-built `QueueTrack` — the LOCAL-mode drag's
/// arm (Explorer / Local Library rows), and the shared tail of the Qobuz one.
pub async fn insert_dragged_queue_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track: qbz_models::QueueTrack,
    to_slot: usize,
) -> Result<(), String> {
    let id = track.id;
    let Some(qt) = crate::playback_qt::stamped(vec![track], None).pop() else {
        log::info!("[qbz-qt] queue: dragged track {id} was filtered out");
        return Ok(());
    };

    // Resolve the drop slot BEFORE the add, while the page still describes the
    // queue the user was looking at. Past the last visible row means "after
    // everything on this page".
    let page_rows = current_page_indices(runtime).await;
    let to_upcoming = match page_rows.get(to_slot) {
        Some(&idx) => idx,
        None => match page_rows.last() {
            Some(&last) => last + 1,
            None => 0,
        },
    };

    let _ = insert_queue_track_at(runtime, qt, to_upcoming).await?;
    Ok(())
}

/// History replay (queue.rs play_history: a fresh single-track queue).
pub async fn play_history(runtime: &Arc<AppRuntime<LoggingAdapter>>, index: usize) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    let Some(track) = state.history.get(index).cloned() else {
        log::warn!("[qbz-qt] queue: play_history {index} out of range");
        return;
    };
    // Through the stamping seam: a history row already carries the origin it
    // was played from, so it survives the replay; a legacy row with none falls
    // back to its own album instead of publishing a dead layers glyph.
    // F1: a history row can be a track Qobuz pulled since it played, or an
    // artist blocked since. Replaying the id the seam just dropped is silence
    // with no toast — the exact failure this return value exists to end.
    let Some(anchor) =
        crate::playback_qt::set_queue_stamped(runtime, vec![track], Some(0), None).await
    else {
        log::info!("[qbz-qt] queue: history row {index} is no longer playable");
        return;
    };
    crate::playback_qt::play_queue_track_public(runtime, anchor.track_id).await;
}

/// Empty the queue — `queue.rs::clear` verbatim.
///
/// `keep_current` is the LIVE `is_playing` flag, not a constant: the reference
/// keeps the now-playing slot only while it is audible, so clearing the queue
/// mid-track empties Up Next and lets the current track finish, and clearing it
/// while paused/stopped wipes the card too. This port used to pass `false` and
/// then call `stop()` on top, which killed playback on every Clear — the one
/// footer button that WAS wired was also the one that diverged.
///
/// The view state resets with it (page 0, search cleared), or the panel would
/// come back showing "page 3 of 1" filtered by a query with nothing left to
/// match.
pub async fn clear_queue(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let playing = runtime.core().get_playback_state().is_playing;
    runtime.core().clear_queue(playing).await;
    {
        let mut view = VIEW.lock().unwrap();
        view.page = 0;
        view.search.clear();
    }
    publish(runtime).await;
    crate::playback_qt::refresh_now_playing(runtime).await;
}

/// Toggle the "stop after this song" marker on the queue track with `id` (a
/// decimal string, matching `QueueRow.id`). Idempotent — tapping the same track
/// clears it. The core auto-clears the marker on queue mutation, and `publish`
/// reflects whatever it currently holds. `queue.rs::toggle_stop_after` 1:1.
pub async fn toggle_stop_after(runtime: &Arc<AppRuntime<LoggingAdapter>>, id: &str) {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    let Ok(track_id) = id.parse::<u64>() else {
        return;
    };
    if runtime.core().get_stop_after().await == Some(track_id) {
        runtime.core().clear_stop_after().await;
    } else {
        runtime.core().set_stop_after(track_id).await;
    }
    publish(runtime).await;
}

/// Whether `InfiniteRadio` autoplay is on (the queue auto-refills with a smart
/// radio when it runs out). Read by the footer's ∞ tint AND by the refill
/// engine in `playback_qt`'s end-of-track arm — same single truth as the
/// reference's `QueueController::is_infinite_play`.
pub fn is_infinite_play() -> bool {
    crate::settings_qt::is_infinite_play()
}

/// Toggle infinite play: flips the persisted autoplay mode between
/// `InfiniteRadio` and `ContinueWithinSource` (`queue.rs::toggle_infinite_play`).
///
/// Note the asymmetry with Settings > Playback's "Continue playback" switch,
/// which flips `ContinueWithinSource` <-> `PlayTrackOnly`: the three modes share
/// ONE preference, so turning infinite play off here lands on
/// `ContinueWithinSource` and that switch reads as ON. That is the reference's
/// behaviour, not a rounding of it.
pub async fn toggle_infinite_play(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    crate::settings_qt::set_infinite_play(!is_infinite_play());
    publish(runtime).await;
}

/// The panel became visible — re-pull the queue (`QueueState.panel-opened`).
///
/// The bridge property still holds the last document, so the panel does not
/// mount blank without this; what it buys is the case the reference names —
/// the queue changed while the panel was closed (session restore, a play from
/// a view that does not republish) — and it is nearly free, because `publish`
/// drops a byte-identical document before it reaches Qt.
pub async fn panel_opened(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    publish(runtime).await;
}

/// Open the Add-to-Playlist picker seeded with the queue's tracks — current
/// first, then upcoming, de-duplicated, in PLAY order (`queue.rs::save_as_playlist`).
///
/// The picker's inline "Create new playlist" row is what turns this into "save
/// the queue as a playlist"; picking an existing one appends. There is no
/// separate save-as-playlist endpoint in the reference either — same modal.
///
/// LOCAL/Plex and ephemeral rows are skipped: their `id` is a library.db row id
/// (or a synthetic ephemeral id), and the picker's Qobuz arm would add an
/// unrelated catalog track under that number.
///
/// Every row the picker can file rides along, by SOURCE: a catalog row its
/// id, a local row (file, Plex, Jellyfin/Subsonic, offline copy) the ref
/// `local_playlist_qt::local_picker_ref_for_queue_track` builds, an
/// ephemeral row nothing. A queue of only local rows opens the picker in
/// local mode (its list is the local playlists, which is the whole list
/// offline); a queue of both kinds carries a MIXED payload the picker
/// routes as the two halves would be on their own. Until 2026-09-14 the
/// local rows were skipped, so an all-local queue opened nothing at all.
pub async fn save_as_playlist(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let state = runtime.core().get_queue_state_full().await;
    let (ids, refs, skipped) =
        picker_payload_for(state.current_track.iter().chain(state.upcoming.iter()));
    if skipped > 0 {
        log::info!(
            "[qbz-qt] queue save-as-playlist: skipped {skipped} row(s) with nothing to file \
             (ephemeral, or a local row without its key)"
        );
    }
    if ids.is_empty() && refs.is_empty() {
        log::warn!("[qbz-qt] queue save-as-playlist: nothing to file");
        return;
    }
    crate::playlist_picker_qt::open_for_mixed(runtime, ids, refs);
}

/// The picker payload for queue rows: catalog ids and local refs, each
/// de-duplicated, plus the count of rows that could not be filed.
fn picker_payload_for<'a>(
    rows: impl Iterator<Item = &'a QueueTrack>,
) -> (Vec<String>, Vec<String>, usize) {
    let mut ids: Vec<String> = Vec::new();
    let mut refs: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<u64> = HashSet::new();
    let mut seen_refs: HashSet<String> = HashSet::new();
    let mut skipped = 0usize;
    for track in rows {
        if crate::local_ephemeral::is_ephemeral_id(track.id as i64) {
            skipped += 1;
            continue;
        }
        if track.is_local {
            match crate::local_playlist_qt::local_picker_ref_for_queue_track(track) {
                Some(r) => {
                    if seen_refs.insert(r.clone()) {
                        refs.push(r);
                    }
                }
                None => skipped += 1,
            }
            continue;
        }
        if seen_ids.insert(track.id) {
            ids.push(track.id.to_string());
        }
    }
    (ids, refs, skipped)
}

/// Open the picker seeded with ONE upcoming row (page-local `page_index`) —
/// the per-track "Add to playlist" entry in the row menu
/// (`queue.rs::add_to_playlist`). Resolves through the same page-local ->
/// queue-wide mapping as every other row action, so it is correct under a
/// search filter and on any page.
pub async fn add_to_playlist(runtime: &Arc<AppRuntime<LoggingAdapter>>, page_index: usize) {
    let Some(&upcoming_index) = current_page_indices(runtime).await.get(page_index) else {
        log::warn!("[qbz-qt] queue: add_to_playlist {page_index} out of range");
        return;
    };
    let state = runtime.core().get_queue_state_full().await;
    let Some(track) = state.upcoming.get(upcoming_index) else {
        log::warn!("[qbz-qt] queue: add_to_playlist {page_index} -> no upcoming track");
        return;
    };
    if crate::local_ephemeral::is_ephemeral_id(track.id as i64) {
        log::warn!(
            "[qbz-qt] queue: add_to_playlist {page_index} is an ephemeral row — nothing to file"
        );
        return;
    }
    if track.is_local {
        // By source, the way the local surfaces do it (2026-09-14).
        match crate::local_playlist_qt::local_picker_ref_for_queue_track(track) {
            Some(r) => crate::playlist_picker_qt::open_for_local_refs(runtime, vec![r]),
            None => log::warn!(
                "[qbz-qt] queue: add_to_playlist {page_index} is a local row without a picker key"
            ),
        }
        return;
    }
    crate::playlist_picker_qt::open_for_ids(runtime, vec![track.id.to_string()]);
}

/// Toggle a heart for a queue row (Qobuz favorite), then republish.
///
/// GUARD: a LOCAL/Plex row's `id` is a library.db row id which is NUMERIC,
/// so `library_qt::is_local_feed_id` (which recognizes local tracks only by
/// their non-numeric FILE-PATH feed ids) would classify it as a Qobuz id and
/// send `add_favorite("track", "<db row id>")` — favoriting an unrelated
/// catalog track. Local hearts have no seam in this port (`local_bulk.rs`
/// documents the same gap), so the mutation is refused here AND the row
/// publishes `isLocal` so the panel can drop the control instead of showing
/// one that cannot work.
pub async fn toggle_favorite(runtime: &Arc<AppRuntime<LoggingAdapter>>, kind: &str, id: &str) {
    if kind == "track" {
        let local = canonical_u64(id)
            .map(|tid| LOCAL_ROW_IDS.lock().unwrap().contains(&tid))
            .unwrap_or(false);
        if local {
            log::warn!(
                "[qbz-qt] queue favorite: {id} is a LOCAL/Plex row — no local \
                 favorites seam in this port, refusing to route it to the \
                 Qobuz favorites API"
            );
            return;
        }
    }
    // Through the shared settle point, not `library_qt::toggle_favorite`
    // alone: the queue panel used to swallow the settled value, so a heart
    // set from here never reached `libraryFavoriteChanged` (LibraryView /
    // AlbumView / ArtistView kept the old glyph) nor the document caches
    // (Home / Search / Recommendations re-published the pre-toggle heart on
    // their next republish). The panel itself still repaints from `publish`.
    if let Some(value) = crate::library_qt::toggle_favorite(runtime, kind, id).await {
        crate::settle_favorite(runtime, kind, id, value).await;
    }
    publish(runtime).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, title: &str) -> QueueTrack {
        // Only the fields the coverflow reads matter; the rest are inert.
        // (QueueTrack has no Default impl — every field is spelled out.)
        QueueTrack {
            id,
            title: title.to_string(),
            version: None,
            artist: format!("artist-{id}"),
            album: String::new(),
            album_version: None,
            duration_secs: 0,
            artwork_url: Some(format!("https://img/{id}.jpg")),
            hires: false,
            bit_depth: None,
            sample_rate: None,
            is_local: false,
            album_id: None,
            artist_id: None,
            streamable: true,
            source: None,
            parental_warning: false,
            source_item_id_hint: None,
            context_kind: None,
            context_id: None,
            isrc: None,
            recording_mbid: None,
        }
    }

    #[test]
    fn save_as_playlist_files_every_source_it_can() {
        let mut local = track(41, "file");
        local.is_local = true;
        local.source = Some("local".into());
        let mut plex = track(4200, "plex");
        plex.is_local = true;
        plex.source = Some("plex".into());
        plex.source_item_id_hint = Some("12345".into());
        let mut jelly = track(77, "jf");
        jelly.is_local = true;
        jelly.source = Some("jellyfin".into());
        jelly.source_item_id_hint = Some("abc".into());
        let mut copy = track(900, "copy");
        copy.is_local = true;
        copy.source = Some("qobuz_download".into());
        copy.source_item_id_hint = Some("58".into());
        let mut keyless = track(78, "lost");
        keyless.is_local = true;
        keyless.source = Some("subsonic".into());
        let rows = [
            track(555, "q"),
            local,
            plex,
            jelly,
            copy,
            keyless,
            track(555, "q again"),
        ];
        let (ids, refs, skipped) = picker_payload_for(rows.iter());
        assert_eq!(ids, vec!["555".to_string()]);
        assert_eq!(
            refs,
            vec![
                "41".to_string(),
                "plex:12345".to_string(),
                "jellyfin:abc".to_string(),
                "58".to_string()
            ]
        );
        assert_eq!(skipped, 1);
    }

    /// §4.4 JSON shape: {index, tracks:[{id,title,artist,artUrl}]} over the
    /// FULL flat queue, history OLDEST-first.
    #[test]
    fn coverflow_doc_shape_and_flat_order() {
        let history = vec![track(3, "h-new"), track(2, "h-mid"), track(1, "h-old")]; // core: most-recent-first
        let current = track(4, "now");
        let upcoming = vec![track(5, "up-0"), track(6, "up-1")];
        let (flat, index) = coverflow_flat(&history, Some(&current), &upcoming);

        // Flat = [h-old, h-mid, h-new, NOW, up-0, up-1]; NOW at history.len.
        let ids: Vec<&str> = flat.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3", "4", "5", "6"]);
        assert_eq!(index, 3);

        let doc = format!(
            "{{\"index\":{index},\"tracks\":{}}}",
            serde_json::to_string(&flat).unwrap()
        );
        let v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        assert_eq!(v["index"], 3);
        let rows = v["tracks"].as_array().unwrap();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[3]["id"], "4");
        assert_eq!(rows[3]["title"], "now");
        assert_eq!(rows[3]["artist"], "artist-4");
        assert_eq!(rows[3]["artUrl"], "https://img/4.jpg");
        // Exactly the four contract fields, nothing else.
        assert_eq!(rows[3].as_object().unwrap().len(), 4);
    }

    /// NOW index with no current track: history.len (the first upcoming
    /// slot), 0 when the whole queue is empty (Slint queue.rs:315-318).
    #[test]
    fn coverflow_index_without_current() {
        let history = vec![track(2, "b"), track(1, "a")];
        let upcoming = vec![track(3, "c")];
        let (flat, index) = coverflow_flat(&history, None, &upcoming);
        let ids: Vec<&str> = flat.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3"]); // no NOW slot
        assert_eq!(index, 2);

        let (flat, index) = coverflow_flat(&[], None, &[]);
        assert!(flat.is_empty());
        assert_eq!(index, 0);
    }

    /// Id-sequence gate: a pure advance keeps the flat id-sequence (tracks
    /// array reused, only `index` moves); any membership/order change
    /// rebuilds.
    #[test]
    fn coverflow_seq_hash_gates_the_rebuild() {
        let h1 = vec![track(2, "b"), track(1, "a")]; // most-recent-first
        let cur = track(3, "c");
        let up = vec![track(4, "d")];
        let (flat1, idx1) = coverflow_flat(&h1, Some(&cur), &up);
        let hash1 = coverflow_seq_hash(&flat1);
        assert_eq!(idx1, 2);

        // PURE ADVANCE materialized: track 3 moved to history, 4 is NOW —
        // the FLAT id-sequence is IDENTICAL ([1,2,3,4]), so the gate skips
        // the tracks rebuild and only `index` moves (state.slint:4857-4864).
        let h2 = vec![track(3, "c"), track(2, "b"), track(1, "a")];
        let cur2 = track(4, "d");
        let (flat2, idx2) = coverflow_flat(&h2, Some(&cur2), &[]);
        assert_eq!(coverflow_seq_hash(&flat2), hash1);
        assert_eq!(idx2, 3);
        assert_ne!(idx2, idx1);

        // Membership change (enqueue): different sequence -> rebuild.
        let up_added = vec![track(4, "d"), track(5, "e")];
        let (flat3, _) = coverflow_flat(&h1, Some(&cur), &up_added);
        assert_ne!(coverflow_seq_hash(&flat3), hash1);

        // Reorder with identical membership: also a rebuild (order-sensitive).
        let up_swap = vec![track(5, "e"), track(4, "d")];
        let (flat4, _) = coverflow_flat(&h1, Some(&cur), &up_swap);
        assert_ne!(coverflow_seq_hash(&flat4), coverflow_seq_hash(&flat3));
    }

    #[test]
    fn extended_projection_is_chronological_and_keeps_core_indices() {
        let state = qbz_models::QueueState {
            current_track: Some(track(4, "now")),
            current_index: Some(3),
            upcoming: vec![track(5, "manual"), track(6, "source")],
            history: vec![track(3, "h-new"), track(2, "h-mid"), track(1, "h-old")],
            shuffle: false,
            repeat: qbz_models::RepeatMode::Off,
            total_tracks: 6,
            stop_after_track_id: None,
            manual_next_count: 1,
        };
        let (rows, current_index) = extended_rows(&state, &HashSet::new(), "");

        let ids: Vec<&str> = rows.iter().map(|row| row.row.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3", "4", "5", "6"]);
        assert_eq!(current_index, 3);
        assert_eq!(rows[0].phase, "history");
        assert_eq!(rows[0].phase_index, 2);
        assert_eq!(rows[2].phase_index, 0);
        assert_eq!(rows[4].row.section, "next-in-queue");
        assert_eq!(rows[5].row.section, "next-up");
    }

    #[test]
    fn extended_search_filters_all_phases_without_reindexing_actions() {
        let state = qbz_models::QueueState {
            current_track: Some(track(2, "current miss")),
            current_index: Some(1),
            upcoming: vec![track(3, "needle upcoming")],
            history: vec![track(1, "needle history")],
            shuffle: false,
            repeat: qbz_models::RepeatMode::Off,
            total_tracks: 3,
            stop_after_track_id: None,
            manual_next_count: 0,
        };
        let (rows, current_index) = extended_rows(&state, &HashSet::new(), "needle");

        assert_eq!(current_index, -1);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].phase, rows[0].phase_index), ("history", 0));
        assert_eq!((rows[1].phase, rows[1].phase_index), ("upcoming", 0));
        assert!(rows.iter().all(|row| row.row.section.is_empty()));
    }
}
