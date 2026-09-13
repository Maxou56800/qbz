//! Playlist detail controller — the QML port of the Slint PlaylistView
//! surface (crates/qbz/src/playlist.rs load + the PlaylistActions glue in
//! main.rs). Publishes ONE JSON document (`playlistJson`).
//!
//! Wired:
//! - load: `get_playlist` (full track list inline) + header mapping
//!   (the playlist's OWN `image_rectangle` graphic else the member-cover
//!   mosaic urls, owner/count/total duration, HTML-stripped description +
//!   word-boundary 160-char short).
//! - play all / shuffle (the playlist track list as the queue).
//! - favorite toggle (owned), follow/unfollow (foreign, subscribe API,
//!   optimistic flip + revert), copy-to-library (create + add all ids),
//!   pin (shared pinned store), rename (`update_playlist`), delete
//!   (`delete_playlist` + nav back).
//! - in-playlist search filter + sort (default/title/artist/album/
//!   duration/added/custom), "custom" = the drag order.
//! - per-row Remove from playlist (owner) via `remove_tracks_from_playlist`
//!   (the playlist's own track-row ids).
//! - drag reorder (issue #589): visible-index move + persistence of the
//!   custom order.
//!
//! LOCAL playlists are NOT out of scope any more (that POC-NOTE was stale and
//! it had already misled one call site into stamping `"source": "qobuz"` on
//! every row): `local_playlist_qt::load` resolves a `local:<uuid>` detail and
//! publishes it THROUGH THIS DOCUMENT via [`adopt_doc`]. Consequences for
//! anyone reading a row here:
//! - `PlaylistDoc.is_local_playlist` says which kind is on screen.
//! - `PlaylistTrackRow.source` is `""` (Qobuz) | `"local"` | `"plex"`, and
//!   `id` lives in a DIFFERENT id space per source. Never read a row's `id`
//!   as a catalog id without checking `source` first — that is the
//!   id-confusion class `playlist_picker_qt.rs`'s header exists to prevent.
//! - `unavailable` rows carry a ref that cannot resolve at all; they render
//!   honestly and every "copy this track somewhere" affordance must be absent
//!   on them. `notStreamable` is a DIFFERENT thing and lives beside it: Qobuz
//!   pulled the recording. That one can heal, it is the only one the
//!   replacement search can act on, and a row carrying it plus
//!   `cacheStatus == 3` is not dead at all — it plays from the download.
//!
//! Known parity deltas:
//! - The custom drag order persists to a per-user `playlist_orders.json`
//!   in the user dir (the Slint uses `playlist_orders.db` with
//!   `(u64, bool, i32)` rows — same behavior, simpler backend for the POC).
//! - Suggested Songs are not ported.
//! - Custom covers, multi-select/bulk actions and public-link sharing are live.
//!   Copying a foreign playlist still does not clone its source artwork into a
//!   new custom-cover override.

use std::sync::{Arc, Mutex};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::{Playlist, QueueTrack, Track};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Doc + rows
// ---------------------------------------------------------------------------

#[derive(Clone, Default, Serialize)]
pub struct PlaylistTrackRow {
    /// Featured performers (see `album_qt::TrackRow::featured`); empty when none.
    #[serde(rename = "featured")]
    pub featured: Vec<crate::album_qt::FeaturedArtist>,
    pub id: String,
    /// The playlist membership row id (== catalog track id for Qobuz
    /// playlists — what remove_tracks_from_playlist takes).
    #[serde(rename = "playlistTrackId")]
    pub playlist_track_id: u64,
    /// The row's slot in the LOADED order (the API's insertion order — what
    /// "Default" restores and "Date added" reads, 2026-09-13). Not serialised.
    #[serde(skip)]
    pub position: i32,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    pub album: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    pub duration: String,
    #[serde(rename = "durationSecs")]
    pub duration_secs: u64,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    /// Bit-depth / rate line for the row's quality badge — every other
    /// producer already emits it; without it the badge shows a bare tier.
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    #[serde(rename = "qualityLabel")]
    pub quality_label: String,
    /// RAW catalog max bit depth / sample rate (kHz) — the SAME two numbers
    /// `quality_detail` / `quality_label` above are derived from.
    ///
    /// THE CONTRACT (reference: `crates/qbz/src/playback.rs:2426`
    /// `make_queue_track`): a queue track carries the NUMBERS, never only the
    /// formatted string. `row_to_queue` below maps this display row into a
    /// `QueueTrack`; without these fields it could not fill them and hardcoded
    /// `None`, which zeroes `quality_state`'s `TRACK_MAX_*` seed — the NPB
    /// AudioStamp then shows a tier with NO "24-bit / 96 kHz" line for every
    /// playlist-sourced play. Re-parsing the formatted string back into
    /// numbers would be lossy and a second source of truth; the producer has
    /// them in hand, so it passes them.
    #[serde(rename = "bitDepth", skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    #[serde(rename = "sampleRate", skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    pub explicit: bool,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Row provenance for a LOCAL playlist's mixed rows: "" (Qobuz — the
    /// default for every Qobuz playlist row) | "local" | "plex".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// A ref that cannot resolve right now — a Plex key the cache does not
    /// know, or a stored "Qobuz" id outside the catalog range (the legacy
    /// mis-typed-drag class). The row renders HONESTLY and stays selectable so
    /// the user can remove it, instead of vanishing: hiding is for genuinely
    /// offline Qobuz rows, not for refs that can never heal on their own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unavailable: bool,
    /// The raw stored ref behind `unavailable`, shown so the user can see WHAT
    /// is broken rather than just that something is.
    #[serde(
        default,
        rename = "unavailableRef",
        skip_serializing_if = "String::is_empty"
    )]
    pub unavailable_ref: String,
    /// Qobuz PULLED this track: the catalog reports `streamable: false` for it
    /// (contract §5.1). DISTINCT from [`Self::unavailable`] above, and the two
    /// must never be merged (§2): that one is a broken LOCAL ref — a Plex key
    /// the cache does not know, a stored id outside the catalog range — which
    /// can never heal, has no replacement, and says nothing about the Qobuz
    /// catalogue. This one names a recording Qobuz removed: it CAN come back
    /// (rights are restored), and it is the only one of the two the replacement
    /// search can act on. A row can carry either, and their treatments differ.
    ///
    /// Absence from the API is NOT this. `qbz_models::Track::is_streamable()`
    /// reads a missing key as AVAILABLE (§3.1), so an endpoint that stays quiet
    /// leaves this `false` and the row renders normally — greying out a whole
    /// view because one endpoint was terse is the failure that would sink the
    /// feature.
    #[serde(
        default,
        rename = "qobuzUnavailable",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub not_streamable: bool,
    /// Refines `qobuzUnavailable`: a pre-release (release still ahead) rather
    /// than a pulled track — `qbz_models::Track::is_upcoming`.
    #[serde(
        default,
        rename = "qobuzUpcoming",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub upcoming: bool,
    /// The recording identifier, carried so the replacement search's ISRC
    /// short-circuit can fire (`qbz-playlist-import/src/match_qobuz.rs:156-159`
    /// scores an ISRC hit 1.0). That is the owner's "a veces cambia el ID del
    /// álbum" case: same recording, new catalog id, identical ISRC — an exact
    /// relink needing no human judgement. Without it the feature degrades to
    /// title/artist scoring and loses the one CERTAIN match. The live capture
    /// confirms a pulled track keeps it
    /// (`album-get-unavailable-track-captured-2026-08-17.json`: `SEWCE0900201`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub isrc: String,
    /// Offline-cache status at build time (0 none / 3 ready) — the twin of
    /// `album_qt::TrackRow::cache_status`. Playlist rows never carried it, so
    /// `TrackRow.qml:154` read `undefined` and fell back to 0 on every playlist
    /// row. That was cosmetic until F5: a PULLED track that is already
    /// DOWNLOADED still plays from disk and must render as "no longer on Qobuz —
    /// playing your downloaded copy", not as dead, and the row cannot tell the
    /// two apart without this.
    #[serde(default, rename = "cacheStatus")]
    pub cache_status: i32,
}

#[derive(Clone, Default, Serialize)]
pub struct PlaylistDoc {
    pub id: String,
    pub name: String,
    pub owner: String,
    #[serde(rename = "ownerId")]
    pub owner_id: u64,
    pub description: String,
    #[serde(rename = "descriptionShort")]
    pub description_short: String,
    /// The playlist's OWN Qobuz artwork (`image_rectangle`), and NOTHING
    /// else — EMPTY for every playlist without one (user playlists, and
    /// editorial ones whose graphic Qobuz omits). The `images*` lists are
    /// member-ALBUM covers; binding them here is what put an album sleeve
    /// where the playlist graphic belongs (same divergence the cards had).
    /// The header renders this CONTAIN — the graphics are landscape and
    /// cropping cuts the wordmark.
    #[serde(rename = "coverUrl")]
    pub cover_url: String,
    #[serde(rename = "coverPath")]
    pub cover_path: String,
    /// Mosaic source for a playlist with no artwork of its own: up to four
    /// de-duplicated MEMBER-ALBUM covers (`images300` > `images150` >
    /// `images`, else the first four distinct track covers — the Slint
    /// header's 2x2 collage reads `tracks[0..3].artwork`). The view feeds
    /// them to the shared `cards/PlaylistCollage.qml`, which resolves the
    /// urls itself and owns the empty (list-music) state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
    /// A user-picked cover override exists (custom_playlist_covers.json) —
    /// the header cover menu flips its Add/Change/Remove rows on this.
    #[serde(rename = "hasCustomCover")]
    pub has_custom_cover: bool,
    pub tracks: Vec<PlaylistTrackRow>,
    #[serde(rename = "trackCount")]
    pub track_count: i32,
    #[serde(rename = "totalDuration")]
    pub total_duration: String,
    #[serde(rename = "isOwner")]
    pub is_owner: bool,
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    #[serde(rename = "isFollowing")]
    pub is_following: bool,
    #[serde(rename = "isCopied")]
    pub is_copied: bool,
    pub pinned: bool,
    pub loading: bool,
    #[serde(rename = "sortField")]
    pub sort_field: String,
    #[serde(rename = "sortAsc")]
    pub sort_asc: bool,
    pub search: String,
    /// This detail is a LOCAL playlist (`local:<uuid>`), not a Qobuz one. The
    /// view drops every Qobuz-only affordance on it (follow, copy, share, the
    /// Qobuz heart) and offers the local ones instead.
    #[serde(
        default,
        rename = "isLocalPlaylist",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_local_playlist: bool,
    /// D8: an OFFLINE-ONLY local playlist. Nothing from one may ever reach
    /// Qobuz — the queue it builds is stamped so the QConnect push site skips
    /// the cloud entirely.
    #[serde(
        default,
        rename = "offlineOnly",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub offline_only: bool,
    /// This QOBUZ detail carries SIDECAR rows — local files and/or Plex tracks
    /// added to it through `playlist_local_tracks` / `playlist_plex_tracks`.
    /// The "carretes paralelos" playlist.
    ///
    /// NOT the same flag as `is_local_playlist`, and it must not be folded into
    /// it: a mixed playlist is still a Qobuz playlist with an owner, a follow
    /// state, a copy action, a share link and a heart, and it keeps every one of
    /// those affordances. What it loses is drag/chevron reorder — see
    /// `apply_custom_order`, which keys its stored order by a `u64` catalog id
    /// and cannot tell one from a library row id.
    #[serde(
        default,
        rename = "isMixed",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_mixed: bool,
}

// ---------------------------------------------------------------------------
// Mixed ("carretes paralelos") playlists
// ---------------------------------------------------------------------------

/// True while the open Qobuz detail carries a source-aware queue snapshot.
/// That includes an online mixed detail and the offline playable subset.
/// Set by the corresponding loaders, cleared by [`reset`] — the same lifetime
/// the reference gives it (`qbz/src/playlist.rs:32`).
///
/// It exists because the DOC is not readable from the two write paths that need
/// the answer synchronously (`main.rs` routes before any await), and because
/// those paths must NOT route on `local_playlist_qt::open_id()`: a mixed detail
/// adopts that snapshot too, so `open_id()` alone would send a Qobuz playlist's
/// remove and reorder into the local-playlist repo, where they would find no
/// rows and write nothing. That is the renders-and-no-ops failure this module
/// has already been bitten by once.
static MIXED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the open ONLINE Qobuz detail is a mixed playlist.
pub fn is_mixed() -> bool {
    MIXED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Drop the mixed latch on navigation, BEFORE the next detail loads.
///
/// Without this the flag would survive between pages for the length of a fetch,
/// and a remove clicked in that window would route by the PREVIOUS playlist's
/// shape. `load` sets it authoritatively at the end; this only closes the gap.
pub fn clear_mixed() {
    MIXED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Stamp a Qobuz detail that is being served by the source-aware local
/// playlist snapshot. Offline details always take this path even when their
/// only visible rows happen to be downloaded Qobuz tracks: playback must use
/// the filtered queue and must never rebuild the full online membership.
pub(crate) fn mark_mixed() {
    MIXED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Remove ONE sidecar row (a local file or a Plex track) from the open MIXED
/// Qobuz playlist, then reload so the page settles on the merged truth rather
/// than on an optimistic patch.
///
/// This arm has to exist, and its absence was not a cosmetic gap: the Qobuz arm
/// parses the display row id as a `u64` and posts it to Qobuz as a
/// `playlist_track_id`. A local row's display id IS its `library.db` row id, in
/// the same numeric space — so without this the click would send a local
/// library rowid to Qobuz as if it were a catalog track, and the row would
/// still be there afterwards.
///
/// A Plex row never rides its numeric id (the synthetic ids do not resolve):
/// its rating key comes back from the open detail's queue snapshot through
/// `local_picker_ref_for_row`, which returns it as `"plex:<key>"`.
pub async fn remove_sidecar_row(runtime: &Arc<AppRuntime<LoggingAdapter>>, row_id: &str) -> bool {
    let Some(playlist_id) = with_doc(|d| d.id.clone()).and_then(|id| id.parse::<u64>().ok()) else {
        return false;
    };
    let source = with_doc(|d| {
        d.tracks
            .iter()
            .find(|t| t.id == row_id)
            .map(|t| t.source.clone())
    })
    .flatten()
    .unwrap_or_default();

    // Resolved rows carry their ref in the live queue (local_picker_ref_for_row);
    // an UNAVAILABLE row is absent from the queue, but its display id already
    // encodes the ref as "<source>:<reference>" (local_playlist_qt::row_to_display).
    // sidecar_removal() falls back to that, so an unresolved Plex / Jellyfin /
    // Subsonic sidecar row can still be removed — without the fallback
    // "Remove from playlist" silently no-opped on a dead sidecar row
    // (2026-09-07 release blocker: a mixed Qobuz playlist whose Plex track no
    // longer resolves rendered "Unavailable track" and could not be deleted).
    let queue_ref = crate::local_playlist_qt::local_picker_ref_for_row(row_id);
    let Some(removal) = sidecar_removal(&source, row_id, queue_ref) else {
        // Not a sidecar row (Qobuz) — the caller must fall through to the Qobuz
        // arm; or a sidecar row whose ref could not be derived at all.
        return false;
    };

    let _ = tokio::task::spawn_blocking(move || {
        crate::library_db_qt::with_db(false, |db| {
            match &removal {
                SidecarRemoval::Local(id) => {
                    db.remove_local_track_from_playlist(playlist_id, *id)?
                }
                SidecarRemoval::Plex(key) => {
                    db.remove_plex_track_from_playlist(playlist_id, key)?
                }
                SidecarRemoval::Remote(source, item) => {
                    db.remove_remote_track_from_playlist(playlist_id, source, item)?
                }
            }
            Ok(())
        })
    })
    .await;

    if let Err(e) = load(runtime, playlist_id).await {
        log::warn!("[qbz-qt] playlist reload after sidecar remove failed: {e}");
    }
    true
}

enum SidecarRemoval {
    Local(i64),
    Plex(String),
    /// A Jellyfin / Subsonic sidecar row: (source, server item id).
    Remote(String, String),
}

/// Resolve a mixed-playlist sidecar row to its removal target.
///
/// `queue_ref` is `local_picker_ref_for_row(row_id)` — present for a RESOLVED
/// row (it has a live queue entry), `None` for an UNAVAILABLE one. The display
/// `row_id` of an unavailable row already encodes the ref as
/// "<source>:<reference>" (`local_playlist_qt::row_to_display`), so it is the
/// fallback that lets a dead Plex/Jellyfin/Subsonic row be removed. Returns
/// `None` for a Qobuz row (the caller falls through to the Qobuz arm) or when
/// no ref can be derived.
fn sidecar_removal(
    source: &str,
    row_id: &str,
    queue_ref: Option<String>,
) -> Option<SidecarRemoval> {
    let strip = |prefix: &str| {
        queue_ref
            .as_deref()
            .and_then(|r| r.strip_prefix(prefix))
            .or_else(|| row_id.strip_prefix(prefix))
            .map(str::to_string)
    };
    match source {
        // A resolved local sidecar row's display id IS its library row id; an
        // unresolved local ref never reaches the sidecar (the SQL JOIN drops
        // it), so a non-numeric id here is genuinely not removable.
        "local" => row_id.parse::<i64>().ok().map(SidecarRemoval::Local),
        "plex" => strip("plex:").map(SidecarRemoval::Plex),
        "jellyfin" => strip("jellyfin:").map(|i| SidecarRemoval::Remote("jellyfin".into(), i)),
        "subsonic" => strip("subsonic:").map(|i| SidecarRemoval::Remote("subsonic".into(), i)),
        _ => None,
    }
}

#[cfg(test)]
mod sidecar_removal_tests {
    use super::{sidecar_removal, SidecarRemoval};

    #[test]
    fn resolved_plex_uses_the_queue_ref() {
        assert!(matches!(
            sidecar_removal("plex", "plex:RESOLVED", Some("plex:RESOLVED".into())),
            Some(SidecarRemoval::Plex(k)) if k == "RESOLVED"
        ));
    }

    #[test]
    fn unavailable_plex_falls_back_to_the_display_id() {
        // The 2026-09-07 blocker: no queue entry, but the row id carries the key.
        assert!(matches!(
            sidecar_removal("plex", "plex:71465", None),
            Some(SidecarRemoval::Plex(k)) if k == "71465"
        ));
    }

    #[test]
    fn unavailable_remote_falls_back_to_the_display_id() {
        assert!(matches!(
            sidecar_removal("jellyfin", "jellyfin:abc", None),
            Some(SidecarRemoval::Remote(s, i)) if s == "jellyfin" && i == "abc"
        ));
        assert!(matches!(
            sidecar_removal("subsonic", "subsonic:xyz", None),
            Some(SidecarRemoval::Remote(s, i)) if s == "subsonic" && i == "xyz"
        ));
    }

    #[test]
    fn local_sidecar_is_the_numeric_row_id() {
        assert!(matches!(sidecar_removal("local", "4321", None), Some(SidecarRemoval::Local(4321))));
        assert!(sidecar_removal("local", "local:nope", None).is_none());
    }

    #[test]
    fn qobuz_row_is_not_a_sidecar() {
        assert!(sidecar_removal("qobuz", "123", None).is_none());
        assert!(sidecar_removal("", "123", None).is_none());
    }
}

/// Tauri's absolute-slot interleave — the `displayTracks` contract. Port of
/// `qbz/src/playlist.rs:455-496`, kept literal on purpose.
///
/// Sidecar rows claim their STORED positions as slots in the merged list; Qobuz
/// tracks fill the remaining slots in server order;
/// `total = max(sum of rows, max stored position + 1)` so stale high slots
/// still render (E3); unclaimed slots with no Qobuz track left are skipped
/// (never a blank row); leftover Qobuz tracks append. Same-slot collisions emit
/// ALL claimants — local first, then plex, in stable claim order — rather than
/// Tauri's Map collapse, which dropped one of them (E1/E2 fix-forward; the
/// stored data is repaired separately by the healing pass). Display numbering
/// is the emit order, contiguous.
pub(crate) fn interleave_rows(
    qobuz: Vec<Track>,
    sidecar: Vec<crate::local_playlist_qt::LoadedRow>,
) -> Vec<crate::local_playlist_qt::LoadedRow> {
    use crate::local_playlist_qt::{LoadedRow, RowItem};
    let qobuz_to_row = |(i, t): (usize, Track)| LoadedRow {
        position: i as i32,
        item: RowItem::Qobuz(Box::new(t)),
    };
    if sidecar.is_empty() {
        return qobuz.into_iter().enumerate().map(qobuz_to_row).collect();
    }
    let sidecar_len = sidecar.len();
    let mut max_pos: i32 = -1;
    let mut buckets: std::collections::HashMap<i32, Vec<LoadedRow>> =
        std::collections::HashMap::new();
    for row in sidecar {
        // Corrupt negative positions claim slot 0 rather than vanishing.
        let pos = row.position.max(0);
        max_pos = max_pos.max(pos);
        buckets.entry(pos).or_default().push(row);
    }
    let total = (qobuz.len() + sidecar_len).max((max_pos + 1) as usize);
    let mut out: Vec<LoadedRow> = Vec::with_capacity(qobuz.len() + sidecar_len);
    let mut qobuz_iter = qobuz.into_iter();
    for pos in 0..total as i32 {
        if let Some(rows) = buckets.remove(&pos) {
            out.extend(rows);
        } else if let Some(track) = qobuz_iter.next() {
            out.push(LoadedRow {
                position: pos,
                item: RowItem::Qobuz(Box::new(track)),
            });
        }
        // else: an unclaimed slot past the Qobuz tracks — a gap, skipped.
    }
    for track in qobuz_iter {
        out.push(LoadedRow {
            position: 0,
            item: RowItem::Qobuz(Box::new(track)),
        });
    }
    // Positions in the merged output are the contiguous display slots; the
    // stored sidecar positions did their job claiming the order.
    for (i, row) in out.iter_mut().enumerate() {
        row.position = i as i32;
    }
    out
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PageState {
    doc: PlaylistDoc,
}

static PAGE: Mutex<Option<PageState>> = Mutex::new(None);

/// The signed-in user's id, stashed at session activation (is_owner math).
static USER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn set_user_id(id: u64) {
    USER_ID.store(id, std::sync::atomic::Ordering::SeqCst);
}

/// Clear all account-scoped playlist authority on logout.
pub fn teardown() {
    *PAGE.lock().unwrap() = None;
    USER_ID.store(0, std::sync::atomic::Ordering::SeqCst);
    OWNED_PLAYLISTS.lock().unwrap().clear();
    FOLLOWED_PLAYLISTS.lock().unwrap().clear();
}

/// The signed-in user's id, or `None` when no session has been activated.
///
/// The Qt twin of the reference's `library_db::current_user_id()`
/// (`main.rs:21024`) — same job, read out of the session snapshot this crate
/// already stashes instead of re-opening the DB. `0` is the never-set
/// sentinel and must NOT be compared against a real `owner.id`, hence the
/// `Option`: `delete_by_id`'s ownership test would otherwise read
/// "unauthenticated" as "owns nothing", which is right, and "owns playlist 0",
/// which is not.
pub(crate) fn current_user_id() -> Option<u64> {
    match USER_ID.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        id => Some(id),
    }
}

// ---------------------------------------------------------------------------
// Ownership / follow authority for playlist CARDS
// ---------------------------------------------------------------------------

/// The ids of playlists in the signed-in user's own playlist list, split by
/// owner: `OWNED` (owner.id == this user) and `FOLLOWED` (a foreign playlist
/// the user is subscribed to — `playlist/subscribe`).
///
/// Why this exists: `PlaylistCard`'s overlay is a TRI-state — owned draws the
/// library heart, foreign-followed draws the check, foreign draws user-plus —
/// and it reads `item.playlistOwned` / `item.playlistFollowing`. Every
/// producer except the Library feed shipped neither, so every playlist card on
/// Discover, Search and Browse collapsed to the third arm: a playlist the user
/// OWNS offered "Follow on Qobuz", and the first click subscribed the user to
/// their own playlist.
///
/// `/playlist/get` carries `owner`, so ownership could be derived per row —
/// but "am I subscribed to this?" cannot: it is only knowable from the user's
/// own playlist list. Both halves therefore come from the SAME snapshot, taken
/// where that list is already fetched: `sidebar_qt::load` at shell entry and
/// `library_qt::load_library`, neither of which adds a request for this.
///
/// Before the first snapshot both sets are empty and every card falls back to
/// the pre-existing behaviour (`false`, the foreign arm) — the same window
/// `fav_cache_qt` has before its disk seed, and never worse than today.
static OWNED_PLAYLISTS: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
static FOLLOWED_PLAYLISTS: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Replace both sets from one `get_user_playlists()` response, as
/// `(playlist id, owner id)` pairs. Callers pass pairs rather than the models
/// so this stays independent of which of the two fetchers happens to run.
pub fn set_user_playlists(pairs: &[(u64, u64)]) {
    crate::library_qt::with_deleted_playlists(|deleted| {
        let uid = USER_ID.load(std::sync::atomic::Ordering::SeqCst);
        let mut owned = std::collections::HashSet::new();
        let mut followed = std::collections::HashSet::new();
        for &(id, owner) in pairs {
            if deleted.contains(&id.to_string()) {
                continue;
            }
            // uid == 0 means "no session id yet" — claiming ownership on an id
            // that has not been set would mark EVERY row owned.
            if uid != 0 && owner == uid {
                owned.insert(id);
            } else {
                followed.insert(id);
            }
        }
        log::info!(
            "[qbz-qt] playlist ownership snapshot: {} owned / {} followed",
            owned.len(),
            followed.len()
        );
        if let Ok(mut g) = OWNED_PLAYLISTS.lock() {
            *g = owned;
        }
        if let Ok(mut g) = FOLLOWED_PLAYLISTS.lock() {
            *g = followed;
        }
    });
}

/// Is the session user id known yet? Ownership answers are meaningless (all
/// false) before it is.
pub fn session_user_known() -> bool {
    USER_ID.load(std::sync::atomic::Ordering::SeqCst) != 0
}

/// Does the signed-in user own this playlist, by OWNER id? The cheap arm —
/// every card producer already has `owner.id` on the row it is mapping, so it
/// needs no set lookup at all.
pub fn owns(owner_id: u64) -> bool {
    let uid = USER_ID.load(std::sync::atomic::Ordering::SeqCst);
    uid != 0 && owner_id == uid
}

/// Does the user own this playlist, by PLAYLIST id? For producers whose row
/// carries no owner (the label page's raw JSON playlists).
pub fn is_owned(playlist_id: u64) -> bool {
    OWNED_PLAYLISTS
        .lock()
        .map(|g| g.contains(&playlist_id))
        .unwrap_or(false)
}

/// Is the user subscribed to this FOREIGN playlist?
pub fn is_following(playlist_id: u64) -> bool {
    FOLLOWED_PLAYLISTS
        .lock()
        .map(|g| g.contains(&playlist_id))
        .unwrap_or(false)
}

/// Keep the follow set in step with a subscribe / unsubscribe that just
/// landed, so the next card built draws the settled glyph instead of the
/// snapshot's. Called from both follow seams (`toggle_follow` on the open
/// page, `set_follow_by_id` from a card overlay).
fn mark_following(playlist_id: u64, following: bool) {
    if let Ok(mut g) = FOLLOWED_PLAYLISTS.lock() {
        if following {
            g.insert(playlist_id);
        } else {
            g.remove(&playlist_id);
        }
    }
}

/// Everything a synthesized sidebar / Library row needs about a playlist the
/// user just followed. `None` = the caller could not obtain it, which downgrades
/// the follow arm to an authoritative refetch.
pub(crate) struct FollowRowMeta {
    pub name: String,
    pub owner: String,
    pub tracks_count: u32,
    pub cover_url: String,
    pub covers: Vec<String>,
}

impl FollowRowMeta {
    /// Build from the API model — for the CARD seam, which holds an id and
    /// nothing else.
    ///
    /// The two artwork fields go through `library_qt`'s own pickers rather than
    /// re-deriving `image_rectangle` / `images300` here: `cover_url` is the
    /// playlist's OWN editorial graphic and `covers` are its MEMBER-ALBUM
    /// sleeves, and conflating the two is the exact bug that put an album
    /// sleeve on every playlist card (`library_qt::map_playlist_row`'s note).
    /// A second copy of that precedence would drift from this one.
    fn from_playlist(playlist: &Playlist) -> Self {
        Self {
            name: playlist.name.clone(),
            owner: playlist.owner.name.clone(),
            tracks_count: playlist.tracks_count,
            cover_url: crate::library_qt::playlist_own_image(playlist),
            covers: crate::library_qt::playlist_cover_urls(playlist),
        }
    }
}

/// THE settle point for a follow / unfollow, wherever the click came from —
/// the header toggle on the open detail and the card-overlay `set_follow_by_id`
/// both end here, the way every heart ends at `crate::emit_library_favorite`.
///
/// Three surfaces have to agree, and before this they did not:
///
/// 1. the ownership snapshot (`mark_following`) — the only one the port
///    updated, which is why the glyph settled and nothing else did;
/// 2. the SIDEBAR, whose rows are `get_user_playlists()` — a followed playlist
///    is in that list, so unfollowing must take the row out;
/// 3. the LIBRARY feed, where the same playlist is a `following` row.
///
/// The unfollow arm patches the two caches and republishes them instead of
/// re-fetching. That is not an optimisation, it is the fix: Qobuz's
/// `playlist/getUserPlaylists` lags a write (the reference documents the same
/// lag on the create side and answers it with a bounded retry,
/// `qbz/src/main.rs:4884`), so the `crate::reload_sidebar()` this seam used to
/// fire re-read the STALE list and put the row it had just removed straight
/// back. The next natural load reconciles.
///
/// The follow arm inserts optimistically. Both seams have the metadata — the
/// header takes it from the open document, the card seam fetches it once
/// (`FollowRowMeta::from_playlist`) — so `None` here means only that the fetch
/// FAILED, and it falls back to `crate::reload_sidebar()`, which is what this
/// seam used to do unconditionally.
///
/// Reference note, stated because this is deliberately NOT 1:1: the Slint's
/// `playlist_set_follow_by_id` (`qbz/src/main.rs:2189-2191`) flips the chip on
/// every visible card and then calls `crate::playback::refresh_sidebar(true)` —
/// which is the QUEUE panel (`qbz/src/playback.rs:42`), not the playlist tree.
/// It never touches the playlist sidebar or the Favorites playlist models on a
/// follow change, so the reference has the same defect the owner reported here.
/// Ported behaviour would keep the bug; the owner asked for the row to leave
/// both surfaces.
fn follow_settled(playlist_id: u64, following: bool, meta: Option<FollowRowMeta>) {
    mark_following(playlist_id, following);
    let id = playlist_id.to_string();
    if following {
        let Some(meta) = meta else {
            // No metadata to synthesize from: take the round trip.
            crate::reload_sidebar();
            return;
        };
        crate::sidebar_qt::insert_qobuz_entry(
            playlist_id,
            &meta.name,
            meta.tracks_count,
            &meta.covers,
        );
        crate::publish_sidebar();
        if crate::library_qt::insert_playlist_row(
            &id,
            &meta.name,
            &meta.owner,
            meta.tracks_count,
            &meta.cover_url,
            meta.covers,
            true,
        ) {
            crate::publish_library_document();
        }
    } else {
        if crate::sidebar_qt::remove_qobuz_entry(playlist_id) {
            crate::publish_sidebar();
        }
        if crate::library_qt::remove_playlist_rows(&id) {
            crate::publish_library_document();
        }
    }
}

fn publish(doc: &PlaylistDoc) {
    let json = serde_json::to_string(doc).unwrap_or_else(|_| "{}".into());
    crate::ui(move |mut b| {
        b.as_mut().set_playlist_json(QString::from(json.as_str()));
    });
}

/// Publish a document built ELSEWHERE and adopt it as the open page.
///
/// The LOCAL playlist detail renders through this same view — the reference
/// does exactly that (its local detail drives `PlaylistState` and the shared
/// row machinery rather than growing a second view), so search / sort /
/// multi-select / artwork all come for free and cannot drift between the two
/// kinds of playlist. `PAGE` is set too, so `with_doc` readers (the sort and
/// search handlers, the membership refresh) see the local page like any other.
pub(crate) fn adopt_doc(doc: PlaylistDoc) {
    publish(&doc);
    *PAGE.lock().unwrap() = Some(PageState { doc });
}

/// Re-resolve any row (or header) artwork that was NOT on disk when the page
/// was built, then republish. The LOCAL loader spawns this after
/// `download_missing` lands a media-server sidecar cover (a `jellyfin:` /
/// `subsonic:` ref), which mirrors the warm-then-republish the Qobuz loader
/// does inline. It reads whatever page is open now, so it is a safe no-op if
/// the user has navigated away.
pub(crate) fn rewarm_row_art() {
    let doc = with_doc(|d| {
        if !d.cover_url.is_empty() && d.cover_path.is_empty() {
            d.cover_path = crate::artwork_qt::cached_path(&d.cover_url);
        }
        for row in d.tracks.iter_mut() {
            if row.art_path.is_empty() && !row.art_url.is_empty() {
                row.art_path = crate::artwork_qt::cached_path(&row.art_url);
            }
        }
        d.clone()
    });
    if let Some(doc) = doc {
        publish(&doc);
    }
}

fn with_doc<R>(f: impl FnOnce(&mut PlaylistDoc) -> R) -> Option<R> {
    let mut guard = PAGE.lock().unwrap();
    guard.as_mut().map(|page| f(&mut page.doc))
}

// ---------------------------------------------------------------------------
// Mapping (playlist.rs to_item + header)
// ---------------------------------------------------------------------------

pub(crate) fn mmss(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub(crate) fn tier(bit_depth: Option<u32>) -> &'static str {
    match bit_depth {
        Some(d) if d >= 24 => "hires",
        Some(_) => "cd",
        None => "",
    }
}

pub(crate) fn quality_label(bit_depth: Option<u32>, sample_rate: Option<f64>) -> String {
    match bit_depth {
        None => String::new(),
        Some(depth) => {
            let prefix = if depth >= 24 { "Hi-Res" } else { "CD" };
            let rate = sample_rate.unwrap_or(if depth >= 24 { 96.0 } else { 44.1 });
            let rate = if rate.fract().abs() < f64::EPSILON {
                format!("{}", rate as i64)
            } else {
                format!("{rate}")
            };
            format!("{prefix} {depth}-bit / {rate} kHz")
        }
    }
}

pub(crate) fn map_track(track: &Track) -> PlaylistTrackRow {
    let mut title = track.title.clone();
    if let Some(v) = track.version.as_ref().filter(|v| !v.is_empty()) {
        title = format!("{title} ({v})");
    }
    let album = track.album.as_ref();
    let (artist, artist_id) = track
        .performer
        .clone()
        .map(|p| (p.name, p.id.to_string()))
        .unwrap_or_default();
    PlaylistTrackRow {
        position: 0,
        featured: crate::album_qt::featured_list(track.performers.as_deref(), &artist, &track.title, &[]),
        // Heart state at build time, from the favourite-id cache — the same
        // O(1) read `album_qt` / `artist_qt` / `label_qt` rows use. It was
        // never stamped here, so `TrackRow.qml` saw `undefined` on every
        // playlist row: a favourited track drew the empty heart and the first
        // click sent `favorite/delete`, REMOVING it from the library.
        is_favorite: crate::fav_cache_qt::contains_track(track.id),
        id: track.id.to_string(),
        // The MEMBERSHIP id, which is NOT the catalog id. This used to be
        // `track.id`, and the two are different numbers for the same row: the
        // catalog id names the recording, `playlist_track_id` names THIS row's
        // slot in THIS playlist. Qobuz's `playlist/deleteTracks` and
        // `playlist/updateTracksPosition` both take the membership id, so the
        // old value made "remove from playlist" address a row that does not
        // exist and made the replacement flow's reposition silently never fire
        // (it looks the dead row up BY this id and found nothing).
        //
        // Falls back to the catalog id only when the endpoint omits it, which
        // is what the previous code assumed unconditionally.
        playlist_track_id: track.playlist_track_id.unwrap_or(track.id),
        title,
        artist,
        artist_id,
        album: album.map(|a| a.title.clone()).unwrap_or_default(),
        album_id: album.map(|a| a.id.clone()).unwrap_or_default(),
        duration: mmss(track.duration),
        duration_secs: track.duration as u64,
        quality_tier: tier(track.maximum_bit_depth).to_string(),
        quality_detail: crate::home_qt::quality_detail_from_parts(
            track.maximum_bit_depth,
            track.maximum_sampling_rate,
        ),
        quality_label: quality_label(track.maximum_bit_depth, track.maximum_sampling_rate),
        // See `PlaylistTrackRow::bit_depth` — the raw catalog numbers ride
        // with the row so `row_to_queue` can hand them to the queue.
        bit_depth: track.maximum_bit_depth,
        sample_rate: track.maximum_sampling_rate,
        explicit: track.parental_warning,
        // Track row art: full variant (best()) — the thumbnail down-tier was reverted after the 2026-08-15 owner smoke (contract 04 §3).
        art_url: album
            .and_then(|a| a.image.best().cloned())
            .unwrap_or_default(),
        // §5.1: the API's own answer, resolved through the ONE helper that is
        // allowed to interpret absence (§3.1). Playlists are the surface the
        // owner says pulled tracks show up on MOST, which is why this row model
        // is the first one plumbed.
        not_streamable: !track.is_streamable(),
        upcoming: track.is_upcoming(),
        // Carried for §6's ISRC short-circuit; see the field's doc comment.
        isrc: track.isrc.clone().unwrap_or_default(),
        // F5: a pulled track we already downloaded is NOT dead. The same O(1)
        // session-set read `album_qt` / `artist_qt` / `label_qt` rows use.
        cache_status: if crate::offline_qt::is_cached(&track.id.to_string()) {
            3
        } else {
            0
        },
        ..Default::default()
    }
}

/// `row_to_queue` for the LOCAL playlist detail, which builds the same rows
/// through the same mapper and needs the same queue entries.
pub(crate) fn row_to_queue_public(row: &PlaylistTrackRow) -> QueueTrack {
    row_to_queue(row)
}

fn row_to_queue(row: &PlaylistTrackRow) -> QueueTrack {
    // PROVENANCE IS LOAD-BEARING (wrong-track hazard, the QConnect
    // track-source-admission P0 family). A LOCAL playlist's detail adopts
    // its rows into this shared page via `local_playlist_qt::adopt_doc`, and
    // its local/Plex rows carry `row.source` + the LIBRARY rowid (or a 2^40
    // synthetic Plex id) in `row.id` — NOT a catalog id. Hardcoding
    // `source: "qobuz"` here passed every QConnect admission guard
    // (`id > 0 && source != local/plex`) and pushed the rowid to the
    // WebSocket as a Qobuz catalog id: the peer renderer played an
    // UNRELATED track (reference guards this at main.rs:11906-11932, "the
    // catalog path below would mis-resolve them"). Typed correctly, the
    // existing guards do their job: with a peer active the add is refused
    // with the castable toast and lands nowhere; with no peer it lands
    // locally, correctly typed, and the sync-on-add predicate skips it.
    // Unavailable rows (a raw path or `kind:ref` id) parse to 0 and fail
    // the `id > 0` guard — they can never reach the WS either.
    let provenance = row.source.as_str();
    let is_local_row = matches!(provenance, "local" | "plex");
    QueueTrack {
        id: row.id.parse().unwrap_or(0),
        title: row.title.clone(),
        version: None,
        artist: row.artist.clone(),
        album: row.album.clone(),
        album_version: None,
        duration_secs: row.duration_secs,
        artwork_url: if row.art_url.is_empty() {
            None
        } else {
            Some(row.art_url.clone())
        },
        // playback.rs `make_queue_track` (:2426): the CATALOG max travels with
        // the queue track. `None` here zeroed `quality_state`'s `TRACK_MAX_*`
        // seed, so a playlist play drew a bare tier on the NPB AudioStamp with
        // no "24-bit / 96 kHz" line — perfect from an album, blank from a
        // playlist. The row carries the numbers now (see the struct).
        hires: row.quality_tier == "hires",
        bit_depth: row.bit_depth,
        sample_rate: row.sample_rate,
        is_local: provenance == "local",
        album_id: if row.album_id.is_empty() {
            None
        } else {
            Some(row.album_id.clone())
        },
        artist_id: row.artist_id.parse::<u64>().ok(),
        // D5: the row's own answer, not a hardcoded yes. This function is the
        // ONLY thing between a playlist row and the queue, so `true` here made
        // both seams blind on the surface where the owner says pulled tracks
        // turn up MOST. A LOCAL/Plex row never sets `not_streamable` (nothing on
        // that path does), so a file on disk still queues — which is correct:
        // Qobuz's streaming rights do not reach it.
        streamable: !row.not_streamable,
        source: Some(if is_local_row {
            provenance.to_string()
        } else {
            "qobuz".to_string()
        }),
        parental_warning: row.explicit,
        source_item_id_hint: None,
        // A ROW does not know which playlist it belongs to. Stamping a kind
        // here with no id was a HALF-stamp: `refresh_now_playing`'s
        // both-or-album match (playback_qt.rs, the port of playback.rs:1959)
        // discards a kind without an id, so every playlist play — card, rail,
        // header, row — published the ALBUM glyph. The origin is stamped where
        // the playlist id is in scope instead (`open_context` / `queue_for`).
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    }
}

/// The OPEN playlist as a playback origin (header Play / Shuffle / row play).
/// None when no playlist is open or its id is empty — an empty context must
/// never be stamped.
fn open_context() -> Option<crate::playback_qt::PlayContext> {
    with_doc(|d| crate::playback_qt::PlayContext::playlist(&d.id)).flatten()
}

/// The playlist's tracks as a playable queue (play-all / shuffle / enqueue).
pub fn current_queue() -> Vec<QueueTrack> {
    PAGE.lock()
        .unwrap()
        .as_ref()
        .map(|p| p.doc.tracks.iter().map(row_to_queue).collect())
        .unwrap_or_default()
}

/// Word-boundary truncation for the 2-line header description
/// (playlist.rs truncate_words).
fn truncate_words(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max).collect();
    let cut = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{}…", truncated[..cut].trim_end())
}

fn total_duration_label(tracks: &[PlaylistTrackRow]) -> String {
    let secs: u64 = tracks.iter().map(|t| t.duration_secs).sum();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
        format!("{hours} h {minutes:02} min")
    } else {
        format!("{minutes} min")
    }
}

// ---------------------------------------------------------------------------
// Header artwork (the same split the cards use — library_qt.rs
// `playlist_own_image` / `playlist_cover_urls`; kept local because those are
// private to that module. Hoisting the pair into one shared helper is the
// obvious follow-up, see the report.)
// ---------------------------------------------------------------------------

/// The playlist's OWN Qobuz graphic (`image_rectangle`, `..._mini` as the
/// lighter fallback). Only editorial playlists carry one.
fn playlist_own_image(playlist: &Playlist) -> String {
    [&playlist.image_rectangle, &playlist.image_rectangle_mini]
        .into_iter()
        .flatten()
        .find_map(|list| list.iter().find(|u| !u.is_empty()).cloned())
        .unwrap_or_default()
}

/// Up to four de-duplicated MEMBER-ALBUM covers for the header mosaic:
/// the server lists first (`images300` > `images150` > `images` — the same
/// picker as the sidebar tree and the cards, so one playlist shows the same
/// mosaic everywhere), else the first four distinct track covers (what the
/// Slint header's 2x2 collage reads).
fn collage_urls(playlist: &Playlist, tracks: &[Track]) -> Vec<String> {
    // A custom playlist cover replaces the whole mosaic on every surface.
    if let Some(p) = crate::cover_artwork_qt::playlist_cover(&playlist.id.to_string()) {
        if std::path::Path::new(&p).is_file() {
            return vec![p];
        }
    }
    let mut out: Vec<String> = Vec::new();
    let push = |url: String, out: &mut Vec<String>| {
        if !url.is_empty() && !out.contains(&url) {
            out.push(url);
        }
    };
    let listed = [&playlist.images300, &playlist.images150, &playlist.images]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty());
    if let Some(list) = listed {
        for url in list {
            push(url.clone(), &mut out);
            if out.len() == 4 {
                return out;
            }
        }
    }
    if out.is_empty() {
        for track in tracks {
            let (album_id, url) = track
                .album
                .as_ref()
                .map(|a| {
                    (
                        a.id.clone(),
                        // Mosaic tiles on the header/cards: the server-listed
                        // arm above prefers images300, so the member-album
                        // fallback requests the matching large variant
                        // (contract 04 §3).
                        a.image.best().cloned().unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            // A member album with a custom cover contributes THAT art.
            let url = crate::cover_artwork_qt::prefer_album_cover(&album_id, url);
            push(url, &mut out);
            if out.len() == 4 {
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

pub async fn load(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
) -> Result<(), String> {
    {
        let mut guard = PAGE.lock().unwrap();
        let doc = &mut guard.get_or_insert_with(PageState::default).doc;
        *doc = PlaylistDoc {
            id: playlist_id.to_string(),
            loading: true,
            sort_field: doc.sort_field.clone(),
            sort_asc: doc.sort_asc,
            ..Default::default()
        };
        publish(doc);
    }

    let mut pl = runtime
        .core()
        .get_playlist(playlist_id)
        .await
        .map_err(|e| format!("get_playlist {playlist_id} failed: {e}"))?;
    // `take` (not clone): the header helpers below still need `&pl`, and the
    // track list is the heavy half of the response.
    let tracks = pl.tracks.take().map(|c| c.items).unwrap_or_default();
    crate::playlist_snapshot_qt::record_detail_detached(
        playlist_id,
        pl.name.clone(),
        pl.owner.name.clone(),
        tracks.iter().map(|track| track.id).collect(),
    );
    // Header artwork, two arms — 1:1 with the cards (PlaylistCard.qml):
    // the playlist's OWN graphic, else the member-cover mosaic. `images[0]`
    // (and the first track's sleeve) is a MEMBER-ALBUM cover: binding it
    // here is exactly what rendered an album sleeve as the playlist's
    // artwork, and it also starved the collage of its tiles.
    // A custom cover beats BOTH arms (own graphic and mosaic).
    let custom = crate::cover_artwork_qt::playlist_cover(&pl.id.to_string())
        .filter(|p| std::path::Path::new(p).is_file());
    let has_custom_cover = custom.is_some();
    let cover_url = custom.clone().unwrap_or_else(|| playlist_own_image(&pl));
    let covers = match custom {
        Some(p) => vec![p],
        None => collage_urls(&pl, &tracks),
    };
    let description = pl
        .description
        .map(|d| qbz_text_utils::strip_html::strip_html(&d))
        .unwrap_or_default();

    // --- Seam A: merge-on-load ---------------------------------------------
    //
    // THE BUG THIS CLOSES: a "mixed" playlist is a QOBUZ playlist (numeric id)
    // with local-file and/or Plex rows attached through the `library.db`
    // sidecar tables `playlist_local_tracks` / `playlist_plex_tracks`. Qt could
    // WRITE those rows (`playlist_picker_qt::write_sidecar_refs`) and COUNT
    // them (`folders_qt` feeds the Playlist Manager's "(N local)" badge) — but
    // this loader built its rows exclusively from the Qobuz API response, so
    // the rows the user had just added never appeared on the page they were
    // added to. Two comments elsewhere in the port claimed this merge already
    // happened; they were describing the reference, not this file.
    //
    // The reader does the healing and the Plex-cache resolve; the interleave
    // places sidecar rows at their STORED absolute slots and fills the rest
    // with the server order. Plex rows are always included online — their
    // availability is connectivity-based (E13), not stored.
    let qobuz_count = tracks.len() as u32;
    let sidecar = tokio::task::spawn_blocking(move || {
        crate::local_playlist_qt::read_sidecar_rows_blocking(playlist_id, qobuz_count, true)
    })
    .await
    .unwrap_or_default();
    let mixed = !sidecar.is_empty();
    let merged = interleave_rows(tracks, sidecar);

    // Display rows + the playable snapshot + the row positions in ONE pass, so
    // a row's display id and its queue entry can never disagree — the same
    // shape `local_playlist_qt::load` uses, and the reason row identity (E11)
    // survives the connectivity flip.
    let mut rows: Vec<PlaylistTrackRow> = Vec::with_capacity(merged.len());
    let mut merged_queue: Vec<QueueTrack> = Vec::new();
    let mut merged_positions: std::collections::HashMap<String, i32> =
        std::collections::HashMap::new();
    for row in &merged {
        let (item, queue) = crate::local_playlist_qt::row_to_display(&row.item);
        merged_positions.insert(item.id.clone(), row.position);
        if let Some(q) = queue {
            merged_queue.push(q);
        }
        rows.push(item);
    }

    // Seam B: a mixed detail plays through local_playlist's merged queue
    // snapshot (source-aware QueueTracks — a local file must never be queued as
    // a catalog id), so `main.rs`'s three playback routes serve it unchanged.
    // A pure-Qobuz detail clears the snapshot instead, or its rows would
    // resolve against the previous playlist's queue.
    MIXED.store(mixed, std::sync::atomic::Ordering::Relaxed);
    if mixed {
        crate::local_playlist_qt::set_open_mixed_snapshot(
            &playlist_id.to_string(),
            merged_queue,
            merged_positions,
        );
    } else {
        crate::local_playlist_qt::clear_open_snapshot();
    }

    // Artwork (the reload_home pattern): disk hits inline, one background
    // download + republish.
    let cover_path = crate::artwork_qt::cached_path(&cover_url);
    let mut missing: Vec<String> = Vec::new();
    if !cover_url.is_empty() && cover_path.is_empty() {
        missing.push(cover_url.clone());
    }
    for row in rows.iter_mut() {
        if row.art_url.is_empty() {
            continue;
        }
        let hit = crate::artwork_qt::cached_path(&row.art_url);
        if hit.is_empty() {
            if !missing.contains(&row.art_url) {
                missing.push(row.art_url.clone());
            }
        } else {
            row.art_path = hit;
        }
    }

    // Header state.
    //
    // The heart used to be derived from `library_qt::with_library(...)` — a
    // raw scan of the Library feed, which `load_library_once()` fills only
    // when the user opens the Library view. The WRITE, meanwhile, decides its
    // direction from `library.db` (`toggle_playlist_favorite`). So on a fresh
    // launch the header drew an empty heart on a playlist that IS hearted and
    // the first click un-hearted it: display and direction read two different
    // sources. `library_qt::is_favorite("playlist", …)` now answers from the
    // library.db mirror in `fav_cache_qt` — the same authority the write reads
    // — and only falls back to the feed.
    let is_favorite = crate::library_qt::is_favorite("playlist", &playlist_id.to_string());
    let is_owner =
        pl.owner.id != 0 && pl.owner.id == USER_ID.load(std::sync::atomic::Ordering::SeqCst);
    // Following is only meaningful for a FOREIGN playlist, and its authority
    // is the user's own playlist list (the ownership snapshot). The feed stays
    // as the fallback for the window before the first snapshot lands.
    // `self::` on purpose: the `let` below shadows the free function in the
    // value namespace, and reading `is_following(...)` inside its own
    // initializer is a trap the next reader should not have to resolve.
    let is_following = !is_owner
        && (self::is_following(playlist_id)
            || crate::library_qt::with_library(|d| {
                d.feed.iter().any(|i| {
                    i.kind == "playlist" && i.id == playlist_id.to_string() && i.playlist_following
                })
            })
            .unwrap_or(false));
    let pinned = crate::sidebar_qt::is_pinned("playlist", &playlist_id.to_string());
    // "Copy to your library" hides once this playlist HAS been copied. The
    // authority is `library.db`'s `copied_playlists` (reference: main.rs:4555
    // seeds `PlaylistState.is-copied` from the identical read) — the port used
    // to keep the flag in the session document only, so a restart offered the
    // copy again on a playlist that already had one.
    let is_copied = !is_owner && crate::library_db_qt::is_playlist_copied(playlist_id);

    // Custom drag order (applied when sort == custom).
    let sort_state =
        with_doc(|d| (d.sort_field.clone(), d.sort_asc)).unwrap_or(("default".into(), true));

    let track_count = rows.len() as i32;
    let total_duration = total_duration_label(&rows);
    let doc = {
        let mut guard = PAGE.lock().unwrap();
        let doc = &mut guard.get_or_insert_with(PageState::default).doc;
        doc.name = pl.name.clone();
        doc.owner = pl.owner.name.clone();
        doc.owner_id = pl.owner.id;
        doc.description_short = truncate_words(&description, 160);
        doc.description = description;
        doc.cover_url = cover_url.clone();
        doc.cover_path = cover_path;
        doc.covers = covers;
        doc.has_custom_cover = has_custom_cover;
        doc.tracks = rows;
        stamp_positions(&mut doc.tracks);
        doc.track_count = track_count;
        doc.total_duration = total_duration;
        doc.is_owner = is_owner;
        doc.is_favorite = is_favorite;
        doc.is_following = is_following;
        doc.is_copied = is_copied;
        doc.pinned = pinned;
        doc.is_mixed = mixed;
        doc.loading = false;
        let (field, asc) = sort_state;
        apply_sort(doc, &field, asc);
        doc.clone()
    };
    publish(&doc);

    // Remember WHAT this playlist is, for the "Recently Played Playlists"
    // rail. Only the metadata — the play EVENT is written at the track-start
    // edge, which sees a QueueTrack and an id and nothing else. Upserting on
    // load rather than on play is what keeps a renamed or re-covered playlist
    // converging, and it cannot put a card on the rail by itself: the rail's
    // query JOINs meta against events, so a playlist merely browsed shows
    // nowhere (there is a test for exactly that).
    qbz_app::settings::playlist_play_history::record_playlist_meta(
        qbz_app::settings::playlist_play_history::PlaylistPlayMeta {
            playlist_id: &playlist_id.to_string(),
            title: &doc.name,
            owner: &doc.owner,
            owner_id: &doc.owner_id.to_string(),
            // The playlist's OWN graphic when it has one (or the user's
            // custom cover); the mosaic covers otherwise. Publishing only the
            // first field is what left the recents rail blank: `cover_url` is
            // empty for every playlist without a graphic of its own, which is
            // most of them, and is precisely why this page falls back to a
            // collage.
            artwork_url: &doc.cover_url,
            own_image: !doc.cover_url.is_empty(),
            covers: &doc.covers,
            track_count: doc.track_count.max(0) as u32,
            source: "qobuz",
        },
    );

    if !missing.is_empty() {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(missing).await;
            let doc = with_doc(|d| {
                if !d.cover_url.is_empty() {
                    d.cover_path = crate::artwork_qt::cached_path(&d.cover_url);
                }
                for row in d.tracks.iter_mut() {
                    if row.art_path.is_empty() {
                        row.art_path = crate::artwork_qt::cached_path(&row.art_url);
                    }
                }
                d.clone()
            });
            if let Some(doc) = doc {
                publish(&doc);
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sort + search (PlaylistActions.set-sort / filter_tracks)
// ---------------------------------------------------------------------------

/// Record the loaded order once, before any sort touches the rows.
pub(crate) fn stamp_positions(rows: &mut [PlaylistTrackRow]) {
    for (i, row) in rows.iter_mut().enumerate() {
        row.position = i as i32;
    }
}

fn apply_sort(doc: &mut PlaylistDoc, field: &str, asc: bool) {
    doc.sort_field = field.to_string();
    doc.sort_asc = asc;
    match field {
        "title" => doc
            .tracks
            .sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "artist" => doc
            .tracks
            .sort_by(|a, b| a.artist.to_lowercase().cmp(&b.artist.to_lowercase())),
        "album" => doc
            .tracks
            .sort_by(|a, b| a.album.to_lowercase().cmp(&b.album.to_lowercase())),
        "duration" => doc.tracks.sort_by_key(|t| t.duration_secs),
        // A MIXED playlist has no usable custom order, and forcing one would
        // scramble the merge that just ran: `apply_custom_order` keys its
        // stored ranks by `t.id.parse::<u64>()`, and a local row's display id
        // is a `library.db` rowid living in the SAME numeric space as a catalog
        // id — the two collide silently. A LocalFile / Unresolved row whose id
        // is a path or a `plex:<key>` does not parse at all and sinks to the
        // end on `u64::MAX`. So a mixed detail keeps the absolute-slot order
        // the interleave computed, which is the order the user actually chose.
        "custom" if !doc.is_mixed => apply_custom_order(doc),
        "custom" => {}
        // "default" / "added": the API insertion order IS the added order
        // (a track is appended when it is added), so both sort by the slot
        // stamped at load — an actual order, not a reversal of whatever the
        // rows happened to be in after a Title sort (issue: the direction
        // toggle needed two clicks and switching fields kept the old order).
        _ => doc.tracks.sort_by_key(|t| t.position),
    }
    // Canonical ascending (oldest-added first for default/added), then
    // reverse for the other direction. Custom keeps its stored order.
    if field != "custom" && !asc {
        doc.tracks.reverse();
    }
}

/// The per-user custom-order sidecar (`<user dir>/playlist_orders.json`:
/// { playlist_id: [track_id, ...] } — the Slint uses playlist_orders.db
/// with (u64, bool, i32) rows; same behavior, simpler backend).
fn orders_path() -> Option<std::path::PathBuf> {
    crate::sidebar_qt::user_dir().map(|d| d.join("playlist_orders.json"))
}

fn load_custom_orders(playlist_id: u64) -> Vec<u64> {
    let Some(path) = orders_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get(playlist_id.to_string()).and_then(|ids| {
                ids.as_array().map(|a| {
                    a.iter()
                        .filter_map(|id| {
                            id.as_str()
                                .and_then(|s| s.parse::<u64>().ok())
                                .or(id.as_u64())
                        })
                        .collect()
                })
            })
        })
        .unwrap_or_default()
}

fn save_custom_orders(playlist_id: u64, ids: &[u64]) {
    let Some(path) = orders_path() else {
        return;
    };
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            playlist_id.to_string(),
            serde_json::json!(ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()),
        );
        if let Ok(text) = serde_json::to_string_pretty(&value) {
            let _ = std::fs::write(&path, text);
        }
    }
}

fn apply_custom_order(doc: &mut PlaylistDoc) {
    let Ok(pid) = doc.id.parse::<u64>() else {
        return;
    };
    let order = load_custom_orders(pid);
    if order.is_empty() {
        return;
    }
    let rank: std::collections::HashMap<u64, usize> =
        order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // Custom-ordered first (sidecar rank), then any track the sidecar
    // doesn't know (appended in current order — playlist.rs parity).
    doc.tracks.sort_by_key(|t| {
        let id = t.id.parse::<u64>().unwrap_or(u64::MAX);
        (rank.get(&id).copied().unwrap_or(usize::MAX), 0usize)
    });
}

pub fn set_sort(field: &str) {
    let doc = with_doc(|d| {
        // Re-pick flips direction for the sortable fields; a new field
        // resets to its natural default (Library All parity).
        // A new field starts at its natural direction: "added" newest
        // first, everything else ascending.
        let asc = if d.sort_field == field {
            !d.sort_asc
        } else {
            field != "added"
        };
        apply_sort(d, field, asc);
        (d.clone(), d.is_local_playlist)
    });
    let Some((doc, is_local)) = doc else { return };
    // "default" is a NO-OP branch in `apply_sort` — it keeps whatever order
    // the document is already in. On a Qobuz list that is right (the API's
    // insertion order is the loaded order, and re-sorting is a reload away).
    // On a LOCAL one it is not: "default" IS the editable repo order, the
    // surface the reorder chevrons write to, so coming back to it after a
    // Title sort has to RESTORE that order rather than leave the rows
    // alphabetical under a label that says otherwise — the view offers the
    // arrows on exactly this field. The repo is the cheap authority here (no
    // network, works offline), so the reload rebuilds it.
    if is_local && field == "default" {
        let Some(id) = crate::local_playlist_qt::open_id() else {
            publish(&doc);
            return;
        };
        publish(&doc);
        let runtime = crate::app();
        crate::spawn(async move {
            crate::local_playlist_qt::load(&runtime, &id).await;
        });
        return;
    }
    publish(&doc);
}

pub fn set_search(query: &str) {
    let doc = with_doc(|d| {
        d.search = query.to_string();
        d.clone()
    });
    if let Some(doc) = doc {
        publish(&doc);
    }
}

// ---------------------------------------------------------------------------
// Header actions
// ---------------------------------------------------------------------------

/// Favorite toggle (owned playlist heart): optimistic flip, then SETTLE on
/// what the store actually did.
///
/// The heart is the qbz-local library.db flag, not a Qobuz favorite — the
/// favorites endpoints have no `playlist_ids` param, so the previous route
/// through the favorites API could never have worked (see
/// `library_qt::toggle_playlist_favorite`). The direction is decided by the
/// db read inside that helper, exactly as the reference does
/// (`playlist_toggle_favorite_by_id`, main.rs:2196), so this must not keep
/// its optimistic guess when the two disagree — it re-seeds the header AND
/// the Library feed row from the returned value.
pub fn toggle_favorite() {
    let Some((id, optimistic, doc)) = with_doc(|d| {
        d.is_favorite = !d.is_favorite;
        (d.id.clone(), d.is_favorite, d.clone())
    }) else {
        return;
    };
    publish(&doc);
    let runtime = crate::app();
    crate::spawn(async move {
        let settled = crate::library_qt::toggle_favorite(&runtime, "playlist", &id).await;
        let Some(settled) = settled else {
            // Unroutable (non-Qobuz id) — undo the optimistic flip.
            revert_favorite(&id, !optimistic);
            return;
        };
        if settled != optimistic {
            revert_favorite(&id, settled);
        }
        crate::emit_library_favorite("playlist", &id, settled);
    });
}

/// Re-seed the open playlist's heart (only while that playlist is still the
/// one on screen — the user may have navigated away during the db write).
fn revert_favorite(id: &str, value: bool) {
    let doc = with_doc(|d| {
        if d.id != id {
            return None;
        }
        d.is_favorite = value;
        Some(d.clone())
    });
    if let Some(Some(doc)) = doc {
        publish(&doc);
    }
}

/// Follow / unfollow a foreign playlist (subscribe API; optimistic flip,
/// revert on error — main.rs playlist_set_follow_by_id).
pub async fn toggle_follow(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    // The open document is the ONE place in the app that already holds
    // everything a synthesized row needs, so the metadata is snapshotted in the
    // same lock hop as the optimistic flip.
    let Some((pid, follow, meta)) = with_doc(|d| {
        d.is_following = !d.is_following;
        let doc = d.clone();
        publish(&doc);
        (
            d.id.parse::<u64>().ok(),
            d.is_following,
            FollowRowMeta {
                name: d.name.clone(),
                owner: d.owner.clone(),
                tracks_count: d.track_count.max(0) as u32,
                cover_url: d.cover_url.clone(),
                covers: d.covers.clone(),
            },
        )
    }) else {
        return;
    };
    let Some(pid) = pid else { return };
    let res = if follow {
        runtime.core().subscribe_playlist(pid).await
    } else {
        runtime.core().unsubscribe_playlist(pid).await
    };
    if let Err(e) = res {
        log::error!("[qbz-qt] playlist {pid} follow={follow} failed: {e}");
        with_doc(|d| {
            d.is_following = !follow;
            let doc = d.clone();
            publish(&doc);
        });
    } else {
        // Settle the ownership snapshot AND the two surfaces the playlist
        // lives on (sidebar + Library feed) — see `follow_settled`.
        follow_settled(pid, follow, Some(meta));
    }
}

/// Pin toggle (the shared pinned store; refreshes the Home rail + sidebar).
pub fn toggle_pin() {
    let doc = with_doc(|d| {
        d.pinned = !d.pinned;
        let doc = d.clone();
        publish(&doc);
        doc
    });
    if let Some(doc) = doc {
        // Pin payload artwork: the playlist's own graphic, else its first
        // member cover so the Pinned rail card still has art (PlaylistCard's
        // `artworkUrl` falls back the same way).
        let art = if doc.cover_url.is_empty() {
            doc.covers.first().cloned().unwrap_or_default()
        } else {
            doc.cover_url.clone()
        };
        crate::toggle_pin("playlist".to_string(), doc.id, doc.name, doc.owner, art);
    }
}

/// "Copy to your library" — clone a foreign playlist into an OWNED one:
/// create + add all track ids, then record the copy (`main.rs:2080`
/// `playlist_copy_by_id`).
///
/// It does NOT unfollow the source, and it is not meant to: the reference's
/// copy path contains no `unsubscribe_playlist` anywhere
/// (`qbz/src/main.rs:2080-2150`), and `mark_playlist_copied` exists purely to
/// hide the Copy button on a second visit (`qbz/src/main.rs:4555`). Following
/// and copying are independent — after a copy the user has BOTH the followed
/// original and their own editable clone.
///
/// Three gaps against that reference are closed here, all of them things the
/// user can see:
///
/// * the ATTRIBUTION line the reference appends to the copied description
///   (verbatim, and untranslated there too — it is a plain `format!`);
/// * the `mark_playlist_copied` write, so the Copy button stays hidden across
///   restarts instead of offering to copy the same playlist a second time
///   (the module POC-NOTE said session-only; `library.db` already has the
///   `copied_playlists` table and `qbz-library` already has both accessors,
///   so no schema touch was needed);
/// * the toasts — success, and the two failure arms that used to log and
///   leave the user staring at an unchanged screen.
///
/// The new playlist is inserted into the sidebar + Library caches rather than
/// re-fetched: `crate::reload_sidebar()` (what this used to call) re-reads
/// `playlist/getUserPlaylists`, which lags a create by seconds — the same lag
/// the importer's bounded retry exists for — so the copy the user just made
/// was simply not there. See `follow_settled` for the same reasoning.
///
/// NOT ported: the reference also copies the source's artwork into the new
/// playlist's `library.db` custom-artwork row (`update_playlist_artwork`,
/// `main.rs:2132`). This port renders no custom playlist artwork at all (the
/// module POC-NOTE), so the write would be unreadable state; porting it means
/// porting the custom-cover feature, not one line here.
pub async fn copy_playlist(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some((pid, name, description, owner, track_ids, covers)) = with_doc(|d| {
        (
            d.id.parse::<u64>().ok(),
            d.name.clone(),
            d.description.clone(),
            d.owner.clone(),
            d.tracks
                .iter()
                .filter_map(|t| t.id.parse::<u64>().ok())
                .collect::<Vec<u64>>(),
            // The member-album collage, NOT `cover_url`: that is the SOURCE's
            // editorial `image_rectangle`, which the copy does not inherit
            // (this port does not write custom artwork — see the doc comment).
            // The copy holds the same tracks, so the same collage is what its
            // own next load will produce.
            d.covers.clone(),
        )
    }) else {
        return;
    };
    let Some(pid) = pid else { return };
    if track_ids.is_empty() {
        log::warn!("[qbz-qt] copy playlist {pid}: no tracks to copy");
        crate::toast_qt::error(qbz_i18n::t("Playlist has no tracks to copy"));
        return;
    }
    // main.rs:2109-2119, verbatim: the attribution is appended to the source
    // description, or becomes the whole description when there is none.
    let attribution = format!("\n\n---\nOriginally curated by {owner} on Qobuz");
    let new_description = if description.trim().is_empty() {
        attribution.trim_start().to_string()
    } else {
        format!("{description}{attribution}")
    };
    let new_playlist = match runtime
        .core()
        .create_playlist(&name, Some(new_description.as_str()), false)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            log::error!("[qbz-qt] copy playlist {pid}: create failed: {e}");
            crate::toast_qt::error(qbz_i18n::t("Failed to copy playlist"));
            return;
        }
    };
    if let Err(e) = runtime
        .core()
        .add_tracks_to_playlist(new_playlist.id, &track_ids)
        .await
    {
        log::error!("[qbz-qt] copy playlist {pid}: add tracks failed: {e}");
    }
    // Persist the copy against the SOURCE id (idempotent). rusqlite is
    // blocking, so it goes off the async path like every other db write here.
    let _ =
        tokio::task::spawn_blocking(move || crate::library_db_qt::mark_playlist_copied(pid)).await;
    // Only the page that asked for the copy gets the flag: the user may have
    // navigated away while the create was in flight (reference `is_open`).
    let source_id = pid.to_string();
    let patched = with_doc(|d| {
        if d.id != source_id {
            return None;
        }
        d.is_copied = true;
        Some(d.clone())
    });
    if let Some(Some(doc)) = patched {
        publish(&doc);
    }
    log::info!(
        "[qbz-qt] playlist {pid} copied to library as {}",
        new_playlist.id
    );
    crate::toast_qt::success(qbz_i18n::t("Copied to your library"));

    // The copy is OWNED, so it joins the sidebar and the Library's favorites
    // bucket. `tracks_count` comes from what was just sent, not from the
    // create response — that response describes an empty playlist.
    let new_id = new_playlist.id;
    let copied_count = track_ids.len() as u32;
    let new_owner = new_playlist.owner.name.clone();
    crate::sidebar_qt::insert_qobuz_entry(new_id, &name, copied_count, &covers);
    crate::publish_sidebar();
    if crate::library_qt::insert_playlist_row(
        &new_id.to_string(),
        &name,
        &new_owner,
        copied_count,
        "",
        covers,
        false,
    ) {
        crate::publish_library_document();
    }
}

/// The id of the playlist whose detail page is currently loaded, if any.
/// `PAGE` holds the LOCAL detail too (`adopt_doc`), so this answers for both
/// kinds — which is what makes [`back_if_showing`] correct for a `local:` id.
pub fn open_doc_id() -> Option<String> {
    with_doc(|d| d.id.clone())
}

/// Navigate back ONLY when the user is standing on `deleted_id`'s detail page
/// (contract §5.1).
///
/// The reference gates its two back-navigations on
/// `NavState.view == ContentView::Playlist && PlaylistState.id == deleted_id`
/// (`main.rs:20996`, `:21054`). Dropping that gate — which
/// `delete_playlist()` did — means deleting a row from the Playlist Manager or
/// from the sidebar throws the user off the surface they were working on.
pub(crate) fn back_if_showing(deleted_id: &str) {
    if crate::nav_qt::current_view() != "playlist" {
        return;
    }
    if open_doc_id().as_deref() != Some(deleted_id) {
        return;
    }
    crate::nav_qt::back();
}

/// Rename + optional description write, BY ID — the seam the detail view's own
/// rename and the shared playlist editor (`playlist_edit_qt`) both go through.
///
/// `description`:
///   * `None` — leave the stored description ALONE. `update_playlist` treats an
///     omitted field as unchanged, and this is what a caller that does not
///     KNOW the description must pass (§5.2).
///   * `Some(d)` — assert it. Only the editor passes this, and only when it
///     actually resolved the real description. The reference seeds `""` and
///     always sends `Some(trimmed)`, so every rename from its manager DELETES
///     the description; that bug is not ported.
///
/// It NEVER navigates and NEVER reloads the sidebar — both are the caller's
/// decision (§5.1). It does patch the OPEN detail document, but only when the
/// renamed playlist happens to be the one on screen: the editor is reachable
/// from the manager and the sidebar, where the detail behind it is somebody
/// else's.
pub async fn rename_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    pid: u64,
    name: &str,
    description: Option<&str>,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("empty playlist name".to_string());
    }
    runtime
        .core()
        .update_playlist(pid, Some(&name), description, None)
        .await
        .map_err(|e| format!("rename playlist {pid} failed: {e}"))?;

    let target = pid.to_string();
    let patched = with_doc(|d| {
        if d.id != target {
            return None;
        }
        d.name = name.clone();
        if let Some(desc) = description {
            // The doc's `description` is the HTML-STRIPPED display copy (see
            // the loader at :545) and `description_short` is derived from it,
            // so both are re-derived here rather than dropping the raw string
            // in and leaving the header showing a stale short.
            let stripped = qbz_text_utils::strip_html::strip_html(desc);
            d.description_short = truncate_words(&stripped, 160);
            d.description = stripped;
        }
        Some(d.clone())
    });
    if let Some(Some(doc)) = patched {
        publish(&doc);
    }
    Ok(())
}

/// Delete BY ID, with the ownership re-derivation the API requires (§5.1).
///
/// Qobuz's `playlist/delete` returns 200 and **no-ops** on a playlist you do
/// not own — the "deleted ok but it stays" bug. A FOLLOWED playlist has to go
/// through `unsubscribe_playlist` instead, and ownership is re-derived here
/// rather than trusted from a card, because the caller may be the sidebar or
/// the manager, where no `owner` was ever fetched.
///
/// A failed ownership check falls back to NOT-OWNED, matching the reference:
/// unsubscribing something you own is a no-op, where deleting something you
/// follow silently does nothing and looks like a broken button.
///
/// **NEVER navigates.** [`back_if_showing`] is the caller's to call.
pub async fn delete_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    pid: u64,
) -> Result<(), String> {
    let me = current_user_id();
    let owned = match runtime.core().get_playlist(pid).await {
        Ok(p) => me.is_some_and(|uid| uid == p.owner.id),
        Err(e) => {
            log::warn!("[qbz-qt] delete playlist {pid}: ownership check failed: {e}");
            false
        }
    };
    let res = if owned {
        runtime.core().delete_playlist(pid).await
    } else {
        runtime.core().unsubscribe_playlist(pid).await
    };
    res.map_err(|e| {
        let verb = if owned { "delete" } else { "unsubscribe" };
        format!("{verb} playlist {pid} failed: {e}")
    })?;
    log::info!(
        "[qbz-qt] playlist {pid} {}",
        if owned { "deleted" } else { "unsubscribed" }
    );
    if owned {
        crate::library_qt::playlist_deleted(&pid.to_string());
        OWNED_PLAYLISTS.lock().unwrap().remove(&pid);
        mark_following(pid, false);
        crate::fav_cache_qt::set("playlist", &pid.to_string(), false);
        crate::sidebar_qt::remove_qobuz_entry(pid);
        crate::playlist_manager_qt::playlist_deleted(pid);
        crate::publish_sidebar();
        crate::publish_library_document();
        // Leave the membership index's target set now instead of waiting out
        // the authoritative-list retirement grace.
        let _ = tokio::task::spawn_blocking(move || {
            crate::library_db_qt::with_db(true, |db| {
                db.delete_playlist_settings(pid)?;
                Ok(db.with_connection(|conn| {
                    qbz_library::qobuz_playlist_snapshot::mark_inactive(conn, pid)
                }))
            })
        })
        .await;
    } else {
        follow_settled(pid, false, None);
    }
    Ok(())
}

/// Rename the OPEN playlist (`QbzBridge.playlistRename`).
///
/// Passes `None` for the description on purpose: this caller never showed the
/// user a description field, so it has nothing to assert and must leave the
/// stored one untouched (§5.2).
pub async fn rename(runtime: &Arc<AppRuntime<LoggingAdapter>>, name: &str) -> Result<(), String> {
    let Some(pid) = with_doc(|d| d.id.parse::<u64>().ok()) else {
        return Err("no playlist open".to_string());
    };
    let Some(pid) = pid else {
        return Err("invalid playlist id".to_string());
    };
    rename_by_id(runtime, pid, name, None).await?;
    crate::reload_sidebar();
    Ok(())
}

/// Delete the OPEN playlist (`QbzBridge.playlistDelete`) and navigate back.
///
/// The back-nav is still here — this caller is by definition standing on that
/// playlist's page — but it is now GATED, so it cannot fire against a page the
/// user navigated to in the meantime.
pub async fn delete_playlist(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> Result<(), String> {
    let Some(pid) = with_doc(|d| d.id.parse::<u64>().ok()) else {
        return Err("no playlist open".to_string());
    };
    let Some(pid) = pid else {
        return Err("invalid playlist id".to_string());
    };
    delete_by_id(runtime, pid).await?;
    crate::reload_sidebar();
    back_if_showing(&pid.to_string());
    Ok(())
}

// ---------------------------------------------------------------------------
// Track actions
// ---------------------------------------------------------------------------

/// Per-row "Remove from playlist" (owner-gated, spec §1.6.1 — the Qobuz
/// API is owner-only).
///
/// `catalog_id` is the DISPLAY row id QML sends (`PlaylistTrackRow::id`, which
/// is the catalog track id — the same value the local/sidecar arms of
/// `crate::playlist_remove_track` route on). Qobuz's `playlist/deleteTracks`
/// addresses the MEMBERSHIP id (`playlist_track_id`) instead, so it is resolved
/// from the open document's matching row here.
///
/// This resolution IS the fix: the membership-id split (row build above) made
/// `id` and `playlist_track_id` two different numbers, but the delete kept
/// keying on the value QML sends — the catalog one — so `find`/`retain` matched
/// nothing and the endpoint got a non-membership id, i.e. the click rendered
/// and no-opped. Same-catalog duplicates resolve to their first occurrence:
/// QML sends the same display id for both copies, so that is the most this key
/// can disambiguate, and it stays offline-safe (a failed API call reloads).
pub async fn remove_track(runtime: &Arc<AppRuntime<LoggingAdapter>>, catalog_id: u64) {
    let Some((pid, membership_id)) = with_doc(|d| {
        if !d.is_owner {
            return None;
        }
        let pid = d.id.parse::<u64>().ok()?;
        let membership_id = d
            .tracks
            .iter()
            .find(|t| t.id.parse::<u64>().ok() == Some(catalog_id))
            .map(|t| t.playlist_track_id)?;
        Some((pid, membership_id))
    })
    .flatten() else {
        return;
    };
    // Optimistic removal of exactly the matched row, keyed on the membership
    // id so a same-catalog duplicate keeps its other copy. The API is the
    // source of truth on failure.
    with_doc(|d| {
        d.tracks.retain(|t| t.playlist_track_id != membership_id);
        d.track_count = d.tracks.len() as i32;
        d.total_duration = total_duration_label(&d.tracks);
        let doc = d.clone();
        publish(&doc);
    });
    match runtime
        .core()
        .remove_tracks_from_playlist(pid, &[membership_id])
        .await
    {
        Ok(()) => {
            // The membership snapshot speaks catalog track ids, not
            // membership-row ids.
            let _ = tokio::task::spawn_blocking(move || {
                crate::library_db_qt::with_db(true, |db| {
                    Ok(db.with_connection(|conn| {
                        qbz_library::qobuz_playlist_snapshot::apply_removed_tracks(
                            conn,
                            pid,
                            &[catalog_id],
                        )
                    }))
                })
            })
            .await;
        }
        Err(e) => {
            log::error!(
                "[qbz-qt] remove membership {membership_id} (catalog {catalog_id}) \
                 from playlist {pid} failed: {e}"
            );
            // Reload to reconcile (bounded-retry equivalent — the Slint
            // reconciles the same way after failed playlist ops).
            let _ = load(runtime, pid).await;
        }
    }
}

/// The open document's row ids, IN THE ORDER THE VIEW RENDERS THEM.
///
/// The Qt twin of the reference's `playlist::full_item_ids()`
/// (`local_playlist.rs:1835` calls it for exactly this). `local_playlist_qt`'s
/// reorder arms speak in visible indices and have to reach the repo positions
/// behind them; `PAGE` holds the LOCAL detail too (`adopt_doc`), so this
/// answers for both kinds of playlist.
pub(crate) fn row_ids() -> Vec<String> {
    with_doc(|d| d.tracks.iter().map(|t| t.id.clone()).collect()).unwrap_or_default()
}

/// Persist the document's CURRENT row order into the custom-order sidecar and
/// stamp the sort as "custom" — the shared tail of the two Qobuz reorder arms
/// (drag drop and the chevrons), so the two can never drift on which of the
/// three things (write, field, direction) they do.
fn stamp_custom_order(d: &mut PlaylistDoc) {
    let ids: Vec<u64> = d
        .tracks
        .iter()
        .filter_map(|t| t.id.parse::<u64>().ok())
        .collect();
    if let Ok(pid) = d.id.parse::<u64>() {
        save_custom_orders(pid, &ids);
    }
    d.sort_field = "custom".to_string();
    d.sort_asc = true;
}

/// Drag reorder (issue #589): move the row at visible index `from` to
/// insertion slot `slot` (0..N), persist the custom order, and switch the
/// sort to "custom" (the Slint's reorder rides the custom sort).
///
/// The QOBUZ arm only — `crate::playlist_reorder` routes a `local:` detail to
/// `local_playlist_qt::reorder_row`, which writes repo positions instead of a
/// sidecar.
///
/// `from`/`slot` are indices into `d.tracks`, which is the UNFILTERED
/// document. The view therefore only offers a reorder while the in-playlist
/// search is empty (the same rule `playlist_manager_qt.rs:236` applies to the
/// manager's arrows), so a filtered index can never be read as a document one.
pub fn reorder_track(from: usize, slot: usize) {
    let doc = with_doc(|d| {
        if !d.is_owner || from >= d.tracks.len() {
            return None;
        }
        let slot = slot.min(d.tracks.len());
        if slot == from || slot == from + 1 {
            return None;
        }
        let row = d.tracks.remove(from);
        let insert_at = if slot > from { slot - 1 } else { slot };
        d.tracks.insert(insert_at.min(d.tracks.len()), row);
        stamp_custom_order(d);
        Some(d.clone())
    });
    if let Some(doc) = doc.flatten() {
        publish(&doc);
    }
}

/// Arrow reorder — move the row `row_id` one slot up (`delta < 0`) or down
/// (`delta > 0`) in the open QOBUZ playlist's custom order.
///
/// The keyboard/mouse-accessible twin of the drag: the same persisted sidecar,
/// the same "custom" stamp, no gesture. `crate::playlist_move_row` routes a
/// `local:` detail to `local_playlist_qt::move_row` instead.
pub fn move_row(row_id: &str, delta: i32) {
    if delta == 0 {
        return;
    }
    let doc = with_doc(|d| {
        if !d.is_owner {
            return None;
        }
        let idx = d.tracks.iter().position(|t| t.id == row_id)?;
        let target = idx as i32 + delta.signum();
        if target < 0 || target as usize >= d.tracks.len() {
            return None; // already first / last
        }
        d.tracks.swap(idx, target as usize);
        stamp_custom_order(d);
        Some(d.clone())
    });
    if let Some(doc) = doc.flatten() {
        publish(&doc);
    }
}

/// Fetch a playlist and build its queue (the card-level actions: the
/// LibPlaylistCard overlay Play and its menu's queueing — no open view
/// needed).
async fn queue_for(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
) -> Result<Vec<QueueTrack>, String> {
    let pl = runtime
        .core()
        .get_playlist(playlist_id)
        .await
        .map_err(|e| format!("get_playlist {playlist_id} failed: {e}"))?;
    let tracks = pl.tracks.map(|c| c.items).unwrap_or_default();
    if tracks.is_empty() {
        return Err(format!("playlist {playlist_id} has no playable tracks"));
    }
    Ok(tracks.iter().map(|t| row_to_queue(&map_track(t))).collect())
}

/// Card overlay Play: fetch + play the whole playlist from the top.
pub async fn play_playlist_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
) -> Result<(), String> {
    let tracks = queue_for(runtime, playlist_id).await?;
    // The card path has NO open doc — the origin comes from the id it was
    // asked to play, not from `open_context()`.
    play_queue_at(
        runtime,
        tracks,
        0,
        crate::playback_qt::PlayContext::playlist(&playlist_id.to_string()),
    )
    .await
}

/// Card menu queueing: the whole playlist after the current track ("next"
/// — reversed so the first lands first, "later" — block tail, "queue" —
/// append).
pub async fn enqueue_playlist_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
    mode: &str,
) -> Result<(), String> {
    // Appended rows carry their OWN origin, so the glyph stays right once the
    // queue advances into them (the Slint leaves its enqueue paths unstamped —
    // playback.rs:4323/:4400 — which is why an enqueued playlist there shows
    // the album; stamping is the strict improvement, same landing page shape).
    let tracks = crate::playback_qt::stamped(
        queue_for(runtime, playlist_id).await?,
        crate::playback_qt::PlayContext::playlist(&playlist_id.to_string()),
    );
    // Empty after the seam: nothing to route, nothing to insert. The seam has
    // already toasted how many rows it dropped.
    if tracks.is_empty() {
        log::info!("[qbz-qt] enqueue_playlist {playlist_id}: every track was filtered");
        return Ok(());
    }
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled, so the local insert + sync tail
    // below only run in local/renderer mode.
    if crate::playback_qt::route_enqueue_to_peer(&tracks, mode).await {
        return Ok(());
    }
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return Ok(());
    };
    let added_castable = crate::playback_qt::batch_all_qconnect_castable(&tracks);
    match mode {
        "next" => {
            for track in tracks.into_iter().rev() {
                runtime.core().add_track_next(track).await;
            }
        }
        "later" => {
            for track in tracks {
                runtime.core().add_track_later(track).await;
            }
        }
        _ => runtime.core().add_tracks(tracks).await,
    }
    // QConnect sync-on-add (#442): push the updated queue to the session;
    // skipped silently for a non-castable batch.
    crate::playback_qt::sync_qconnect_after_add(added_castable).await;
    crate::queue_qt::publish(runtime).await;
    Ok(())
}

/// Follow / unfollow a playlist by id (card overlay follow — the header
/// toggle handles the open view's optimistic flip).
pub async fn set_follow_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
    follow: bool,
) {
    let res = if follow {
        runtime.core().subscribe_playlist(playlist_id).await
    } else {
        runtime.core().unsubscribe_playlist(playlist_id).await
    };
    if let Err(e) = res {
        log::error!("[qbz-qt] playlist {playlist_id} follow={follow} failed: {e}");
    } else {
        // Same settle point as the header toggle. The UNFOLLOW arm needs no
        // metadata — it only removes rows.
        //
        // The FOLLOW arm buys it with ONE `get_playlist`, which is what the
        // reference's copy path pays for the same reason (`qbz/src/main.rs:2087`).
        // What that replaces is the `crate::reload_sidebar()` this seam used to
        // fire, which was wrong twice over: it re-read `playlist/getUserPlaylists`,
        // a list that LAGS the subscribe, so the row it was fetched for could
        // still be missing; and it never touched the Library feed at all, while
        // the Library loads exactly ONCE per session (`main.rs::load_library_once`)
        // — so a playlist followed from a Discover / Search card stayed out of My
        // Library for the rest of the session unless the user hit the toolbar's
        // manual refresh. That is the owner's finding seen from the follow side.
        //
        // A failed metadata fetch degrades to `None`, which is the refetch this
        // seam used to do unconditionally — never worse than before.
        let meta = if follow {
            match runtime.core().get_playlist(playlist_id).await {
                Ok(playlist) => Some(FollowRowMeta::from_playlist(&playlist)),
                Err(e) => {
                    log::warn!(
                        "[qbz-qt] playlist {playlist_id} follow: row metadata fetch failed: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };
        follow_settled(playlist_id, follow, meta);
    }
}

/// Add tracks to a playlist by id (the DnD drop path): Qobuz catalog ids
/// via the membership API, then refresh the sidebar + the open detail when
/// it matches.
pub async fn add_tracks(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
    track_ids: &[u64],
) {
    if track_ids.is_empty() {
        return;
    }
    match runtime
        .core()
        .add_tracks_to_playlist(playlist_id, track_ids)
        .await
    {
        Ok(()) => {
            log::info!(
                "[qbz-qt] dropped {} track(s) onto playlist {playlist_id}",
                track_ids.len()
            );
        }
        Err(e) => {
            log::error!("[qbz-qt] drop add to playlist {playlist_id} failed: {e}");
            return;
        }
    }
    refresh_after_membership_change(runtime, playlist_id).await;
}

/// The refresh every membership WRITE owes: the sidebar's per-playlist track
/// count is stale, and a playlist page open on this id has to re-merge or the
/// rows just added stay invisible until the user navigates away and back
/// (the reference's E12 refresh).
///
/// Shared with the Add-to-Playlist picker, which writes to a playlist the user
/// may well be looking at.
pub(crate) async fn refresh_after_membership_change(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    playlist_id: u64,
) {
    crate::reload_sidebar();
    let open_id = with_doc(|d| d.id.parse::<u64>().ok()).flatten();
    if open_id == Some(playlist_id) {
        let _ = load(runtime, playlist_id).await;
    }
}

// ---------------------------------------------------------------------------
// Playback (play-all / shuffle / row play / row enqueue)
// ---------------------------------------------------------------------------

/// Every play in this module funnels here, and it goes through the SHARED
/// stamping seam (`playback_qt::set_queue_stamped`) rather than
/// `core().set_queue` — bypassing it was the second half of the lost-context
/// bug: even a correctly stamped queue would have been fine, but an unstamped
/// one got no derive either.
async fn play_queue_at(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    start: usize,
    context: Option<crate::playback_qt::PlayContext>,
) -> Result<(), String> {
    if tracks.is_empty() {
        return Err("playlist has no playable tracks".to_string());
    }
    let start = start.min(tracks.len() - 1);
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return Ok(());
    };
    // F1 (contract §5.3): the anchor is whatever survived the seam's filter at
    // the remapped index. This is the playlist path, and playlists are where
    // the owner says pulled tracks show up MOST — reading the id off the
    // pre-filter list here is the difference between "skips the dead track" and
    // "plays nothing, says nothing".
    let Some(anchor) =
        crate::playback_qt::set_queue_stamped(runtime, tracks, Some(start), context).await
    else {
        log::info!("[qbz-qt] playlist play: every track was filtered, queue untouched");
        return Ok(());
    };
    let first_id = anchor.track_id;
    crate::queue_qt::publish(runtime).await;
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if crate::playback_qt::route_play_remote(runtime, first_id, "play_queue_at").await {
        return Ok(());
    }
    crate::playback_qt::play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    crate::playback_qt::refresh_now_playing(runtime).await;
    Ok(())
}

/// Header Play: the playlist's tracks (current sort) from the top.
pub async fn play_all(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> Result<(), String> {
    let tracks = current_queue();
    play_queue_at(runtime, tracks, 0, open_context()).await
}

/// Header context-menu queueing: insert the OPEN playlist without fetching it
/// again. Stamping happens while the open playlist id is still in scope, so
/// the appended rows keep their playlist origin when playback reaches them.
pub async fn enqueue_all(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    mode: &str,
) -> Result<(), String> {
    let tracks = crate::playback_qt::stamped(current_queue(), open_context());
    if tracks.is_empty() {
        return Ok(());
    }
    crate::playback_qt::enqueue_track_list_mode(runtime, tracks, mode).await
}

/// Header Shuffle: reorder THIS list and play the top of the shuffled order.
///
/// The flag alone is not a shuffle. Raising `set_shuffle(true)` and calling
/// `play_all` starts on the playlist's #1 track every single time — the core's
/// mode only randomises what comes NEXT — so the one track the user actually
/// hears when they press Shuffle was deterministic. Owner ruling 2026-08-01:
/// every shuffle must be genuinely random. Same shape as
/// `playback_qt::play_track_list_in`'s shuffle arm and as the reference's
/// `play_album_shuffled` (playback.rs:3945-3957): mix the Vec, start at 0.
/// The MODE is still raised, so continuing past this list stays shuffled.
pub async fn play_shuffled(runtime: &Arc<AppRuntime<LoggingAdapter>>) -> Result<(), String> {
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return Ok(());
    };
    runtime.core().set_shuffle(true).await;
    crate::now_playing::set_shuffle(true);
    let mut tracks = current_queue();
    crate::playback_qt::xorshift_shuffle(&mut tracks);
    play_queue_at(runtime, tracks, 0, open_context()).await
}

/// Row play: the playlist as the queue, starting at this track.
pub async fn play_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: &str,
) -> Result<(), String> {
    let start = with_doc(|d| d.tracks.iter().position(|t| t.id == track_id)).flatten();
    let tracks = current_queue();
    play_queue_at(runtime, tracks, start.unwrap_or(0), open_context()).await
}

/// Row ⋯ queueing: one playlist track into the EXISTING queue
/// ("next" -> add_track_next, "later" -> add_track_later, "queue" -> append).
pub async fn enqueue_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: &str,
    mode: &str,
) -> Result<(), String> {
    let row = with_doc(|d| d.tracks.iter().find(|t| t.id == track_id).cloned())
        .flatten()
        .ok_or_else(|| format!("track {track_id} not in the open playlist"))?;
    // NOT `.expect("one row in, one row out")` any more: the enqueue seam
    // filters now (F2 + D5), so one row in can legitimately be ZERO rows out.
    // The `expect` would have PANICKED on a right-click of a pulled row — the
    // exact row the owner's smoke right-clicks to reach the replacement action.
    let Some(qt) = crate::playback_qt::stamped(vec![row_to_queue(&row)], open_context()).pop()
    else {
        log::info!("[qbz-qt] playlist enqueue: track {track_id} was filtered out");
        return Ok(());
    };
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled, so the local insert + sync tail
    // below only run in local/renderer mode.
    if crate::playback_qt::route_track_to_peer(&qt, mode).await {
        return Ok(());
    }
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return Ok(());
    };
    let added_castable = crate::playback_qt::batch_all_qconnect_castable(std::slice::from_ref(&qt));
    match mode {
        "next" => runtime.core().add_track_next(qt).await,
        "later" => runtime.core().add_track_later(qt).await,
        _ => runtime.core().add_track(qt).await,
    }
    // QConnect sync-on-add (#442): push the updated queue to the session;
    // skipped silently for a non-castable add.
    crate::playback_qt::sync_qconnect_after_add(added_castable).await;
    crate::queue_qt::publish(runtime).await;
    Ok(())
}
