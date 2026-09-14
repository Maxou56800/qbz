// TrackRow — THE track list row (primitives/TrackRow.slint, POC arm
// subset), consolidated in phase 22 from FOUR copies (AlbumView
// .AlbumTrackRow, PlaylistView.PlTrackRow, LibraryView.TrackListRow,
// SearchView.SearchTrackRow). 50px, radius 8: number→play cell (pause
// swap + accent pill when it is the now-playing row — Slint-universal),
// optional 36px art cell, title+explicit / artist column, optional 220px
// album link, duration, quality (BARE QualityBadgeFull — the tier label over
// the exact bit-depth / sample-rate line), optional
// heart, optional download slot (offline column — glyph stub or reserved
// spacer until the offline cache lands, POC-NOTE), ⋯ CardMenu.
//
// item contract: { id, title, artist, artistId, album, albumId, number?,
// duration, qualityTier, qualityDetail, explicit, isFavorite, artPath? }
// (plus playlistTrackId for the remove-from-playlist arm, and the OPTIONAL
// `source` / `unavailable` a mixed-source producer carries — the Add to
// playlist gate below is the one arm that reads them, plus the OPTIONAL
// `qobuzUnavailable`, which is a DIFFERENT thing from `unavailable` and is
// documented where it is read). `qualityDetail`
// is the bare exact-quality string ("16-bit / 44.1 kHz"); when a producer
// does not carry it yet the badge degrades to the tier label alone rather
// than to a blank cell. `item.isFavorite` SEEDS the row's own `favorite`
// property and is never written back — see the heart block below for why a
// plain-JS-object field cannot be the live state.
//
// Arms: showArtwork / showAlbum / showFavorite / showDownload /
// downloadGlyph / showMenu / zebra / artistLink / clickPlays /
// menuShowLater / menuShowGoTo / menuShowFavorite / menuShowRemove.
// (`qualityStyle` is RETIRED — kept declared, inert, see below.)
// Signals: playRequested() (per-site play: album-scoped, playlist-scoped
// or plain), enqueueRequested(mode) ("next"|"later"|"queue"),
// removeRequested(), bodyDragStarted(index) (fired BEFORE the shared
// dragStart — the #589 reorder pre-hook), mixtapeRequested() (the host
// builds the MyQBZ AddItem payload), goToRequested(kind) (only when
// `routeGoToExternally` is set), playlistAddRequested() (only when
// `routePlaylistAddExternally` is set — the local-mode "Add to playlist"),
// replaceRequested() (only when `routeReplaceExternally` is set — the
// "Find available version" repair for a track Qobuz pulled).
// Favorite toggling, Go-to-artist/album, Share and Track info are
// identical on every site — handled internally. The row BODY is the drag
// source (6px threshold, ghost + sidebar drops in main.rs) and its RIGHT
// press opens the very same menu the ⋯ button does.
//
// --- Menu inventory vs primitives/TrackContextMenu.slint ----------------
// The .slint menu is FLAT (no separators) in this order: Play now · Play
// next · Play later · Add to queue · Create QBZ radio · Create Qobuz radio ·
// Add to library · Add to mixtape · Add to playlist · Remove from
// playlist(danger) · Share Qobuz link · Share Song.link · Make available
// offline | Refresh + Remove offline copy(danger) · Go to album · Go to
// artist · Track info. Everything the Qt bridge has a seam for is here, in
// that order; the rest is gated OFF by the `has*Seam` constants below
// rather than rendered as a row that silently does nothing.
//
// Deliberately NOT consolidated here (Slint-distinct): QueuePanel.QueueRow
// (QueueItem data + queue-op menu + press-and-hold reorder) and
// LibraryView.FeedListRow (mixed-kind feed row — inline in Slint too).
//
// --- COLUMN GEOMETRY LIVES IN rows/TrackCols.qml ------------------------
// Not in this file. rows/TrackListHeader.qml reads the same object, so the
// header of a list and the rows in it cannot disagree. If you change a
// column width, change it THERE — a literal typed back into this file is
// the exact defect the owner reported (the header laid out only the
// labelled columns, so every label sat right of the column it named).

import QtQuick
// Qt's own attached ToolTip, for the pulled-track glyphs ONLY. The shell's
// shared bubble (controls/QbzTooltip.qml) is reached by naming the AppShell
// instance, which a recycled delegate cannot do — the same trade
// controls/QbzMultiSelectBar.qml and views/AlbumView.qml already made.
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../immersive"
import "../shell"
import "../theme"

Rectangle {
    property bool kioskHost: false

    id: root

    property var item: ({})
    property int number: 0
    property bool showArtwork: false
    property bool showSource: false
    property bool showAlbum: false
    property bool showFavorite: true
    property bool showDownload: false
    property bool downloadGlyph: false
    property bool showMenu: true
    /// The row is EPHEMERAL — a disc, a disc image or an ad-hoc folder.
    ///
    /// It has no `library.db` row and no catalog id, so every entry that
    /// writes a reference meant to outlive the session (favourite, playlist,
    /// mixtape, offline cache, share) has nothing to point at once the disc
    /// comes out. The QUEUE block stays, because a queue is exactly as
    /// temporary as the session is.
    ///
    /// Entries are ABSENT rather than rendered-and-inert — the same rule the
    /// `has*Seam` gates in `menuModel` already follow.
    property bool queueOnly: false
    property bool zebra: false
    property bool artistLink: false
    property bool clickPlays: true
    // Debounce for clickPlays rows (see onReleased).
    property real _lastPlayRequestMs: 0
    /// RETIRED and INERT. primitives/TrackRow.slint has exactly ONE quality
    /// form (the bare QualityBadgeFull below) — there was never an icon/text
    /// split to arm. Kept declared ONLY so the two existing call sites that
    /// still assign it (AlbumView.qml `"text"`, local/LocalTrackRow.qml
    /// `"icon"`) keep loading; assigning a non-existent property is a hard
    /// QML instantiation error. Both assignments are listed for removal.
    property string qualityStyle: "icon"
    property bool menuShowLater: true
    property bool menuShowGoTo: true
    property bool menuShowFavorite: true
    property bool menuShowRemove: false
    /// Reorder arm (primitives/TrackRow.slint:87 `show-reorder`): a leading
    /// 22px gutter with an up/down chevron stack. ON = this list is in an
    /// order the user OWNS and can rewrite (the playlist detail under custom
    /// sort; a LOCAL playlist under its natural order, whose repo `position`
    /// IS that order). Default off, so no existing call site grows a gutter.
    ///
    /// It is also the gate for `preventStealing` on the row-body drag — see
    /// the DRAG vs. DRAG-TO-SCROLL block at the bottom of this file.
    property bool showReorder: false
    /// Use the row body's vertical reorder gesture without drawing the
    /// playlist-specific chevron gutter. QueueView owns its insertion line
    /// and consumes the shared drag coordinates; all existing rows inherit
    /// `showReorder` and therefore keep their current gesture policy.
    property bool reorderDrag: root.showReorder
    /// QueueView replaces the compact window-level drag pill with a complete
    /// TrackRow while the pointer remains over its list. Other surfaces keep
    /// the portable pill because their target may live outside the view.
    property bool inlineDragVisual: false
    /// Ends of the list: the reference dims nothing, but a chevron that
    /// renders and no-ops is the defect class this round is fixing, so the
    /// first row's ↑ and the last row's ↓ are drawn disabled instead.
    property bool canMoveUp: true
    property bool canMoveDown: true
    // Drag source arm (additive; default = the existing behaviour). The
    // Local Library rows set this false: their `item.id` is a local DB row
    // id, and the sidebar drop handler forwards whatever it receives to
    // `playlist add-tracks` as a QOBUZ catalog id — a local row dropped on a
    // playlist would silently add an unrelated catalog track.
    property bool draggable: true
    /// LOCAL-mode drag arm (2026-08-30): the row IS a drag source, but its
    /// `item.id` is a LOCAL library row id, so it enters the shared drag via
    /// `dragStartLocal` — a typed payload the drop handlers resolve
    /// source-aware, never a number that could be read as a catalog id.
    /// Independent of `draggable` on purpose: `catalogRow` below keeps
    /// meaning "item.id is a Qobuz catalog id".
    property bool localDragSource: false
    readonly property bool dragSource: root.draggable || root.localDragSource
    /// A live transport guard separate from catalogue availability. The full
    /// queue uses it when QConnect is active: incompatible rows stay visible
    /// and keep their non-transport menu actions, but cannot play or drag.
    property bool playBlocked: false
    /// Full queue only: give the now-playing row a persistent themed fill in
    /// addition to TrackRow's existing pill/equalizer indicator.
    property bool activeBackground: false
    /// QueueView can distinguish duplicate ids across history/current/upcoming
    /// and nominate the one structural current row as active.
    property bool overrideActiveMatch: false
    property bool activeMatch: false
    /// Optional queue-state marker that replaces the idle number cell.
    property string leadingMarkerIcon: ""
    /// QueueView needs queue-operation entries rather than the catalog-wide
    /// TrackContextMenu inventory. Null preserves the standard model.
    property var menuEntriesOverride: null
    signal menuActionRequested(string action)
    /// Multi-select arm (primitives/TrackRow.slint:58 `multi-select-mode`):
    /// the leading play/number cell becomes a selection checkbox, the WHOLE
    /// row body is a selection target, and the row's own gestures (play,
    /// double-click, drag, right-click menu) stand down. Default off, so every
    /// existing call site is untouched.
    property bool selectMode: false
    property bool checked: false
    /// `modifiers` is the mouse event's, straight through — Shift is what
    /// turns a click into a range (controls/SelectionModel.qml). Hosts that
    /// do not care can keep a no-argument handler; QML allows it.
    signal toggleSelect(int modifiers)
    // Per-row artwork placeholder (see the 36px cell below). The host view
    // owns the phase clock so one timer drives every row.
    property bool artPending: false
    property bool skelPhase: false
    property int artSettleMs: 0
    // A ListView reuse-pool item stays alive and keeps receiving signals.
    // Hosts that enable reuse flip this attached-state twin from pooled /
    // reused handlers so an offstage playing row cannot keep pulsing and an
    // old track cannot consume cache/favourite settle signals.
    property bool recycleActive: true

    // Catalog-backed row? `draggable` is ALREADY the "item.id is a Qobuz
    // catalog track id" predicate (the Local Library / ephemeral rows set it
    // false because their id is a local DB row id — see the note above), so
    // the two entries that hit the Qobuz catalog by id ride it instead of
    // asking every host to pass a new arm.
    readonly property bool catalogRow: root.draggable
    property bool menuShowShare: root.catalogRow
    property bool menuShowTrackInfo: root.catalogRow

    // --- Seams the Qt bridge does NOT have yet (menu-parity round) -------
    // Each maps to a TrackContextMenu.slint entry. OFF = the row is not
    // built at all, never rendered-and-inert. Flip the constant AND fill the
    // matching `menuAction` branch when the invokable lands; the entry then
    // appears in its .slint-correct slot. Why each is missing:
    //   radio        no radio invokable on any Qt bridge object
    //   songlink     needs the ISRC -> Deezer -> Odesli round-trip (backend)
    //   offlineCache no per-track cache bridge (the download cell is a stub)
    readonly property bool hasRadioSeam: false
    readonly property bool hasSonglinkSeam: false
    // Live since the offline port: catalog (Qobuz) rows only — local/Plex
    // rows have no offline tier to download into (TrackContextMenu.slint
    // gates on the same qobuz-actions rule).
    readonly property bool hasOfflineCacheSeam: root.catalogRow && !root.localSourceRow

    // --- Offline cache status (TrackRow.slint:639-702) --------------------
    // 0 none · 1 queued · 2 downloading · 3 ready · 4 failed. Seeded from
    // the document row; live updates arrive on QbzShell's
    // trackCacheStatusChanged and are adopted only for THIS row's id.
    property int cacheStatus: root.item.cacheStatus !== undefined ? root.item.cacheStatus : 0
    function setCacheStatus(status) {
        root.cacheStatus = status
    }
    Connections {
        target: QbzShell
        enabled: root.recycleActive && root.hasOfflineCacheSeam
        function onTrackCacheStatusChanged(trackId, status, progress) {
            if (trackId === (root.item.id || "")) root.cacheStatus = status
        }
    }

    // --- PULLED FROM THE QOBUZ CATALOGUE (contract §5.2) ------------------
    // `item.qobuzUnavailable` is the API saying `streamable: false` for THIS
    // track. The producer stamps it from `qbz_models::Track::is_streamable()`
    // — never from an absent key (that is exactly what the contract's §3.1
    // `Option<bool>` exists to keep apart), never from a 403, never from a
    // network error. A producer that does not carry the field yet leaves it
    // `undefined`, which reads as "available" here: absence is not a claim.
    //
    // It is a NEW field and deliberately NOT `item.unavailable`, which this
    // file already reads a few lines below. That one is a BROKEN LOCAL REF (a
    // Plex key the cache does not know, a stored id outside the catalog
    // range). The two look alike and behave oppositely: a broken ref stays
    // fully SELECTABLE so the user can delete it and can never heal on its
    // own, while a pulled Qobuz track is inert and CAN be repaired by the
    // replacement search. One flag for both would collapse two treatments into
    // whichever was written last.
    readonly property bool pulled: root.item.qobuzUnavailable === true

    /// THE DEAD ROW — dim, inert, alert glyph.
    ///
    /// `cacheStatus === 3` (downloaded) is excluded on purpose: a pulled track
    /// the user already downloaded STILL PLAYS. `qbz-core/src/core.rs:716`
    /// resolves the offline tier before any network call and before any
    /// availability question, so the file on disk is served regardless of what
    /// the catalogue says. Treating it as dead would take away a song the user
    /// owns — a worse bug than the one this treatment fixes, and it would bite
    /// hardest in offline mode. It gets `pulledCached` below instead.
    readonly property bool pulledDead: root.pulled && root.cacheStatus !== 3
    /// The whole release is gone from the catalog (`releaseUnavailable`,
    /// decided by the row's producer): the album link and "Go to album" go
    /// dark instead of opening an empty page (2026-09-13).
    readonly property bool releaseGone: root.item.releaseUnavailable === true

    /// Pulled from the catalogue, but playable from the downloaded copy. Keeps
    /// its play cell, its heart and its click; gains only an honest badge,
    /// because the user still needs to know the track is gone from Qobuz —
    /// the moment they clear the download it dies.
    readonly property bool pulledCached: root.pulled && root.cacheStatus === 3

    /// "Find available version" — HOST-routed, the `routeGoToExternally`
    /// idiom. Only the host knows which playlist is open, whether the user
    /// OWNS it (Qobuz refuses a write to anyone else's) and that the row is a
    /// catalog membership rather than a local ref. Default off, so no other
    /// surface grows the entry.
    property bool routeReplaceExternally: false
    signal replaceRequested()
    readonly property bool hasReplaceSeam: root.pulled && root.routeReplaceExternally

    // --- Add to playlist -------------------------------------------------
    // LIVE since QbzPlaylistPicker landed. It is NOT a flat constant, because
    // the entry's correctness depends on WHICH ID SPACE this row's `item.id`
    // lives in — the one thing the picker's whole design exists to keep
    // unmistakable (playlist_picker_qt.rs's four-case matrix):
    //
    //   catalogRow          `item.id` is a Qobuz catalog id -> openForTrack.
    //   localSourceRow      the row carries its own source word and it names
    //                       a NON-CATALOG origin -> `item.id` is a library.db
    //                       row id or a namespaced synthetic id (Plex,
    //                       Jellyfin, Subsonic). Sending it to the Qobuz arm
    //                       adds an UNRELATED track, so the entry is only
    //                       offered when the HOST takes the routing
    //                       (`routePlaylistAddExternally`, below), and is
    //                       ABSENT otherwise rather than wrong.
    //   item.unavailable    a stored ref that cannot resolve at all
    //                       (playlist_qt.rs PlaylistTrackRow.unavailable);
    //                       there is no track to copy anywhere.
    //
    // Ephemeral rows never reach this decision: their surface sets
    // `queueOnly: true`, so `menuModel` returns after the transport block.
    // An ALLOWLIST of non-catalog words, not `!== "qobuz"`: "offline" is an
    // offline-CACHED Qobuz row whose id IS a catalog id, so inverting the test
    // would strip Add-to-playlist from the rows that legitimately have it.
    // The Subsonic brand spellings fold here for the same reason they fold in
    // `SourceIcon.isSubsonic` — a row stamped "navidrome" is not a Qobuz row.
    readonly property bool localSourceRow: {
        var s = root.item.source
        return s === "local" || s === "plex" || s === "jellyfin"
            || s === "subsonic" || s === "navidrome" || s === "gonic"
            || s === "airsonic" || s === "astiga"
    }
    /// Hand "Add to playlist" to the HOST (the `routeGoToExternally` idiom):
    /// the host answers `playlistAddRequested()` with the LOCAL-mode call its
    /// surface can make. The local playlist detail is the one that needs it —
    /// only Rust can turn a detail row's display id into a source-aware ref
    /// (`local_playlist_qt::local_picker_ref_for_row`).
    property bool routePlaylistAddExternally: false
    signal playlistAddRequested()
    /// OFF for a ref that cannot resolve (`unavailable`) AND for a track Qobuz
    /// pulled (`pulled`) — both would SPREAD a row that cannot play.
    ///
    /// `pulled`, not `pulledDead`: a downloaded copy plays for THIS user on
    /// THIS machine, but what gets stored in the playlist is the Qobuz catalog
    /// id, which is dead for every other surface, every other device and
    /// everyone the playlist is shared with. Seeding that is the one thing
    /// worth refusing outright — the user can still repair the row where it
    /// already lives.
    property bool hasPlaylistAddSeam: (root.item.unavailable === true || root.pulled)
        ? false
        : (root.routePlaylistAddExternally
            || (root.catalogRow && !root.localSourceRow))
    // LIVE since the MyQBZ domain landed: QbzMyQbzAdd.open() takes a JSON
    // array of AddItem, so the row only has to ASK — it does not know its own
    // itemType/source, which is why the payload is built by the host (see
    // `mixtapeRequested` below). Not `readonly`: a host whose ids are not
    // Qobuz/local mixtape-addressable can turn it back off.
    /// Same rule as `hasPlaylistAddSeam`: a mixtape stores the catalog id too,
    /// so a pulled track added to one is a row that resolves to nothing the
    /// next time the collection is played.
    property bool hasMixtapeSeam: !root.pulled

    signal playRequested()
    signal enqueueRequested(string mode)
    signal removeRequested()
    signal bodyDragStarted(int index)
    /// Reorder gutter (`showReorder`). The HOST answers — only it knows the
    /// row's id and which playlist is open (TrackRow.slint routes the same
    /// pair through `media-action("track", id, "move-up"/"move-down")`).
    signal moveUpRequested()
    signal moveDownRequested()
    /// "Add to mixtape" — the HOST answers, because only the host knows the
    /// row's `itemType` / `source` (a MyQBZ AddItem needs both) and building
    /// the array here would hardcode `("track", "qobuz")` for every surface.
    signal mixtapeRequested()

    // --- Go-to routing: internal by default, host-routed on request -------
    // The go-to actions (menu AND the artist-column link) navigate through
    // QbzArtist.openArtist / QbzAlbum.openAlbum here, which is right on every
    // catalog surface. It is WRONG where the row is a child of another entity
    // whose host must resolve the target — MyQBZ's expanded detail rows, where
    // Slint routes both paths through the SAME shared handler
    // (`media-action("artist", id, "open")`, TrackRow.slint:530). Flipping
    // `routeGoToExternally` hands both sites to the host as
    // `goToRequested(kind)`; it defaults false, so no existing call site moves.
    property bool routeGoToExternally: false
    signal goToRequested(string kind)   // "artist" | "album"

    // --- THE heart state (a real QML property, never `item.isFavorite`) ---
    // The row used to flip its glyph with `root.item.isFavorite = !…`. That is
    // a write into a plain JS object and it does nothing at all here, twice
    // over — both halves measured under qml6 6.11.1 in this exact delegate
    // shape (`Repeater { model: JSON.parse(...) }` + `required property var
    // modelData` + `item: modelData`):
    //
    //   1. QML does not re-evaluate a binding when a plain JS object's
    //      property is mutated — there is no notifier to fire. The glyph
    //      binding kept its old value on the same tick AND on the next.
    //   2. `item: modelData` does not even alias the row: assigning a `var`
    //      property from `modelData` yields an object that compares `!==` to
    //      both `modelData` and the source array element (Qt round-trips it
    //      through QVariant). So the write did not reach the model either —
    //      not the delegate's `modelData`, not the host's array.
    //
    // A DECLARED property has a notifier, so the optimistic flip repaints
    // immediately. The binding below keeps the document authoritative: it is
    // re-established whenever the host hands over a new row object, so a
    // republished document (or a page re-open) overrides a stale local flip.
    property bool favorite: root.item.isFavorite === true
    onItemChanged: {
        // Both properties are writable live state. A signal/click breaks their
        // seed binding, so a recycled delegate must explicitly re-establish
        // both bindings when ListView hands it a different model row.
        root.favorite = Qt.binding(function () {
            return root.item.isFavorite === true
        })
        root.cacheStatus = Qt.binding(function () {
            return root.item.cacheStatus !== undefined
                ? root.item.cacheStatus : 0
        })
        if (rowMenuLoader.item)
            rowMenuLoader.item.close()
    }

    /// The heart click and the menu's "Add to / Remove from Library" row —
    /// ONE implementation, because they used to diverge silently.
    function toggleFavorite() {
        root.favorite = !root.favorite
        QbzLibrary.libraryToggleFavorite("track", root.item.id)
    }

    // Settle. `libraryFavoriteChanged` (key `{kind}:{id}`, emitted by
    // main.rs::emit_library_favorite) carries the value the WRITE produced:
    // the flipped one when it landed, the UNCHANGED one when it failed. This
    // is the favourite twin of the pin fan-out the cards already have
    // (cards/AlbumCard.qml) and it is what rolls a 404'd un-favorite back —
    // and what keeps the OTHER surfaces showing this same track honest
    // (the album page's row, the queue, a search result) without anybody
    // republishing a document.
    Connections {
        target: QbzLibrary
        enabled: root.recycleActive
        function onLibraryFavoriteChanged(key, value) {
            var tid = (root.item && root.item.id !== undefined) ? root.item.id : ""
            if (tid !== "" && key === "track:" + tid)
                root.favorite = value
        }
    }

    QbzTheme { id: theme }

    // --- Polarity-baked tokens for the play cell (#638) -------------------
    // `theme.alphaTier(pct)` IS Slint's `Theme.alpha-N`: one ramp per theme,
    // white-based on dark, black-based on light. The `.length` guard covers
    // the pre-publish frame QbzTheme documents (its baked fallback carries no
    // ramp) — without it the disc would render fill-less AND ring-less, which
    // is the exact failure this block exists to prevent. The literals are the
    // Slint dark values and their light-polarity mirrors.
    readonly property color playCellFill: theme.alphaTiers.length > 0
        ? theme.alphaTier(10) : (theme.isDark ? "#1affffff" : "#1a000000")
    readonly property color playCellRing: theme.alphaTiers.length > 0
        ? theme.alphaTier(15) : (theme.isDark ? "#26ffffff" : "#26000000")
    /// Slint's `Theme.text-primary` icon tint, runtime-tinted (QbzIcon.qml /
    /// src/icon_tint_qt.rs) so it is the live token under all 36 themes. Every
    /// host it lands on is a THEME surface — the play cell's polarity-baked
    /// alpha disc, and `surface-elevated` for the row's trailing controls.
    readonly property string playGlyphTint: "textPrimary"
    /// TrackRow.slint:123-125 — the row hover is `Theme.alpha-8`, not
    /// surface-hover, and the .slint says why: "Hover uses the polarity-baked
    /// alpha ramp so the hover state is visible on light themes too". The
    /// zebra stripe stays the literal, per the same comment ("Zebra kept
    /// as-is to avoid a visible dark-theme stripe shift").
    readonly property color rowHoverBg: theme.alphaTiers.length > 0
        ? theme.alphaTier(8) : (theme.isDark ? "#14ffffff" : "#14000000")
    readonly property color rowActiveBg: theme.alphaTiers.length > 0
        ? theme.alphaTier(10) : (theme.isDark ? "#1affffff" : "#1a000000")

    width: parent ? parent.width : 0
    height: kioskHost ? (showReorder ? 88 : 64) : 50
    radius: 8
    // Hover fill is OFF on a dead row (views/purchases/PurchaseListRow.qml
    // does the same on its unavailable rows): a row that lights up under the
    // pointer reads as clickable, and this one is not. The zebra stripe stays
    // — it is structure, not affordance.
    color: (root.activeBackground && root.isActive)
        ? rowActiveBg
        : (hovered && !root.pulledDead && !root.playBlocked)
        ? rowHoverBg
        : (zebra && number % 2 === 0 ? "#07ffffff" : "transparent")

    // `playArea` is in here for the reason TrackPlayCell.slint:76-79 spells
    // out: the cell owns a hover-enabled area of its own, and "Slint's
    // has-hover does not propagate to ancestor TouchAreas" — Qt's does not
    // either, so without this the row (and the play circle with it) would go
    // un-hovered exactly while the pointer sits on the play button.
    // The reorder chevrons are in the same set for the same reason: they are
    // hover-enabled areas ON the row, and the row must not un-hover under
    // them. Their ids resolve whether or not the gutter is `visible` — the
    // objects always exist (no Loader / no `if`), only their painting is off.
    readonly property bool hovered: trArea.containsMouse || favArea.containsMouse
        || moreArea.containsMouse || playArea.containsMouse
        || chevUp.hovered || chevDown.hovered
    // THIS row is the now-playing one. Two ids are compared, not one, and the
    // second is not redundancy: an OFFLINE row's queue id is the QOBUZ CATALOG
    // id (the offline tier is keyed on it) while every Local Library list
    // draws that row with its `library.db` id, so the first test alone can
    // never match a downloaded track on the very view it was played from — the
    // pill and the eq bars stayed dark there. `npLocalTrackId` is published
    // for Offline rows ONLY (playback_qt::refresh_now_playing says why), so
    // the second test is inert for every other source and cannot light a row
    // by colliding with an id from another space. Mirrors the reference's
    // TrackRow.slint:116-118 / TrackPlayCell.slint:85-87.
    readonly property bool isActive: root.overrideActiveMatch
        ? root.activeMatch
        : ((item.id || "") !== ""
            && (QbzPlayer.npTrackId === (item.id || "")
                || (QbzPlayer.npLocalTrackId !== ""
                    && QbzPlayer.npLocalTrackId === (item.id || ""))))

    // The animated now-playing indicator pref (AppearanceState
    // .play-indicator-animation; the toggle lives in AppearanceSettings).
    // Read by parsing the settings document — the AlbumView headerGradient
    // precedent; the settings snapshot republishes on every settings write,
    // which is the only time this re-evaluates.
    readonly property bool playIndicatorAnim: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                return JSON.parse(raw).playIndicatorAnimation === true
            } catch (e) { /* fall through */ }
        }
        return false
    }
    // "Show featured artists in track rows" (AppearanceSettings), ON by
    // default: only an explicit false turns it off, so the rows never flash
    // without names before the first settings snapshot lands. Same read as
    // playIndicatorAnim otherwise: the settings document republishes on every
    // settings write, which is the only time this re-evaluates.
    readonly property bool showFeaturedArtists: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                return JSON.parse(raw).showFeaturedArtists !== false
            } catch (e) { /* fall through */ }
        }
        return true
    }
    // The animated eq bars carry the playing state in the play cell when the
    // pref is ON (TrackPlayCell.slint:99-100 `show-bars`): only while this
    // row's track is actually PLAYING, at rest (hover reveals the pause
    // affordance instead), and never in select mode. The Loader is part of
    // the performance contract: an expanded multidisc album may contain
    // hundreds of non-virtualized rows, and each eager EqualizerBars would
    // otherwise receive the shared 30 Hz pulse even while hidden. Exactly
    // one indicator exists: the active row while it is visible at rest.
    readonly property bool showBars: root.playIndicatorAnim
        && root.recycleActive && root.isActive && QbzPlayer.npPlaying && !root.hovered
        && !root.selectMode

    // --- Column geometry: rows/TrackCols.qml, NOT literals ---------------
    // Every width, the padding and the gap come from the one file that
    // rows/TrackListHeader.qml also reads, so the header above a list of
    // these rows lines up with them by construction. The title column's
    // arithmetic lives there too (`titleWidth`) — the header asks the same
    // function the same question and gets the same answer.
    TrackCols { id: cols; kioskHost: root.kioskHost }

    /// One chevron of the reorder gutter. Declared at the file's top level
    /// (an inline component must be — the same rule TrackListHeader's
    /// `ColLabel` follows), not inside the Row.
    ///
    /// It reads NOTHING off an outer id: `width` is set by the two call
    /// sites, which are in the outer scope and can reach `cols` there. An
    /// inline component that reaches for an enclosing id is legal only under
    /// the default (unbound) ComponentBehavior, and this file does not
    /// declare the pragma either way — so it simply does not depend on it.
    component ReorderChevron: Item {
        id: chev
        property string glyph: "chevron-up"
        property bool armed: true
        /// Read by the row's own `hovered` (see it): a hover-enabled MouseArea
        /// on top BLOCKS hover to the areas below it, so without republishing
        /// it here the row un-hovers the moment the pointer enters the gutter
        /// — the same defect this file already documents for the play cell.
        readonly property bool hovered: chevArea.containsMouse
        signal activated()
        height: 18
        QbzIcon {
            name: chev.glyph
            width: 14
            height: 14
            anchors.centerIn: parent
            opacity: chev.armed ? 1.0 : 0.35
            tintName: (chev.armed && chevArea.containsMouse) ? "textPrimary" : "muted"
        }
        MouseArea {
            id: chevArea
            anchors.fill: parent
            // ENABLED even when the chevron is not armed, and the click is
            // swallowed instead. A DISABLED MouseArea does not accept the
            // press at all, so it fell through to the row body underneath
            // (`trArea`, z:-1) — and this surface has `clickPlays: true`, so
            // clicking the greyed-out ↑ on the first row STARTED PLAYBACK.
            // The reference has no disabled state at all (its chevrons are
            // always armed and the Rust arm no-ops at the ends), so a dead
            // click there is genuinely dead; ours has to make it dead too.
            hoverEnabled: true
            cursorShape: chev.armed ? Qt.PointingHandCursor : Qt.ArrowCursor
            onClicked: {
                if (chev.armed)
                    chev.activated()
            }
        }
    }

    readonly property int titleColWidth: cols.titleWidth(root.width,
        root.showArtwork, root.showAlbum, root.showFavorite,
        root.showDownload, root.showMenu, root.showReorder, root.showSource)

    // Static now-playing mark: 3px accent pill on the left edge. With the
    // animation pref ON the eq bars in the play cell carry the state instead
    // (TrackRow.slint:127-137 — the pill is the pref-OFF arm).
    Rectangle {
        visible: root.isActive && !root.playIndicatorAnim
        x: 2
        y: 7
        width: 3
        height: parent.height - 14
        radius: 1.5
        color: theme.accent
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: cols.padH
        anchors.rightMargin: cols.padH
        spacing: cols.gap
        // THE DIM (contract §5.2). On the Row, not on `root`: the row's menu
        // and its now-playing edge mark are siblings of this item, and neither
        // should fade. 0.5 is views/purchases/PurchaseListRow.qml's value —
        // the only precedent for this state in the tree, so the two screens
        // agree.
        opacity: (root.pulledDead || root.playBlocked) ? 0.5 : 1.0

        // --- Reorder gutter (primitives/TrackRow.slint:270-311) ----------
        // The LEADING cell on a reorderable list: a 22px column with an
        // up/down chevron stack, 18px each. It is the affordance a user who
        // cannot (or does not want to) drag reorders with — the reference
        // ships it and the port had only the drag, which is half of owner
        // finding 8.
        //
        // Reuse note: the Playlist Manager's equivalent is
        // views/playlistmanager/PmGridCard.qml's 22px header (chevron-up ·
        // grip-vertical · chevron-down through PmActionButton). That one is
        // a GRID card with 140px of width to spend; this is a 50px table row
        // whose gutter is 22px wide, so the same three-across header does not
        // fit and PmActionButton's 22px disc would leave no room for the
        // second chevron. The geometry here is the reference row's own.
        Item {
            visible: root.showReorder
            width: cols.colReorder
            height: parent.height

            Column {
                anchors.verticalCenter: parent.verticalCenter
                spacing: 0
                ReorderChevron {
                    id: chevUp
                    height: root.kioskHost ? 44 : 18
                    width: cols.colReorder
                    glyph: "chevron-up"
                    armed: root.canMoveUp
                    onActivated: root.moveUpRequested()
                }
                ReorderChevron {
                    id: chevDown
                    height: root.kioskHost ? 44 : 18
                    width: cols.colReorder
                    glyph: "chevron-down"
                    armed: root.canMoveDown
                    onActivated: root.moveDownRequested()
                }
            }
        }

        // Number cell — the number-variant of primitives/TrackPlayCell.slint
        // (:172-232), the play affordance shared by every track row.
        //
        // --- LIGHT-THEME LEGIBILITY (Slint 2.0.2 / #638, "Track-row play
        // --- glyph … legible on light themes") ---------------------------
        // TrackPlayCell.slint:201-207 states the defect and the fix in its
        // own words: "Circular backing so the play/pause glyph is legible on
        // EVERY theme (the old bare #ffffff glyph vanished on light themes —
        // white-on-white, no scrim like the artwork variant has). Fill/border/
        // shadow use the polarity-baked alpha ramp (black-alpha on light,
        // white-alpha on dark) and the glyph uses text-primary."
        //
        // The port had the pre-fix shape: a hardcoded `#3dffffff` disc, no
        // ring, and the "primary" icon bake — which is a literal #ffffff, not
        // text-primary. Both halves are now theme-correct: theme.alphaTier()
        // is white-based on dark themes and black-based on light (qbz-theme
        // colors.rs:125-129, republished per theme by theme_qt.rs), and
        // `playGlyphTint` is the real text-primary token, runtime-tinted.
        //
        // Numbers off the .slint: 24px disc, 1.5px ring ("a 1px stroked circle
        // aliases badly in femtovg", :215), 16px glyph.
        // The accent-ring arm (:208-229) is the playing row: transparent fill,
        // accent circumference, accent glyph.
        // POC-NOTE: the .slint also drops a 6px `card-shadow` under the disc.
        // Qt's DropShadow is a Qt5Compat graphical effect and renders NOTHING
        // on this port's software path (the finding that killed ColorOverlay
        // in QbzIcon), so the shadow is dropped rather than faked; the ring
        // carries the separation on its own.
        Item {
            id: playCell
            width: cols.colNumber
            height: 40
            anchors.verticalCenter: parent.verticalCenter
            // `show-overlay: row-hovered || is-active` (TrackPlayCell.slint
            // :93) — the playing row keeps the affordance at rest, which is
            // what the accent ring is FOR. The port showed it on hover only,
            // so the ring never appeared.
            // A dead row never offers the play disc: the affordance IS the
            // claim that clicking does something.
            readonly property bool showOverlay: (root.hovered || root.isActive)
                && !root.pulledDead && !root.playBlocked
            // `accent-ring` (:108): the playing row's static form — only with
            // the animation pref OFF; with it ON the eq bars carry the state.
            readonly property bool accentRing: !root.playIndicatorAnim
                && root.isActive && QbzPlayer.npPlaying
            // `show-bars` (TrackPlayCell.slint:184-187): the animated eq bars
            // replace the disc at rest on the playing row. The Loader avoids
            // constructing a pulse receiver in every inactive track row.
            Loader {
                active: root.showBars
                anchors.centerIn: parent
                sourceComponent: EqualizerBars {
                    active: true
                    tint: theme.accent
                }
            }
            // Selection checkbox — TrackRow.slint:392-419: a 13px disc, accent
            // when selected, muted/text-primary ring when not. It REPLACES the
            // number and the play disc (both gate on !selectMode below), so
            // the leading column keeps its width and nothing else shifts.
            Rectangle {
                visible: root.selectMode
                anchors.centerIn: parent
                width: 14
                height: 14
                radius: 7
                color: root.checked ? theme.accent : "transparent"
                border.width: root.checked ? 0 : 1.5
                border.color: selArea.containsMouse ? theme.textPrimary : theme.textMuted
                Behavior on color { ColorAnimation { duration: 120 } }
                QbzIcon {
                    visible: root.checked
                    name: "check"
                    width: 8
                    height: 8
                    anchors.centerIn: parent
                    // The glyph sits ON the accent fill — the same selector
                    // controls/QbzCheckbox.qml routes through, not a literal
                    // #ffffff (which is under 2.6:1 on 16 of the 35 palettes).
                    tintName: theme.accentGlyphTint
                }
                MouseArea {
                    id: selArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.toggleSelect(Qt.NoModifier)
                }
            }
            Text {
                visible: !playCell.showOverlay && !root.selectMode && !root.pulledDead
                    && root.leadingMarkerIcon === ""
                anchors.centerIn: parent
                text: root.number
                color: theme.textMuted
                font.pixelSize: 13
            }
            // PULLED FROM THE CATALOGUE — the alert glyph takes the track
            // number's slot, so the leading column still says "what this row
            // is" and nothing reflows.
            //
            // `circle-alert`, NOT `triangle-alert` (contract §A F11): the
            // download column of THIS row already draws `triangle-alert` for
            // `cacheStatus === 4` (a failed cache), and two identical glyphs
            // meaning two different things on one 50px row is a defect, not a
            // style. `circle-alert` is also what the Tauri frontend used for
            // exactly this state (TrackRow.svelte's CircleAlert), so the
            // meaning is not being invented here. Both glyphs exist in every
            // tint dir (qml/assets/icons/*/circle-alert.svg).
            //
            // A Loader, so a normal row instantiates NOTHING — not the glyph,
            // not the MouseArea, and not the attached ToolTip, which is a
            // QObject per item the moment it is referenced. A 16K-row list
            // must not pay for a state a handful of rows are ever in.
            Loader {
                anchors.centerIn: parent
                width: 20
                height: 20
                active: root.pulledDead && !root.selectMode
                visible: active
                sourceComponent: Item {
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "circle-alert"
                        width: 15
                        height: 15
                        tintName: "favorite"
                    }
                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        // No cursorShape override: an Arrow over an inert row
                        // is the honest signal, and PurchaseListRow.qml does
                        // the same on its unavailable rows.
                        ToolTip.visible: containsMouse
                        ToolTip.delay: 300
                        // A pre-release (`qobuzUpcoming`: streaming rights
                        // start in the future) is "not yet"; anything else
                        // unavailable keeps the REUSED msgid the reactive
                        // skip path toasts.
                        ToolTip.text: root.item.qobuzUpcoming === true
                            ? QbzSession.tr("This track is not yet available", QbzSession.trRev)
                            : QbzSession.tr("This track is no longer available", QbzSession.trRev)
                    }
                }
            }
            Rectangle {
                visible: playCell.showOverlay && !root.showBars && !root.selectMode
                anchors.centerIn: parent
                width: 24
                height: 24
                radius: 12
                color: playCell.accentRing ? "transparent" : root.playCellFill
                border.width: 1.5
                border.color: playCell.accentRing ? theme.accent : root.playCellRing
                QbzIcon {
                    anchors.centerIn: parent
                    // `show-pause` (:101-104): PAUSE only while the playing
                    // row is hovered — at rest the playing row shows PLAY,
                    // "an affordance, not a state badge: the accent ring +
                    // the row's edge mark carry the state".
                    name: playCell.accentRing && root.hovered ? "pause" : "play-fill"
                    width: 16
                    height: 16
                    tintName: playCell.accentRing ? "accent" : root.playGlyphTint
                }
            }
            // TrackPlayCell.slint:234-243 — the cell is its OWN click target:
            // a non-current cell plays the track, the current cell toggles
            // play/pause. Without it the circle was a control that rendered
            // and did nothing on the album view (clickPlays:false there, so
            // the press fell through to the row body, which only reacts to a
            // double-click). Declared LAST so it sits ABOVE the glyph, and it
            // is above the row-body area regardless (that one is pinned z:-1).
            MouseArea {
                id: playArea
                anchors.fill: parent
                // Declared BEFORE the checkbox in z-order terms? No — the
                // checkbox is declared FIRST and this fills the same cell, so
                // it would swallow the checkbox's press. Disabling it in
                // select mode is what hands the cell over (the play
                // affordance is invisible there anyway).
                //
                // The cell is its OWN click target (TrackPlayCell.slint
                // :234-243), so disabling the row body is not enough: without
                // the `pulledDead` term, clicking the alert glyph would start
                // a play that cannot succeed.
                enabled: !root.selectMode && !root.pulledDead && !root.playBlocked
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    if (root.isActive) QbzPlayer.togglePlay()
                    else root.playRequested()
                }
            }
        }
        // 36px artwork cell (showArtwork arm).
        Rectangle {
            visible: root.showArtwork
            width: cols.colArt
            height: cols.colArt
            anchors.verticalCenter: parent.verticalCenter
            radius: 4
            color: theme.surfaceElevated
            // No clip: RoundedImage confines itself on both of its arms, and a
            // rectangular scissor never produced this radius anyway. Same
            // per-row batch-root cost as the quality cell above.
            RoundedImage {
                anchors.fill: parent
                source: root.showArtwork ? (root.item.artPath || "") : ""
                radius: 4
            }
            // Per-item cover placeholder — clears when THIS row's cover lands,
            // which is what makes a long list read as progressive instead of
            // filling in one lump. Host views drive the three properties;
            // default-off, so no existing call site changes.
            QbzSkeleton {
                variant: "art"
                anchors.fill: parent
                blockRadius: 4
                visible: root.artPending
                phase: root.skelPhase
                settleMs: root.artSettleMs
            }
        }
        Item {
            visible: root.showSource
            width: cols.colSource
            height: parent.height
            SourceIcon {
                anchors.centerIn: parent
                kind: root.item.source || "qobuz"
                mono: true
                glyphSize: 15
                plexSize: 16
                qobuzSize: 16
                localTint: "muted"
            }
        }
        // Title (+ explicit) / artist.
        Column {
            width: root.titleColWidth
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Row {
                spacing: 6
                Text {
                    text: root.item.title || ""
                    // Muted, not just dimmed: the Row's 0.5 opacity fades the
                    // whole cell uniformly, and the TITLE is the part that has
                    // to stop reading as live content.
                    color: (root.pulledDead || root.playBlocked)
                        ? theme.textMuted : theme.textPrimary
                    font.pixelSize: root.kioskHost ? 16 : 14
                    font.weight: theme.weightMedium
                    elide: Text.ElideRight
                    width: Math.min(implicitWidth, parent.parent.width
                        - (root.item.explicit ? 22 : 0)
                        - (root.pulledCached ? 22 : 0))
                }
                Rectangle {
                    visible: root.item.explicit === true
                    width: 16
                    height: 16
                    radius: 3
                    anchors.verticalCenter: parent.verticalCenter
                    color: theme.surfaceElevated
                    Text {
                        anchors.centerIn: parent
                        text: "E"
                        color: theme.textMuted
                        font.pixelSize: 10
                        font.weight: theme.weightSemibold
                    }
                }
                // CACHED BUT PULLED (contract §5.3, finding F5). Qobuz removed
                // the track, but the user DOWNLOADED it and `qbz-core` resolves
                // the offline tier before any availability question, so it
                // still plays perfectly. This row is therefore NOT the dead
                // row — it keeps its play cell, its heart and its click, and
                // all it gains is an honest badge, because the user still needs
                // to know it is gone from the catalogue: the moment they clear
                // the download, it dies.
                //
                // `cloud-off`, which reads as "not in the cloud any more" and
                // collides with nothing on this row (the download column shows
                // `circle-check-big` for the copy that makes this state
                // possible). Loader for the same per-row cost reason as the
                // dead glyph above.
                Loader {
                    anchors.verticalCenter: parent.verticalCenter
                    width: 16
                    height: 16
                    active: root.pulledCached
                    visible: active
                    sourceComponent: Item {
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "cloud-off"
                            width: 14
                            height: 14
                            tintName: "muted"
                        }
                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            ToolTip.visible: containsMouse
                            ToolTip.delay: 300
                            // NEW msgid — the one string this state needs and
                            // the catalogs do not carry (the i18n lane adds it
                            // to all eight). Not foldable into "This track is
                            // no longer available": that sentence would be a
                            // lie on a row that is playing right now.
                            ToolTip.text: QbzSession.tr(
                                "No longer on Qobuz — playing your downloaded copy",
                                QbzSession.trRev)
                        }
                    }
                }
            }
            Text {
                id: artistLine
                width: parent.width
                visible: (root.item.artist || "") !== ""
                // Main artist first, then the featured performers, one comma
                // separated list. Local / media-server rows carry no
                // `featured` and stay as they are. With links enabled and
                // featured names present, every name is its own StyledText
                // link: the main artist opens its page as before; a featured
                // name opens its page when the album credits gave it an id
                // and the search for that name otherwise. StyledText elides
                // like plain text (probed: truncated stays true on one line).
                readonly property var featured: root.showFeaturedArtists ? (root.item.featured || []) : []
                readonly property bool linkMode: root.artistLink && featured.length > 0
                function esc(s) {
                    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
                }
                function hex(c) {
                    return "#" + [c.r, c.g, c.b].map(function (v) {
                        return ("0" + Math.round(v * 255).toString(16)).slice(-2)
                    }).join("")
                }
                function link(href, name) {
                    var hot = artistLine.hoveredLink === href
                    return '<a href="' + href + '"><font color="'
                        + hex(hot ? theme.textPrimary : theme.textMuted) + '">' + esc(name) + '</font></a>'
                }
                textFormat: linkMode ? Text.StyledText : Text.PlainText
                text: {
                    var names = featured.map(function (f) { return f.name })
                    if (!linkMode)
                        return (root.item.artist || "") + (names.length ? ", " + names.join(", ") : "")
                    var parts = [root.item.artistId ? link("main", root.item.artist) : esc(root.item.artist)]
                    for (var i = 0; i < featured.length; i++)
                        parts.push(link("f:" + i, featured[i].name))
                    return parts.join(", ")
                }
                linkColor: theme.textMuted
                color: root.artistLink && root.item.artistId && artistLinkArea.containsMouse
                    ? theme.textPrimary : theme.textMuted
                font.pixelSize: 12
                elide: Text.ElideRight
                onLinkActivated: function (href) {
                    if (href === "main") {
                        if (root.routeGoToExternally) root.goToRequested("artist")
                        else QbzArtist.openArtist(root.item.artistId)
                        return
                    }
                    var f = featured[parseInt(href.slice(2))]
                    if (!f) return
                    if (f.id) QbzArtist.openArtist(f.id)
                    else QbzSearch.searchSubmit(f.name)
                }
                HoverHandler {
                    enabled: artistLine.linkMode
                    cursorShape: artistLine.hoveredLink !== "" ? Qt.PointingHandCursor : Qt.ArrowCursor
                }
                MouseArea {
                    id: artistLinkArea
                    anchors.fill: parent
                    enabled: root.artistLink && !!root.item.artistId && !artistLine.linkMode
                    hoverEnabled: true
                    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                    // Same routing as the menu's "Go to artist" — Slint drives
                    // both through one handler, so gating only the menu would
                    // leave the LINK navigating to the track's own artist.
                    onClicked: {
                        if (root.routeGoToExternally) root.goToRequested("artist")
                        else QbzArtist.openArtist(root.item.artistId)
                    }
                }
            }
        }
        // Album (link) column (showAlbum arm).
        Text {
            visible: root.showAlbum
            width: cols.colAlbum
            anchors.verticalCenter: parent.verticalCenter
            text: root.item.album || ""
            color: (albumArea.containsMouse && albumArea.enabled) ? theme.accent : theme.textMuted
            font.pixelSize: 12
            elide: Text.ElideRight
            MouseArea {
                id: albumArea
                anchors.fill: parent
                enabled: !!root.item.albumId && !root.releaseGone
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                // The names ride along so a release the catalog lost can
                // still be named on its page (album_qt::publish_unavailable).
                onClicked: QbzAlbum.openAlbumFrom(root.item.albumId, root.item.album || "", root.item.artist || "")
            }
        }
        // Duration.
        Text {
            width: cols.colDuration
            anchors.verticalCenter: parent.verticalCenter
            text: root.item.duration || ""
            color: theme.textMuted
            font.pixelSize: 12
                horizontalAlignment: Text.AlignHCenter
            }
            QbzIcon {
                visible: !playCell.showOverlay && !root.selectMode
                    && root.leadingMarkerIcon !== ""
                anchors.centerIn: parent
                name: root.leadingMarkerIcon
                width: 13
                height: 13
                tintName: "accent"
            }
        // Quality (92px) — the BARE badge, 1:1 with primitives/TrackRow.slint
        // 578-592: a 92px cell, `alignment: center` on both axes, holding a
        // QualityBadgeFull with `show-icon: false` + `bare: true`. That is the
        // tier label ("CD"/"HI-RES"/"MP3"/"LOSSLESS") stacked over the exact
        // bit-depth / sample-rate line, with no chip background or border, so
        // it blends into the row instead of reading as a contained badge.
        // The .slint has ONE form here — no icon variant, no bare-text
        // variant — which is why `qualityStyle` above is inert.
        //
        // The cell keeps its FIXED 92px. That number is a term of
        // `cellsRight`, which is what sizes the title column, so a wider badge
        // MUST NOT widen the cell or the title would be pushed into the
        // trailing controls. The badge is centred and the cell clips; at 8/9px
        // the longest real detail ("24-bit / 352.8 kHz") measures ~80px, so
        // the clip is a guard, never the normal path.
        //
        // Cheaper than what it replaces, too: QualityMini resolves to
        // QualityBadge, which carries a ToolTip popup and a hover MouseArea
        // per row; this one is two Texts.
        Item {
            width: cols.colQuality
            height: parent.height
            // No `clip: true`: a clip is an unconditional batch root
            // (qsgbatchrenderer visitClipNode), so one here cost a draw call
            // PER VISIBLE ROW and severed merging on both sides of it. The
            // badge bounds itself instead — real details measure ~80px in a
            // 92px cell, so the elide never fires.
            QualityBadgeFull {
                anchors.centerIn: parent
                tier: root.item.qualityTier || ""
                detail: root.item.qualityDetail || ""
                showIcon: false
                bare: true
                maxTextWidth: cols.colQuality
            }
        }
        // Favorite (showFavorite arm).
        Rectangle {
            visible: root.showFavorite
            width: cols.colFavorite
            height: cols.colFavorite
            radius: theme.radiusSm
            anchors.verticalCenter: parent.verticalCenter
            color: favArea.containsMouse ? theme.surfaceElevated : "transparent"
            QbzIcon {
                // The SLOT stays (the cell keeps its `visible` arm) and only
                // the glyph goes. Hiding the whole cell would collapse it out
                // of the Row and shift every column left of it — and
                // rows/TrackListHeader.qml lays out from the same TrackCols
                // object, so the header would no longer name the columns under
                // it. Suppressing the affordance without moving the grid is
                // what the reference did too (Tauri PlaylistDetailView
                // :2712-2713).
                visible: !root.pulledDead
                anchors.centerIn: parent
                name: root.favorite ? "heart-filled" : "heart"
                width: 16
                height: 16
                // TrackRow.slint:619 — hover raises the glyph to
                // Theme.text-primary, NOT to white (same #638 hole).
                tintName: root.favorite
                    ? "favorite"
                    : (favArea.containsMouse ? root.playGlyphTint : "muted")
            }
            MouseArea {
                id: favArea
                anchors.fill: parent
                // TrackRow.slint:527/558/623/688 — the in-row controls stand
                // down in select mode so their click reaches the row body.
                // Without it, aiming at a row and hitting the heart
                // un-favourites a track the user meant to tick. A dead row's
                // heart is not drawn at all (above), so the area under it must
                // not stay live either.
                enabled: !root.selectMode && !root.pulledDead
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.toggleFavorite()
            }
        }
        // Download slot (showDownload arm) — the offline-cache column,
        // TrackRow.slint:639-702: cloud when absent, spinner while queued /
        // downloading, accent check when ready, red alert on failure. Click
        // map: ready -> remove, anything else -> (re)download. The slot is
        // reserved wherever showDownload is on so the grid stays aligned;
        // the glyph itself also shows when a surface that hides the column
        // has a row with a live copy (`cacheStatus > 0`).
        Item {
            visible: root.showDownload || (root.hasOfflineCacheSeam && root.cacheStatus > 0)
            width: cols.colDownload
            height: cols.colDownload
            anchors.verticalCenter: parent.verticalCenter

            QbzIcon {
                // A pulled track has no file url to fetch, so the download
                // affordance could only ever fail — the slot keeps its width
                // (see the favourite cell for why) and loses its glyph.
                visible: root.cacheStatus === 0 && !root.pulledDead
                anchors.centerIn: parent
                name: "cloud-download"
                width: 16
                height: 16
                tintName: dlArea.containsMouse ? root.playGlyphTint : "muted"
            }
            QbzSpinner {
                visible: root.cacheStatus === 1 || root.cacheStatus === 2
                anchors.centerIn: parent
                size: 16
            }
            QbzIcon {
                visible: root.cacheStatus === 3
                anchors.centerIn: parent
                name: "circle-check-big"
                width: 16
                height: 16
                tintName: "accent"
            }
            QbzIcon {
                // Never alongside the pulled-track alert in the number cell:
                // one row, one alert.
                visible: root.cacheStatus === 4 && !root.pulledDead
                anchors.centerIn: parent
                name: "triangle-alert"
                width: 16
                height: 16
                tintName: "favorite"
            }
            MouseArea {
                id: dlArea
                anchors.fill: parent
                enabled: root.hasOfflineCacheSeam && !root.selectMode && !root.pulledDead
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    if (root.cacheStatus === 3) QbzPlayer.uncacheTrack(root.item.id)
                    else QbzPlayer.cacheTrack(root.item.id)
                }
            }
        }
        // ⋯ menu (showMenu arm).
        Rectangle {
            visible: root.showMenu
            width: cols.colMenu
            height: cols.colMenu
            radius: theme.radiusSm
            anchors.verticalCenter: parent.verticalCenter
            color: moreArea.containsMouse ? theme.surfaceElevated : "transparent"
            QbzIcon {
                anchors.centerIn: parent
                name: "ellipsis"
                width: 16
                height: 16
                // TrackRow.slint:750 — Theme.text-primary on hover.
                tintName: moreArea.containsMouse ? root.playGlyphTint : "muted"
            }
            MouseArea {
                id: moreArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: function (mouse) { root.openRowMenu(moreArea, mouse.x, mouse.y) }
            }
        }
    }
    // LAZY. This is a QtQuick.Controls Popup with a Repeater over its
    // entries, and it was built EAGERLY for every row — so a long list
    // constructed one whole popup per visible row, and rebuilt them all
    // on every scroll step, to show a menu only ever opened on click.
    // Same lazy-Loader idiom this file already uses for the clipboard
    // helper and the info modal.
    Loader {
        id: rowMenuLoader
        active: false
        sourceComponent: CardMenu { kioskHost: root.kioskHost;
            menuWidth: 224
            entries: root.menuModel()
            onPicked: function (a) { root.menuAction(a) }
        }
    }
    /// Build the popup on first use, then open it. Called by `openRowMenu`
    /// below, which owns the empty-menu guard.
    function openRowMenuLazy(anchor, x, y) {
        root._menuAnchor = anchor
        root._menuX = x
        root._menuY = y
        rowMenuLoader.active = true
        rowMenuLoader.item.openAtCursor(anchor, x, y)
    }

    // --- Copy submenu + clipboard (2026-09-13) --------------------------------
    // Both lazy, like the row menu: a list row must not carry a Popup tree
    // or a TextEdit per delegate. Reopened where the row menu was.
    property Item _menuAnchor: null
    property real _menuX: 0
    property real _menuY: 0
    Loader {
        id: copyMenuLoader
        active: false
        sourceComponent: CardMenu {
            kioskHost: root.kioskHost
            menuWidth: 224
            entries: [
                { "label": QbzSession.tr("Track name", QbzSession.trRev), "icon": "copy", "action": "copy-title" },
                { "label": QbzSession.tr("Track - Album - Artist", QbzSession.trRev), "icon": "clipboard", "action": "copy-full" }
            ]
            onPicked: function (a) { root.copyRow(a) }
        }
    }
    Loader {
        id: clipboardLoader
        active: false
        sourceComponent: QbzClipboard { }
    }
    function openCopyMenu() {
        if (!root._menuAnchor)
            return
        copyMenuLoader.active = true
        copyMenuLoader.item.openAtCursor(root._menuAnchor, root._menuX, root._menuY)
    }
    function copyRow(which) {
        var parts = [root.item.title || ""]
        if (which === "copy-full") {
            if ((root.item.album || "") !== "") parts.push(root.item.album)
            if ((root.item.artist || "") !== "") parts.push(root.item.artist)
        }
        clipboardLoader.active = true
        clipboardLoader.item.copy(parts.join(" - "))
    }

    // TrackContextMenu.slint, in its order. Every row here reaches a live
    // seam; the seamless ones are gated off above.
    /// Open the row menu, unless it would come up EMPTY.
    ///
    /// A pulled track refuses transport, favourite, playlist/mixtape add, both
    /// share links, track info and the cache actions, so on a surface that does
    /// not offer the repair — an album page, search results — nothing is left.
    /// An empty popup is worse than no popup: it reads as a broken control
    /// rather than as "there is nothing to do with this row".
    function openRowMenu(anchor, x, y) {
        if (root.menuModel().length === 0)
            return
        root.openRowMenuLazy(anchor, x, y)
    }

    function menuModel() {
        if (root.menuEntriesOverride !== null)
            return root.menuEntriesOverride
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var m = []
        // A DEAD row does not get transport entries. D5 keeps an unavailable
        // track out of the queue at the seam, so "Play next" here would render
        // and do nothing — the rendered-and-inert defect the `has*Seam`
        // constants above already refuse ("OFF = the row is not built at all").
        // The row keeps its menu because the menu is the ONLY way to reach the
        // repair, which is exactly why the row must stay right-clickable.
        if (!root.pulledDead) {
            m.push({ "label": t("Play now", r), "icon": "play-fill", "action": "play" })
            m.push({ "label": t("Play next", r), "icon": "list-start", "action": "next" })
            // #442 "Play later" — the end of the MANUAL block (after everything
            // already queued by hand, before the source resumes).
            if (root.menuShowLater)
                m.push({ "label": t("Play later", r), "icon": "list-plus", "action": "later" })
            m.push({ "label": t("Add to queue", r), "icon": "list-end", "action": "queue" })
        }
        // An ephemeral row stops here: everything below writes something that
        // is supposed to survive the session, and there will be nothing left
        // to resolve.
        if (root.queueOnly)
            return m
        // THE REPAIR. First entry on a dead row, because it is the only reason
        // that row's menu is worth opening at all. Host-routed — see
        // `routeReplaceExternally`.
        if (root.hasReplaceSeam)
            m.push({ "label": t("Find available version", r), "icon": "refresh-cw",
                     "action": "find-replacement" })
        if (root.hasRadioSeam) {
            m.push({ "label": t("Create QBZ radio", r), "icon": "radio", "action": "radio-qbz" })
            m.push({ "label": t("Create Qobuz radio", r), "icon": "radio", "action": "radio-qobuz" })
        }
        // The menu and the heart cell agree: neither offers the favourite on a
        // dead row.
        if (root.menuShowFavorite && !root.pulledDead)
            m.push({ "label": root.favorite ? t("Remove from Library", r) : t("Add to Library", r),
                     "icon": root.favorite ? "heart-filled" : "heart", "action": "favorite" })
        if (root.hasMixtapeSeam)
            m.push({ "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" })
        if (root.hasPlaylistAddSeam)
            m.push({ "label": t("Add to playlist", r), "icon": "list-music", "action": "add-to-playlist" })
        if (root.menuShowRemove)
            m.push({ "label": t("Remove from playlist", r), "icon": "trash-2",
                     "action": "remove", "danger": true })
        // Neither share link survives a pull: the Qobuz page for a withdrawn
        // track is gone, and Song.link resolves THROUGH it. Both rendered and
        // led nowhere, which is the same rendered-and-inert defect the
        // transport entries above are refused for.
        if (root.menuShowShare && !root.pulled)
            m.push({ "label": t("Share Qobuz link", r), "icon": "link", "action": "share-qobuz" })
        if (root.hasSonglinkSeam && !root.pulled)
            m.push({ "label": t("Share Song.link", r), "icon": "link", "action": "share-songlink" })
        // The dead row offers no cache action at all (there is no stream url
        // left to fetch), and the cached-but-pulled row must not be able to
        // destroy its only copy with a refresh.
        if (root.hasOfflineCacheSeam && !root.pulledDead) {
            // TrackContextMenu.slint:174-199 — the offline block tracks the
            // row's live status: not-cached rows get the download arm, ready
            // rows get refresh + remove.
            if (root.cacheStatus === 3) {
                // A REFRESH on a pulled track would re-fetch a stream url that
                // no longer exists and can only end with the copy gone — the
                // one copy that is still playing. Remove stays: the user may
                // legitimately want the disk space, and it is their call.
                if (!root.pulledCached)
                    m.push({ "label": t("Refresh offline copy", r), "icon": "refresh-cw", "action": "recache" })
                m.push({ "label": t("Remove offline copy", r), "icon": "trash-2", "action": "uncache", "danger": true })
            } else {
                m.push({ "label": t("Make available offline", r), "icon": "cloud-download", "action": "cache" })
            }
        }
        if (root.menuShowGoTo && root.item.albumId && !root.releaseGone)
            m.push({ "label": t("Go to album", r), "icon": "disc-3", "action": "go-album" })
        if (root.menuShowGoTo && root.item.artistId)
            m.push({ "label": t("Go to artist", r), "icon": "user", "action": "go-artist" })
        // Track info needs the catalog record the pull took away, so the modal
        // opens on nothing.
        if (root.menuShowTrackInfo && !root.pulled)
            m.push({ "label": t("Track info", r), "icon": "info", "action": "track-info" })
        // "Buy on Qobuz" (2026-09-13): the store page; store_qt answers with a
        // toast when the catalog says the release is not sold.
        if (root.catalogRow && !root.localSourceRow && !root.pulledDead)
            m.push({ "label": t("Buy on Qobuz", r), "icon": "shopping-bag", "action": "buy" })
        // Copy (2026-09-13): a submenu — the track name, or "Track - Album - Artist".
        if ((root.item.title || "") !== "")
            m.push({ "label": t("Copy", r), "icon": "copy", "action": "copy", "submenu": true })
        return m
    }

    function menuAction(a) {
        if (root.menuEntriesOverride !== null) {
            for (var i = 0; i < root.menuEntriesOverride.length; i++) {
                var entry = root.menuEntriesOverride[i]
                if (entry.action === a && entry.external === true) {
                    root.menuActionRequested(a)
                    return
                }
            }
        }
        if (a === "play") root.playRequested()
        else if (a === "next") root.enqueueRequested("next")
        else if (a === "later") root.enqueueRequested("later")
        else if (a === "queue") root.enqueueRequested("queue")
        else if (a === "go-artist") {
            if (root.routeGoToExternally) root.goToRequested("artist")
            else QbzArtist.openArtist(root.item.artistId)
        }
        else if (a === "go-album") {
            if (root.routeGoToExternally) root.goToRequested("album")
            else QbzAlbum.openAlbumFrom(root.item.albumId, root.item.album || "", root.item.artist || "")
        }
        else if (a === "buy") QbzAlbum.buyTrack(root.item.id || "")
        else if (a === "copy") root.openCopyMenu()
        else if (a === "favorite") root.toggleFavorite()
        else if (a === "mixtape") root.mixtapeRequested()
        // Add to playlist. The internal arm is the CATALOG one and nothing
        // else can reach it: `hasPlaylistAddSeam` only builds the entry for a
        // `catalogRow` that is not a local/plex row, or when the host has
        // taken the routing. Belt and braces on the dispatch too, so a host
        // that sets `routePlaylistAddExternally` and forgets the handler
        // cannot fall through into the Qobuz call with a local id.
        else if (a === "add-to-playlist") {
            if (root.routePlaylistAddExternally) root.playlistAddRequested()
            else if (root.catalogRow && !root.localSourceRow)
                QbzPlaylistPicker.openForTrack(root.item.id)
        }
        // "Find available version" — the HOST answers: only it knows the
        // playlist id, whether the user owns it, and that this row is a
        // catalog membership. `hasReplaceSeam` already gates the entry on the
        // host having taken the routing, so there is no internal arm to fall
        // through to.
        else if (a === "find-replacement") root.replaceRequested()
        else if (a === "remove") root.removeRequested()
        else if (a === "cache") QbzPlayer.cacheTrack(root.item.id)
        else if (a === "uncache") QbzPlayer.uncacheTrack(root.item.id)
        else if (a === "recache") QbzPlayer.recacheTrack(root.item.id)
        else if (a === "share-qobuz") QbzAlbum.shareTrackQobuz(root.item.id || "")
        else if (a === "share-songlink") QbzAlbum.shareTrackLink(root.item.id || "")
        else if (a === "track-info") root.openTrackInfo()
    }

    // --- Track info (QbzAlbum.openTrackInfo + shell/TrackInfoModal) ------
    // Same lazy-Loader reason: the modal is a full Popup tree and a list row
    // must not instantiate one per delegate. Activated on demand, torn down
    // on close (Qt.callLater so the Popup is not destroyed mid-signal).
    Loader {
        id: trackInfoLoader
        active: false
        sourceComponent: TrackInfoModal { }
    }
    Connections {
        target: trackInfoLoader.item
        ignoreUnknownSignals: true
        function onClosed() { Qt.callLater(function () { trackInfoLoader.active = false }) }
    }
    function openTrackInfo() {
        if (!root.item.id)
            return
        trackInfoLoader.active = true
        trackInfoLoader.item.openFor(root.item.id)
    }

    // Shared drag (the row BODY is the source — TrackRow.slint): press-drag
    // >6px starts it (bodyDragStarted fires FIRST, the #589 pre-hook);
    // release plays (clickPlays) or ignores (album view: double-click).
    property bool dragging: false
    property point downPos: Qt.point(0, 0)

    // --- DRAG vs. DRAG-TO-SCROLL (owner finding 8) -----------------------
    // The reference does not solve this because it does not HAVE it: Slint's
    // ListView is not mouse-drag-scrollable, so its row-body drag has no
    // competitor. Qt's is — every one of these rows lives in a Flickable, and
    // a Flickable filters its children's mouse events and takes the grab once
    // the press travels past `QStyleHints.startDragDistance` (~10px). Our
    // drag arms at 6px, so BOTH fire: the ghost appears, the list then takes
    // the grab, this area gets `canceled` and never `released`, `dragging`
    // stays true and the ghost never ends.
    //
    // The resolution is the one this port ALREADY made for the same collision
    // in shell/QueuePanel.qml:479-491 — `preventStealing` scoped to the rows
    // that are actually reorderable, described there as "the Qt equivalent of
    // QueueSidebar.slint's `interactive: false` on the queue Flickable, but
    // without that side effect: Qt's Flickable also drops WHEEL events when
    // it is non-interactive, so wheel + scrollbar keep working, and only a
    // press that starts ON a reorderable row is denied to the flick." Same
    // trade here, same shape, so the two reorderable lists in the app behave
    // identically: while a list is reorderable it is not drag-scrollable, and
    // the wheel and the scrollbar carry scrolling.
    //
    // Scoped to `showReorder` on purpose: on every OTHER surface the row-body
    // drag is the add-to-a-sidebar-playlist gesture, which shipped with the
    // >6px arm and drag-scroll both live, and re-timing it is not this
    // round's job. `onCanceled` below is the universal half — it costs no
    // gesture change and it is what stops a stolen drag from stranding the
    // ghost anywhere.
    //
    // Rejected: constraining the drag AXIS (a reorder drag is vertical, i.e.
    // the same axis as the scroll — it separates nothing), and moving the
    // drag source onto a grip handle (see the gutter above: 22 x 50 leaves no
    // room beside the two chevrons, and the chevrons already are the non-drag
    // affordance a grip would duplicate).

    /// Release / cancel teardown — ONE implementation, because the two used
    /// to be able to disagree about whether the shared drag had ended.
    function endBodyDrag() {
        if (root.dragging) {
            QbzShell.dragEnd()
            root.dragging = false
            return true
        }
        return false
    }
    // Called by a recycling host before this object enters ListView's pool.
    // Popups are window-overlay children, so merely hiding the delegate would
    // leave them open and tied to the wrong track after reuse.
    function releaseForReuse() {
        root.endBodyDrag()
        if (rowMenuLoader.item)
            rowMenuLoader.item.close()
        if (trackInfoLoader.item)
            trackInfoLoader.item.close()
        Qt.callLater(function () {
            if (!root.recycleActive) {
                rowMenuLoader.active = false
                trackInfoLoader.active = false
            }
        })
    }
    MouseArea {
        id: trArea
        anchors.fill: parent
        // BEHIND the row's own controls. Declaration order is z-order in QML,
        // and this fills the whole row from the LAST declaration — so it sat on
        // top of the heart / cloud / menu buttons and swallowed their presses.
        // propagateComposedEvents does not save them: `pressed` is not a
        // composed event, so only the row's own double-click still worked.
        z: -1
        hoverEnabled: true
        propagateComposedEvents: true
        // Right press opens the SAME menu as ⋯ (rowMenu), at the pointer.
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        // An Arrow over a dead row, whatever the surface's click policy — the
        // pointer must not promise a play that is blocked below.
        cursorShape: (!root.pulledDead && !root.playBlocked
                      && (root.clickPlays || root.selectMode))
            ? Qt.PointingHandCursor : Qt.ArrowCursor
        // Keep the grab so the enclosing Flickable cannot turn our drag into a
        // scroll — see the block above.
        //
        // TWO arms, because the two gestures need opposite trades:
        //
        //  reorder rows — held from the PRESS. A reorder drag is vertical,
        //      i.e. the same axis as the scroll, so there is nothing to
        //      discriminate on and the list gives up drag-scrolling.
        //  every other row — held from the moment OUR drag arms (`dragging`).
        //      Until then the press belongs to the Flickable, so vertical
        //      drag-to-scroll still works; once armed the grab is ours and the
        //      pointer can leave the row.
        //
        // That second arm is the AlbumView bug (owner, 2026-08-10: "el evento
        // dragging comienza pero muere cuando sale del track row"). Off a
        // reorderable list this was plain `false`: our drag armed at 6px, the
        // Flickable stole the grab at `startDragDistance` (~10px) a moment
        // later, `onCanceled` fired, and the drag died at roughly the row
        // boundary — never reaching the sidebar. Arming at 6px and claiming
        // the grab there wins the race by construction, because 6 < 10.
        preventStealing: (root.reorderDrag && root.dragSource && !root.selectMode)
                         || root.dragging
        onPressed: function (mouse) {
            if (mouse.button === Qt.RightButton) {
                // TrackRow.slint:201 — the row menu is suppressed in
                // multi-select mode (a right-click toggles nothing there).
                if (root.showMenu && !root.selectMode)
                    root.openRowMenu(trArea, mouse.x, mouse.y)
                mouse.accepted = true
                return
            }
            root.downPos = Qt.point(mouse.x, mouse.y)
        }
        onPositionChanged: function (mouse) {
            // Only a LEFT press drags — a right press is the context gesture.
            // Dragging a row onto a sidebar playlist is off in select mode
            // (TrackRow.slint keeps the whole body as a selection target).
            // A dead row is not a drag source either: dropping it on a sidebar
            // playlist would add a track that cannot play, which is precisely
            // the state this treatment exists to stop spreading.
            if (!pressed || !(pressedButtons & Qt.LeftButton) || !root.dragSource
                || root.selectMode || root.pulledDead || root.playBlocked) return
            const g = mapToItem(null, mouse.x, mouse.y)
            const dx = mouse.x - root.downPos.x
            const dy = mouse.y - root.downPos.y
            // ARMING, and it is axis-aware off a reorderable list.
            //
            // A reorder drag is VERTICAL — the same axis as the scroll — so
            // there is nothing to discriminate on and that arm stays "6px in
            // any direction" (the list has already given up drag-scrolling
            // via `preventStealing`, see below).
            //
            // Everywhere else the gesture is "carry this row to the sidebar
            // (or the queue)", which is HORIZONTAL, while drag-to-scroll is
            // vertical. So they separate cleanly, and both survive: a
            // vertical press is never claimed and the Flickable scrolls it; a
            // horizontal one arms here and takes the grab.
            //
            // The reference's own note rejects an axis constraint — but it
            // rejects it FOR REORDER, where it "separates nothing". Here it
            // separates everything, which is why this arm exists and that one
            // does not.
            const armed = root.reorderDrag
                ? (Math.abs(dx) > 6 || Math.abs(dy) > 6)
                : (Math.abs(dx) > 6 && Math.abs(dx) > Math.abs(dy))
            if (!root.dragging && armed) {
                root.dragging = true
                root.bodyDragStarted(root.number)
                if (root.localDragSource)
                    QbzShell.dragStartLocal(String(root.item.id),
                        root.item.title || "",
                        (root.item.artist || "") + " · " + (root.item.album || ""),
                        g.x, g.y, root.inlineDragVisual)
                else
                    QbzShell.dragStart(root.item.id, root.item.title || "",
                        (root.item.artist || "") + " · " + (root.item.album || ""),
                        g.x, g.y, root.inlineDragVisual)
            }
            if (root.dragging) QbzShell.dragMove(g.x, g.y)
        }
        onReleased: function (mouse) {
            if (mouse.button === Qt.RightButton)
                return
            const wasDragging = root.endBodyDrag()
            // In select mode the WHOLE row body is a selection target
            // (TrackRow.slint:174) — never a play.
            if (root.selectMode) {
                root.toggleSelect(mouse.modifiers)
                mouse.accepted = true
                return
            }
            // THE CLICK IS BLOCKED UP FRONT (D4): no request, no toast, no
            // error — the row already SAYS it is dead, so clicking it must be
            // a no-op rather than a failure the user has to read. The right
            // press never reaches here (it returns above), so the context menu
            // — the only route to the replacement action — still opens.
            //
            // AFTER the select-mode arm, not before it: in select mode a click
            // means "tick this row", not "play it", so blocking it there would
            // buy nothing (no play is issued either way) while leaving the row
            // half-selectable — its checkbox has its own live MouseArea in the
            // leading cell, and the bulk bar's "Select all" reaches it
            // regardless. A row the user cannot tick by clicking but CAN tick
            // by hitting the 14px disc is the worse of the two states.
            if (root.pulledDead || root.playBlocked) {
                mouse.accepted = true
                return
            }
            if (wasDragging) {
                mouse.accepted = true
            } else if (root.clickPlays) {
                // ONE play per gesture. A double-click is two releases plus
                // the doubleClicked handler: three `playRequested`s within
                // ~300 ms, i.e. three restarts of the same track (2026-08-29
                // smoke: three "served from OFFLINE cache" in 230 ms). The
                // second release inside the double-click window is dropped
                // here; the doubleClicked arm below already yields to
                // clickPlays rows.
                const now = Date.now()
                if (now - root._lastPlayRequestMs > 400) {
                    root._lastPlayRequestMs = now
                    root.playRequested()
                }
            } else {
                mouse.accepted = false
            }
        }
        // Grab lost (the Flickable stole it, the delegate was recycled, the
        // window lost focus). Without this the shared drag stayed "active"
        // with no way to end it and the ghost never disappeared.
        onCanceled: root.endBodyDrag()
        onDoubleClicked: function (mouse) {
            // ":156 — In select mode a double-click just selects (via
            // `clicked`); no play." The single-click release above has already
            // toggled twice by then, which lands back where it started, so the
            // double-click must not add a third.
            if (root.selectMode)
                return
            // The album view plays on DOUBLE-click (clickPlays:false there),
            // so the single-click block above is not enough on its own.
            if (root.pulledDead)
                return
            // A clickPlays row already played on the first release; a third
            // emission here restarted the track a third time.
            if (root.clickPlays)
                return
            if (mouse.button === Qt.LeftButton)
                root.playRequested()
        }
    }
}
