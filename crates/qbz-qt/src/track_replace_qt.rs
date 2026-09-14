//! "Find available version" — replace a track Qobuz PULLED from the catalogue
//! with a live one, in a playlist the user owns (contract §6).
//!
//! The reference (Tauri's `TrackReplacementModal.svelte`) had this feature and
//! shipped it with four defects; Slint never had it at all. All four are fixed
//! here rather than ported:
//!
//!  1. **It was unranked.** Tauri showed Qobuz's raw relevance order and let the
//!     human sort it out. The weighted matcher that already lives in
//!     `qbz-playlist-import` — ISRC short-circuit, title 0.6 / artist 0.3 /
//!     album 0.1, duration bonus, stop-word normalisation, quality tie-break —
//!     does the ranking here, through its `rank_candidates` entry point.
//!  2. **It never tried the ISRC.** Licensing churn most often re-publishes the
//!     SAME recording under a new track id with the SAME ISRC, and the dead row
//!     keeps its `isrc` (confirmed in the 2026-08-17 capture of album
//!     `0886443985094`). That case needs no human judgement at all, so
//!     `find_by_isrc` runs FIRST and its hit is pinned to the top of the list.
//!  3. **It removed before it added**, so a failed add lost the track outright.
//!     The order here is add -> reposition -> remove: if the add fails nothing
//!     was destroyed and the user simply still has a dead row.
//!  4. **It did not preserve position** — the replacement landed at the end and
//!     the computed index was used only in a `console.log`. Owner ruling §C.1:
//!     preserve it, through `/playlist/updateTracksPosition`.
//!
//! # The two guards, and why each exists
//!
//! **Same-id (§A F10).** If the ISRC lookup answers with the DEAD track itself
//! — which it can, because a pulled track keeps its metadata and Qobuz's search
//! still indexes it — then "replace" would be `add` (a `no_duplicate` no-op)
//! followed by `remove`, and the user would lose the row outright while being
//! told it was repaired. Any candidate whose id equals the dead track's is
//! dropped before it can ever be selected, and [`apply`] re-checks it.
//!
//! **Failed remove (§A F10, second half).** Add-then-remove trades "lose the
//! track" for "transient duplicate", and the duplicate is transient only if the
//! remove succeeds. When it does not, the playlist permanently holds BOTH rows.
//! This is not rolled back: a rollback is a second write that can fail the same
//! way, and it would delete the only good copy. Instead the playlist is
//! refreshed so the user SEES both rows, and the toast says plainly that the
//! old one is still there and has to go by hand — never that a reload will fix
//! it, because nothing will.
//!
//! # Position is a nicety, never a reason to abort
//!
//! The reposition step needs the membership id the server minted for the row we
//! just appended, which only a re-fetch can tell us, and `insert_before`'s exact
//! encoding is the one part of the new verb no capture in the repo pins down. So
//! every failure in that step is a `log::warn!` and a different success toast —
//! the repair still stands, at the end of the playlist, exactly where the
//! reference always put it.

use std::sync::{LazyLock, Mutex};

use cxx_qt_lib::QString;
use qbz_models::{Album, Track};
use qbz_playlist_import::{rank_candidates, ImportTrack};
use serde::{Deserialize, Serialize};

/// The reference's limit, and the matcher's own `SEARCH_LIMIT`. Twenty rows is
/// as many as a human will actually read.
const SEARCH_LIMIT: u32 = 20;
const RELEASE_TITLE_WEIGHT: f32 = 0.75;
const RELEASE_ARTIST_WEIGHT: f32 = 0.25;
const RELEASE_MIN_SCORE: f32 = 0.65;

// ---------------------------------------------------------------------------
// D3 — the session memory of tracks that died under the player
// ---------------------------------------------------------------------------

/// Ids the player found terminally unavailable DURING THIS RUN (contract §5.1
/// clause 2, ruling D3).
///
/// SESSION-ONLY on purpose. Tauri kept the equivalent set in `localStorage`
/// under `'qbz-unavailable-tracks'`, never expired it and never revalidated it,
/// so a track Qobuz restored stayed dead forever. A process-lifetime set gives
/// the same protection within a session — the reactive skip walk does not have
/// to re-learn the same dead track on every pass — and costs nothing on the next
/// launch, where `streamable` is re-read from the API anyway.
///
/// Two writers, both wired: [`forget`], on a successful replacement (the old id
/// must not keep poisoning a row the user just repaired), and [`mark`] from
/// `playback_qt::auto_skip_unavailable`, which is where a terminal
/// `TrackUnavailable` is recognised. [`contains`] is read by the queue-drop
/// predicate, so a track that dies under the player is never offered to the
/// player again this run.
///
/// KNOWN LIMIT, stated rather than papered over: the ROWS already on screen do
/// not re-render when a track dies this way — they were serialized from an API
/// response that said `streamable: true`. The row greys out on the next publish
/// of that view. The queue half is immediate; the visual half is eventually
/// consistent, and the up-front API flag is what covers the common case.
pub(crate) mod session_unavailable {
    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex};

    static IDS: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

    /// Remember that `track_id` failed terminally. Called by the reactive skip
    /// walk when it recognises a `TrackUnavailable`.
    pub(crate) fn mark(track_id: u64) {
        IDS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(track_id);
    }

    /// Did this track already die under the player this run? The second half of
    /// the detection predicate, beside `Track::is_streamable()`.
    pub(crate) fn contains(track_id: u64) -> bool {
        IDS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&track_id)
    }

    /// Drop the memory of one id. The replacement flow calls this on success:
    /// the row is gone from the playlist, and were the same catalog id to come
    /// back (rights restored), a stale entry would keep marking it dead for the
    /// rest of the session.
    pub(crate) fn forget(track_id: u64) {
        IDS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&track_id);
    }
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

/// The dead row the modal was opened for. Built by the host view, which is the
/// only place that knows the playlist and the row's membership id.
///
/// No row INDEX: the host's index is into the DISPLAYED list, which under a
/// search filter or a non-default sort is not the playlist's own order. The
/// slot is derived from the authoritative playlist in [`reposition`].
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadRow {
    pub playlist_id: String,
    /// The MEMBERSHIP id — what `remove_tracks_from_playlist` takes.
    pub playlist_track_id: String,
    /// The CATALOG id — what the same-id guard compares against.
    pub track_id: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    /// Present on a pulled track (the capture confirms it) and the reason the
    /// exact relink is reachable at all. Empty degrades the flow to text
    /// matching, which is still better than the reference.
    #[serde(default)]
    pub isrc: String,
    #[serde(default)]
    pub duration_secs: u64,
}

/// Withdrawn release opened from Library > All. Album search chooses the new
/// release; the old track evidence lets the explicit primary action resolve
/// and replace the corresponding favourite inside that release.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSeed {
    /// `track` replaces one favourite with its equivalent inside the chosen
    /// release; `album` replaces the withdrawn release favourite itself.
    pub target_kind: String,
    pub album_id: String,
    pub album_title: String,
    #[serde(default)]
    pub track_id: String,
    #[serde(default)]
    pub track_title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album_artist: String,
    #[serde(default)]
    pub isrc: String,
    #[serde(default)]
    pub duration_secs: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct CandidateRow {
    pub id: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    pub duration: String,
    /// Release mode uses this for the catalog year. Empty for playlist-track
    /// candidates so the existing row layout remains unchanged.
    pub year: String,
    pub score: f32,
    /// ISRC-identical to the dead row: the SAME recording under a new id. The
    /// modal says so, because it is the difference between a certainty and a
    /// good guess.
    pub exact: bool,
    /// Below `MIN_MATCH_SCORE`, the floor the importer refuses to auto-match on.
    /// The row is still OFFERED — a human is confirming, and a weak candidate is
    /// often the only one that exists — but it is labelled, so "the modal
    /// suggested it" never reads as "the app is sure".
    pub weak: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct ReplaceDoc {
    pub open: bool,
    /// `track` (playlist mutation) or `release` (Library discovery/navigation).
    pub mode: String,
    #[serde(rename = "targetKind")]
    pub target_kind: String,
    pub loading: bool,
    pub applying: bool,
    pub query: String,
    #[serde(rename = "deadTitle")]
    pub dead_title: String,
    #[serde(rename = "deadArtist")]
    pub dead_artist: String,
    pub candidates: Vec<CandidateRow>,
    #[serde(rename = "selectedId")]
    pub selected_id: String,
    #[serde(rename = "selectedAlbumId")]
    pub selected_album_id: String,
    #[serde(rename = "hasExact")]
    pub has_exact: bool,
}

#[derive(Default)]
struct ReplaceState {
    open: bool,
    mode: String,
    loading: bool,
    applying: bool,
    dead: DeadRow,
    release: ReleaseSeed,
    query: String,
    candidates: Vec<CandidateRow>,
    selected_id: String,
    /// Bumped on every open and every re-search. A search that lands AFTER the
    /// user retyped must not overwrite the newer results — and the picker's
    /// `if !open { return }` guard is not enough here, because a re-search
    /// leaves the modal open the whole time.
    generation: u64,
}

static STATE: LazyLock<Mutex<ReplaceState>> = LazyLock::new(|| Mutex::new(ReplaceState::default()));

fn with_state<R>(f: impl FnOnce(&mut ReplaceState) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

fn publish() {
    let doc = with_state(|st| ReplaceDoc {
        open: st.open,
        mode: st.mode.clone(),
        target_kind: if st.mode == "release" {
            st.release.target_kind.clone()
        } else {
            "playlist".into()
        },
        loading: st.loading,
        applying: st.applying,
        query: st.query.clone(),
        dead_title: if st.mode == "release" && st.release.target_kind == "track" {
            st.release.track_title.clone()
        } else if st.mode == "release" {
            st.release.album_title.clone()
        } else {
            st.dead.title.clone()
        },
        dead_artist: if st.mode == "release" {
            st.release.artist.clone()
        } else {
            st.dead.artist.clone()
        },
        candidates: st.candidates.clone(),
        selected_id: st.selected_id.clone(),
        selected_album_id: st
            .candidates
            .iter()
            .find(|candidate| candidate.id == st.selected_id)
            .map(|candidate| candidate.album_id.clone())
            .unwrap_or_default(),
        has_exact: st.candidates.iter().any(|c| c.exact),
    });
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    crate::track_replace_bridge::ui(move |mut b| {
        b.as_mut().set_replace_json(QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
// Invokables
// ---------------------------------------------------------------------------

/// Open the modal for one dead row. `payload_json` is the host's [`DeadRow`].
pub(crate) fn open(payload_json: &str) {
    let dead: DeadRow = match serde_json::from_str(payload_json) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[qbz-qt] track replace: unreadable payload: {e}");
            return;
        }
    };
    if dead.playlist_id.is_empty() || dead.track_id.is_empty() {
        log::warn!("[qbz-qt] track replace: payload without a playlist or track id, ignored");
        return;
    }

    // The reference's query, verbatim: "title artist", nothing clever. The
    // matcher does the work; the query only has to reach the right 20 rows.
    let query = format!("{} {}", dead.title, dead.artist);
    let generation = with_state(|st| {
        st.open = true;
        st.mode = "track".into();
        st.loading = true;
        st.applying = false;
        st.dead = dead.clone();
        st.release = ReleaseSeed::default();
        st.query = query.clone();
        // A stale list would show the PREVIOUS row's candidates under this
        // row's title for as long as the search takes.
        st.candidates.clear();
        st.selected_id.clear();
        st.generation = st.generation.wrapping_add(1);
        st.generation
    });
    publish();

    crate::spawn(async move { run_search(dead, query, generation).await });
}

/// Open the same ranked-candidate surface in Library RELEASE mode. Unlike the
/// playlist path this does not swap a track: the selected result is opened and
/// all favourite cleanup stays behind its own explicit action.
pub(crate) fn open_release(payload_json: &str) {
    let release: ReleaseSeed = match serde_json::from_str(payload_json) {
        Ok(seed) => seed,
        Err(error) => {
            log::warn!("[qbz-qt] release replace: unreadable payload: {error}");
            return;
        }
    };
    let valid_target = match release.target_kind.as_str() {
        "album" => true,
        "track" => !release.track_id.is_empty() && !release.track_title.trim().is_empty(),
        _ => false,
    };
    if release.album_id.is_empty() || release.album_title.trim().is_empty() || !valid_target {
        log::warn!("[qbz-qt] release replace: invalid album/track replacement payload, ignored");
        return;
    }

    // The query is intentionally the edition-stripped TITLE ALONE. Artist is
    // a ranking signal, not a word that can push the right release beyond the
    // API's first 20 hits.
    let normalized = qbz_external_reco::normalize_catalog_name(&release.album_title);
    let query = if normalized.is_empty() {
        release.album_title.trim().to_string()
    } else {
        normalized
    };
    let generation = with_state(|st| {
        st.open = true;
        st.mode = "release".into();
        st.loading = true;
        st.applying = false;
        st.dead = DeadRow::default();
        st.release = release.clone();
        st.query = query.clone();
        st.candidates.clear();
        st.selected_id.clear();
        st.generation = st.generation.wrapping_add(1);
        st.generation
    });
    publish();
    crate::spawn(async move { run_release_search(release, query, generation).await });
}

/// The query is editable and re-searchable (the reference's one good idea).
pub(crate) fn search(query: &str) {
    let query = query.trim().to_string();
    if query.is_empty() {
        return;
    }
    let Some((mode, dead, release, generation)) = with_state(|st| {
        if !st.open || st.applying {
            return None;
        }
        st.query = query.clone();
        st.loading = true;
        st.generation = st.generation.wrapping_add(1);
        Some((
            st.mode.clone(),
            st.dead.clone(),
            st.release.clone(),
            st.generation,
        ))
    }) else {
        return;
    };
    publish();

    crate::spawn(async move {
        if mode == "release" {
            run_release_search(release, query, generation).await;
        } else {
            run_search(dead, query, generation).await;
        }
    });
}

/// Pick a candidate. An id that is not in the list is ignored rather than
/// stored: the apply path trusts this field, and a selection the user cannot
/// see is exactly the state the same-id guard exists to keep out.
pub(crate) fn select(candidate_id: &str) {
    let changed = with_state(|st| {
        if st.applying || !st.candidates.iter().any(|c| c.id == candidate_id) {
            return false;
        }
        st.selected_id = candidate_id.to_string();
        true
    });
    if changed {
        publish();
    }
}

/// Non-mutating inspection path shared by both picker modes. Release rows use
/// their own id; playlist-track rows carry their embedded album id. A terse
/// search result with no album id leaves the button disabled in QML and is
/// guarded again here.
pub(crate) fn open_selected_album() {
    let album_id = with_state(|st| {
        if st.applying || !st.open {
            return String::new();
        }
        st.candidates
            .iter()
            .find(|candidate| candidate.id == st.selected_id)
            .map(|candidate| candidate.album_id.clone())
            .unwrap_or_default()
    });
    if album_id.is_empty() {
        return;
    }
    close();
    crate::open_album(album_id);
}

pub(crate) fn close() {
    with_state(|st| {
        st.open = false;
        st.mode.clear();
        st.loading = false;
        st.applying = false;
        st.dead = DeadRow::default();
        st.release = ReleaseSeed::default();
        st.query.clear();
        st.candidates.clear();
        st.selected_id.clear();
    });
    publish();
}

/// Take the applying latch back down without touching anything else — the
/// shared tail of every refusal and every failure in [`apply`], so no path can
/// leave the modal wedged with a spinning confirm button.
fn stop_applying() {
    with_state(|st| st.applying = false);
    publish();
}

// ---------------------------------------------------------------------------
// The search: ISRC first, then the ranked text search
// ---------------------------------------------------------------------------

/// Both paths drop the dead track's own id (the same-id guard) and both keep
/// only streamable candidates — offering a replacement that is itself dead is
/// the one outcome this whole feature exists to prevent.
async fn run_search(dead: DeadRow, query: String, generation: u64) {
    let runtime = crate::app();
    let dead_id: u64 = dead.track_id.parse().unwrap_or(0);

    // 1. The exact relink. A hit here is the "same recording, new album id"
    //    case and needs no human judgement, so it is pinned at the top and
    //    preselected — but it is still SHOWN, never applied silently.
    let mut exact: Option<Track> = None;
    if !dead.isrc.is_empty() {
        let catalog = crate::external_reco_qt::CoreRecoCatalog {
            runtime: runtime.clone(),
        };
        if let Some(hit) = qbz_external_reco::find_by_isrc(&catalog, &dead.isrc).await {
            if hit.id == dead_id {
                // THE SAME-ID GUARD. Qobuz's search still indexes the pulled
                // track, so the ISRC lookup can hand back the very row that is
                // dead. Accepting it would make `add` a no_duplicate no-op and
                // the following `remove` would then delete the track outright —
                // the user loses the row and is told it was repaired.
                log::info!(
                    "[qbz-qt] track replace: ISRC {} resolved to the dead track {} itself, ignored",
                    dead.isrc,
                    dead_id
                );
            } else {
                exact = Some(hit);
            }
        }
    }

    // 2. The ranked text search. `ImportTrack` is the matcher's input shape and
    //    a playlist row fills every field of it that scores.
    let source = ImportTrack {
        title: dead.title.clone(),
        artist: dead.artist.clone(),
        album: (!dead.album.is_empty()).then(|| dead.album.clone()),
        duration_ms: (dead.duration_secs > 0).then(|| dead.duration_secs * 1000),
        isrc: (!dead.isrc.is_empty()).then(|| dead.isrc.clone()),
        provider_id: None,
        provider_url: None,
    };
    let found = runtime
        .core()
        .search_tracks(&query, SEARCH_LIMIT, 0, None)
        .await
        .map(|page| page.items)
        .unwrap_or_else(|e| {
            log::warn!("[qbz-qt] track replace: search '{query}' failed: {e}");
            Vec::new()
        });

    let mut rows: Vec<CandidateRow> = Vec::new();
    if let Some(hit) = exact.as_ref() {
        rows.push(map_candidate(hit, 1.0, true));
    }
    for (track, score) in rank_candidates(&source, &found) {
        // The same-id guard again, on the text path: the pulled track is still
        // indexed and will usually be the TOP text hit for its own title.
        if track.id == dead_id {
            continue;
        }
        if exact.as_ref().map(|hit| hit.id) == Some(track.id) {
            continue;
        }
        rows.push(map_candidate(&track, score, false));
    }

    let art_urls: Vec<String> = rows
        .iter()
        .filter(|row| row.art_path.is_empty() && !row.art_url.is_empty())
        .map(|row| row.art_url.clone())
        .collect();

    let landed = with_state(|st| {
        // Landed late: the user retyped, or closed the modal. Dropping the
        // result is correct — the newer search owns the list.
        if !st.open || st.generation != generation {
            return false;
        }
        st.selected_id = rows.first().map(|row| row.id.clone()).unwrap_or_default();
        st.candidates = rows;
        st.loading = false;
        true
    });
    if !landed {
        return;
    }
    publish();

    // Covers arrive after the list, exactly like every other rail in this port:
    // the decision aids the human actually reads (title, artist, album, tier,
    // duration) are already on screen.
    if !art_urls.is_empty() {
        crate::artwork_qt::download_missing(art_urls).await;
        let still_ours = with_state(|st| {
            if !st.open || st.generation != generation {
                return false;
            }
            for row in st.candidates.iter_mut() {
                if row.art_path.is_empty() && !row.art_url.is_empty() {
                    row.art_path = crate::artwork_qt::cached_path(&row.art_url);
                }
            }
            true
        });
        if still_ours {
            publish();
        }
    }
}

/// Release search deliberately uses `/album/search` rather than trying to
/// infer a new album id from track hits. The user asked to choose among
/// editions, so one row must equal one release.
async fn run_release_search(release: ReleaseSeed, query: String, generation: u64) {
    let runtime = crate::app();
    let found = runtime
        .core()
        .search_albums(&query, SEARCH_LIMIT, 0, None)
        .await
        .map(|page| page.items)
        .unwrap_or_else(|error| {
            log::warn!("[qbz-qt] release replace: search '{query}' failed: {error}");
            Vec::new()
        });

    let rows: Vec<CandidateRow> = rank_release_candidates(&release, &found)
        .into_iter()
        .map(|(album, score)| map_release_candidate(&album, score))
        .collect();
    let art_urls: Vec<String> = rows
        .iter()
        .filter(|row| row.art_path.is_empty() && !row.art_url.is_empty())
        .map(|row| row.art_url.clone())
        .collect();

    let landed = with_state(|st| {
        if !st.open || st.mode != "release" || st.generation != generation {
            return false;
        }
        st.selected_id = rows.first().map(|row| row.id.clone()).unwrap_or_default();
        st.candidates = rows;
        st.loading = false;
        true
    });
    if !landed {
        return;
    }
    publish();

    if !art_urls.is_empty() {
        crate::artwork_qt::download_missing(art_urls).await;
        let still_ours = with_state(|st| {
            if !st.open || st.mode != "release" || st.generation != generation {
                return false;
            }
            for row in st.candidates.iter_mut() {
                if row.art_path.is_empty() && !row.art_url.is_empty() {
                    row.art_path = crate::artwork_qt::cached_path(&row.art_url);
                }
            }
            true
        });
        if still_ours {
            publish();
        }
    }
}

/// The ranked release candidates for a title (and artist) the catalog no
/// longer has — the album page's "you might be interested in these" rail
/// (2026-09-13). Same query normalisation, search and ranking as the
/// replacement flow; no modal state is touched. Empty when nothing to ask.
pub(crate) async fn ranked_alternatives(album_id: &str, title: &str, artist: &str) -> Vec<Album> {
    let seed: ReleaseSeed = match serde_json::from_value(serde_json::json!({
        "targetKind": "album",
        "albumId": album_id,
        "albumTitle": title,
        "artist": artist,
        "albumArtist": artist,
    })) {
        Ok(seed) => seed,
        Err(_) => return Vec::new(),
    };
    let normalized = qbz_external_reco::normalize_catalog_name(title);
    let query = if normalized.is_empty() {
        title.trim().to_string()
    } else {
        normalized
    };
    if query.is_empty() {
        return Vec::new();
    }
    let runtime = crate::app();
    let found = runtime
        .core()
        .search_albums(&query, SEARCH_LIMIT, 0, None)
        .await
        .map(|page| page.items)
        .unwrap_or_else(|error| {
            log::warn!("[qbz-qt] album alternatives: search '{query}' failed: {error}");
            Vec::new()
        });
    rank_release_candidates(&seed, &found)
        .into_iter()
        .map(|(album, _)| album)
        .collect()
}

fn release_quality(album: &Album) -> (u32, f64) {
    let bit_depth = album
        .audio_info
        .as_ref()
        .and_then(|info| info.maximum_bit_depth)
        .or(album.maximum_bit_depth)
        .unwrap_or(0);
    let sample_rate = album
        .audio_info
        .as_ref()
        .and_then(|info| info.maximum_sampling_rate)
        .or(album.maximum_sampling_rate)
        .unwrap_or(0.0);
    (bit_depth, sample_rate)
}

/// Best semantic match first; quality only breaks a score tie. The sort is
/// stable so Qobuz relevance remains the final tie-breaker.
fn rank_release_candidates(seed: &ReleaseSeed, candidates: &[Album]) -> Vec<(Album, f32)> {
    let seed_artist = if seed.album_artist.trim().is_empty() {
        &seed.artist
    } else {
        &seed.album_artist
    };
    let mut ranked: Vec<(Album, f32)> = candidates
        .iter()
        .filter(|album| album.id != seed.album_id && album.is_streamable())
        .map(|album| {
            let title = crate::album_qt::format_album_title(&album.title, album.version.as_deref());
            let title_score = qbz_external_reco::catalog_similarity(&seed.album_title, &title);
            let artist_score =
                qbz_external_reco::catalog_similarity(seed_artist, &album.artist.name);
            let score = title_score * RELEASE_TITLE_WEIGHT
                + if seed_artist.trim().is_empty() {
                    0.0
                } else {
                    artist_score * RELEASE_ARTIST_WEIGHT
                };
            (album.clone(), score)
        })
        .collect();

    fn bucket(score: f32) -> i32 {
        (score * 100.0).round() as i32
    }
    ranked.sort_by(|(a, a_score), (b, b_score)| {
        let (a_depth, a_rate) = release_quality(a);
        let (b_depth, b_rate) = release_quality(b);
        bucket(*b_score)
            .cmp(&bucket(*a_score))
            .then_with(|| b_depth.cmp(&a_depth))
            .then_with(|| b_rate.total_cmp(&a_rate))
    });
    ranked
}

fn map_release_candidate(album: &Album, score: f32) -> CandidateRow {
    let art_url = album.image.best().cloned().unwrap_or_default();
    let (bit_depth, sample_rate) = release_quality(album);
    let date = album
        .dates
        .as_ref()
        .and_then(|dates| {
            dates
                .original
                .as_deref()
                .or(dates.stream.as_deref())
                .or(dates.download.as_deref())
        })
        .or(album.release_date_original.as_deref());
    CandidateRow {
        id: album.id.clone(),
        album_id: album.id.clone(),
        title: crate::album_qt::format_album_title(&album.title, album.version.as_deref()),
        artist: album.artist.name.clone(),
        album: String::new(),
        art_path: crate::artwork_qt::cached_path(&art_url),
        art_url,
        quality_tier: crate::playlist_qt::tier((bit_depth > 0).then_some(bit_depth)).to_string(),
        quality_detail: crate::home_qt::quality_detail_from_parts(
            (bit_depth > 0).then_some(bit_depth),
            (sample_rate > 0.0).then_some(sample_rate),
        ),
        duration: String::new(),
        year: qbz_text_utils::dates::release_label(date),
        score,
        exact: false,
        weak: score < RELEASE_MIN_SCORE,
    }
}

fn map_candidate(track: &Track, score: f32, exact: bool) -> CandidateRow {
    let album = track.album.as_ref();
    let art_url = album
        .and_then(|a| a.image.best().cloned())
        .unwrap_or_default();
    CandidateRow {
        id: track.id.to_string(),
        album_id: album.map(|album| album.id.clone()).unwrap_or_default(),
        // The version suffix is load-bearing HERE above anywhere else: "(2011
        // Remaster)" is often the entire difference between the candidates.
        title: match track.version.as_ref().filter(|v| !v.is_empty()) {
            Some(version) => format!("{} ({version})", track.title),
            None => track.title.clone(),
        },
        artist: track
            .performer
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        album: album.map(|a| a.title.clone()).unwrap_or_default(),
        art_path: crate::artwork_qt::cached_path(&art_url),
        art_url,
        quality_tier: crate::playlist_qt::tier(track.maximum_bit_depth).to_string(),
        quality_detail: crate::home_qt::quality_detail_from_parts(
            track.maximum_bit_depth,
            track.maximum_sampling_rate,
        ),
        duration: crate::playlist_qt::mmss(track.duration),
        year: String::new(),
        score,
        exact,
        // An ISRC hit is never weak, whatever the text says about it.
        weak: !exact && score < qbz_playlist_import::MIN_MATCH_SCORE,
    }
}

// ---------------------------------------------------------------------------
// Apply: ADD -> REPOSITION -> REMOVE
// ---------------------------------------------------------------------------

/// See the module header for the playlist transaction. Release mode uses the
/// same safety invariant for favourites: add the live replacement first and
/// only then remove the unavailable heart.
pub(crate) fn apply() {
    let Some((mode, dead, release, selected)) = with_state(|st| {
        if st.applying || st.selected_id.is_empty() || !st.open {
            return None;
        }
        st.applying = true;
        Some((
            st.mode.clone(),
            st.dead.clone(),
            st.release.clone(),
            st.selected_id.clone(),
        ))
    }) else {
        return;
    };
    publish();

    if mode == "release" {
        apply_library_replacement(release, selected);
        return;
    }

    let (Ok(pid), Ok(new_id), Ok(dead_ptid)) = (
        dead.playlist_id.parse::<u64>(),
        selected.parse::<u64>(),
        dead.playlist_track_id.parse::<u64>(),
    ) else {
        log::error!("[qbz-qt] track replace: unparseable ids, refusing to write");
        stop_applying();
        return;
    };
    let dead_id = dead.track_id.parse::<u64>().unwrap_or(0);

    // Belt and braces on the same-id guard: the list already drops it, so
    // reaching here means something upstream changed and the write must not go
    // out — `add` would be a no-op and `remove` would then delete the track.
    if new_id == dead_id {
        log::error!(
            "[qbz-qt] track replace: candidate {new_id} IS the dead track — \
             the add would no-op and the remove would delete it, refusing"
        );
        stop_applying();
        return;
    }

    let runtime = crate::app();
    crate::spawn(async move {
        // STEP 1 — ADD. First, so a failure here destroys nothing: the user is
        // left with the dead row they already had.
        if let Err(e) = runtime.core().add_tracks_to_playlist(pid, &[new_id]).await {
            log::error!("[qbz-qt] track replace: add {new_id} to playlist {pid} failed: {e}");
            stop_applying();
            crate::toast_qt::error(qbz_i18n::t("Could not replace the track"));
            return;
        }

        // STEP 2 — REPOSITION (owner ruling §C.1). Best-effort by contract: a
        // failure here leaves a GOOD repair at the end of the playlist, which
        // is exactly what the reference always shipped.
        let positioned = reposition(&runtime, pid, new_id, dead_ptid).await;

        // STEP 3 — REMOVE the dead row. The one write whose failure the user
        // has to act on: the playlist then holds BOTH rows, permanently, and no
        // reload will resolve it.
        if let Err(e) = runtime
            .core()
            .remove_tracks_from_playlist(pid, &[dead_ptid])
            .await
        {
            log::error!(
                "[qbz-qt] track replace: the replacement landed but removing the dead row \
                 {dead_ptid} from playlist {pid} failed: {e}"
            );
            // Refresh FIRST, so the sentence the user reads is already true on
            // screen: both rows are there, and one of them is theirs to delete.
            crate::playlist_qt::refresh_after_membership_change(&runtime, pid).await;
            close();
            crate::toast_qt::error(qbz_i18n::t(
                "The replacement was added, but the unavailable track could not be removed — remove it manually",
            ));
            return;
        }

        // The row is gone and the repair stands: a stale session entry for the
        // old id would keep marking it dead for the rest of the run (D3).
        session_unavailable::forget(dead_id);

        crate::playlist_qt::refresh_after_membership_change(&runtime, pid).await;
        close();
        // Two msgids, not one with a spliced clause: the position outcome
        // changes what the sentence CLAIMS, and every locale must be free to
        // order it its own way.
        crate::toast_qt::success(if positioned {
            qbz_i18n::t("Track replaced")
        } else {
            qbz_i18n::t("The replacement was added at the end of the playlist")
        });
    });
}

fn release_track_source(seed: &ReleaseSeed) -> ImportTrack {
    ImportTrack {
        title: seed.track_title.clone(),
        artist: seed.artist.clone(),
        album: (!seed.album_title.is_empty()).then(|| seed.album_title.clone()),
        duration_ms: (seed.duration_secs > 0).then(|| seed.duration_secs * 1000),
        isrc: (!seed.isrc.is_empty()).then(|| seed.isrc.clone()),
        provider_id: None,
        provider_url: None,
    }
}

/// Resolve the old recording inside the release the human selected. Album
/// track payloads can omit `performer`; fill only that absent evidence from the
/// album's main artist before using the shared matcher. ISRC equality remains
/// its 1.0 short-circuit. A text-only guess below the importer's own confidence
/// floor is refused.
fn matching_track_in_release(seed: &ReleaseSeed, album: &Album) -> Option<(Track, f32, bool)> {
    let old_id = seed.track_id.parse::<u64>().unwrap_or(0);
    let mut candidates = album.tracks.as_ref()?.items.clone();
    candidates.retain(|track| track.id != old_id && track.is_streamable());
    for track in &mut candidates {
        if track.performer.is_none() && !album.artist.name.is_empty() {
            track.performer = Some(album.artist.clone());
        }
    }

    let source = release_track_source(seed);
    let (track, score) = rank_candidates(&source, &candidates).into_iter().next()?;
    let exact = !seed.isrc.trim().is_empty()
        && track
            .isrc
            .as_deref()
            .map(|isrc| seed.isrc.eq_ignore_ascii_case(isrc))
            .unwrap_or(false);
    (exact || score >= qbz_playlist_import::MIN_MATCH_SCORE).then_some((track, score, exact))
}

fn apply_library_replacement(seed: ReleaseSeed, album_id: String) {
    let runtime = crate::app();
    crate::spawn(async move {
        if seed.target_kind == "album" {
            log::info!(
                "[qbz-qt] library album replace: {} -> {}",
                seed.album_id,
                album_id
            );
            replace_library_favorite(&runtime, "album", &seed.album_id, &album_id).await;
            return;
        }

        let album = match runtime.core().get_album(&album_id).await {
            Ok(album) => album,
            Err(error) => {
                log::error!(
                    "[qbz-qt] library track replace: selected album {album_id} failed: {error}"
                );
                stop_applying();
                crate::toast_qt::error(qbz_i18n::t("Could not open the selected release"));
                return;
            }
        };
        let Some((replacement, score, exact)) = matching_track_in_release(&seed, &album) else {
            log::warn!(
                "[qbz-qt] library track replace: no confident match for {} in album {album_id}",
                seed.track_id
            );
            stop_applying();
            crate::toast_qt::error(qbz_i18n::t(
                "Could not find a matching track in this release",
            ));
            return;
        };
        let new_id = replacement.id.to_string();
        if new_id == seed.track_id {
            log::error!(
                "[qbz-qt] library track replace: selected release resolved to dead id {new_id}"
            );
            stop_applying();
            crate::toast_qt::error(qbz_i18n::t(
                "Could not find a matching track in this release",
            ));
            return;
        }
        log::info!(
            "[qbz-qt] library track replace: {} -> {} via album {} (score={score:.3}, exact_isrc={exact})",
            seed.track_id,
            new_id,
            album_id
        );

        replace_library_favorite(&runtime, "track", &seed.track_id, &new_id).await;
    });
}

/// Add-first favourite swap shared by album and track tombstones. Every server
/// success settles the feed/cache independently; a failed removal deliberately
/// keeps both hearts because deleting the new live one would be a second write
/// exposed to the same failure.
async fn replace_library_favorite(
    runtime: &std::sync::Arc<qbz_app::shell::AppRuntime<qbz_core::LoggingAdapter>>,
    kind: &str,
    old_id: &str,
    new_id: &str,
) {
    if old_id == new_id {
        log::error!("[qbz-qt] library {kind} replace: old and new id are both {old_id}");
        stop_applying();
        crate::toast_qt::error(qbz_i18n::t("Could not add the replacement favorite"));
        return;
    }

    // Add first. A failed create leaves the old favourite untouched.
    if let Err(error) = runtime.core().add_favorite(kind, new_id).await {
        log::error!("[qbz-qt] library {kind} replace: add favourite {new_id} failed: {error}");
        stop_applying();
        crate::toast_qt::error(qbz_i18n::t("Could not add the replacement favorite"));
        return;
    }

    crate::fav_cache_qt::set(kind, new_id, true);
    crate::library_qt::set_feed_favorite(kind, new_id, true);
    let mut membership_changed =
        crate::library_qt::reconcile_qobuz_favorite(runtime, kind, new_id, true).await;
    crate::emit_library_favorite(kind, new_id, true);

    let old_removed = match runtime.core().remove_favorite(kind, old_id).await {
        Ok(()) => {
            crate::fav_cache_qt::set(kind, old_id, false);
            crate::library_qt::set_feed_favorite(kind, old_id, false);
            membership_changed |=
                crate::library_qt::reconcile_qobuz_favorite(runtime, kind, old_id, false).await;
            crate::emit_library_favorite(kind, old_id, false);
            if kind == "track" {
                if let Ok(old_track_id) = old_id.parse::<u64>() {
                    session_unavailable::forget(old_track_id);
                }
            }
            true
        }
        Err(error) => {
            log::error!(
                "[qbz-qt] library {kind} replace: replacement {new_id} landed but removing {old_id} failed: {error}"
            );
            false
        }
    };

    if membership_changed {
        crate::publish_library_document();
    }
    close();
    if old_removed {
        crate::toast_qt::success(qbz_i18n::t("Favorite replaced"));
    } else {
        crate::toast_qt::error(qbz_i18n::t(
            "The replacement was added, but the unavailable favorite could not be removed",
        ));
    }
}

/// Move the freshly-appended replacement into the dead row's slot.
///
/// Returns whether the move actually happened. EVERY exit is a `false` plus a
/// `log::warn!`, never an error the caller propagates: the add already landed,
/// and the repair must not be abandoned over a cosmetic ordering.
///
/// The membership id of the appended row is not knowable without a re-fetch —
/// `addTracks` answers with the playlist envelope, not with the id it minted —
/// so the authoritative playlist is read back and the row is found by
/// `(catalog id == new_id)`, taking the LAST such row: the user may legitimately
/// already own another copy of that recording earlier in the list, and the one
/// we just appended is the final one.
///
/// `insert_before` is the dead row's own 0-based index, so that after step 3
/// removes it the replacement occupies exactly the slot it vacated.
async fn reposition(
    runtime: &std::sync::Arc<qbz_app::shell::AppRuntime<qbz_core::LoggingAdapter>>,
    playlist_id: u64,
    new_id: u64,
    dead_ptid: u64,
) -> bool {
    let playlist = match runtime.core().get_playlist(playlist_id).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                "[qbz-qt] track replace: re-fetch of playlist {playlist_id} failed ({e}) — \
                 the replacement stays at the end"
            );
            return false;
        }
    };
    let Some(items) = playlist.tracks.as_ref().map(|c| &c.items) else {
        log::warn!(
            "[qbz-qt] track replace: playlist {playlist_id} came back without its tracks — \
             the replacement stays at the end"
        );
        return false;
    };

    let Some(dead_slot) = items
        .iter()
        .position(|t| t.playlist_track_id == Some(dead_ptid))
    else {
        // The dead row is already gone (a second client, or a retry after a
        // partially-applied attempt). Nothing to slot in front of.
        log::warn!(
            "[qbz-qt] track replace: the dead membership {dead_ptid} is no longer in playlist \
             {playlist_id} — skipping the reposition"
        );
        return false;
    };
    let Some(new_ptid) = items
        .iter()
        .rev()
        .find(|t| t.id == new_id)
        .and_then(|t| t.playlist_track_id)
    else {
        log::warn!(
            "[qbz-qt] track replace: the appended track {new_id} carries no membership id in \
             playlist {playlist_id} — the replacement stays at the end"
        );
        return false;
    };

    if let Err(e) = runtime
        .core()
        .update_playlist_tracks_position(playlist_id, &[new_ptid], dead_slot as u32)
        .await
    {
        log::warn!(
            "[qbz-qt] track replace: updateTracksPosition({playlist_id}, {new_ptid}, \
             insert_before={dead_slot}) failed ({e}) — the replacement stays at the end"
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: &str, title: &str, artist: &str, depth: u32, rate: f64) -> Album {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": title,
            "artist": { "id": 1, "name": artist },
            "streamable": true,
            "maximum_bit_depth": depth,
            "maximum_sampling_rate": rate
        }))
        .expect("album fixture")
    }

    #[test]
    fn release_query_cleanup_strips_edition_noise_and_leading_punctuation() {
        assert_eq!(
            qbz_external_reco::normalize_catalog_name("...And Justice for All (Remastered 2018)"),
            "and justice for all"
        );
    }

    #[test]
    fn release_ranking_filters_old_and_unavailable_ids_and_uses_quality_for_ties() {
        let seed = ReleaseSeed {
            target_kind: "album".into(),
            album_id: "old".into(),
            album_title: "...And Justice for All (Remastered 2018)".into(),
            artist: "Metallica".into(),
            album_artist: "Metallica".into(),
            ..Default::default()
        };
        let same = album("old", "...And Justice for All", "Metallica", 24, 192.0);
        let mut unavailable = album("gone", "...And Justice for All", "Metallica", 24, 192.0);
        unavailable.streamable = Some(false);
        let cd = album("cd", "...And Justice for All", "Metallica", 16, 44.1);
        let hires = album("hires", "...And Justice for All", "Metallica", 24, 96.0);
        let wrong_artist = album(
            "tribute",
            "...And Justice for All",
            "Various Artists",
            24,
            192.0,
        );

        let ranked = rank_release_candidates(&seed, &[same, unavailable, cd, wrong_artist, hires]);
        let ids: Vec<&str> = ranked.iter().map(|(album, _)| album.id.as_str()).collect();
        assert_eq!(ids, vec!["hires", "cd", "tribute"]);
    }

    #[test]
    fn release_candidate_keeps_version_year_and_quality_as_decision_aids() {
        let mut candidate = album("new", "Album", "Artist", 24, 96.0);
        candidate.version = Some("Remastered 2024".into());
        candidate.release_date_original = Some("1988-09-07".into());
        let row = map_release_candidate(&candidate, 1.0);
        assert_eq!(row.title, "Album (Remastered 2024)");
        assert_eq!(row.artist, "Artist");
        assert!(!row.year.is_empty());
        assert!(!row.quality_detail.is_empty());
    }

    #[test]
    fn selected_release_resolves_the_live_equivalent_track() {
        let seed = ReleaseSeed {
            target_kind: "track".into(),
            album_id: "old-album".into(),
            album_title: "Album (Remastered 2018)".into(),
            track_id: "10".into(),
            track_title: "One (Remastered 2018)".into(),
            artist: "Metallica".into(),
            album_artist: "Metallica".into(),
            isrc: "US-OLD-123".into(),
            duration_secs: 447,
        };
        let mut selected = album("new-album", "Album", "Metallica", 24, 96.0);
        selected.tracks = Some(qbz_models::TracksContainer {
            items: vec![serde_json::from_value(serde_json::json!({
                "id": 20,
                "title": "One",
                "isrc": "US-OLD-123",
                "duration": 447,
                "streamable": true
            }))
            .unwrap()],
            total: 1,
        });

        let (track, score, exact) = matching_track_in_release(&seed, &selected).unwrap();
        assert_eq!(track.id, 20);
        assert_eq!(score, 1.0);
        assert!(exact);
    }

    #[test]
    fn selected_release_refuses_an_unrelated_track() {
        let seed = ReleaseSeed {
            target_kind: "track".into(),
            album_id: "old-album".into(),
            album_title: "Album".into(),
            track_id: "10".into(),
            track_title: "One".into(),
            artist: "Metallica".into(),
            album_artist: "Metallica".into(),
            ..Default::default()
        };
        let mut selected = album("new-album", "Album", "Metallica", 24, 96.0);
        selected.tracks = Some(qbz_models::TracksContainer {
            items: vec![serde_json::from_value(serde_json::json!({
                "id": 21,
                "title": "Completely Different",
                "performer": { "id": 2, "name": "Someone Else" },
                "duration": 120,
                "streamable": true
            }))
            .unwrap()],
            total: 1,
        });

        assert!(matching_track_in_release(&seed, &selected).is_none());
    }
}
