//! "Add to playlist" picker — port of `crates/qbz/src/playlist_picker.rs` and
//! the `PlaylistPickerActions` / `DuplicateConfirmActions` handlers in the
//! reference's `main.rs:20070-20615`.
//!
//! One modal, three states in one document: the playlist list (filterable), the
//! inline "Create new playlist" panel, and the duplicate-confirm sub-modal that
//! opens when the chosen target already holds some of the carried tracks.
//!
//! The picker CARRIES a set of tracks and adds them wherever the user points.
//! That is what makes the queue's "save as playlist" work without a save-as
//! endpoint: it seeds the picker with the whole queue and the inline create row
//! turns it into a named playlist — the reference does exactly this
//! (`queue.rs::save_as_playlist`), and every other host (track menus, album /
//! artist / label pages, multi-select bars) is the same call with a different
//! id list.
//!
//! # The list: locals ALWAYS, Qobuz conditionally
//!
//! [`load`] lists the LOCAL playlists (library.db) first and unconditionally,
//! then appends the Qobuz set — unless offline, where the Qobuz half is hidden
//! entirely (decisions D3/D11: a Qobuz playlist cannot be written to offline,
//! so offering it would be a dead target). For a user running QBZ with no Qobuz
//! account, the local half is the WHOLE list.
//!
//! # The four-case matrix (target kind × payload kind)
//!
//! | target | payload | route |
//! |---|---|---|
//! | `local:<uuid>` | local refs | `local_playlist_qt::add_local_refs_blocking` |
//! | `local:<uuid>` | Qobuz ids | `local_playlist_qt::add_qobuz_tracks_blocking` |
//! | Qobuz `u64` | local refs | the library.db playlist SIDECAR ([`write_sidecar_refs`]) |
//! | Qobuz `u64` | Qobuz ids | `add_tracks_to_playlist` + the duplicate check |
//!
//! The payload kind rides [`Payload`], a two-variant enum rather than a
//! `Vec<String>` plus an `is_local` flag. With the flag, "refs carried as
//! catalog ids" stays a REPRESENTABLE state, and it is not a state a reviewer
//! can spot by eye: a LocalLibrary row id like `42` parses as a `u64` perfectly
//! happily — it simply means a different track once it reaches Qobuz. With the
//! enum the Qobuz endpoints can only ever be handed a `Payload::Qobuz`, so the
//! confusion is unrepresentable instead of merely avoided. (Same idiom, and the
//! same reason, as `local_playlist_qt::PlaylistRef`.)
//!
//! The duplicate check runs on the Qobuz-target + Qobuz-ids case ONLY — it is
//! the only target whose membership the API can be asked about.
//!
//! # OWED PORT (recorded, not silently dropped)
//!
//! Reco event logging. The reference logs one `PlaylistAdd` per Qobuz id
//! (`reco::log_playlist_add`, with `None` as the playlist id when the target is
//! a local or brand-new playlist) and deliberately does NOT log local refs,
//! whose ids do not resolve against the Qobuz catalog. This port has no reco
//! store at all: `auth_qt.rs`'s header lists `reco` among the per-user stores
//! its session activation does not bind, so there is nothing to log into and
//! even the pre-existing Qobuz→Qobuz add logs nothing. The SOURCE GATE is
//! nevertheless honoured structurally — the local-ref arms below never hold a
//! Qobuz id, so when the store does land there is no path for a non-catalog id
//! to leak into it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use serde::Serialize;

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

/// What the picker is carrying. Two variants, never a `Vec<String>` + flag —
/// see the module header.
#[derive(Clone)]
pub enum Payload {
    /// Decimal Qobuz catalog track ids.
    Qobuz(Vec<u64>),
    /// LocalLibrary local-mode refs: `"<i64>"` library row ids (resolved
    /// source-aware at insert time to a local path / an offline-copy Qobuz id)
    /// or `"plex:<rating key>"` Plex rows.
    LocalRefs(Vec<String>),
    /// Both at once — the QUEUE saved as a playlist while it holds catalog
    /// and local rows (queue_qt::save_as_playlist). Routed as the two
    /// halves would be on their own; the confusion the enum exists to make
    /// unrepresentable stays so, each half in its own typed slot.
    Mixed { qobuz: Vec<u64>, local: Vec<String> },
}

impl Default for Payload {
    fn default() -> Self {
        Self::Qobuz(Vec::new())
    }
}

impl Payload {
    fn len(&self) -> usize {
        match self {
            Self::Qobuz(v) => v.len(),
            Self::LocalRefs(v) => v.len(),
            Self::Mixed { qobuz, local } => qobuz.len() + local.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drives the counter's msgid ("Adding {} local track(s)").
    fn is_local(&self) -> bool {
        matches!(self, Self::LocalRefs(_))
    }

    fn clear(&mut self) {
        *self = Self::Qobuz(Vec::new());
    }
}

#[derive(Clone, Default, Serialize)]
pub struct PickerRow {
    pub id: String,
    pub name: String,
    /// Pre-formatted "N tracks" line (plural resolved in Rust, like the
    /// reference's `tracks_line`); empty for an empty playlist.
    #[serde(rename = "tracksLine")]
    pub tracks_line: String,
    /// LOCAL playlist (library.db, id `local:<uuid>`) — the row draws a
    /// hard-drive glyph and the write goes to the repo, not the API.
    #[serde(rename = "isLocal")]
    pub is_local: bool,
}

/// One "Already in" strip card: a writable playlist that already holds some
/// of the carried tracks.
#[derive(Clone, Serialize)]
pub struct AlreadyRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "isLocal")]
    pub is_local: bool,
    /// How many of the carried tracks it holds (distinct; the "N of M" card).
    pub contained: u32,
}

#[derive(Default, Serialize)]
pub struct DupDoc {
    pub open: bool,
    #[serde(rename = "duplicateCount")]
    pub duplicate_count: usize,
    #[serde(rename = "totalCount")]
    pub total_count: usize,
    #[serde(rename = "playlistName")]
    pub playlist_name: String,
    pub busy: bool,
}

#[derive(Default, Serialize)]
pub struct PickerDoc {
    pub open: bool,
    pub loading: bool,
    /// Number of tracks the picker is carrying — the modal's subtitle.
    #[serde(rename = "trackCount")]
    pub track_count: usize,
    /// The carried payload is LOCAL refs — only changes the counter's wording
    /// ("Adding {} local track(s)"); the row list is the same either way.
    #[serde(rename = "localMode")]
    pub local_mode: bool,
    /// Rows AFTER the name filter.
    pub playlists: Vec<PickerRow>,
    /// Total rows before the filter, so the modal can tell "you have no
    /// playlists" apart from "none matched".
    pub total: usize,
    pub filter: String,
    /// Inline create panel: revealed, and busy while the create runs.
    #[serde(rename = "creatingOpen")]
    pub creating_open: bool,
    pub creating: bool,
    pub dup: DupDoc,
    /// The session's last successful add target, pinned above the list.
    /// Present only while it still exists as a writable row, and retained
    /// under a search only when it matches the query.
    #[serde(rename = "lastUsed")]
    pub last_used: Option<PickerRow>,
    /// Writable playlists that already hold carried tracks — the containment
    /// strip. Meaningful only per `already_state`.
    #[serde(rename = "alreadyIn")]
    pub already_in: Vec<AlreadyRow>,
    /// "loading" | "complete" | "updating" | "unavailable" — the honest
    /// tri-state (plus the in-flight arm) the strip renders under.
    #[serde(rename = "alreadyState")]
    pub already_state: String,
}

/// Everything the modal owns between opens. One mutex: every field is written
/// from bridge invokables (Qt thread) and from spawned tasks alike.
#[derive(Default)]
struct PickerState {
    open: bool,
    loading: bool,
    /// The carried tracks, in the order the host supplied them.
    payload: Payload,
    /// The unfiltered row cache — `search` filters this rather than re-fetching.
    all_rows: Vec<PickerRow>,
    filter: String,
    creating_open: bool,
    creating: bool,
    /// Duplicate-confirm context, parked between the check and the user's
    /// answer (the reference's `DUP_CONFIRM_STASH`). `None` = sub-modal closed.
    dup: Option<DupContext>,
    dup_busy: bool,
    /// Containment strip rows, `None` while the query is in flight.
    already_rows: Option<Vec<AlreadyRow>>,
    /// "complete" | "updating" | "unavailable" once resolved.
    already_state: &'static str,
    /// Monotonic open counter — a containment result landing after the picker
    /// was closed and reopened must not dress the NEW payload.
    epoch: u64,
}

struct DupContext {
    playlist_id: u64,
    playlist_name: String,
    /// Every carried id (add-all uses this whole list).
    all_ids: Vec<u64>,
    /// The subset the target already holds (add-new-only subtracts these).
    duplicate_ids: HashSet<u64>,
    total_tracks: usize,
}

static STATE: std::sync::LazyLock<Mutex<PickerState>> =
    std::sync::LazyLock::new(|| Mutex::new(PickerState::default()));

/// Session-local "Last playlist": the id of the last target an add SUCCEEDED
/// against. Never persisted (process exit resets it, by design), never set on
/// a mere open or a failed operation, and validated against the live row list
/// on every publish — a deleted or no-longer-writable playlist simply stops
/// resolving.
static LAST_USED: Mutex<Option<String>> = Mutex::new(None);

/// Record a confirmed successful add. `n` guards the "nothing actually
/// landed" arms (0-row sidecar writes, empty diffs).
fn set_last_used(playlist_id: &str, n: usize) {
    if n == 0 {
        return;
    }
    if let Ok(mut g) = LAST_USED.lock() {
        *g = Some(playlist_id.to_string());
    }
}

/// Case-insensitive substring over the playlist name — the reference's
/// `filter-changed` rank pass reduced to a filter, because QML can drop rows
/// directly and does not need Slint's rank-and-reorder workaround.
fn matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(query)
}

/// Rebuild the document from the live state and post it to Qt.
fn publish() {
    let doc = {
        let st = STATE.lock().unwrap();
        let query = st.filter.trim().to_lowercase();
        // The pinned last target: resolved against the LIVE row list, so a
        // playlist that disappeared or stopped being writable stops pinning;
        // under a search it survives only when it matches the query.
        let last_used: Option<PickerRow> = LAST_USED
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .and_then(|id| st.all_rows.iter().find(|r| r.id == id).cloned())
            .filter(|r| matches(&r.name, &query));
        let playlists: Vec<PickerRow> = st
            .all_rows
            .iter()
            .filter(|r| matches(&r.name, &query))
            // De-duplicated from the pinned row, never listed twice.
            .filter(|r| last_used.as_ref().map(|l| l.id != r.id).unwrap_or(true))
            .cloned()
            .collect();
        PickerDoc {
            open: st.open,
            loading: st.loading,
            track_count: st.payload.len(),
            local_mode: st.payload.is_local(),
            playlists,
            total: st.all_rows.len(),
            filter: st.filter.clone(),
            creating_open: st.creating_open,
            creating: st.creating,
            dup: match st.dup.as_ref() {
                Some(ctx) => DupDoc {
                    open: true,
                    duplicate_count: ctx.duplicate_ids.len(),
                    total_count: ctx.total_tracks,
                    playlist_name: ctx.playlist_name.clone(),
                    busy: st.dup_busy,
                },
                None => DupDoc::default(),
            },
            last_used,
            already_in: st.already_rows.clone().unwrap_or_default(),
            already_state: if st.already_rows.is_none() {
                "loading".to_string()
            } else {
                st.already_state.to_string()
            },
        }
    };
    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    crate::playlist_picker_bridge::ui(move |mut b| {
        b.as_mut().set_picker_json(QString::from(json.as_str()));
    });
}

/// Open the picker seeded with decimal Qobuz track ids — the reference's
/// `open_for_ids(.., local = false)`. Unparseable entries are dropped here, at
/// the ONE place that is allowed to read a carried string as a catalog id.
///
/// Callable from any thread: everything it touches is the mutex plus a Qt hop.
pub fn open_for_ids(runtime: &Runtime, ids: Vec<String>) {
    let parsed: Vec<u64> = ids.iter().filter_map(|s| s.parse::<u64>().ok()).collect();
    open_payload(runtime, Payload::Qobuz(parsed));
}

/// Open the picker for a WHOLE catalog album. The album-card ⋯ menu carries
/// only the album id, so the track list is fetched here (the
/// `album_qt::add_to_mixtape` fetch pattern) and the streamable tracks'
/// catalog ids become the carried payload. A withdrawn/empty album opens
/// nothing.
pub fn open_for_album(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        let album = match runtime.core().get_album(&album_id).await {
            Ok(a) => a,
            Err(e) => {
                log::warn!("[qbz-qt] playlist picker: album fetch failed: {e}");
                return;
            }
        };
        let ids: Vec<u64> = album
            .tracks
            .as_ref()
            .map(|t| {
                t.items
                    .iter()
                    .filter(|tr| tr.is_streamable())
                    .map(|tr| tr.id)
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            log::warn!("[qbz-qt] playlist picker: album {album_id} has no addable tracks");
            return;
        }
        open_payload(&runtime, Payload::Qobuz(ids));
    });
}

/// Open the picker seeded with LocalLibrary local-mode refs — the reference's
/// `open_for_ids(.., local = true)`. The refs are carried VERBATIM: nothing on
/// this path may parse them as numbers (see the module header).
///
/// Build the refs with `local_playlist_qt::local_picker_ref_for_track` (a
/// LocalLibrary row) or `local_picker_ref_for_row` (an open local-playlist
/// detail row) — never by hand.
pub fn open_for_local_refs(runtime: &Runtime, refs: Vec<String>) {
    open_payload(runtime, Payload::LocalRefs(refs));
}

/// Open the picker with catalog ids AND local refs — the queue saved as a
/// playlist. Either half may be empty, in which case this is the matching
/// single-kind open.
pub fn open_for_mixed(runtime: &Runtime, ids: Vec<String>, refs: Vec<String>) {
    let qobuz: Vec<u64> = ids.iter().filter_map(|s| s.parse::<u64>().ok()).collect();
    match (qobuz.is_empty(), refs.is_empty()) {
        (true, _) => open_payload(runtime, Payload::LocalRefs(refs)),
        (false, true) => open_payload(runtime, Payload::Qobuz(qobuz)),
        (false, false) => open_payload(runtime, Payload::Mixed { qobuz, local: refs }),
    }
}

fn open_payload(runtime: &Runtime, payload: Payload) {
    if payload.is_empty() {
        return;
    }
    let epoch = {
        let mut st = STATE.lock().unwrap();
        st.open = true;
        st.loading = true;
        st.payload = payload;
        // A stale row list would flash the PREVIOUS open's playlists under the
        // new track count for as long as the fetch takes.
        st.all_rows.clear();
        st.filter.clear();
        st.creating_open = false;
        st.creating = false;
        st.dup = None;
        st.dup_busy = false;
        st.already_rows = None;
        st.already_state = "";
        st.epoch += 1;
        st.epoch
    };
    publish();

    let runtime = runtime.clone();
    crate::spawn(async move {
        let rows = load(&runtime).await;
        {
            let mut st = STATE.lock().unwrap();
            // The user may have closed the picker while the fetch was in
            // flight; landing rows into a closed picker would re-open it.
            if !st.open || st.epoch != epoch {
                return;
            }
            st.all_rows = rows;
            st.loading = false;
        }
        publish();
        refresh_containment(epoch).await;
    });
}

/// Compute the containment strip for the CURRENT payload against the live
/// row list, and the honest tri-state it renders under. Row names come from
/// the already-loaded picker rows, which doubles as the writability/liveness
/// join: a containment hit whose playlist is not offered as a target (offline
/// Qobuz half, followed list, deleted) resolves to nothing and is dropped.
async fn refresh_containment(epoch: u64) {
    let (payload, rows) = {
        let st = STATE.lock().unwrap();
        if !st.open || st.epoch != epoch || st.loading {
            return;
        }
        (st.payload.clone(), st.all_rows.clone())
    };
    let (already, state) = tokio::task::spawn_blocking(move || {
        use qbz_library::playlist_membership::{
            playlists_containing, ContainmentTarget, PlaylistTrackRef,
        };
        use qbz_library::qobuz_playlist_snapshot::{index_state, MembershipIndexState};

        let refs: Vec<PlaylistTrackRef> = match &payload {
            Payload::Qobuz(ids) => ids.iter().map(|&id| PlaylistTrackRef::Qobuz(id)).collect(),
            Payload::LocalRefs(strings) => {
                crate::local_playlist_qt::membership_refs_blocking(strings)
            }
            Payload::Mixed { qobuz, local } => {
                let mut refs: Vec<PlaylistTrackRef> = qobuz
                    .iter()
                    .map(|&id| PlaylistTrackRef::Qobuz(id))
                    .collect();
                refs.extend(crate::local_playlist_qt::membership_refs_blocking(local));
                refs
            }
        };
        // Only the Qobuz snapshot can be incomplete; the sidecar and local
        // tables are locally authoritative (QBZ is their only writer).
        let snapshot_dependent = refs.iter().any(|r| {
            matches!(r, PlaylistTrackRef::Qobuz(_))
                || matches!(
                    r,
                    PlaylistTrackRef::Library {
                        qobuz_track_id: Some(_),
                        ..
                    }
                )
        });
        let result = crate::library_db_qt::with_db(false, |db| {
            Ok(db.with_connection(|conn| {
                let contained = playlists_containing(conn, &refs)?;
                let state = index_state(conn)?;
                Ok::<_, rusqlite::Error>((contained, state))
            }))
        })
        .and_then(Result::ok);
        let Some((contained, state)) = result else {
            return (Vec::new(), "unavailable");
        };
        let state = if !snapshot_dependent {
            "complete"
        } else {
            match state {
                MembershipIndexState::Complete => "complete",
                // "Still updating" would be a lie while offline — nothing is.
                MembershipIndexState::Updating { .. } => {
                    if crate::offline_fwd::engine().is_offline() {
                        "unavailable"
                    } else {
                        "updating"
                    }
                }
                MembershipIndexState::Unavailable => "unavailable",
            }
        };
        let already: Vec<AlreadyRow> = contained
            .into_iter()
            .filter_map(|c| {
                let id = match &c.target {
                    ContainmentTarget::Qobuz(pid) => pid.to_string(),
                    ContainmentTarget::Local(id) => id.clone(),
                };
                rows.iter().find(|r| r.id == id).map(|r| AlreadyRow {
                    id,
                    name: r.name.clone(),
                    is_local: r.is_local,
                    contained: c.contained,
                })
            })
            .take(20)
            .collect();
        (already, state)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), "unavailable"));
    {
        let mut st = STATE.lock().unwrap();
        if !st.open || st.epoch != epoch {
            return;
        }
        st.already_rows = Some(already);
        st.already_state = state;
    }
    publish();
}

/// Hydrator progress callback: while the picker is open, each committed
/// membership snapshot (and the final state flip to Complete) re-answers the
/// containment question live.
pub fn membership_index_progressed() {
    let epoch = {
        let st = STATE.lock().unwrap();
        if !st.open || st.loading {
            return;
        }
        st.epoch
    };
    crate::spawn(async move {
        refresh_containment(epoch).await;
    });
}

/// The picker rows: the LOCAL playlists first and ALWAYS, then the Qobuz set —
/// skipped entirely while offline (D3/D11; a Qobuz playlist cannot be written
/// to offline, so listing one would only offer a dead target).
///
/// The local half is a library.db read, so it runs on a blocking worker.
async fn load(runtime: &Runtime) -> Vec<PickerRow> {
    let mut out: Vec<PickerRow> = tokio::task::spawn_blocking(|| {
        crate::local_playlist_qt::list_blocking()
            .into_iter()
            .map(|p| PickerRow {
                id: p.id,
                name: p.name,
                tracks_line: tracks_line(p.track_count),
                is_local: true,
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    if crate::offline_fwd::engine().is_offline() {
        return out;
    }
    match runtime.core().get_user_playlists().await {
        Ok(playlists) => out.extend(
            playlists
                .into_iter()
                // Followed playlists are read-only on the API and must never
                // be offered as add targets. Before the session user id is
                // known ownership cannot be told apart, so the pre-existing
                // behaviour (list everything) is kept for that window rather
                // than blanking the Qobuz half.
                .filter(|p| {
                    !crate::playlist_qt::session_user_known()
                        || crate::playlist_qt::owns(p.owner.id)
                })
                .map(|p| PickerRow {
                    id: p.id.to_string(),
                    name: p.name,
                    tracks_line: tracks_line(p.tracks_count),
                    is_local: false,
                }),
        ),
        Err(e) => log::warn!("[qbz-qt] playlist picker load failed: {e}"),
    }
    out
}

/// The row's "N tracks" line; empty for an empty playlist (the reference's
/// `tracks_line`).
fn tracks_line(count: u32) -> String {
    if count > 0 {
        qbz_i18n::tf("{} track", "{} tracks", count as i64, &[&count.to_string()])
    } else {
        String::new()
    }
}

pub fn search(query: &str) {
    STATE.lock().unwrap().filter = query.to_string();
    publish();
}

pub fn show_create(open: bool) {
    {
        let mut st = STATE.lock().unwrap();
        st.creating_open = open;
    }
    publish();
}

/// Close and clear the carried tracks, resetting the inline-create and filter
/// affordances so the next open starts clean (`on_close` in the reference).
pub fn close() {
    {
        let mut st = STATE.lock().unwrap();
        st.open = false;
        st.loading = false;
        st.payload.clear();
        st.all_rows.clear();
        st.filter.clear();
        st.creating_open = false;
        st.creating = false;
        st.dup = None;
        st.dup_busy = false;
    }
    publish();
}

/// Row click — the four-case matrix (module header). The target kind comes
/// from the row id (`local:<uuid>` vs a decimal Qobuz id), the payload kind
/// from [`Payload`]; the two are independent, which is why this is a matrix
/// and not a branch.
pub fn pick(playlist_id: &str) {
    let (payload, name) = {
        let st = STATE.lock().unwrap();
        let name = st
            .all_rows
            .iter()
            .find(|r| r.id == playlist_id)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        (st.payload.clone(), name)
    };
    if payload.is_empty() {
        return;
    }
    let runtime = crate::app();

    // Cases 1 and 2 — LOCAL playlist target: the write goes to the library.db
    // repo, which works offline and without a Qobuz account (D7 routing).
    // Routed through `PlaylistRef`, the same guard idiom this module's header
    // cites for its own `Payload` enum: the Qobuz half below cannot be handed
    // anything but a ref that already failed the local test.
    let target_ref = crate::local_playlist_qt::PlaylistRef::parse(playlist_id);
    if let Some(crate::local_playlist_qt::PlaylistRef::Local(target)) = target_ref.clone() {
        let open_target = target.clone();
        // The target name is already resolved above; closing now matches the
        // reference, which drops the modal before the write starts.
        close();
        crate::spawn(async move {
            let added = tokio::task::spawn_blocking(move || match payload {
                // Case 1 — local refs: resolved source-aware at insert
                // (offline copy -> Qobuz ref, Plex key -> Plex ref, else the
                // file path). NOT logged to reco: not catalog ids.
                Payload::LocalRefs(refs) => {
                    crate::local_playlist_qt::add_local_refs_blocking(&target, &refs)
                }
                // Case 2 — Qobuz ids onto a local playlist. The reference logs
                // these to reco with `None` as the playlist id (a local
                // playlist has no Qobuz id); this port has no reco store —
                // see the module header's OWED PORT.
                Payload::Qobuz(ids) => {
                    crate::local_playlist_qt::add_qobuz_tracks_blocking(&target, &ids)
                }
                // Cases 1 + 2 at once: the queue's mixed payload.
                Payload::Mixed { qobuz, local } => {
                    crate::local_playlist_qt::add_qobuz_tracks_blocking(&target, &qobuz)
                        + crate::local_playlist_qt::add_local_refs_blocking(&target, &local)
                }
            })
            .await
            .unwrap_or(0);
            set_last_used(&open_target, added);
            toast_added(added, &name);
            refresh_local_target(&runtime, &open_target).await;
        });
        return;
    }

    let Some(crate::local_playlist_qt::PlaylistRef::Qobuz(pid)) = target_ref else {
        return;
    };

    match payload {
        // Cases 3 + 4 at once — the queue's MIXED payload: the local refs
        // ride the sidecar right away, and the catalog ids take the Qobuz arm
        // below (its duplicate check included) through one more pass of this
        // function with only them carried.
        Payload::Mixed { qobuz, local } => {
            let qobuz_count = crate::sidebar_qt::playlist_track_count(pid).unwrap_or(0);
            let sidecar_runtime = runtime.clone();
            crate::spawn(async move {
                let written = write_sidecar_refs(pid, qobuz_count, local).await;
                log::info!(
                    "[qbz-qt] playlist picker: {written} local row(s) filed on the sidecar of {pid}"
                );
                crate::playlist_qt::refresh_after_membership_change(&sidecar_runtime, pid).await;
            });
            {
                let mut st = STATE.lock().unwrap();
                st.payload = Payload::Qobuz(qobuz);
            }
            pick(playlist_id);
        }
        // Case 3 — local refs onto a QOBUZ playlist. They attach through the
        // library.db sidecar tables (the same tables the merged detail
        // renders), never through the API: a library row id posted to Qobuz
        // would silently mean a different track.
        Payload::LocalRefs(refs) => {
            close();
            // Base = the Qobuz block size from the sidebar's session cache.
            let qobuz_count = crate::sidebar_qt::playlist_track_count(pid).unwrap_or(0);
            crate::spawn(async move {
                let written = write_sidecar_refs(pid, qobuz_count, refs).await;
                set_last_used(&pid.to_string(), written);
                toast_added(written, &name);
                // E12: the open detail re-merges so the rows show up now.
                crate::playlist_qt::refresh_after_membership_change(&runtime, pid).await;
            });
        }
        // Case 4 — Qobuz ids onto a Qobuz playlist: the API, and the ONLY case
        // with a duplicate check (the only target whose membership can be
        // asked about). Duplicates found => park the context and open the
        // confirm sub-modal; none => add directly and toast.
        //
        // A FAILED dup check falls through to a direct add rather than
        // blocking the action: a transient API error must not cost the user
        // their click.
        Payload::Qobuz(ids) => {
            crate::spawn(async move {
                match runtime.core().check_playlist_duplicates(pid, &ids).await {
                    Ok(dup) if dup.duplicate_count > 0 => {
                        {
                            let mut st = STATE.lock().unwrap();
                            st.dup = Some(DupContext {
                                playlist_id: pid,
                                playlist_name: name,
                                all_ids: ids,
                                duplicate_ids: dup.duplicate_track_ids,
                                total_tracks: dup.total_tracks,
                            });
                            st.dup_busy = false;
                        }
                        publish();
                    }
                    Ok(_) => {
                        close();
                        add_and_toast(&runtime, pid, ids, name).await;
                    }
                    Err(e) => {
                        log::warn!(
                            "[qbz-qt] playlist picker: dup check failed, adding directly: {e}"
                        );
                        close();
                        add_and_toast(&runtime, pid, ids, name).await;
                    }
                }
            });
        }
    }
}

/// Duplicate-confirm answer. `add_all` adds every carried id; otherwise only
/// the ones the target does not already hold (`all \ duplicates`) — and when
/// that difference is empty there is nothing to do but close.
pub fn confirm_duplicates(add_all: bool) {
    let Some(ctx) = STATE.lock().unwrap().dup.take() else {
        return;
    };
    let to_add: Vec<u64> = if add_all {
        ctx.all_ids
    } else {
        ctx.all_ids
            .into_iter()
            .filter(|id| !ctx.duplicate_ids.contains(id))
            .collect()
    };
    if to_add.is_empty() {
        close();
        return;
    }
    {
        let mut st = STATE.lock().unwrap();
        st.dup_busy = true;
    }
    publish();

    let pid = ctx.playlist_id;
    let name = ctx.playlist_name;
    let runtime = crate::app();
    crate::spawn(async move {
        add_and_toast(&runtime, pid, to_add, name).await;
        close();
    });
}

pub fn cancel_duplicates() {
    {
        let mut st = STATE.lock().unwrap();
        st.dup = None;
        st.dup_busy = false;
    }
    publish();
}

/// Inline "Create new playlist" -> create, add the carried tracks, close, toast.
/// Re-entrancy is guarded by `creating`, which the confirm button also reads:
/// a double click would otherwise create two playlists with the same name.
///
/// D8: OFFLINE the new playlist is a LOCAL one (library.db). A Qobuz create
/// cannot succeed without a session, and the local playlist is a first-class
/// target for a user who has no Qobuz account at all.
pub fn create_and_add(name: &str) {
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    let payload = {
        let mut st = STATE.lock().unwrap();
        if st.creating {
            return;
        }
        st.creating = true;
        st.payload.clone()
    };
    publish();

    let runtime = crate::app();
    if crate::offline_fwd::engine().is_offline() {
        crate::spawn(async move {
            let created = tokio::task::spawn_blocking({
                let name = name.clone();
                // offline_only = true, mirroring the reference's offline
                // create: the playlist is never offered for upload and never
                // reaches a QConnect push.
                move || crate::local_playlist_qt::create_blocking(&name, None, true)
            })
            .await
            .ok()
            .flatten();
            let Some(new_id) = created else {
                log::error!("[qbz-qt] playlist picker: offline create-and-add failed");
                {
                    let mut st = STATE.lock().unwrap();
                    st.creating = false;
                }
                publish();
                crate::toast_qt::error(qbz_i18n::t("Couldn't create the playlist"));
                return;
            };
            crate::sidebar_qt::insert_local_entry(&new_id, &name);
            crate::publish_sidebar();
            let added = tokio::task::spawn_blocking({
                let new_id = new_id.clone();
                move || match payload {
                    Payload::LocalRefs(refs) => {
                        crate::local_playlist_qt::add_local_refs_blocking(&new_id, &refs)
                    }
                    Payload::Qobuz(ids) => {
                        crate::local_playlist_qt::add_qobuz_tracks_blocking(&new_id, &ids)
                    }
                    Payload::Mixed { qobuz, local } => {
                        crate::local_playlist_qt::add_qobuz_tracks_blocking(&new_id, &qobuz)
                            + crate::local_playlist_qt::add_local_refs_blocking(&new_id, &local)
                    }
                }
            })
            .await
            .unwrap_or(0);
            close();
            // Create-and-add counts as a use even when the payload was empty.
            set_last_used(&new_id, added.max(1));
            toast_added(added, &name);
            // The new playlist has to appear in the sidebar, or the user's
            // only evidence it exists is the toast — and `reload_sidebar`
            // is a no-op offline, which is exactly this branch.
            crate::reload_sidebar_including_local();
        });
        return;
    }

    crate::spawn(async move {
        match runtime.core().create_playlist(&name, None, false).await {
            Ok(playlist) => {
                let pid = playlist.id;
                crate::sidebar_qt::insert_qobuz_entry(
                    pid,
                    &playlist.name,
                    playlist.tracks_count,
                    &[],
                );
                crate::publish_sidebar();
                // Snapshot the newborn header FIRST (owned, empty, captured):
                // the incremental producer below refuses playlists it has
                // never captured, and the authoritative list will not name
                // this one until Qobuz's post-write lag clears.
                {
                    let owner_name = playlist.owner.name.clone();
                    let created_name = name.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::library_db_qt::with_db(true, |db| {
                            Ok(db.with_connection(|conn| {
                                qbz_library::qobuz_playlist_snapshot::record_created_playlist(
                                    conn,
                                    pid,
                                    &created_name,
                                    Some(owner_name.as_str()).filter(|o| !o.is_empty()),
                                )
                            }))
                        })
                    })
                    .await;
                }
                set_last_used(&pid.to_string(), 1);
                match payload {
                    Payload::Qobuz(ids) if !ids.is_empty() => {
                        add_and_toast(&runtime, pid, ids, name).await;
                    }
                    // Local refs onto the freshly created QOBUZ playlist ride
                    // the SAME sidecar as picking an existing one — a new
                    // playlist's Qobuz block is empty, so the base is 0.
                    //
                    // DIVERGENCE, recorded: the reference re-parses its carried
                    // ref strings as `u64` here and posts them to the API, so a
                    // library row id `42` would be added as catalog track 42.
                    // That is precisely the confusion `Payload` exists to make
                    // unrepresentable, and it is not reproduced.
                    Payload::LocalRefs(refs) if !refs.is_empty() => {
                        let written = write_sidecar_refs(pid, 0, refs).await;
                        toast_added(written, &name);
                        crate::playlist_qt::refresh_after_membership_change(&runtime, pid).await;
                    }
                    // The queue's mixed payload: local refs on the sidecar,
                    // catalog ids through the API, one toast for the ids.
                    Payload::Mixed { qobuz, local } => {
                        if !local.is_empty() {
                            let written = write_sidecar_refs(pid, 0, local).await;
                            log::info!(
                                "[qbz-qt] playlist picker: {written} local row(s) filed on the new playlist {pid}"
                            );
                        }
                        if !qobuz.is_empty() {
                            add_and_toast(&runtime, pid, qobuz, name).await;
                        }
                        crate::playlist_qt::refresh_after_membership_change(&runtime, pid).await;
                    }
                    _ => {}
                }
                close();
                crate::reload_sidebar_including_local();
            }
            Err(e) => {
                log::error!("[qbz-qt] playlist picker: create-and-add failed: {e}");
                {
                    let mut st = STATE.lock().unwrap();
                    st.creating = false;
                }
                publish();
                crate::toast_qt::error(qbz_i18n::t("Couldn't create the Qobuz playlist"));
            }
        }
    });
}

/// Shared-drag DROP: the picker's add matrix without the modal. Routes a
/// payload onto a sidebar target the same way `pick` would — local refs to a
/// local playlist or the Qobuz sidecar, Qobuz ids to a local playlist — with
/// the same success tail (last-used, toast, refreshes). The Qobuz-ids-onto-
/// Qobuz-playlist case stays with its pre-existing `playlist_qt::add_tracks`
/// caller and never reaches here.
pub async fn add_dropped_payload_to_target(runtime: &Runtime, target_id: &str, payload: Payload) {
    if payload.is_empty() {
        return;
    }
    match crate::local_playlist_qt::PlaylistRef::parse(target_id) {
        Some(crate::local_playlist_qt::PlaylistRef::Local(target)) => {
            let added = tokio::task::spawn_blocking({
                let target = target.clone();
                move || match payload {
                    Payload::LocalRefs(refs) => {
                        crate::local_playlist_qt::add_local_refs_blocking(&target, &refs)
                    }
                    Payload::Qobuz(ids) => {
                        crate::local_playlist_qt::add_qobuz_tracks_blocking(&target, &ids)
                    }
                    Payload::Mixed { qobuz, local } => {
                        crate::local_playlist_qt::add_qobuz_tracks_blocking(&target, &qobuz)
                            + crate::local_playlist_qt::add_local_refs_blocking(&target, &local)
                    }
                }
            })
            .await
            .unwrap_or(0);
            set_last_used(&target, added);
            toast_added(added, "");
            refresh_local_target(runtime, &target).await;
        }
        Some(crate::local_playlist_qt::PlaylistRef::Qobuz(pid)) => {
            let (ids, refs) = match payload {
                Payload::LocalRefs(refs) => (Vec::new(), refs),
                Payload::Mixed { qobuz, local } => (qobuz, local),
                Payload::Qobuz(_) => return,
            };
            let had_ids = !ids.is_empty();
            let qobuz_count = crate::sidebar_qt::playlist_track_count(pid).unwrap_or(0);
            let written = if refs.is_empty() {
                0
            } else {
                write_sidecar_refs(pid, qobuz_count, refs).await
            };
            if had_ids {
                add_and_toast(runtime, pid, ids, String::new()).await;
            }
            set_last_used(&pid.to_string(), written.max(1));
            if written > 0 || !had_ids {
                toast_added(written, "");
            }
            crate::playlist_qt::refresh_after_membership_change(runtime, pid).await;
        }
        None => {}
    }
}

/// Seam C — attach local-mode refs to a QOBUZ playlist through the library.db
/// sidecar tables. Returns how many refs were written.
///
/// The position comes from `next_playlist_sidecar_position(pid, qobuz_count)`
/// — `max(qobuz_count + local_count + plex_count, MAX(position) + 1)` — taken
/// ONCE and incremented per row, so the batch lands after the whole merged list
/// AND past every stored position. It must NEVER be derived from the ref list's
/// own index: the reference records that its earlier 0-based `enumerate` write
/// collided slots in the absolute-slot interleave and SILENTLY LOST rows.
///
/// Re-adding an existing ref MOVES it to the append slot rather than
/// duplicating it (the writes are `INSERT OR REPLACE`; edge E4).
async fn write_sidecar_refs(pid: u64, qobuz_count: u32, refs: Vec<String>) -> usize {
    tokio::task::spawn_blocking(move || {
        crate::library_db_qt::with_db(true, |db| {
            let mut next = db.next_playlist_sidecar_position(pid, qobuz_count)?;
            let mut written = 0usize;
            for r in &refs {
                if let Some(key) = r.strip_prefix("plex:") {
                    db.add_plex_track_to_playlist(pid, key, next)?;
                } else if let Some(item) = r.strip_prefix("jellyfin:") {
                    db.add_remote_track_to_playlist(pid, "jellyfin", item, next)?;
                } else if let Some(item) = r.strip_prefix("subsonic:") {
                    db.add_remote_track_to_playlist(pid, "subsonic", item, next)?;
                } else if let Ok(lid) = r.parse::<i64>() {
                    db.add_local_track_to_playlist(pid, lid, next)?;
                } else {
                    log::warn!("[qbz-qt] playlist picker: unrecognized local ref {r}");
                    continue;
                }
                next += 1;
                written += 1;
            }
            Ok(written)
        })
        .unwrap_or(0)
    })
    .await
    .unwrap_or(0)
}

/// The Qobuz write path: add `ids` to `pid`, refresh, then toast the count.
/// Every Qobuz-target caller funnels here so the success/failure handling
/// cannot drift between the direct add, the two duplicate answers and
/// create-and-add.
async fn add_and_toast(runtime: &Runtime, pid: u64, ids: Vec<u64>, name: String) {
    let n = ids.len();
    if let Err(e) = runtime.core().add_tracks_to_playlist(pid, &ids).await {
        log::error!("[qbz-qt] playlist picker: add to playlist {pid} failed: {e}");
        crate::toast_qt::error(qbz_i18n::t("Failed to add tracks"));
        return;
    }
    set_last_used(&pid.to_string(), n);
    snapshot_added_detached(pid, ids);
    // The sidebar's per-playlist track count is now stale, and a playlist page
    // open on this id must re-merge (the reference's E12 refresh).
    crate::playlist_qt::refresh_after_membership_change(runtime, pid).await;
    toast_added(n, &name);
}

/// Move the membership snapshot with a CONFIRMED add, off-thread, so the
/// containment answer is already right the next time the picker opens —
/// without waiting for the hydrator or a detail load.
fn snapshot_added_detached(pid: u64, ids: Vec<u64>) {
    tokio::task::spawn_blocking(move || {
        let applied = crate::library_db_qt::with_db(true, |db| {
            Ok(db.with_connection(|conn| {
                qbz_library::qobuz_playlist_snapshot::apply_added_tracks(conn, pid, &ids)
            }))
        })
        .and_then(Result::ok)
        .unwrap_or(false);
        if applied {
            membership_index_progressed();
        } else {
            // Membership never captured: the hydrator owns this playlist.
            crate::playlist_index_qt::poke();
        }
    });
}

/// The shared success toast (the reference's `toast_added_tracks`). Every arm
/// of the matrix ends here, so the wording cannot drift between them. A zero
/// count is silent: the repo layer already logged WHY nothing landed.
fn toast_added(n: usize, name: &str) {
    if n == 0 {
        return;
    }
    let msg = if name.is_empty() {
        qbz_i18n::tf(
            "Added {} track",
            "Added {} tracks",
            n as i64,
            &[&n.to_string()],
        )
    } else {
        qbz_i18n::tf(
            "Added {} track to {}",
            "Added {} tracks to {}",
            n as i64,
            &[&n.to_string(), name],
        )
    };
    crate::toast_qt::success(msg);
}

/// Re-render the open LOCAL detail after an add that targeted it.
///
/// DIVERGENCE, recorded: the reference toasts and stops. Its local-mode picker
/// is opened from the Local Library, so "the target playlist is the page you
/// are looking at" does not arise there; in this port the same picker is
/// reachable with that playlist open, and a user with no Qobuz account — the
/// people local playlists exist for — would watch the add do visibly nothing.
/// The sidebar is deliberately NOT reloaded (1:1 with the reference): a local
/// row shows a cover collage, not a count, and a reload costs a Qobuz fetch.
async fn refresh_local_target(runtime: &Runtime, target: &str) {
    if crate::local_playlist_qt::open_id().as_deref() == Some(target) {
        let _ = crate::local_playlist_qt::load(runtime, target).await;
    }
}
