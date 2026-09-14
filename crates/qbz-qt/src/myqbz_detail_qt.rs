//! My QBZ — collection / mixtape DETAIL controller. The Qt port of
//! `crates/qbz/src/myqbz_detail.rs` (spec 02 §6.2), publishing ONE JSON
//! document (`QbzMyQbz.detailJson`, spec 02 §5.2).
//!
//! Shape follows `playlist_qt.rs`: a module-level `Mutex` document + a cached
//! full item list backing a client-side filter -> search -> sort that
//! re-derives the visible rows. The Slint uses `thread_local!` because its
//! caches hold `!Send` `slint::Image`s; Qt documents are plain JSON, so the
//! caches are plain statics reachable from the tokio tasks with no GUI hop.
//!
//! Load order (spec 02 §6.2 `navigate`): `reset()` (clears the three caches +
//! the prefs gate, publishes `{loading:true, found:true}`) -> BLOCK fetch (the
//! collection AND its persisted accordion set, one `spawn_blocking`) ->
//! `apply()` (header -> hero mosaic -> restore view-prefs -> open the persist
//! gate -> restore the pruned open set -> FULL_ITEMS -> rebuild) -> ONE artwork
//! pass + ONE republish -> `resolve_items` (fire-and-forget, <= 6 concurrent)
//! -> `ensure_expanded` (a no-op unless the restored set opened something).
//!
//! Two divergences from the reference are REPRODUCED on purpose and must not
//! be "unified" (spec 02 §6.2 + review items 23/26):
//! - the hero mosaic's custom-cover predicate is a bare `is_some()`, while the
//!   header's `hasCustomCover` also requires the file to resolve. A collection
//!   whose stored cover file was deleted therefore shows the kind glyph in the
//!   hero and the full mosaic in the GRID.
//! - a RESOLVED album row's `typeLabel` is UNTRANSLATED (`classify_release_type`
//!   returns the marked English literal and the reference never wraps it in
//!   `t()`), while an unresolved one reads the translated `t("ALBUM")`.
//!
//! Offline loads use the active cache index both to hide unavailable entries
//! and to hydrate cached Qobuz quality/type badges without calling the API.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::mixtape::{
    AlbumSource, CollectionKind, CollectionPlayMode, ItemType, MixtapeCollection,
    MixtapeCollectionItem,
};
use qbz_models::QueueTrack;
use serde::Serialize;
use tokio::sync::Semaphore;

/// `resolve_items` fan-out cap (spec 02 §7 T18 / §12 #8). A 200-item
/// artist_collection would otherwise issue 200 concurrent `albums/get` calls;
/// the Slint has no cap and that is the load-bearing perf difference.
const MAX_RESOLVE_CONCURRENCY: usize = 6;

// ---------------------------------------------------------------------------
// Documents (spec 02 §5.2)
// ---------------------------------------------------------------------------

/// One inline track row for the expanded view-mode. The keys are exactly what
/// `qml/rows/TrackRow.qml` reads off its `item` (spec 02 §5.2), plus `source`,
/// which the HOST delegate needs for its `draggable` arm (a local row's `id` is
/// a library.db row id and the sidebar drop handler would forward it as a Qobuz
/// catalog id). `number` is deliberately absent — it is a delegate property
/// (`property int number`), not an item field.
#[derive(Clone, Default, Serialize)]
pub struct InlineTrackRow {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(rename = "artistId")]
    pub artist_id: String,
    /// Always "" — the inline list is rendered under its own album.
    pub album: String,
    #[serde(rename = "albumId")]
    pub album_id: String,
    /// "m:ss", or "--:--" when the duration is unknown (never "0:00").
    pub duration: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    pub explicit: bool,
    /// Seeds `TrackRow`'s own `favorite` property; never written back (local
    /// hearts are unwired in this port and MyQBZ mixes sources).
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Always "" — the rows are mounted with `showArtwork: false`.
    #[serde(rename = "artPath")]
    pub art_path: String,
    /// "qobuz" | "local" | "offline" | "plex" — the delegate's `draggable`
    /// gate, and a `SourceIcon.qml` kind. `"offline"` is the badge of a Qobuz
    /// download/purchase, which is a local FILE with a cloud glyph; it appears
    /// here because the resolved rows now carry the source's real word instead
    /// of a flattened `"local"` (see `source_kind_of`).
    pub source: String,
}

/// One render row (spec 02 §5.2 `DetailRow`).
#[derive(Clone, Default, Serialize)]
pub struct DetailRow {
    /// 0-based PERSISTED position — the stable key. Display is +1.
    pub position: i32,
    #[serde(rename = "itemType")]
    pub item_type: String,
    /// The STORED source ("qobuz" | "local"), which is what the source filter
    /// matches on.
    pub source: String,
    #[serde(rename = "sourceItemId")]
    pub source_item_id: String,
    pub title: String,
    pub subtitle: String,
    #[serde(rename = "subtitleIsLink")]
    pub subtitle_is_link: bool,
    /// Resolved Qobuz artist id; "" until `resolve_items` lands (and for
    /// local/Plex items, whose tracks carry none).
    #[serde(rename = "artistId")]
    pub artist_id: String,
    /// Resolved album id for a TRACK row (source-aware), so its click opens the
    /// album it is on; "" until `resolve_items` lands. Albums use `sourceItemId`.
    #[serde(rename = "albumId")]
    pub album_id: String,
    /// RESOLVED source ("qobuz" | "plex" | "local" | "offline") — this is how a
    /// Plex item stored as `AlbumSource::Local` finally reads "plex", and a
    /// Qobuz download stored the same way reads "offline". Both come from the
    /// registry's claim on the resolved row (`source_kind_of`), never from a
    /// literal.
    #[serde(rename = "sourceKind")]
    pub source_kind: String,
    /// Every distinct source across the item's tracks (a playlist can mix them);
    /// empty until `resolve_items` lands. `source_kind` stays the primary one.
    #[serde(rename = "sourceKinds")]
    pub source_kinds: Vec<String>,
    #[serde(rename = "typeLabel")]
    pub type_label: String,
    #[serde(rename = "qualityTier")]
    pub quality_tier: String,
    #[serde(rename = "qualityDetail")]
    pub quality_detail: String,
    /// True until `resolve_items` fills this row — drives the quality skeleton.
    #[serde(rename = "qualityResolving")]
    pub quality_resolving: bool,
    #[serde(rename = "tracksText")]
    pub tracks_text: String,
    #[serde(rename = "yearText")]
    pub year_text: String,
    #[serde(rename = "artUrl")]
    pub art_url: String,
    #[serde(rename = "artPath")]
    pub art_path: String,
    /// Alias of `artUrl` — spec 01 §13.1 spells the row cover
    /// `artworkUrl`/`artworkPath` while spec 02 §5.2 spells it `artUrl`/`artPath`.
    /// Both are emitted (filled in `publish`) so either QML spelling resolves;
    /// an unread key costs one string per row.
    #[serde(rename = "artworkUrl")]
    pub artwork_url: String,
    #[serde(rename = "artworkPath")]
    pub artwork_path: String,
    /// The SECOND cover, for the GRID arm — same row, bigger rung.
    ///
    /// `artUrl` is the 50px list thumbnail (the reference's `_50` downscale,
    /// `myqbz_detail.rs:269`), and the grid used to read that same field into a
    /// 200px `AlbumCard`: a 4x upscale, which is the owner's "la calidad de la
    /// portada es menor que la del albumcard". The owner's ruling (2026-07-31)
    /// is "list sí deben ser los de 50, grid 200", i.e. the row carries BOTH
    /// sizes rather than one being re-derived per view-mode switch — a
    /// re-derive republishes the document, and a republish hands `model:` a new
    /// array, which resets the view's scroll offset (see `RowPatch`). Two
    /// strings per row buy an instant list/grid toggle.
    ///
    /// See `GRID_ART_TARGET` for why the rung is 600 and not 200.
    #[serde(rename = "artUrlLarge")]
    pub art_url_large: String,
    #[serde(rename = "artPathLarge")]
    pub art_path_large: String,
    /// Heart state for a row whose `sourceItemId` IS a Qobuz catalog album id
    /// (stored source `qobuz` + item type `album`); `false` for everything
    /// else, which is also everything the grid card hides the heart on.
    ///
    /// Read through `fav_cache_qt::is_album_favorite`, the same door every
    /// other card PRODUCER in this port uses (`home_qt`, `library_qt`,
    /// `album_qt`) — never `library_qt::is_favorite`, which ORs in a linear
    /// scan of the whole Library feed and is O(rows x feed) on a 200-item
    /// collection.
    #[serde(rename = "isFavorite")]
    pub is_favorite: bool,
    /// Pin state, same gate and the same producer door as `is_favorite`
    /// (`sidebar_qt::is_pinned`). Both settle themselves after a click through
    /// `QbzLibrary.libraryFavoriteChanged` / `pinChanged`, so neither needs a
    /// `RowPatch` field.
    #[serde(rename = "isPinned")]
    pub is_pinned: bool,
    pub selected: bool,
    #[serde(rename = "canExpand")]
    pub can_expand: bool,
    /// THE per-row accordion state — is this row's inline block open?
    ///
    /// ADDITIVE, and a deliberate divergence from BOTH references
    /// (owner-authorised 2026-07-30): neither `MixtapeDetailView.slint` nor the
    /// Tauri build had a per-row accordion, they render inline tracks only as a
    /// whole-list VIEW MODE ("no chevron"). The owner asked for the accordion
    /// anyway. The view mode SURVIVES but no longer means "open all" (owner,
    /// 2026-08-01): it only decides whether the arm carries the chevron.
    ///
    /// It is ONE notion of open, not two: `OPEN_ROWS` is the persisted truth,
    /// written by the chevron alone, and this flag is derived from it in
    /// `to_item` (AND-ed with the details arm, which is a render gate, not a
    /// second state). Do not add a `viewMode === "expanded"` test in QML.
    #[serde(rename = "rowOpen")]
    pub row_open: bool,
    #[serde(rename = "tracksLoaded")]
    pub tracks_loaded: bool,
    #[serde(rename = "expandLoading")]
    pub expand_loading: bool,
    #[serde(rename = "inlineTracks")]
    pub inline_tracks: Vec<InlineTrackRow>,
}

/// A PARTIAL row update for the `detailRowsPatched` channel — only the fields
/// that actually changed travel. QML `Object.assign`s each entry onto the live
/// row object it finds by `position` and bumps `patchRev` once for the batch.
///
/// This is the shape fix for the republish storm the audit measured
/// (`2026-07-30-myqbz-audit/detail.md` #7/#8): `apply_resolved` cloned and
/// `serde_json::to_string`'d the ENTIRE `DetailDoc` once per resolved item —
/// 34 full serialize -> parse -> patch cycles for one collection, 200 for a big
/// one, all on the GUI thread — and `patch_expanded` did the same while the
/// document carried every already-loaded row's `inlineTracks`, i.e. O(n²).
/// The QML side already patches row objects IN PLACE, so the document never
/// needed to travel whole for a one-row change.
///
/// `position` is the stable persisted key and the only join column QML needs;
/// `sourceItemId` rides along for logging and for a QML-side sanity check.
/// A patch is a DELTA — a freshly mounted view has missed every delta already
/// sent, which is why `detail_resync` exists.
#[derive(Clone, Default, Serialize)]
struct RowPatch {
    position: i32,
    #[serde(rename = "sourceItemId")]
    source_item_id: String,
    #[serde(rename = "sourceKind", skip_serializing_if = "Option::is_none")]
    source_kind: Option<String>,
    #[serde(rename = "sourceKinds", skip_serializing_if = "Option::is_none")]
    source_kinds: Option<Vec<String>>,
    #[serde(rename = "qualityTier", skip_serializing_if = "Option::is_none")]
    quality_tier: Option<String>,
    #[serde(rename = "qualityDetail", skip_serializing_if = "Option::is_none")]
    quality_detail: Option<String>,
    #[serde(rename = "typeLabel", skip_serializing_if = "Option::is_none")]
    type_label: Option<String>,
    #[serde(rename = "artistId", skip_serializing_if = "Option::is_none")]
    artist_id: Option<String>,
    #[serde(rename = "albumId", skip_serializing_if = "Option::is_none")]
    album_id: Option<String>,
    #[serde(rename = "tracksText", skip_serializing_if = "Option::is_none")]
    tracks_text: Option<String>,
    #[serde(rename = "qualityResolving", skip_serializing_if = "Option::is_none")]
    quality_resolving: Option<bool>,
    #[serde(rename = "artUrl", skip_serializing_if = "Option::is_none")]
    art_url: Option<String>,
    #[serde(rename = "artPath", skip_serializing_if = "Option::is_none")]
    art_path: Option<String>,
    /// The alias twin of `artUrl` — `publish` fills both in one place, so a row
    /// patch that moves one MUST move the other or the two go out of step.
    #[serde(rename = "artworkUrl", skip_serializing_if = "Option::is_none")]
    artwork_url: Option<String>,
    #[serde(rename = "artworkPath", skip_serializing_if = "Option::is_none")]
    artwork_path: Option<String>,
    /// The GRID rung (`DetailRow::art_url_large`). Travels with the small pair
    /// on the resolve backfill — the grid delegate reads these two and nothing
    /// else, so a patch that moved only `artUrl` left the grid on its
    /// placeholder glyph for the whole session.
    #[serde(rename = "artUrlLarge", skip_serializing_if = "Option::is_none")]
    art_url_large: Option<String>,
    #[serde(rename = "artPathLarge", skip_serializing_if = "Option::is_none")]
    art_path_large: Option<String>,
    #[serde(rename = "rowOpen", skip_serializing_if = "Option::is_none")]
    row_open: Option<bool>,
    #[serde(rename = "tracksLoaded", skip_serializing_if = "Option::is_none")]
    tracks_loaded: Option<bool>,
    #[serde(rename = "expandLoading", skip_serializing_if = "Option::is_none")]
    expand_loading: Option<bool>,
    #[serde(rename = "inlineTracks", skip_serializing_if = "Option::is_none")]
    inline_tracks: Option<Vec<InlineTrackRow>>,
}

impl RowPatch {
    /// An empty patch addressed at `row`. Fill only what changed.
    fn of(row: &DetailRow) -> Self {
        Self {
            position: row.position,
            source_item_id: row.source_item_id.clone(),
            ..Self::default()
        }
    }
}

/// The published detail document (spec 02 §5.2 `DetailDoc`).
#[derive(Clone, Serialize)]
pub struct DetailDoc {
    pub id: String,
    pub kind: String,
    #[serde(rename = "kindLabel")]
    pub kind_label: String,
    pub name: String,
    pub description: String,
    pub meta: String,
    #[serde(rename = "itemCount")]
    pub item_count: i32,
    #[serde(rename = "playMode")]
    pub play_mode: String,
    pub loading: bool,
    pub found: bool,
    #[serde(rename = "hasCustomCover")]
    pub has_custom_cover: bool,
    #[serde(rename = "customCoverPath")]
    pub custom_cover_path: String,
    #[serde(rename = "coverCount")]
    pub cover_count: i32,
    pub cols: i32,
    #[serde(rename = "heroCellUrls")]
    pub hero_cell_urls: Vec<String>,
    #[serde(rename = "heroCellPaths")]
    pub hero_cell_paths: Vec<String>,
    /// Alias of `heroCellUrls` — spec 01 §13.1 names the hero cells `urls`.
    /// Filled in `publish`.
    pub urls: Vec<String>,
    pub search: String,
    pub sort: String,
    #[serde(rename = "sortDir")]
    pub sort_dir: String,
    #[serde(rename = "typeFilter")]
    pub type_filter: String,
    #[serde(rename = "srcQobuz")]
    pub src_qobuz: bool,
    #[serde(rename = "srcPlex")]
    pub src_plex: bool,
    #[serde(rename = "srcLocal")]
    pub src_local: bool,
    #[serde(rename = "viewMode")]
    pub view_mode: String,
    #[serde(rename = "selectMode")]
    pub select_mode: bool,
    #[serde(rename = "selectedCount")]
    pub selected_count: i32,
    #[serde(rename = "filterCount")]
    pub filter_count: i32,
    #[serde(rename = "hasAnyFilter")]
    pub has_any_filter: bool,
    /// Sidebar visibility (the collapsible My QBZ tree's per-element flag). The
    /// hero overflow menu flips it — "Hide from sidebar" / "Show in sidebar" —
    /// which is the unhide surface for a row the sidebar dropped.
    pub hidden: bool,
    pub items: Vec<DetailRow>,
}

impl Default for DetailDoc {
    /// The §18 toolbar defaults (`list` / `position` / `asc` / `all` / no
    /// sources). `found` starts TRUE so the not-found state is only ever
    /// reached deliberately.
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: String::new(),
            kind_label: String::new(),
            name: String::new(),
            description: String::new(),
            meta: String::new(),
            item_count: 0,
            play_mode: "in_order".to_string(),
            loading: false,
            found: true,
            has_custom_cover: false,
            custom_cover_path: String::new(),
            cover_count: 0,
            cols: 2,
            hero_cell_urls: Vec::new(),
            hero_cell_paths: Vec::new(),
            urls: Vec::new(),
            search: String::new(),
            sort: "position".to_string(),
            sort_dir: "asc".to_string(),
            type_filter: "all".to_string(),
            src_qobuz: false,
            src_plex: false,
            src_local: false,
            view_mode: "list".to_string(),
            select_mode: false,
            selected_count: 0,
            filter_count: 0,
            has_any_filter: false,
            hidden: false,
            items: Vec::new(),
        }
    }
}

/// The live-resolved per-row display values `MixtapeCollectionItem` cannot
/// carry (spec 02 §6.2 `resolve_from_tracks`).
#[derive(Clone, Default)]
struct ResolvedItem {
    /// "qobuz" | "plex" | "local".
    source_kind: String,
    quality_tier: String,
    quality_detail: String,
    /// Uppercased TYPE label. For albums this is the UNTRANSLATED
    /// `classify_release_type` literal — see the module header.
    type_label: String,
    /// First resolved track's artwork with the `file://` prefix STRIPPED,
    /// backfilling rows stored with empty art.
    artwork_url: String,
    artist_id: String,
    /// The first resolved track's album id (source-aware: a Qobuz album id, or a
    /// local/media-server album key), so a TRACK row can open the album it is on.
    album_id: String,
    /// Resolved track count for an ALBUM or PLAYLIST row (`None` for a track), so
    /// the tracks column shows a real number instead of staying empty.
    tracks_count: Option<u32>,
    /// Every DISTINCT source across the item's tracks, in first-seen order: a
    /// playlist can mix Qobuz + local/media-server, so its row shows one glyph
    /// per source. An album/track resolves to a single entry.
    source_kinds: Vec<String>,
}

// ---------------------------------------------------------------------------
// State (spec 02 §6.2 "cache homes in Qt")
// ---------------------------------------------------------------------------

static DOC: Mutex<Option<DetailDoc>> = Mutex::new(None);

/// The full, ORIGINAL-order item list for the open collection — the canonical
/// source the toolbar derives the visible list from.
static FULL_ITEMS: Mutex<Vec<MixtapeCollectionItem>> = Mutex::new(Vec::new());

/// Expanded-mode inline tracks, keyed `"{source}|{source_item_id}"`. Survives
/// every re-derive so a sort/search does not re-fetch.
static INLINE_CACHE: LazyLock<Mutex<HashMap<String, Vec<InlineTrackRow>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// THE per-row accordion OPEN set, same key as `INLINE_CACHE`.
///
/// ONE notion of "is this row open", with exactly ONE writer: the chevron
/// (`toggle_row_expand`). The `expanded` segment used to write it too — entering
/// the details arm force-opened every expandable row — and the owner asked for
/// the opposite (2026-08-01): the view starts CLOSED and remembers. Surviving a
/// re-derive is deliberate: a sort or a search must not fold the rows the user
/// opened.
///
/// PERSISTED per collection in `collection_open_rows.json`
/// (`myqbz_prefs_qt::{load,save,remove}_open_rows`): seeded in `apply` from the
/// pruned stored keys, written by `persist_open_rows` on every chevron flip,
/// cleared in-memory by `reset`/`teardown` (which never touch the file).
static OPEN_ROWS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// resolveItems results, same key. A MISS is what flags a row
/// `qualityResolving` (the skeleton).
static RESOLVE_CACHE: LazyLock<Mutex<HashMap<String, ResolvedItem>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Select-mode selection, by PERSISTED position.
///
/// The Slint keeps `selected` on the render row and `to_item` hardcodes it
/// `false`, so a sort/filter/search silently clears every checkbox AND leaves
/// `selected_count` stale — a bulk action on a "3 selected" bar then does
/// nothing. Holding the set here reproduces the visible behaviour (the set is
/// cleared when a filter/sort/search INPUT changed) while keeping the count and
/// the bulk actions in agreement with what the user sees (spec 02 §12 #13).
static SELECTED: LazyLock<Mutex<HashSet<i32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Fingerprint of the last derive's filter/sort/search inputs — what decides
/// whether a rebuild is a RE-DERIVE (clears the selection) or a row patch.
static LAST_FILTER_KEY: Mutex<Option<String>> = Mutex::new(None);

/// Per-collection view-prefs hydration gate: `false` from `reset()` until
/// `apply()` has restored the stored prefs. Every persist is suppressed while
/// it is false, or an early setter clobbers the prefs about to be restored
/// (spec 02 §7 T15).
///
/// THIS IS THE ONLY COPY OF THIS LATCH. `myqbz_prefs_qt` used to declare a
/// second one and gate `save_view_prefs` on it; nothing ever opened that one, so
/// the per-collection prefs were never persisted at all. Do not re-add a gate
/// on the store side — this module owns the lifecycle (`reset` closes, `apply`
/// opens AFTER the restore, `teardown` closes).
static PREFS_HYDRATED: AtomicBool = AtomicBool::new(false);

fn with_doc<R>(f: impl FnOnce(&mut DetailDoc) -> R) -> R {
    let mut guard = DOC.lock().unwrap();
    let doc = guard.get_or_insert_with(DetailDoc::default);
    f(doc)
}

/// Serialize + hop. The two alias key pairs are filled HERE, in one place, so
/// a row patch can never leave them out of step with their canonical twin.
fn publish(doc: &DetailDoc) {
    let mut out = doc.clone();
    out.urls = out.hero_cell_urls.clone();
    for row in out.items.iter_mut() {
        row.artwork_url = row.art_url.clone();
        row.artwork_path = row.art_path.clone();
    }
    let json = serde_json::to_string(&out).unwrap_or_else(|_| "{}".into());
    crate::myqbz_bridge::ui(move |mut b| {
        b.as_mut().set_detail_json(QString::from(json.as_str()));
    });
}

fn publish_current() {
    let doc = with_doc(|d| d.clone());
    publish(&doc);
}

/// Send a batch of PARTIAL row updates over `QbzMyQbz.detailRowsPatched`
/// instead of republishing the whole document (see `RowPatch`).
///
/// A SIGNAL, not a property: cxx-qt property setters compare before emitting,
/// so two identical consecutive patches would silently drop the second, and a
/// delta must never be deduplicated. The `id` is the collection the patch was
/// computed against — QML drops a batch addressed at another document, which is
/// the same guard `apply_resolved` makes on the Rust side.
fn publish_rows(collection_id: &str, patches: Vec<RowPatch>) {
    if patches.is_empty() {
        return;
    }
    #[derive(Serialize)]
    struct Envelope<'a> {
        id: &'a str,
        rows: &'a [RowPatch],
    }
    let json = serde_json::to_string(&Envelope {
        id: collection_id,
        rows: &patches,
    })
    .unwrap_or_else(|_| "{}".into());
    crate::myqbz_bridge::ui(move |mut b| {
        b.as_mut().detail_rows_patched(QString::from(json.as_str()));
    });
}

/// Re-hand the current document to a FRESH view instance.
///
/// `AppShell.qml:192` mounts this view through a `Loader { source: ... }`, so
/// nav-away DESTROYS it and nav-back builds a new one whose only input is the
/// last published `detailJson`. Every `RowPatch` since that publish is a delta
/// the new instance never saw — resolved quality badges, loaded inline tracks
/// and open rows would all come back missing. One full publish per mount is the
/// price of the patch channel, and it is paid exactly once.
pub(crate) fn resync() {
    publish_current();
}

// ---------------------------------------------------------------------------
// String helpers (myqbz_detail.rs:177-249)
// ---------------------------------------------------------------------------

fn kind_str(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Mixtape => "mixtape",
        CollectionKind::Collection => "collection",
        CollectionKind::ArtistCollection => "artist_collection",
    }
}

/// Eyebrow label, uppercase + translated, matching the grid card.
fn kind_label(kind: CollectionKind) -> String {
    match kind {
        CollectionKind::Mixtape => qbz_i18n::t("MIXTAPE"),
        CollectionKind::ArtistCollection => qbz_i18n::t("ARTIST"),
        CollectionKind::Collection => qbz_i18n::t("COLLECTION"),
    }
}

/// Same label from the already-published `kind` string (the language-switch
/// republish has no typed `CollectionKind` to hand).
fn kind_label_from_str(kind: &str) -> String {
    match kind {
        "mixtape" => qbz_i18n::t("MIXTAPE"),
        "artist_collection" => qbz_i18n::t("ARTIST"),
        "collection" => qbz_i18n::t("COLLECTION"),
        _ => String::new(),
    }
}

fn play_mode_str(mode: CollectionPlayMode) -> &'static str {
    match mode {
        CollectionPlayMode::InOrder => "in_order",
        CollectionPlayMode::AlbumShuffle => "album_shuffle",
    }
}

pub(crate) fn source_str(source: AlbumSource) -> &'static str {
    match source {
        AlbumSource::Qobuz => "qobuz",
        AlbumSource::Local => "local",
    }
}

pub(crate) fn item_type_str(t: ItemType) -> &'static str {
    match t {
        ItemType::Album => "album",
        ItemType::Track => "track",
        ItemType::Playlist => "playlist",
    }
}

/// Always "album(s)" regardless of the item types, 1:1 with the grid meta.
fn album_count_label(count: usize) -> String {
    qbz_i18n::tf("{} album", "{} albums", count as i64, &[&count.to_string()])
}

/// TYPE cell for an UNRESOLVED row — translated, uppercase.
fn type_label(t: ItemType) -> String {
    match t {
        ItemType::Album => qbz_i18n::t("ALBUM"),
        ItemType::Track => qbz_i18n::t("TRACK"),
        ItemType::Playlist => qbz_i18n::t("PLAYLIST"),
    }
}

/// Track-count release heuristic (`qbz/src/album_map.rs:193`): `<=3` Single,
/// `<=6` EP, else Album. The reference marks these literals at the definition
/// and the MyQBZ call site does NOT translate them — reproduced verbatim.
fn classify_release_type(track_count: Option<u32>) -> &'static str {
    match track_count {
        Some(n) if n <= 3 => "Single",
        Some(n) if n <= 6 => "EP",
        _ => "Album",
    }
}

/// TRACKS column: "1" for a track, else the count or an em-dash (U+2014).
fn tracks_text(item: &MixtapeCollectionItem) -> String {
    // A track has no track count — the column stays EMPTY for it. An album or
    // playlist shows its count: the stored one if present, otherwise "" until
    // `resolve_items` backfills the resolved track count (#766).
    match item.item_type {
        ItemType::Track => String::new(),
        _ => item.track_count.map(|n| n.to_string()).unwrap_or_default(),
    }
}

fn year_text(item: &MixtapeCollectionItem) -> String {
    item.year.map(|y| y.to_string()).unwrap_or_default()
}

/// Cover rung for the GRID arm's `AlbumCard` (200px artwork well).
///
/// The owner's words are "list sí deben ser los de 50, grid 200" (2026-07-31).
/// 200 is not a rung Qobuz serves: the static CDN has a FIXED ladder —
/// 50 / 100 / 150 / 230 / 300 / 600 / max / org, which is exactly the token
/// list `myqbz_qt::small_qobuz_url` rewrites — and `_200.jpg` answers **404**
/// (probed against a live cover on 2026-08-01: 50/150/230/300/600 -> 200,
/// 200 -> 404). Emitting the literal 200 would have swapped a low-res cover
/// for NO cover.
///
/// The rung is 600, not the smallest one that covers 200 (which is 230),
/// because the owner's actual complaint was "la calidad de la portada es menor
/// que la del albumcard" and the instruction was "USAR el AlbumCard normal" —
/// and every other `AlbumCard` in the app is fed `album.image.large`, which IS
/// `_600.jpg`. 230 would still read softer than its neighbours:
/// `theme/RoundedImage.qml` decodes at native size with no `sourceSize`, so a
/// 200pt well is 400 device px on a HiDPI screen and 230 upscales ~1.7x. "200"
/// was the CARD's size, not a CDN rung.
///
/// Cost, accepted deliberately: a Qobuz row now caches `_50` (list) and `_600`
/// (grid), so a collection's first open fetches both. The owner ruled on
/// exactly this trade — "si había un motivo para hacer ligera esa vista en
/// Tauri y Slint... ya no más, Qt sí puede con esas vistas" (2026-08-01).
const GRID_ART_TARGET: u32 = 600;

/// Is this item's `source_item_id` a Qobuz CATALOG ALBUM id?
///
/// THE gate for the grid card's catalog-only affordances (heart, pin, "Block
/// this album"). Both halves matter:
///   * the stored `source` — `AlbumSource::Qobuz` is what makes the id a
///     catalog id at all (a local or Plex item's is a path or a `plex:` key);
///   * the item TYPE — a Qobuz TRACK item carries a track id and a Qobuz
///     PLAYLIST item a playlist id, and `libraryToggleFavorite("album", …)` on
///     either would heart a different entity, or none.
///
/// The RESOLVED `source_kind` is deliberately not consulted: it can read
/// "offline" for a Qobuz download, which is still a catalog album, and "plex"
/// for an item stored as Local — the stored source is the authority on what
/// the id IS.
fn is_catalog_album(item: &MixtapeCollectionItem) -> bool {
    item.source == AlbumSource::Qobuz
        && item.item_type == ItemType::Album
        && !item.source_item_id.is_empty()
}

/// Stable per-item cache key. `source_item_id` alone is the logical key;
/// pairing it with the source makes a qobuz-vs-local collision impossible.
fn cache_key(source: &str, source_item_id: &str) -> String {
    format!("{source}|{source_item_id}")
}

/// "m:ss"; a zero/missing duration renders "--:--", never "0:00".
fn track_duration_str(secs: u64) -> String {
    if secs == 0 {
        "--:--".to_string()
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

fn inline_track_title(track: &QueueTrack) -> String {
    match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title),
        None => track.title.clone(),
    }
}

/// SPEC-03: album blacklist drop inside the filter chain
/// (`myqbz_detail.rs:443` = `artist_blacklist::is_album_blacklisted`). The
/// blacklist manager is spec 03; until it lands this is the one-line swap point.
fn album_blocked(_album_id: &str) -> bool {
    false
}

// ---------------------------------------------------------------------------
// DB read path
// ---------------------------------------------------------------------------

/// Load one collection (items hydrated by the repo). `None` when the DB is
/// unavailable or the id is unknown. SYNCHRONOUS — callers wrap it in
/// `spawn_blocking`.
pub(crate) fn get_collection(id: &str) -> Option<MixtapeCollection> {
    crate::library_db_qt::with_db(false, |db| {
        Ok(db.with_connection(|conn| {
            qbz_mixtape::repo::get_collection(conn, id).unwrap_or_else(|e| {
                log::warn!("[qbz-qt] myqbz_detail get_collection({id}) failed: {e}");
                None
            })
        }))
    })
    .flatten()
}

/// Persist source-resolved cover references for legacy MyQBZ items that were
/// inserted without an artwork snapshot. One DB connection handles the whole
/// resolve wave; the repository refuses to overwrite any non-empty value.
fn backfill_resolved_artwork(
    collection_id: &str,
    repairs: &[(AlbumSource, String, String)],
) -> usize {
    crate::library_db_qt::with_db(false, |db| {
        Ok(db.with_connection(|conn| {
            repairs.iter().fold(0usize, |count, (source, id, art)| {
                match qbz_mixtape::repo::backfill_item_artwork(
                    conn,
                    collection_id,
                    source.clone(),
                    id,
                    art,
                ) {
                    Ok(changed) => count + changed,
                    Err(error) => {
                        log::warn!(
                            "[qbz-qt] MyQBZ artwork backfill failed for {collection_id}/{id}: {error}"
                        );
                        count
                    }
                }
            })
        }))
    })
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Row building
// ---------------------------------------------------------------------------

fn to_item(
    item: &MixtapeCollectionItem,
    resolved_cache: &HashMap<String, ResolvedItem>,
    inline_cache: &HashMap<String, Vec<InlineTrackRow>>,
    selected: &HashSet<i32>,
    open_rows: &HashSet<String>,
    arm_open: bool,
) -> DetailRow {
    let source = source_str(item.source);
    // `small_qobuz_url` only rewrites Qobuz CDN `_<size>.jpg` urls; running it
    // on a local filesystem path (or a Plex `/library/...` path) corrupts it.
    //
    // TWO rungs off the ONE stored url: 50 for the list row, `GRID_ART_TARGET`
    // for the 200px grid card (see `DetailRow::art_url_large`). The non-Qobuz
    // guard applies to BOTH — a local path or a Plex `/library/...` path is
    // passed through raw on both fields, which also means the two are the same
    // string there and the grid gets whatever the file is.
    let stored_art = item.artwork_url.as_deref().filter(|u| !u.is_empty());
    let rung = |target: u32| -> String {
        match stored_art {
            Some(u) if item.source == AlbumSource::Qobuz => {
                crate::myqbz_qt::small_qobuz_url(u, target)
            }
            Some(u) => u.to_string(),
            None => String::new(),
        }
    };
    let mut art_url = rung(50);
    let mut art_url_large = rung(GRID_ART_TARGET);

    let key = cache_key(source, &item.source_item_id);
    let (source_kind, quality_tier, quality_detail, type_label_text, artist_id, quality_resolving) =
        match resolved_cache.get(&key) {
            Some(r) => {
                // Backfill the row cover from the resolved track when the
                // stored `artwork_url` was empty (disco-builder local items).
                if art_url.is_empty() && !r.artwork_url.is_empty() {
                    art_url = r.artwork_url.clone();
                }
                // The backfill is a RESOLVED track's own artwork (a local file
                // path, or whatever rung the catalog handed back) — it is NOT
                // re-rung, exactly as the small field is not, so the two stay
                // the same string and the grid never asks for a size the
                // backfill source cannot serve.
                if art_url_large.is_empty() && !r.artwork_url.is_empty() {
                    art_url_large = r.artwork_url.clone();
                }
                (
                    r.source_kind.clone(),
                    r.quality_tier.clone(),
                    r.quality_detail.clone(),
                    r.type_label.clone(),
                    r.artist_id.clone(),
                    false,
                )
            }
            None => (
                source.to_string(),
                String::new(),
                String::new(),
                type_label(item.item_type),
                String::new(),
                true,
            ),
        };

    let (inline_tracks, tracks_loaded) = match inline_cache.get(&key) {
        Some(tracks) => (tracks.clone(), true),
        None => (Vec::new(), false),
    };

    // A track has nothing to show, so it never carries an open state — the
    // chevron cell stays empty for it and its row body keeps the whole gesture.
    let can_expand = matches!(item.item_type, ItemType::Album | ItemType::Playlist);

    DetailRow {
        position: item.position,
        item_type: item_type_str(item.item_type).to_string(),
        source: source.to_string(),
        source_item_id: item.source_item_id.clone(),
        title: item.title.clone(),
        subtitle: item.subtitle.clone().unwrap_or_default(),
        subtitle_is_link: item.source == AlbumSource::Qobuz
            && item
                .subtitle
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false),
        artist_id,
        album_id: String::new(),
        source_kind,
        source_kinds: Vec::new(),
        type_label: type_label_text,
        quality_tier,
        quality_detail,
        quality_resolving,
        tracks_text: tracks_text(item),
        year_text: year_text(item),
        art_path: crate::artwork_qt::cached_path(&art_url),
        art_url,
        artwork_url: String::new(),
        artwork_path: String::new(),
        art_path_large: crate::artwork_qt::cached_path(&art_url_large),
        art_url_large,
        // The catalog gate, decided ONCE here: a Qobuz ALBUM item's
        // `source_item_id` IS the catalog album id, so its heart and its pin
        // are the same entity every other AlbumCard in the app talks about. A
        // track, a playlist, a local or a Plex item's is not — those answer
        // `false` and the grid card hides both affordances rather than drawing
        // a dead one (`MyQbzDetailView.qml`'s `cCatalogAlbum`, same predicate).
        is_favorite: is_catalog_album(item)
            && crate::fav_cache_qt::is_album_favorite(&item.source_item_id),
        is_pinned: is_catalog_album(item)
            && crate::sidebar_qt::is_pinned("album", &item.source_item_id),
        selected: selected.contains(&item.position),
        can_expand,
        // Re-derives (sort / search / filter) rebuild every row, so the open
        // state has to be re-read off the set here or the accordion would snap
        // shut on the first keystroke.
        //
        // `arm_open` is the ARM gate, not a second notion of open: the chevron
        // exists in the DETAILS arm only (`MyQbzDetailView.qml` `rowExpander`),
        // so the list and grid arms must not render an inline block the user has
        // no affordance to close. The SET is the persisted truth and survives the
        // round trip untouched — which is what makes an open row still open when
        // the user comes back to details (and after a restart).
        row_open: can_expand && arm_open && open_rows.contains(&key),
        tracks_loaded,
        expand_loading: false,
        inline_tracks,
    }
}

fn build_rows(items: &[MixtapeCollectionItem], arm_open: bool) -> Vec<DetailRow> {
    let resolved = RESOLVE_CACHE.lock().unwrap();
    let inline = INLINE_CACHE.lock().unwrap();
    let selected = SELECTED.lock().unwrap();
    let open_rows = OPEN_ROWS.lock().unwrap();
    items
        .iter()
        .map(|it| to_item(it, &resolved, &inline, &selected, &open_rows, arm_open))
        .collect()
}

/// Re-derive every row's `rowOpen` from the shared set for the CURRENT arm,
/// in place — no rebuild of the model, no re-derive of the filter chain, so the
/// caches, the selection and the scroll position all survive (which is what
/// `set_view_mode` promises). The caller publishes.
///
/// This is the whole of what a view-mode switch does to the accordion now:
/// leaving the details arm hides the inline blocks (their affordance is gone),
/// coming back restores exactly what the user had open. The SET is never
/// touched here — it is the persisted truth.
fn resync_row_open(d: &mut DetailDoc) {
    let arm_open = d.view_mode == "expanded";
    let open = OPEN_ROWS.lock().unwrap();
    for row in d.items.iter_mut() {
        let want = row.can_expand
            && arm_open
            && open.contains(&cache_key(&row.source, &row.source_item_id));
        row.row_open = want;
    }
}

/// The source word a RESOLVED track renders as — the glyph kind in
/// `SourceIcon.qml` and the `sourceKind` field of a detail row.
///
/// It comes from the ROW, never from a literal. What it replaces (twice) was
///
/// ```ignore
/// track.source.clone().unwrap_or_else(|| if track.is_local { "local" } else { "qobuz" })
/// ```
///
/// — a hand-written fallback that guessed a source out of one boolean, and
/// which drops every word that is not exactly `"local"` or `"qobuz"`.
/// `SourceRegistry::claim` applies the ONE source-word table
/// (`qbz_source::SourceId::from_word`) to all six words that really occur, and
/// for a LOCAL row the BADGE keeps the distinction the glyph needs: a Qobuz
/// download is a local FILE (source `local`) but must draw the cloud glyph
/// (badge `offline`), which the source id alone collapses.
///
/// A row nothing claims (`Unclaimed` / `Ambiguous`) falls back to its badge,
/// which is `"local"` for an unknown word — never `"qobuz"`, because guessing
/// Qobuz from "not otherwise recognised" is exactly the mis-attribution the
/// seam deletes.
fn source_kind_of(track: &QueueTrack) -> &'static str {
    let raw = qbz_source::RawRef::from_queue_track(track);
    match qbz_source::registry().claim(&raw).map(|m| m.source()) {
        Ok(id) if id != qbz_source::SourceId::LOCAL => id.as_str(),
        _ => raw.badge.as_str(),
    }
}

/// One inline track row from a resolved `QueueTrack` (spec 02 §5.2).
fn track_to_item(track: &QueueTrack) -> InlineTrackRow {
    let quality_tier = match track.bit_depth {
        Some(d) if d >= 24 => "hires",
        Some(_) => "cd",
        None if track.hires => "hires",
        None => "",
    };
    let quality_detail = if quality_tier.is_empty() {
        String::new()
    } else {
        crate::home_qt::quality_detail_from_parts(track.bit_depth, track.sample_rate)
    };
    let source = source_kind_of(track).to_string();

    InlineTrackRow {
        id: track.id.to_string(),
        title: inline_track_title(track),
        artist: track.artist.clone(),
        artist_id: track.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        album: String::new(),
        album_id: track.album_id.clone().unwrap_or_default(),
        duration: track_duration_str(track.duration_secs),
        quality_tier: quality_tier.to_string(),
        quality_detail,
        explicit: track.parental_warning,
        is_favorite: false,
        art_path: String::new(),
        source,
    }
}

// ---------------------------------------------------------------------------
// Filter -> search -> sort (myqbz_detail.rs:423-496)
// ---------------------------------------------------------------------------

/// Fingerprint of every input a re-derive depends on.
fn filter_key(d: &DetailDoc) -> String {
    format!(
        "{}|{}|{}|{}|{}{}{}",
        d.search.trim().to_lowercase(),
        d.sort,
        d.sort_dir,
        d.type_filter,
        u8::from(d.src_qobuz),
        u8::from(d.src_plex),
        u8::from(d.src_local),
    )
}

fn filtered_sorted(d: &DetailDoc) -> Vec<MixtapeCollectionItem> {
    let query = d.search.trim().to_lowercase();
    let type_filter = d.type_filter.as_str();
    let (sq, sp, sl) = (d.src_qobuz, d.src_plex, d.src_local);
    let any_source = sq || sp || sl;
    let desc = d.sort_dir == "desc";

    let mut view: Vec<MixtapeCollectionItem> = {
        let items = FULL_ITEMS.lock().unwrap();
        items
            .iter()
            // Blacklist drop FIRST: album-type Qobuz items only, by their own
            // id (`source_item_id` IS the Qobuz album id).
            .filter(|it| {
                !(matches!(it.item_type, ItemType::Album)
                    && it.source == AlbumSource::Qobuz
                    && album_blocked(&it.source_item_id))
            })
            .filter(|it| type_filter == "all" || item_type_str(it.item_type) == type_filter)
            // Source filter on the STORED source, not the resolved one — 1:1.
            .filter(|it| {
                if !any_source {
                    return true;
                }
                let kind = source_str(it.source);
                (sq && kind == "qobuz") || (sp && kind == "plex") || (sl && kind == "local")
            })
            .filter(|it| {
                if query.is_empty() {
                    return true;
                }
                it.title.to_lowercase().contains(&query)
                    || it
                        .subtitle
                        .as_deref()
                        .map(|s| s.to_lowercase().contains(&query))
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    };

    match d.sort.as_str() {
        "name" => view.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "year" => view.sort_by(|a, b| a.year.unwrap_or(0).cmp(&b.year.unwrap_or(0))),
        "tracks" => {
            view.sort_by(|a, b| a.track_count.unwrap_or(0).cmp(&b.track_count.unwrap_or(0)))
        }
        _ => view.sort_by(|a, b| a.position.cmp(&b.position)),
    }
    if desc {
        view.reverse();
    }
    view
}

/// Re-derive the render model + the derived toolbar badges. Does NOT publish —
/// `apply` and `refresh_view` own that, so a load does one publish, not three.
fn rebuild(d: &mut DetailDoc) {
    let key = filter_key(d);
    let inputs_changed = {
        let mut last = LAST_FILTER_KEY.lock().unwrap();
        let changed = last.as_deref() != Some(key.as_str());
        *last = Some(key);
        changed
    };
    if inputs_changed {
        // 1:1 with the reference (a re-derive drops every checkbox); unlike the
        // reference, the COUNT below is recomputed from the same set, so the
        // bulk bar can never claim a selection the rows do not show.
        SELECTED.lock().unwrap().clear();
    }

    let view = filtered_sorted(d);
    // The rows come out of `OPEN_ROWS` — the ONLY thing that opens a row is the
    // user's chevron. Entering the details arm used to force every expandable
    // row open here (`open_all_expandable`); the owner asked for the opposite
    // (2026-08-01): the view starts fully CLOSED and remembers what he opened.
    let arm_open = d.view_mode == "expanded";
    d.items = build_rows(&view, arm_open);
    d.selected_count = SELECTED.lock().unwrap().len() as i32;

    let source_count = i32::from(d.src_qobuz) + i32::from(d.src_plex) + i32::from(d.src_local);
    d.filter_count = source_count + i32::from(d.type_filter != "all");
    d.has_any_filter =
        d.type_filter != "all" || source_count > 0 || d.sort != "position" || d.sort_dir == "desc";
}

/// Re-derive, resolve the newly-visible rows' artwork, publish, and — while in
/// expanded view-mode — fetch the inline tracks of the rows the re-derive just
/// revealed.
///
/// The artwork pass belongs here as well as in `load`: the restored view-prefs
/// may have filtered rows OUT on the initial load, so a toolbar change can
/// reveal a row whose cover was never fetched. This is the reference's
/// `refresh_row_covers` (`myqbz_detail.rs:1294` names it as the second caller of
/// the artwork dispatch) — without it those rows keep the placeholder until the
/// collection is re-opened.
///
/// The `ensure_expanded` tail is the reference's `ensure_expanded_if_active`
/// (`qbz/src/main.rs:6153-6161`), which it calls after ALL FIVE toolbar setters
/// (search, sort, type filter, source filter, reset). A row that was filtered
/// out has never been fetched, so without this it appears in expanded mode with
/// no inline tracks and no spinner until the user toggles the mode again.
/// Idempotent — rows already loaded or loading are skipped, and a row cached in
/// `INLINE_CACHE` re-hydrates in `to_item` rather than re-fetching. `load` calls
/// it once after `apply` for the same reason, since the RESTORED open set can
/// carry rows whose tracks this session has never fetched; that is not the old
/// auto-expand, which opened rows the user never opened.
pub(crate) fn refresh_view() {
    with_doc(rebuild);
    let missing = attach_artwork();
    publish_current();
    dispatch_downloads(missing);
    // Unconditional now that `ensure_expanded` keys off the per-row OPEN state
    // rather than the view mode: with nothing open it is one pass over the rows
    // and no fetch. It matters for a row the user opened, then filtered out and
    // back in — the re-derive rebuilt it from `OPEN_ROWS` as open, but its
    // in-flight spinner died with the old row object and its tracks must be
    // re-fetched.
    ensure_expanded();
}

// ---------------------------------------------------------------------------
// Hero mosaic (myqbz_detail.rs:359-416)
// ---------------------------------------------------------------------------

/// The hero's 0/4/9-cell mosaic at hero size 186 (cell ~93 for 2x2 -> `_150`,
/// ~62 for 3x3 -> `_50`).
///
/// DIVERGENCE, reproduced: `has_custom` here is a bare `is_some()` — no
/// `is_empty()` filter, no `exists()` check, unlike the grid card. Do not
/// unify the two predicates (spec 02 §6.2 / review item 23).
fn apply_hero_mosaic(d: &mut DetailDoc, c: &MixtapeCollection) {
    let item_count = c.items.len();
    let has_custom = c.custom_artwork_path.is_some();

    let cols: usize = if c.kind == CollectionKind::Collection && item_count >= 9 {
        3
    } else {
        2
    };
    let cell_count = cols * cols;
    let cover_count = if has_custom || item_count == 0 {
        0
    } else {
        cell_count
    };
    let target: u32 = if cols == 3 { 50 } else { 150 };

    let mut urls: Vec<String> = Vec::with_capacity(cover_count);
    for i in 0..cover_count {
        let url = match c.items.get(i) {
            Some(it) => match it.artwork_url.as_deref() {
                // Qobuz cells only — local/Plex paths pass through raw.
                Some(u) if !u.is_empty() && it.source == AlbumSource::Qobuz => {
                    crate::myqbz_qt::small_qobuz_url(u, target)
                }
                Some(u) if !u.is_empty() => u.to_string(),
                _ => String::new(),
            },
            None => String::new(),
        };
        urls.push(url);
    }

    d.cols = cols as i32;
    d.cover_count = cover_count as i32;
    // Reset the resolved cells so a re-open cannot show a stale tile.
    d.hero_cell_paths = vec![String::new(); urls.len()];
    d.hero_cell_urls = urls;
}

// ---------------------------------------------------------------------------
// Artwork pass (ONE cached_path sweep, ONE download, ONE republish — T17)
// ---------------------------------------------------------------------------

/// Resolve every hero cell + row cover through the disk cache, returning the
/// urls still missing (deduped, non-empty).
fn attach_artwork() -> Vec<String> {
    if crate::kiosk_profile_qt::active() {
        return Vec::new();
    }
    with_doc(|d| {
        // The custom cover was already resolved through `cached_path` in
        // `apply` (it is a local file, never a download).
        if d.hero_cell_paths.len() != d.hero_cell_urls.len() {
            d.hero_cell_paths = vec![String::new(); d.hero_cell_urls.len()];
        }
        let mut missing: Vec<String> = Vec::new();
        let push = |url: &str, missing: &mut Vec<String>| {
            if !url.is_empty() && !missing.iter().any(|u| u == url) {
                missing.push(url.to_string());
            }
        };
        for i in 0..d.hero_cell_urls.len() {
            let url = d.hero_cell_urls[i].clone();
            if url.is_empty() {
                continue;
            }
            let hit = crate::artwork_qt::cached_path(&url);
            if hit.is_empty() {
                push(&url, &mut missing);
            } else {
                d.hero_cell_paths[i] = hit;
            }
        }
        for row in d.items.iter_mut() {
            // BOTH rungs, or the grid asks for a file nothing ever fetches:
            // `artUrlLarge` is a DIFFERENT url from `artUrl` (`_230` vs `_50`)
            // and therefore a different disk-cache entry, so a sweep that only
            // knew about the small one left every grid cell empty.
            if !row.art_url.is_empty() && row.art_path.is_empty() {
                let hit = crate::artwork_qt::cached_path(&row.art_url);
                if hit.is_empty() {
                    let url = row.art_url.clone();
                    push(&url, &mut missing);
                } else {
                    row.art_path = hit;
                }
            }
            if !row.art_url_large.is_empty() && row.art_path_large.is_empty() {
                let hit = crate::artwork_qt::cached_path(&row.art_url_large);
                if hit.is_empty() {
                    let url = row.art_url_large.clone();
                    push(&url, &mut missing);
                } else {
                    row.art_path_large = hit;
                }
            }
        }
        missing
    })
}

pub(crate) fn kiosk_detail_artwork(first: i32, last: i32, px: i32) {
    if !crate::kiosk_profile_qt::active() || first < 0 || last < first {
        return;
    }
    let (id, jobs) = with_doc(|d| {
        (
            d.id.clone(),
            d.items
                .iter()
                .skip(first as usize)
                .take((last - first + 1).min(128) as usize)
                .filter(|r| !r.art_url.is_empty())
                .map(|r| {
                    (
                        r.position,
                        r.source_item_id.clone(),
                        crate::myqbz_qt::kiosk_art_url(&r.art_url, px),
                    )
                })
                .collect::<Vec<_>>(),
        )
    });
    crate::spawn(async move {
        crate::artwork_qt::download_missing(jobs.iter().map(|j| j.2.clone()).collect()).await;
        if !crate::kiosk_profile_qt::active() {
            return;
        }
        let doc = with_doc(|d| {
            if d.id != id {
                return None;
            }
            let mut changed = false;
            for (position, item_id, url) in jobs {
                if let Some(row) = d
                    .items
                    .iter_mut()
                    .find(|r| r.position == position && r.source_item_id == item_id)
                {
                    let path = crate::artwork_qt::cached_path(&url);
                    if !path.is_empty() && row.art_path != path {
                        row.art_path = path;
                        changed = true;
                    }
                }
            }
            changed.then(|| d.clone())
        });
        if let Some(doc) = doc {
            publish(&doc);
        }
    });
}

/// ONE background download pass, then ONE republish (T17 — never republish per
/// cover). No-op on an empty list.
fn dispatch_downloads(missing: Vec<String>) {
    if missing.is_empty() {
        return;
    }
    crate::spawn(async move {
        crate::artwork_qt::download_missing(missing).await;
        refresh_artwork_paths();
    });
}

/// Re-resolve whatever is still unresolved after a download, then publish.
fn refresh_artwork_paths() {
    let doc = with_doc(|d| {
        if d.hero_cell_paths.len() != d.hero_cell_urls.len() {
            d.hero_cell_paths = vec![String::new(); d.hero_cell_urls.len()];
        }
        for i in 0..d.hero_cell_urls.len() {
            if d.hero_cell_paths[i].is_empty() {
                let url = d.hero_cell_urls[i].clone();
                d.hero_cell_paths[i] = crate::artwork_qt::cached_path(&url);
            }
        }
        for row in d.items.iter_mut() {
            if row.art_path.is_empty() {
                row.art_path = crate::artwork_qt::cached_path(&row.art_url);
            }
            // The grid rung is its own cache entry — see `attach_artwork`.
            if row.art_path_large.is_empty() {
                row.art_path_large = crate::artwork_qt::cached_path(&row.art_url_large);
            }
        }
        d.clone()
    });
    publish(&doc);
}

// ---------------------------------------------------------------------------
// View-prefs bridge (spec 02 §12 #11 — the file is SHARED with the Slint build)
// ---------------------------------------------------------------------------
//
// The store lives in `myqbz_prefs_qt` (`collection_view_prefs.json`). The two
// calls are isolated here so the coupling to that module's naming is in ONE
// place.

fn restore_prefs(d: &mut DetailDoc, id: &str) {
    let prefs = crate::myqbz_prefs_qt::load_view_prefs(id);
    d.view_mode = prefs.view_mode;
    d.sort = prefs.sort_by;
    d.sort_dir = prefs.sort_dir;
    d.type_filter = prefs.type_filter;
    d.src_qobuz = prefs.src_qobuz;
    d.src_plex = prefs.src_plex;
    d.src_local = prefs.src_local;
}

/// Persist the open collection's toolbar prefs — gated on the hydration flag,
/// no-op on an empty id. The 7 §18 fields only; search + select-mode stay
/// transient.
fn persist_prefs() {
    if !PREFS_HYDRATED.load(Ordering::SeqCst) {
        return;
    }
    let payload = with_doc(|d| {
        if d.id.is_empty() {
            return None;
        }
        Some((
            d.id.clone(),
            crate::myqbz_prefs_qt::Prefs {
                view_mode: d.view_mode.clone(),
                sort_by: d.sort.clone(),
                sort_dir: d.sort_dir.clone(),
                type_filter: d.type_filter.clone(),
                src_qobuz: d.src_qobuz,
                src_plex: d.src_plex,
                src_local: d.src_local,
            },
        ))
    });
    if let Some((id, prefs)) = payload {
        crate::myqbz_prefs_qt::save_view_prefs(&id, &prefs);
    }
}

/// Persist the accordion's OPEN set for the collection on screen.
///
/// Called from `toggle_row_expand`, which runs on the GUI thread (a QML
/// invokable), so the write is pushed onto `spawn_blocking` — every disk helper
/// in this port is blocking and none of them may sit on the Qt event loop.
///
/// The snapshot is taken INSIDE the blocking task, not at call time: two fast
/// chevron clicks spawn two tasks that the runtime may run in either order, and
/// a task that carried its own older snapshot could then land last and persist a
/// stale set. Reading the live set in the task makes both writes converge on
/// whatever the current state is. The collection id is re-checked there too — a
/// task queued while the user navigated away must not write another
/// collection's file.
///
/// Local UI state end to end: no network verb, no Qobuz session, works offline
/// and on a machine that has never logged in (the store degrades to a no-op when
/// there is no per-user directory, exactly like the view prefs).
fn persist_open_rows() {
    let collection_id = current_id();
    if collection_id.is_empty() {
        return;
    }
    crate::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            if current_id() != collection_id {
                return;
            }
            let keys: Vec<String> = OPEN_ROWS.lock().unwrap().iter().cloned().collect();
            crate::myqbz_prefs_qt::save_open_rows(&collection_id, &keys);
        })
        .await;
    });
}

// ---------------------------------------------------------------------------
// reset / apply
// ---------------------------------------------------------------------------

/// Clear the view to its loading state before a fresh load, so a re-open does
/// not flash the previous collection's hero + rows.
fn reset() {
    FULL_ITEMS.lock().unwrap().clear();
    INLINE_CACHE.lock().unwrap().clear();
    RESOLVE_CACHE.lock().unwrap().clear();
    // In-memory only — the on-disk set for this collection is untouched, and
    // `apply` seeds this one back from it.
    OPEN_ROWS.lock().unwrap().clear();
    SELECTED.lock().unwrap().clear();
    *LAST_FILTER_KEY.lock().unwrap() = None;
    // Close the persist gate until `apply` restores this collection's prefs.
    PREFS_HYDRATED.store(false, Ordering::SeqCst);
    let doc = with_doc(|d| {
        *d = DetailDoc {
            loading: true,
            found: true,
            ..DetailDoc::default()
        };
        d.clone()
    });
    publish(&doc);
}

/// Apply a freshly-loaded collection. The ORDER is load-bearing: the persist
/// gate opens AFTER the restore, so the restore itself is not re-persisted.
///
/// `stored_open` is this collection's persisted accordion set, read off disk by
/// the caller's `spawn_blocking` (never here — `apply` runs inline).
fn apply(c: MixtapeCollection, stored_open: Vec<String>) {
    with_doc(|d| {
        let item_count = c.items.len();
        d.id = c.id.clone();
        d.kind = kind_str(c.kind).to_string();
        d.kind_label = kind_label(c.kind);
        d.name = c.name.clone();
        d.description = c.description.clone().unwrap_or_default();
        d.meta = album_count_label(item_count);
        d.item_count = item_count as i32;
        d.play_mode = play_mode_str(c.play_mode).to_string();
        d.hidden = c.hidden;
        d.found = true;

        // Header custom cover: non-empty AND resolvable. `cached_path` returns
        // "" for a path that is not a readable file, which folds the
        // reference's `exists()` + decodable pair into one call.
        let resolved_cover = c
            .custom_artwork_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(crate::artwork_qt::cached_path)
            .unwrap_or_default();
        d.has_custom_cover = !resolved_cover.is_empty();
        d.custom_cover_path = resolved_cover;

        apply_hero_mosaic(d, &c);

        restore_prefs(d, &c.id);
        PREFS_HYDRATED.store(true, Ordering::SeqCst);

        // Restore the accordion, PRUNED to the rows that still exist and can
        // actually expand. A key whose item was removed from the collection (or
        // whose type changed) would otherwise sit in the file forever and, worse,
        // could re-open a DIFFERENT row if that id came back under another type.
        // The pruned set is what the next chevron click persists, so the file
        // self-heals without a write on every open.
        {
            let live: HashSet<String> = c
                .items
                .iter()
                .filter(|it| matches!(it.item_type, ItemType::Album | ItemType::Playlist))
                .map(|it| cache_key(source_str(it.source), &it.source_item_id))
                .collect();
            let mut open = OPEN_ROWS.lock().unwrap();
            open.clear();
            open.extend(stored_open.into_iter().filter(|k| live.contains(k)));
        }

        *FULL_ITEMS.lock().unwrap() = c.items;
        rebuild(d);
        d.loading = false;
    });
}

/// Mark the load as not-found (the id resolved to no collection).
fn apply_not_found() {
    let doc = with_doc(|d| {
        d.loading = false;
        d.found = false;
        d.clone()
    });
    publish(&doc);
}

// ---------------------------------------------------------------------------
// resolveItems (spec 02 §6.2, capped at MAX_RESOLVE_CONCURRENCY)
// ---------------------------------------------------------------------------

/// Derive a row's resolved display values from its resolved tracks. Album-level
/// quality = the first resolved track's quality (24-bit+ = Hi-Res), the same
/// rule every other surface uses.
fn resolve_from_tracks(item: &MixtapeCollectionItem, tracks: &[QueueTrack]) -> ResolvedItem {
    let stored = source_str(item.source);
    let first = tracks.first();

    // The RESOLVED source, from the row itself (`source_kind_of`). An item that
    // resolved to nothing keeps its STORED source — there is no row to ask.
    let source_kind = match first {
        Some(t) => source_kind_of(t).to_string(),
        None => stored.to_string(),
    };

    let quality_tier = match first {
        Some(t) => match t.bit_depth {
            Some(d) if d >= 24 => "hires",
            Some(_) => "cd",
            None if t.hires => "hires",
            None => "",
        },
        None => "",
    };
    let quality_detail = match (first, quality_tier.is_empty()) {
        (Some(t), false) => crate::home_qt::quality_detail_from_parts(t.bit_depth, t.sample_rate),
        _ => String::new(),
    };

    // Albums resolve their release type from the resolved track count; the
    // literal is NOT translated here — see the module header.
    let type_label_text = match item.item_type {
        ItemType::Album => classify_release_type(Some(tracks.len() as u32)).to_uppercase(),
        other => type_label(other),
    };

    // First resolved track's artwork, with the `file://` prefix STRIPPED: the
    // artwork channel reads a bare filesystem path.
    let artwork_url = first
        .and_then(|t| t.artwork_url.clone())
        // NOT strip_prefix: the artwork channel wants a bare filesystem path,
        // and on Windows a real file:// URL strips to `/C:/Users/...`.
        .map(|u| qbz_models::fs_url::local_path(&u))
        .unwrap_or_default();

    let artist_id = first
        .and_then(|t| t.artist_id)
        .map(|id| id.to_string())
        .unwrap_or_default();

    // The first resolved track's album id, so a TRACK row opens its album.
    // Whatever the source hands back (a Qobuz album id, a media-server album
    // key) — `crate::open_album` already routes Qobuz-vs-local by its shape.
    let album_id = first.and_then(|t| t.album_id.clone()).unwrap_or_default();

    // Track count for the tracks column — a track carries none; an album or
    // playlist reports how many tracks resolved.
    let tracks_count = match item.item_type {
        ItemType::Track => None,
        _ => Some(tracks.len() as u32),
    };

    // Every distinct source across the tracks (a playlist can mix them).
    let mut source_kinds: Vec<String> = Vec::new();
    for track in tracks {
        let sk = source_kind_of(track).to_string();
        if !source_kinds.contains(&sk) {
            source_kinds.push(sk);
        }
    }

    ResolvedItem {
        source_kind,
        quality_tier: quality_tier.to_string(),
        quality_detail,
        type_label: type_label_text,
        artwork_url,
        artist_id,
        album_id,
        tracks_count,
        source_kinds,
    }
}

/// Resolve a cached Qobuz item's badge from the local offline index. This is
/// the offline equivalent of `resolve_from_tracks`; Qobuz playlist membership
/// cannot be derived locally and deliberately returns `None`.
async fn resolve_offline_cached(item: &MixtapeCollectionItem) -> Option<ResolvedItem> {
    use qbz_offline_cache::OfflineCacheStatus;

    if item.source != AlbumSource::Qobuz {
        return None;
    }
    let offline = crate::offline_qt::get().await?;
    let guard = offline.db.lock().await;
    let db = guard.as_ref()?;

    let (info, ready_count) = match item.item_type {
        ItemType::Album => {
            let ready: Vec<qbz_offline_cache::CachedTrackInfo> = db
                .get_album_tracks(&item.source_item_id)
                .ok()?
                .into_iter()
                .filter(|track| matches!(track.status, OfflineCacheStatus::Ready))
                .collect();
            let count = ready.len();
            (ready.into_iter().next()?, count)
        }
        ItemType::Track => {
            let id = item.source_item_id.parse::<u64>().ok()?;
            let info = db.get_track(id).ok().flatten()?;
            if !matches!(info.status, OfflineCacheStatus::Ready) {
                return None;
            }
            (info, 1)
        }
        ItemType::Playlist => return None,
    };

    let quality_tier = match info.bit_depth {
        Some(depth) if depth >= 24 => "hires",
        Some(_) => "cd",
        None if info.quality.to_ascii_lowercase().contains("hires") => "hires",
        None => "",
    };
    let quality_detail = if quality_tier.is_empty() {
        String::new()
    } else {
        crate::home_qt::quality_detail_from_parts(info.bit_depth, info.sample_rate)
    };
    let type_label_text = match item.item_type {
        ItemType::Album => {
            let count = item
                .track_count
                .filter(|count| *count > 0)
                .map(|count| count as u32)
                .unwrap_or(ready_count as u32);
            classify_release_type(Some(count)).to_uppercase()
        }
        other => type_label(other),
    };

    Some(ResolvedItem {
        source_kind: "qobuz".to_string(),
        quality_tier: quality_tier.to_string(),
        quality_detail,
        type_label: type_label_text,
        artwork_url: String::new(),
        artist_id: String::new(),
        album_id: String::new(),
        tracks_count: None,
        source_kinds: vec!["qobuz".to_string()],
    })
}

/// Insert into `RESOLVE_CACHE` and patch the RENDERED row IN PLACE. A full
/// array replacement would reset `ListView.contentY` and throw the user to the
/// top (spec 02 §9.1) — and this is not a re-derive, so it must not clear the
/// selection either.
///
/// The patch now travels as a `RowPatch` over `detailRowsPatched` instead of a
/// clone + full serialisation of the document per resolved item (audit #7): the
/// document stays authoritative in `DOC`, only the changed fields cross the
/// hop. Rows are matched with `filter`, not `find`, so a collection holding the
/// same album twice hydrates both copies rather than only the first.
fn apply_resolved(item: &MixtapeCollectionItem, resolved: ResolvedItem, collection_id: &str) {
    let key = cache_key(source_str(item.source), &item.source_item_id);
    RESOLVE_CACHE.lock().unwrap().insert(key, resolved.clone());

    let stored_art_empty = item
        .artwork_url
        .as_deref()
        .map(|u| u.is_empty())
        .unwrap_or(true);

    let (patches, backfill) = with_doc(|d| {
        let mut patches: Vec<RowPatch> = Vec::new();
        if d.id != collection_id {
            return (patches, None);
        }
        let mut backfill: Option<String> = None;
        for row in d
            .items
            .iter_mut()
            .filter(|r| r.source_item_id == item.source_item_id)
        {
            row.source_kind = resolved.source_kind.clone();
            row.source_kinds = resolved.source_kinds.clone();
            row.quality_tier = resolved.quality_tier.clone();
            row.quality_detail = resolved.quality_detail.clone();
            row.type_label = resolved.type_label.clone();
            row.artist_id = resolved.artist_id.clone();
            row.album_id = resolved.album_id.clone();
            // Album/playlist tracks column: show the resolved count (a track keeps
            // its empty cell).
            if !matches!(item.item_type, ItemType::Track) {
                if let Some(n) = resolved.tracks_count {
                    row.tracks_text = n.to_string();
                }
            }
            row.quality_resolving = false;

            let mut patch = RowPatch::of(row);
            patch.source_kind = Some(row.source_kind.clone());
            patch.source_kinds = Some(row.source_kinds.clone());
            patch.quality_tier = Some(row.quality_tier.clone());
            patch.quality_detail = Some(row.quality_detail.clone());
            patch.type_label = Some(row.type_label.clone());
            patch.artist_id = Some(row.artist_id.clone());
            patch.album_id = Some(row.album_id.clone());
            patch.tracks_text = Some(row.tracks_text.clone());
            patch.quality_resolving = Some(false);

            if row.art_url.is_empty() && !resolved.artwork_url.is_empty() {
                row.art_url = resolved.artwork_url.clone();
                row.art_path = crate::artwork_qt::cached_path(&row.art_url);
                if row.art_path.is_empty() && stored_art_empty && backfill.is_none() {
                    backfill = Some(row.art_url.clone());
                }
                // Both spellings, exactly as `publish` fills them.
                patch.art_url = Some(row.art_url.clone());
                patch.art_path = Some(row.art_path.clone());
                patch.artwork_url = Some(row.art_url.clone());
                patch.artwork_path = Some(row.art_path.clone());
            }
            // The GRID rung, backfilled from the SAME resolved url and NOT
            // re-rung (`to_item` does the same) — a resolved track's artwork is
            // whatever its source hands back, and asking a local file for a
            // `_230` variant is meaningless. Its own `if`, not the small one's
            // branch: the two fields are only ever equal by accident and a row
            // can perfectly well have one filled and the other empty.
            if row.art_url_large.is_empty() && !resolved.artwork_url.is_empty() {
                row.art_url_large = resolved.artwork_url.clone();
                row.art_path_large = crate::artwork_qt::cached_path(&row.art_url_large);
                if row.art_path_large.is_empty() && stored_art_empty && backfill.is_none() {
                    backfill = Some(row.art_url_large.clone());
                }
                patch.art_url_large = Some(row.art_url_large.clone());
                patch.art_path_large = Some(row.art_path_large.clone());
            }
            patches.push(patch);
        }
        (patches, backfill)
    });
    publish_rows(collection_id, patches);
    if let Some(url) = backfill.filter(|_| !crate::kiosk_profile_qt::active()) {
        crate::spawn(async move {
            crate::artwork_qt::download_missing(vec![url]).await;
            refresh_artwork_paths();
        });
    }
}

/// Resolve every not-yet-cached item's tracks and hydrate its row.
/// Fire-and-forget: a failure leaves the stored-source defaults in place.
///
/// Since the `qbz-source` wiring, this pass NO LONGER NEEDS A QOBUZ CLIENT:
/// `fetch_item_tracks` used to return early with an empty vec when there was
/// none, so a local or Plex row rendered a grey square, an empty quality
/// skeleton and no inline tracks for the whole session whenever it was offline
/// or logged out. Each item now resolves against its own source, and only the
/// Qobuz ones fail when the catalog is unreachable. `runtime` survives as the
/// per-task handle the resolve tasks are spawned with; the resolver itself is
/// the process-wide registry.
fn resolve_items(runtime: Arc<AppRuntime<LoggingAdapter>>) {
    let pending: Vec<MixtapeCollectionItem> = {
        let items = FULL_ITEMS.lock().unwrap();
        let cache = RESOLVE_CACHE.lock().unwrap();
        items
            .iter()
            .filter(|it| !cache.contains_key(&cache_key(source_str(it.source), &it.source_item_id)))
            .cloned()
            .collect()
    };
    if pending.is_empty() {
        return;
    }
    let collection_id = current_id();
    if collection_id.is_empty() {
        return;
    }
    crate::spawn(async move {
        let semaphore = Arc::new(Semaphore::new(MAX_RESOLVE_CONCURRENCY));
        let offline = crate::offline_fwd::engine().status().is_offline();
        let mut handles = Vec::with_capacity(pending.len());
        for item in pending {
            let sem = Arc::clone(&semaphore);
            let runtime = Arc::clone(&runtime);
            let collection_id = collection_id.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore open");
                // The user may have navigated to another collection while this
                // task queued behind the cap — its result is then dead weight.
                if current_id() != collection_id {
                    return None;
                }
                let resolved = if offline {
                    resolve_offline_cached(&item).await
                } else {
                    None
                };
                let resolved = match resolved {
                    Some(resolved) => resolved,
                    None => {
                        let tracks = crate::myqbz_play_qt::fetch_item_tracks(&runtime, &item).await;
                        resolve_from_tracks(&item, &tracks)
                    }
                };
                let repair = item
                    .artwork_url
                    .as_deref()
                    .is_none_or(str::is_empty)
                    .then(|| resolved.artwork_url.clone())
                    .filter(|art| !art.is_empty())
                    .map(|art| (item.source.clone(), item.source_item_id.clone(), art));
                apply_resolved(&item, resolved, &collection_id);
                repair
            }));
        }
        let mut repairs = Vec::new();
        for handle in handles {
            if let Ok(Some(repair)) = handle.await {
                repairs.push(repair);
            }
        }
        if !repairs.is_empty() {
            let id = collection_id.clone();
            let repaired =
                tokio::task::spawn_blocking(move || backfill_resolved_artwork(&id, &repairs))
                    .await
                    .unwrap_or_default();
            if repaired > 0 {
                // The index cards keep item snapshots in their mosaic cache.
                // One reload after the whole resolve wave makes the repair
                // visible when Back returns there; never one reload per item.
                crate::myqbz_qt::reload_grids();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Expanded mode (spec 02 §6.2 ensure_expanded)
// ---------------------------------------------------------------------------

fn full_item_by_source_id(source_item_id: &str) -> Option<MixtapeCollectionItem> {
    FULL_ITEMS
        .lock()
        .unwrap()
        .iter()
        .find(|it| it.source_item_id == source_item_id)
        .cloned()
}

/// Ensure every OPEN row's inline tracks are loaded. Marks the pending rows
/// loading in ONE pass, sends that as a row patch, then resolves each.
/// Idempotent — already-cached rows are skipped, so re-entering expanded mode
/// is instant.
///
/// The gate is `row_open`, not `can_expand`: with the accordion, "expandable"
/// is no longer "will be shown". Nothing force-opens rows any more, so this
/// fetches only what the USER opened — that is the whole of what makes the
/// chevron lazy, and it is why entering the details arm on a 200-item
/// collection now costs zero requests. `toggle_row_expand` fetches its own
/// single row directly rather than routing through here, so one chevron never
/// sweeps the list.
///
/// DEVIATION (reported): the fan-out is capped at `MAX_RESOLVE_CONCURRENCY`,
/// the same cap T18 mandates for `resolve_items`. The reference is uncapped and
/// an expanded 200-item artist_collection would open 200 sockets; the visible
/// behaviour (rows filling in progressively) is identical.
pub(crate) fn ensure_expanded() {
    let (collection_id, pending, patches) = with_doc(|d| {
        let mut pending: Vec<String> = Vec::new();
        let mut patches: Vec<RowPatch> = Vec::new();
        for row in d.items.iter_mut() {
            if row.row_open && !row.tracks_loaded && !row.expand_loading {
                row.expand_loading = true;
                let mut patch = RowPatch::of(row);
                patch.expand_loading = Some(true);
                patches.push(patch);
                pending.push(row.source_item_id.clone());
            }
        }
        (d.id.clone(), pending, patches)
    });
    if pending.is_empty() {
        return;
    }
    publish_rows(&collection_id, patches);

    let runtime = crate::app();
    crate::spawn(async move {
        let semaphore = Arc::new(Semaphore::new(MAX_RESOLVE_CONCURRENCY));
        let mut handles = Vec::with_capacity(pending.len());
        for source_item_id in pending {
            let sem = Arc::clone(&semaphore);
            let runtime = Arc::clone(&runtime);
            let collection_id = collection_id.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore open");
                fetch_row_tracks(&runtime, source_item_id, collection_id).await;
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
    });
}

/// Fetch ONE row's tracks, cache them, and patch that row. The single body
/// behind both entry points — `ensure_expanded`'s capped fan-out and the
/// chevron's `toggle_row_expand` — so the two can never drift.
///
/// Re-checks the open collection first: the user may have navigated away while
/// this task queued behind the concurrency cap, and the result is then dead
/// weight (`patch_expanded` drops it a second time on the same test).
async fn fetch_row_tracks(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    source_item_id: String,
    collection_id: String,
) {
    if current_id() != collection_id {
        return;
    }
    let Some(item) = full_item_by_source_id(&source_item_id) else {
        // No backing item (should not happen) — clear the spinner.
        patch_expanded(&source_item_id, &collection_id, None);
        return;
    };
    let tracks = crate::myqbz_play_qt::fetch_item_tracks(runtime, &item).await;
    let rows: Vec<InlineTrackRow> = tracks.iter().map(track_to_item).collect();
    let key = cache_key(source_str(item.source), &item.source_item_id);
    INLINE_CACHE.lock().unwrap().insert(key, rows.clone());
    patch_expanded(&source_item_id, &collection_id, Some(rows));
}

/// Patch ONE row's expansion state in place (never a re-derive).
///
/// Sends a `RowPatch`, not the document (audit #8): the document carries EVERY
/// loaded row's `inlineTracks`, so republishing it per expanded item made
/// entering expanded mode on a 200-item collection quadratic — item 200's
/// publish serialised ~3000 track objects that QML then re-parsed.
fn patch_expanded(source_item_id: &str, collection_id: &str, rows: Option<Vec<InlineTrackRow>>) {
    let patches = with_doc(|d| {
        let mut patches: Vec<RowPatch> = Vec::new();
        if d.id != collection_id {
            return patches;
        }
        for row in d
            .items
            .iter_mut()
            .filter(|r| r.source_item_id == source_item_id)
        {
            row.expand_loading = false;
            let mut patch = RowPatch::of(row);
            patch.expand_loading = Some(false);
            if let Some(ref rows) = rows {
                row.tracks_loaded = true;
                row.inline_tracks = rows.clone();
                patch.tracks_loaded = Some(true);
                patch.inline_tracks = Some(rows.clone());
            }
            patches.push(patch);
        }
        patches
    });
    publish_rows(collection_id, patches);
}

/// THE CHEVRON — toggle ONE row's inline block (the feature the owner asked
/// for: "una fila colapsable con el contenido dentro").
///
/// Opening fetches only THIS row's tracks, and only the first time: a second
/// open is a pure state flip because `INLINE_CACHE` survives, `to_item`
/// re-hydrates `inlineTracks` from it, and `tracks_loaded` short-circuits the
/// fetch. Closing never drops the cache.
///
/// It is the ONLY writer of `OPEN_ROWS` — the view-mode segment stopped writing
/// it (see `set_view_mode`), so "open" means exactly "the user opened this row".
/// The view mode itself is NOT touched either: a chevron does not flip the
/// segment, and closing every row by hand does not leave the segment stranded.
///
/// Every flip is PERSISTED (`persist_open_rows`), which is what makes the
/// accordion survive a view switch, a re-open and a restart.
pub(crate) fn toggle_row_expand(source_item_id: &str) {
    let mut fetch = false;
    let (collection_id, patches) = with_doc(|d| {
        let mut patches: Vec<RowPatch> = Vec::new();
        let id = d.id.clone();
        // The affordance lives in the details arm only, and so does the state
        // it writes: opening a row from anywhere else would render an inline
        // block with no chevron to close it. The QML never offers the gesture
        // there — this is the invariant enforced where the state lives.
        if d.view_mode != "expanded" {
            return (id, patches);
        }
        let Some(row) = d
            .items
            .iter_mut()
            .find(|r| r.source_item_id == source_item_id)
        else {
            return (id, patches);
        };
        if !row.can_expand {
            return (id, patches);
        }
        let now_open = !row.row_open;
        {
            let key = cache_key(&row.source, &row.source_item_id);
            let mut open = OPEN_ROWS.lock().unwrap();
            if now_open {
                open.insert(key);
            } else {
                open.remove(&key);
            }
        }
        row.row_open = now_open;

        let mut patch = RowPatch::of(row);
        patch.row_open = Some(now_open);
        if now_open && !row.tracks_loaded && !row.expand_loading {
            row.expand_loading = true;
            patch.expand_loading = Some(true);
            fetch = true;
        }
        patches.push(patch);
        (id, patches)
    });
    // Exactly one patch is pushed, and only on the path that actually flipped a
    // row — every early return above (wrong arm, no such row, not expandable)
    // leaves `patches` empty. So this is the "did the set change" flag, and it
    // keeps a click that changed nothing from spending a read-modify-write on
    // the sidecar.
    let flipped = !patches.is_empty();
    publish_rows(&collection_id, patches);
    if flipped {
        persist_open_rows();
    }
    if !fetch || collection_id.is_empty() {
        return;
    }
    let source_item_id = source_item_id.to_string();
    let runtime = crate::app();
    crate::spawn(async move {
        fetch_row_tracks(&runtime, source_item_id, collection_id).await;
    });
}

// ---------------------------------------------------------------------------
// Toolbar (spec 02 §6.2 :499-562)
// ---------------------------------------------------------------------------

/// Transient — the ONLY setter that does not persist.
pub(crate) fn search(query: &str) {
    with_doc(|d| d.search = query.to_string());
    refresh_view();
}

/// Re-selecting the active field flips asc/desc; a new field resets to asc.
pub(crate) fn set_sort(field: &str) {
    with_doc(|d| {
        if d.sort == field {
            d.sort_dir = if d.sort_dir == "asc" { "desc" } else { "asc" }.to_string();
        } else {
            d.sort = field.to_string();
            d.sort_dir = "asc".to_string();
        }
    });
    persist_prefs();
    refresh_view();
}

pub(crate) fn set_type_filter(value: &str) {
    with_doc(|d| d.type_filter = value.to_string());
    persist_prefs();
    refresh_view();
}

/// Multi-select; the menu stays open in the view.
pub(crate) fn toggle_source_filter(kind: &str) {
    with_doc(|d| match kind {
        "qobuz" => d.src_qobuz = !d.src_qobuz,
        "plex" => d.src_plex = !d.src_plex,
        "local" => d.src_local = !d.src_local,
        _ => {}
    });
    persist_prefs();
    refresh_view();
}

/// Type `all`, no sources, sort `position` asc. The search query is left
/// INTACT (the reference's reset does not clear it, and `hasAnyFilter` excludes
/// search).
pub(crate) fn reset_filters() {
    with_doc(|d| {
        d.type_filter = "all".to_string();
        d.src_qobuz = false;
        d.src_plex = false;
        d.src_local = false;
        d.sort = "position".to_string();
        d.sort_dir = "asc".to_string();
    });
    persist_prefs();
    refresh_view();
}

/// `list` | `grid` | `expanded`. The reference does NOT re-derive here (the
/// item list is unchanged, only the layout switches), so neither do we: mutate,
/// persist, PUBLISH. The caches, the selection and the scroll position survive.
///
/// The segment does NOT open or close anything any more (owner, 2026-08-01):
/// `expanded` used to mean "open all" and leaving it meant "close all, and
/// forget". Both are gone. The segment now only decides whether the arm RENDERS
/// the accordion — the chevron lives in the details arm alone
/// (`MyQbzDetailView.qml` `rowExpander`), so the list and grid arms must not
/// show an inline block there is no affordance to close. `resync_row_open`
/// re-derives the flag from the untouched set, which is why a row the user
/// opened is still open when he comes back to details.
pub(crate) fn set_view_mode(mode: &str) {
    with_doc(|d| {
        d.view_mode = mode.to_string();
        resync_row_open(d);
    });
    persist_prefs();
    publish_current();
    if mode == "expanded" {
        // Restoring the arm can reveal rows whose inline tracks were never
        // fetched (a restored set on a fresh session). Idempotent, and a no-op
        // when nothing is open.
        ensure_expanded();
    }
}

/// Toggle multi-select edit mode. Leaving clears the selection.
pub(crate) fn toggle_select_mode() {
    let doc = with_doc(|d| {
        let on = !d.select_mode;
        if !on {
            SELECTED.lock().unwrap().clear();
            for row in d.items.iter_mut() {
                row.selected = false;
            }
            d.selected_count = 0;
        }
        d.select_mode = on;
        d.clone()
    });
    publish(&doc);
}

/// Toggle one row's selection by its stable POSITION, then recount. A row patch
/// — never a re-derive.
pub(crate) fn toggle_item_select(position: i32) {
    let doc = with_doc(|d| {
        let now_selected = {
            let mut set = SELECTED.lock().unwrap();
            if set.contains(&position) {
                set.remove(&position);
                false
            } else {
                set.insert(position);
                true
            }
        };
        if let Some(row) = d.items.iter_mut().find(|r| r.position == position) {
            row.selected = now_selected;
        }
        d.selected_count = SELECTED.lock().unwrap().len() as i32;
        d.clone()
    });
    publish(&doc);
}

/// Clear the selection (uncheck every row + zero the count) while STAYING in
/// select-mode. Used after a bulk action completes.
pub(crate) fn clear_selection() {
    let doc = with_doc(|d| {
        SELECTED.lock().unwrap().clear();
        for row in d.items.iter_mut() {
            row.selected = false;
        }
        d.selected_count = 0;
        d.clone()
    });
    publish(&doc);
}

/// The selected row POSITIONS, ascending.
pub(crate) fn selected_positions() -> Vec<i32> {
    let mut positions: Vec<i32> = SELECTED.lock().unwrap().iter().copied().collect();
    positions.sort_unstable();
    positions
}

/// The full `MixtapeCollectionItem`s for the selected positions, in ascending
/// position order (the render order may differ). The bulk add/enqueue payloads
/// need the numeric year / track_count the render row does not carry.
pub(crate) fn selected_full_items() -> Vec<MixtapeCollectionItem> {
    let positions = selected_positions();
    let items = FULL_ITEMS.lock().unwrap();
    positions
        .iter()
        .filter_map(|p| items.iter().find(|it| it.position == *p).cloned())
        .collect()
}

// ---------------------------------------------------------------------------
// Open-document accessors (the bridge invokables carry no id)
// ---------------------------------------------------------------------------

pub(crate) fn current_id() -> String {
    with_doc(|d| d.id.clone())
}

pub(crate) fn current_kind() -> String {
    with_doc(|d| d.kind.clone())
}

pub(crate) fn current_play_mode() -> String {
    with_doc(|d| d.play_mode.clone())
}

pub(crate) fn current_name() -> String {
    with_doc(|d| d.name.clone())
}

pub(crate) fn current_description() -> String {
    with_doc(|d| d.description.clone())
}

/// The RENDERED row's stored `itemType` for `source_item_id`, defaulting to
/// `"album"` when no row carries it (1:1 with `qbz/src/main.rs:6795-6804`, which
/// reads the same value off the item model with the same fallback).
///
/// Used by the inline-track menu's go-to arm: the parent's REAL type decides
/// album view vs playlist view, and hardcoding "album" would mis-route a
/// playlist parent.
pub(crate) fn row_item_type(source_item_id: &str) -> String {
    with_doc(|d| {
        d.items
            .iter()
            .find(|r| r.source_item_id == source_item_id)
            .map(|r| r.item_type.clone())
            .unwrap_or_else(|| "album".to_string())
    })
}

// ---------------------------------------------------------------------------
// Load / navigation
// ---------------------------------------------------------------------------

/// Load (or RE-load) the collection `id` into the detail document. Does NOT
/// touch the nav stack — `open` does that, and the mutation reloads must not
/// push a second entry.
pub(crate) async fn load(runtime: &Arc<AppRuntime<LoggingAdapter>>, id: String) {
    reset();

    // ONE blocking hop for both reads: the collection (DB) and this
    // collection's persisted accordion set (a small per-user JSON). Neither may
    // run on the GUI thread, and pairing them keeps the load at one hop.
    let fetch_id = id.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        get_collection(&fetch_id).map(|c| {
            let open = crate::myqbz_prefs_qt::load_open_rows(&fetch_id);
            (c, open)
        })
    })
    .await
    .ok()
    .flatten();

    let Some((mut collection, stored_open)) = loaded else {
        log::warn!("[qbz-qt] myqbz_detail load({id}): collection not found");
        apply_not_found();
        return;
    };

    if crate::offline_fwd::engine().status().is_offline() {
        let before = collection.items.len();
        let items: Vec<&MixtapeCollectionItem> = collection.items.iter().collect();
        let availability = crate::myqbz_qt::offline_availability(&items).await;
        drop(items);
        collection
            .items
            .retain(|item| availability.item_available(item));
        let hidden = before.saturating_sub(collection.items.len());
        if hidden > 0 {
            log::info!("[qbz-qt] myqbz_detail offline filter hid {hidden} unavailable item(s)");
        }
    }

    apply(collection, stored_open);
    let missing = attach_artwork();
    publish_current();
    dispatch_downloads(missing);

    resolve_items(Arc::clone(runtime));
    // The restored set can carry rows the user left open: they render open with
    // an empty body until something fetches their tracks. This is NOT the old
    // auto-expand — with nothing restored (the default, and every first open) it
    // is one pass over the rows and no fetch at all.
    ensure_expanded();
}

/// Grid card click: push the route, then load.
pub(crate) fn open(id: String) {
    crate::nav_qt::record_with("mixtapedetail", serde_json::json!({ "id": &id }));
    let runtime = crate::app();
    crate::spawn(async move {
        load(&runtime, id).await;
    });
}

/// Re-run the load for the same id after a mutation (the reference's
/// "-> reload"), WITHOUT a second nav record.
pub(crate) fn reload(id: String) {
    if id.is_empty() {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        load(&runtime, id).await;
    });
}

/// Reload the open detail only when `id` IS the one on screen (the hero
/// Shuffle's play-mode side effect, spec 02 §6.3).
pub(crate) fn reload_if_open(id: &str) {
    if !id.is_empty() && current_id() == id {
        reload(id.to_string());
    }
}

/// Re-publish with the Rust-built strings rebuilt in the new locale
/// (`apply_language`, W18): the kind eyebrow, the "N albums" plural and every
/// unresolved row's TYPE label.
pub(crate) fn republish() {
    let doc = with_doc(|d| {
        if !d.kind.is_empty() {
            d.kind_label = kind_label_from_str(&d.kind);
        }
        d.meta = album_count_label(d.item_count.max(0) as usize);
        rebuild(d);
        d.clone()
    });
    publish(&doc);
}

/// Drop every per-user cache (logout, W8) so the next account inherits nothing.
pub(crate) fn teardown() {
    FULL_ITEMS.lock().unwrap().clear();
    INLINE_CACHE.lock().unwrap().clear();
    RESOLVE_CACHE.lock().unwrap().clear();
    OPEN_ROWS.lock().unwrap().clear();
    SELECTED.lock().unwrap().clear();
    *LAST_FILTER_KEY.lock().unwrap() = None;
    PREFS_HYDRATED.store(false, Ordering::SeqCst);
    let doc = with_doc(|d| {
        *d = DetailDoc::default();
        d.clone()
    });
    publish(&doc);
}

// ---------------------------------------------------------------------------
// Navigation out of a row, and the bulk-action dispatch
// ---------------------------------------------------------------------------

/// Row / card click: open the item it stands for. 1:1 with the Slint handler
/// (`qbz/src/main.rs:6270-6294`): album AND track items both open an album view
/// — `crate::open_album` is what routes Qobuz-vs-local and records history, so
/// the source argument is deliberately unused here, exactly as it is there.
pub(crate) fn open_item(_source: String, item_type: String, source_item_id: String) {
    match item_type.as_str() {
        "album" | "track" => crate::open_album(source_item_id),
        "playlist" => crate::open_playlist(source_item_id),
        other => {
            log::warn!("[qbz-qt] myqbz_detail open_item: unknown type {other}");
        }
    }
}

/// The artist link on a row, routed by SOURCE (`qbz/src/main.rs:6302-6325`).
/// Stored items only carry the artist NAME, so a Qobuz row needs the numeric id
/// the resolve pass derived from its first track — and when that resolve has not
/// landed yet the click is IGNORED rather than falling back to the name, because
/// the name would open the LocalLibrary artist, i.e. the wrong page.
pub(crate) fn open_artist(source: String, artist_name: String, artist_id: String) {
    if source == "qobuz" {
        if artist_id.trim().is_empty() {
            log::warn!(
                "[qbz-qt] myqbz_detail open_artist: qobuz item '{artist_name}' has no resolved \
                 artist id yet — ignoring click"
            );
            return;
        }
        crate::open_artist(artist_id);
        return;
    }
    if !artist_name.trim().is_empty() {
        // local / plex -> the LocalLibrary Artists tab, BY NAME.
        crate::local_album_actions::open_artist_by_name(artist_name);
    }
}

/// The multi-select bar's seven actions (`qbz/src/main.rs:6448-6541`). Every arm
/// reads the selection here rather than taking it as an argument, so the QML
/// only ever sends the action id.
pub(crate) fn bulk_action(id: String) {
    match id.as_str() {
        "add-to-queue" | "play-next" => {
            if selected_positions().is_empty() {
                return;
            }
            crate::myqbz_play_qt::bulk_enqueue(id.as_str() == "play-next");
        }
        "play-later" => {
            if selected_positions().is_empty() {
                return;
            }
            crate::myqbz_play_qt::bulk_enqueue_later();
        }
        "add-to-mixtape" => {
            let items: Vec<crate::myqbz_add_qt::AddItem> = selected_full_items()
                .iter()
                .map(|it| crate::myqbz_add_qt::AddItem {
                    item_type: item_type_str(it.item_type).to_string(),
                    source: source_str(it.source).to_string(),
                    source_item_id: it.source_item_id.clone(),
                    title: it.title.clone(),
                    subtitle: it.subtitle.clone(),
                    artwork_url: it.artwork_url.clone(),
                    year: it.year,
                    track_count: it.track_count,
                })
                .collect();
            if items.is_empty() {
                return;
            }
            crate::myqbz_add_qt::open_items(items);
        }
        "remove-selected" => crate::myqbz_edit_qt::remove_selected(),
        "clear" => clear_selection(),
        // Resolve every selected container through qbz-source, then pass only
        // the resulting Qobuz catalog tracks to the Qobuz playlist picker.
        // Local/Plex tracks are skipped by the shared source registry.
        "add-to-playlist" => {
            let items = selected_full_items();
            if items.is_empty() {
                return;
            }
            crate::spawn(async move {
                let runtime = crate::app();
                let ids =
                    crate::myqbz_play_qt::resolve_bulk_qobuz_track_ids(&runtime, &items).await;
                if ids.is_empty() {
                    crate::toast_qt::error(qbz_i18n::t("No tracks to add"));
                    return;
                }
                crate::playlist_picker_qt::open_for_ids(&runtime, ids);
            });
        }
        other => {
            log::warn!("[qbz-qt] myqbz_detail bulk_action: unknown id {other}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (pure functions only — no Qt, no network, no engine)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        position: i32,
        title: &str,
        kind: ItemType,
        source: AlbumSource,
    ) -> MixtapeCollectionItem {
        MixtapeCollectionItem {
            collection_id: "c".into(),
            position,
            item_type: kind,
            source,
            source_item_id: format!("id{position}"),
            title: title.into(),
            subtitle: Some("Artist".into()),
            artwork_url: None,
            year: Some(2000 + position),
            track_count: Some(position + 1),
            added_at: 0,
        }
    }

    #[test]
    fn tracks_text_is_empty_for_track_and_unknown_album_count() {
        // #766: a single track has no track count — the column stays empty.
        let track = item(1, "B", ItemType::Track, AlbumSource::Qobuz);
        assert_eq!(tracks_text(&track), "");
        // An album shows its stored count (item() seeds position + 1 = 1).
        let album = item(0, "A", ItemType::Album, AlbumSource::Qobuz);
        assert_eq!(tracks_text(&album), "1");
        // An album with an unknown count stays empty until `resolve_items`
        // backfills the resolved count — no em-dash placeholder.
        let mut unknown = item(2, "C", ItemType::Album, AlbumSource::Qobuz);
        unknown.track_count = None;
        assert_eq!(tracks_text(&unknown), "");
    }

    #[test]
    fn duration_placeholder_is_never_zero() {
        assert_eq!(track_duration_str(0), "--:--");
        assert_eq!(track_duration_str(61), "1:01");
    }

    #[test]
    fn release_type_heuristic_matches_album_map() {
        assert_eq!(classify_release_type(Some(3)), "Single");
        assert_eq!(classify_release_type(Some(6)), "EP");
        assert_eq!(classify_release_type(Some(7)), "Album");
        assert_eq!(classify_release_type(None), "Album");
    }

    #[test]
    fn cache_key_never_collides_across_sources() {
        assert_ne!(cache_key("qobuz", "1"), cache_key("local", "1"));
    }

    /// The accordion's ONE state: `rowOpen` is derived from the shared set on
    /// every rebuild (so a sort or a keystroke cannot fold an open row), and a
    /// TRACK never carries it — it has nothing to show, and its row body must
    /// keep the whole click gesture.
    #[test]
    fn row_open_is_derived_from_the_shared_open_set() {
        let resolved = HashMap::new();
        let inline = HashMap::new();
        let selected = HashSet::new();
        let album = item(0, "A", ItemType::Album, AlbumSource::Qobuz);
        let track = item(1, "B", ItemType::Track, AlbumSource::Qobuz);

        let mut open = HashSet::new();
        open.insert(cache_key("qobuz", &album.source_item_id));
        open.insert(cache_key("qobuz", &track.source_item_id));

        let album_row = to_item(&album, &resolved, &inline, &selected, &open, true);
        assert!(album_row.can_expand);
        assert!(album_row.row_open);

        let track_row = to_item(&track, &resolved, &inline, &selected, &open, true);
        assert!(!track_row.can_expand);
        assert!(!track_row.row_open);

        let closed: HashSet<String> = HashSet::new();
        assert!(!to_item(&album, &resolved, &inline, &selected, &closed, true).row_open);
    }

    /// The ARM gate: outside the details arm no row renders open, and the SET
    /// is not consulted destructively — the very same set still opens the row
    /// when the arm comes back. This is what makes a view switch lossless while
    /// keeping the list and grid arms free of inline blocks they cannot close.
    #[test]
    fn row_open_is_gated_by_the_details_arm_without_losing_the_set() {
        let resolved = HashMap::new();
        let inline = HashMap::new();
        let selected = HashSet::new();
        let album = item(0, "A", ItemType::Album, AlbumSource::Qobuz);
        let mut open = HashSet::new();
        open.insert(cache_key("qobuz", &album.source_item_id));

        assert!(!to_item(&album, &resolved, &inline, &selected, &open, false).row_open);
        assert!(to_item(&album, &resolved, &inline, &selected, &open, true).row_open);
        assert!(open.contains(&cache_key("qobuz", &album.source_item_id)));
    }

    /// A fresh collection starts CLOSED: nothing but the user's own chevron puts
    /// a key in the set, so an empty set means every row renders collapsed even
    /// in the details arm.
    #[test]
    fn a_collection_with_no_stored_state_starts_fully_closed() {
        let resolved = HashMap::new();
        let inline = HashMap::new();
        let selected = HashSet::new();
        let open: HashSet<String> = HashSet::new();
        for (i, kind) in [ItemType::Album, ItemType::Playlist, ItemType::Track]
            .into_iter()
            .enumerate()
        {
            let it = item(i as i32, "X", kind, AlbumSource::Qobuz);
            let row = to_item(&it, &resolved, &inline, &selected, &open, true);
            assert!(!row.row_open, "{kind:?} row must start closed");
        }
    }

    /// A patch carries the join key plus ONLY what changed — the point of the
    /// channel. An untouched field must be absent, not `null`, so QML's
    /// `Object.assign` cannot blank a value it was not told about.
    #[test]
    fn row_patch_serialises_only_the_changed_fields() {
        let mut row = DetailRow {
            position: 7,
            source_item_id: "abc".into(),
            ..DetailRow::default()
        };
        row.row_open = true;
        let mut patch = RowPatch::of(&row);
        patch.row_open = Some(true);
        let json = serde_json::to_string(&patch).unwrap();
        assert!(json.contains("\"position\":7"));
        assert!(json.contains("\"sourceItemId\":\"abc\""));
        assert!(json.contains("\"rowOpen\":true"));
        assert!(!json.contains("qualityTier"));
        assert!(!json.contains("inlineTracks"));
    }

    #[test]
    fn filter_key_tracks_every_derive_input() {
        let mut a = DetailDoc::default();
        let base = filter_key(&a);
        a.search = " Foo ".into();
        assert_ne!(filter_key(&a), base);
        let mut b = DetailDoc::default();
        b.src_plex = true;
        assert_ne!(filter_key(&b), base);
        let mut c = DetailDoc::default();
        c.sort_dir = "desc".into();
        assert_ne!(filter_key(&c), base);
    }
}
