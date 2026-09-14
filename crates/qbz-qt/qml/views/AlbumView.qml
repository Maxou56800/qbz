// Album detail page — QML port of album/AlbumPageView.slint.
//
// Header (224px cover, title, credited-artist line, meta with label link,
// description + Read more, CircleAction row), divider, toolbar (quality
// badge + track search), column header, track list (Disc/work headers,
// TrackRow replica with the playing-row pill + number↔play cell + live
// heart), label/awards sidebar, and the two bottom carousels ("From the
// same artist", "Listening suggestions").
//
// POC-NOTEs: the offline download column (TrackRow status glyphs + ⋯ menu
// rows) and multi-select + bulk bar are LIVE. Still out: DiscHeaderMenu
// per-disc actions.
//
// Header atmosphere (AlbumPageView.slint:161-189, 221-257): the
// artwork-tinted band IS wired now, through the shared
// controls/HeaderGradient.qml (route B — see that file for why the blurred
// route is not available on this path). It brings the .slint's header-colour
// rules with it: with the band on, the header sits on a DARK backdrop, so
// the text goes light regardless of theme (`hdrStrong` / `hdrBody`,
// .slint:169-172) and the CircleActions switch to their overlay palette
// (`hdrOverlay` = the .slint's inverted `hdr-on-surface`, :179).

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import Qt.labs.qmlmodels
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../shell"
import "../theme"

Rectangle {
    id: root
    // Transparent while the ambient background is active (phase 14 —
    // HomeView.slint:163: the frosted content panel shows through).
    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn

    // Round to the AppShell content-frame bezel (Radius.md): QML clips
    // are rectangular, so the frame's own rounding never reaches the
    // view — the view's own fill must round instead.
    radius: 12

    QbzTheme { id: theme }

    /// Header cover edge. One constant because the real header, its skeleton
    /// twin and both text columns all measure off it — they drifted apart at
    /// five separate literals before. The Row has no explicit height, so the
    /// header grows with this on its own.
    readonly property int coverPx: compactHeaderPref ? 112 : 260
    readonly property int headerGapPx: compactHeaderPref ? 20 : 32

    /// How far past the header divider the artwork atmosphere reaches: the 8px
    /// spacer below the divider plus half of the 52px track toolbar band, so
    /// the gradient dies across the middle of the quality chip and the search
    /// box instead of stopping at the divider.
    readonly property int atmoReach: 1 + 8 + 26

    // The view's album + url-keyed cover map (artwork pipeline).
    //
    // Applied imperatively so a SAME-ALBUM deferred rail publish can preserve
    // the ListView offset. Binding `model` to a freshly parsed JS array makes
    // QQuickItemView reset contentY whenever Similar albums / Suggestions / More
    // from this artist lands — the forced jump the owner caught while reading.
    property var album: ({})
    property string documentAlbumId: ""
    property int albumDocumentEpoch: 0
    property real albumRestoreY: 0

    function parseAlbumDocument() {
        try {
            return JSON.parse(QbzAlbum.albumJson)
        } catch (e) {
            return ({})
        }
    }

    function applyAlbumDocument() {
        var next = root.parseAlbumDocument()
        var nextHeader = next.header || ({})
        var nextId = nextHeader.id !== undefined && nextHeader.id !== null
            ? String(nextHeader.id) : ""
        var sameAlbum = nextId !== "" && nextId === root.documentAlbumId
        var savedY = sameAlbum ? pageFlick.contentY : 0
        root.albumDocumentEpoch += 1
        var epoch = root.albumDocumentEpoch
        root.album = next
        if (nextId !== "")
            root.documentAlbumId = nextId
        if (!sameAlbum || savedY <= pageFlick.originY)
            return
        root.albumRestoreY = savedY
        // The model reset and its content-height polish happen after the
        // document assignment. One deferred restore coalesces a publish burst
        // and clamps if the replacement document is genuinely shorter.
        Qt.callLater(function () {
            if (epoch !== root.albumDocumentEpoch
                    || nextId !== root.documentAlbumId)
                return
            pageFlick.forceLayout()
            var minY = pageFlick.originY
            var maxY = minY + Math.max(0, pageFlick.contentHeight - pageFlick.height)
            pageFlick.contentY = Math.max(minY, Math.min(root.albumRestoreY, maxY))
        })
    }

    readonly property var albumHeader: album.header || ({})
    readonly property var tracks: album.tracks || []
    readonly property var purchase: album.purchase || ({})
    readonly property var localPurchaseVariants: purchase.localVariants || []
    readonly property bool showPlaybackSource: {
        for (var i = 0; i < localPurchaseVariants.length; i++) {
            if (localPurchaseVariants[i].selectable === true)
                return true
        }
        return false
    }

    function albumTrackIdsJson() {
        var ids = []
        for (var i = 0; i < root.tracks.length; i++) {
            var id = String(root.tracks[i].id || "")
            if (/^\d+$/.test(id)) ids.push(id)
        }
        return JSON.stringify(ids)
    }

    function playbackSourceOptions() {
        var out = [{
            "label": QbzSession.tr("Qobuz · stream/offline copy", QbzSession.trRev),
            "mode": "qobuz", "formatId": 0, "enabled": true
        }]
        var selected = Number(root.purchase.selectedFormatId || 0)
        for (var i = 0; i < root.localPurchaseVariants.length; i++) {
            var variant = root.localPurchaseVariants[i]
            if (variant.selectable !== true && Number(variant.formatId) !== selected)
                continue
            var label = QbzSession.tr("Local purchase", QbzSession.trRev)
                + " · " + (variant.label || String(variant.formatId))
            if (variant.selectable !== true)
                label += " · " + QbzSession.tr("Unavailable", QbzSession.trRev)
            out.push({ "label": label, "mode": "purchase",
                       "formatId": Number(variant.formatId),
                       "enabled": variant.selectable === true })
        }
        return out
    }

    function playbackSourceIndex(options) {
        if ((root.purchase.playbackMode || "qobuz") !== "purchase")
            return 0
        var selected = Number(root.purchase.selectedFormatId || 0)
        for (var i = 1; i < options.length; i++) {
            if (Number(options[i].formatId) === selected)
                return i
        }
        return 0
    }

    readonly property bool purchaseStatusVisible: {
        var state = root.purchase.entitlementState || ""
        if (state === "owned-album" || state === "owned-tracks")
            return true
        if (state !== "stale")
            return false
        var known = root.purchase.lastKnownEntitlementState || ""
        return known === "owned-album" || known === "owned-tracks"
    }
    readonly property string purchaseStatusText: {
        var known = root.purchase.entitlementState === "stale"
            ? root.purchase.lastKnownEntitlementState
            : root.purchase.entitlementState
        if (known === "owned-tracks") {
            var n = Number(root.purchase.purchasedTrackCount || 0)
            return n === 1
                ? QbzSession.tr("1 track purchased", QbzSession.trRev)
                : QbzSession.tr("{} tracks purchased", QbzSession.trRev)
                    .replace("{}", String(n))
        }
        return QbzSession.tr("Purchased", QbzSession.trRev)
    }

    function selectedPlaybackVariant() {
        if ((root.purchase.playbackMode || "qobuz") !== "purchase")
            return null
        var selected = Number(root.purchase.selectedFormatId || 0)
        for (var i = 0; i < root.localPurchaseVariants.length; i++) {
            if (Number(root.localPurchaseVariants[i].formatId) === selected)
                return root.localPurchaseVariants[i]
        }
        return null
    }

    function playbackQualityTier() {
        var variant = root.selectedPlaybackVariant()
        if (!variant)
            return albumHeader.qualityTier || ""
        var formatId = Number(variant.formatId || 0)
        if (formatId === 56 || formatId === 55) return "dsd"
        if (formatId === 27 || formatId === 7) return "hires"
        if (formatId === 6) return "cd"
        if (formatId === 5) return "mp3"
        return albumHeader.qualityTier || ""
    }

    function playbackQualityDetail() {
        var variant = root.selectedPlaybackVariant()
        return variant ? (variant.label || "") : (albumHeader.qualityDetail || "")
    }

    function selectPlaybackSource(option) {
        if (!option || option.enabled !== true)
            return
        var currentMode = root.purchase.playbackMode || "qobuz"
        var currentFormat = Number(root.purchase.selectedFormatId || 0)
        if (option.mode === currentMode
                && (option.mode !== "purchase"
                    || Number(option.formatId) === currentFormat))
            return
        QbzPurchases.setAlbumPlaybackMode(
            albumHeader.id, option.mode,
            Number(option.formatId), root.albumTrackIdsJson())
    }
    // One global opt-in for every album page. `settingsJson` is the live
    // channel (including clicks made in another open AlbumView); the album
    // document carries the persisted cold-start fallback until that snapshot
    // arrives.
    readonly property bool compactHeaderPref: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                var d = JSON.parse(raw)
                if (d.compactAlbumHeader !== undefined)
                    return d.compactAlbumHeader === true
            } catch (e) { /* fall through to the document copy */ }
        }
        return album.compactHeader === true
    }
    property bool headerLayoutReady: false
    property int headerLayoutEpoch: 0
    property real headerSavedOffset: 0
    property bool headerSnapshotValid: false
    function toggleCompactHeader() {
        // Capture before settingsJson changes: dependent geometry bindings and
        // this property's change handler have no guaranteed observer order.
        root.headerSavedOffset = pageFlick.contentY - pageFlick.originY
        root.headerSnapshotValid = true
        QbzBridge.settingsBool("compact-album-header", !root.compactHeaderPref)
    }
    onCompactHeaderPrefChanged: {
        if (!root.headerLayoutReady)
            return
        // An inline ListView header changes originY with its height. Keep the
        // viewport's offset from that origin so expanding pins the visible top
        // edge and grows the header downward instead of behind the viewport.
        var savedOffset = root.headerSnapshotValid
            ? root.headerSavedOffset
            : pageFlick.contentY - pageFlick.originY
        root.headerSnapshotValid = false
        var epoch = ++root.headerLayoutEpoch
        Qt.callLater(function () {
            if (epoch !== root.headerLayoutEpoch)
                return
            pageFlick.forceLayout()
            var minY = pageFlick.originY
            var maxY = minY + Math.max(0, pageFlick.contentHeight - pageFlick.height)
            pageFlick.contentY = Math.max(minY,
                Math.min(minY + savedOffset, maxY))
        })
    }
    // `header.awards`, NOT `album.awards`. It is a field of AlbumHeader
    // (src/album_qt.rs:171), not of AlbumViewData — read one level too high it
    // is `undefined`, `|| []` swallows that, and the AWARDS block simply never
    // renders. Which is what it did, for every album, since the block was
    // written: an empty Repeater draws nothing and reports nothing, and
    // `hasSidebar` counted the same empty list, so an album whose ONLY sidebar
    // content was its awards showed no sidebar at all.
    readonly property var awards: albumHeader.awards || []
    property var coverMap: ({})
    // Client-side track search (AlbumActions.search equivalent).
    property string trackQuery: ""

    // ---- Header atmosphere (AlbumPageView.slint:161-189) -----------------
    // The pref, LIVE where possible: the settings snapshot is only published
    // on settings-view open / mutation, so on a cold start it is empty and
    // the document's own copy (album_qt.rs `headerGradient`) answers instead.
    readonly property bool headerGradientPref: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                var d = JSON.parse(raw)
                if (d.albumHeaderGradient !== undefined)
                    return d.albumHeaderGradient === true
            } catch (e) { /* fall through to the document copy */ }
        }
        return album.headerGradient !== false
    }
    // .slint:168 — the album's own atmosphere is SUPPRESSED under the
    // app-wide dynamic background (they clash); the dynamic background then
    // provides the dark backdrop instead.
    readonly property bool headerAtmoOn: headerGradientPref && !ambientOn
    // .slint:167 — dark backdrop from EITHER source means light header text.
    readonly property bool headerLight: headerGradientPref || ambientOn
    readonly property color hdrStrong: headerLight ? "#ffffff" : theme.textPrimary
    readonly property color hdrBody: headerLight ? "#e0ffffff" : theme.textSecondary
    // (the .slint declares an `hdr-muted` tier too, :173, but never binds it
    //  to anything — not ported rather than ported dead)
    // .slint:179 — with no dark backdrop the circles use the on-surface arm.
    readonly property bool hdrOverlay: headerLight

    readonly property var visibleTracks: {
        if (trackQuery === "") return tracks
        var q = trackQuery.toLowerCase()
        return tracks.filter(function (t) {
            return t.title.toLowerCase().indexOf(q) >= 0
        })
    }

    // ---- Loading staging (album_qt.rs publishes in passes) ---------------
    // The PRIMARY document (header + tracks) lands as soon as /album/get
    // answers; each bottom rail arrives later carrying its own flag, so the
    // page is usable while Qobuz suggestions and the Last.fm row resolve.
    //
    // A rail is shown when it HAS cards, its placeholder when its flag is
    // still up. Both false = the section is ABSENT: `moreLoading` is seeded
    // false when the album has no artist id and `similarLoading` false when
    // Last.fm is not connected, so nothing ever spins forever on a row that
    // will never arrive.
    readonly property bool primaryLoading: QbzAlbum.albumLoading && tracks.length === 0
    readonly property bool moreLoading: album.moreLoading === true
                                        && (album.moreFromArtist || []).length === 0
    readonly property bool suggestionsLoading: album.suggestionsLoading === true
                                               && (album.suggestions || []).length === 0
    readonly property bool similarLoading: album.similarLoading === true
                                           && (album.similarAlbums || []).length === 0

    // ONE 900ms phase for every placeholder on the page (QbzSkeleton's COST
    // note: N placeholders, 1 timer). Stops dead when nothing is pending.
    Timer {
        id: skeletonPhase
        property bool on: false
        interval: 900
        repeat: true
        running: root.visible && (root.primaryLoading || root.moreLoading
                                  || root.suggestionsLoading || root.similarLoading)
        onTriggered: on = !on
    }

    // Placeholder cards that fill one rail (SectionRail's 232px pitch).
    readonly property int railSkeletonCount:
        Math.max(1, Math.min(8, Math.ceil((root.width - 64) / 232)))

    // The vertical ListView gets ONE uniform 10px layout cell per slice of
    // content. A visual item lives in the first cell of its run and paints
    // over the following inert cells (50px row = 5 cells, 40px header = 4).
    // Uniform delegate height is deliberate: Qt estimates contentHeight for
    // variable delegates/sections, which makes a scrollbar thumb resize and
    // skip as new sizes are discovered. Here count * 10 is exact from frame 1.
    readonly property int listCellPx: 10
    readonly property int trackRowPx: 50
    readonly property int trackHeaderPx: 40
    // SectionRail/RailSkeleton are 286px. Reserve 290 so the tape stays on
    // the 10px grid; the final 4px is harmless air before the next section.
    readonly property int railSlotPx: 290

    function appendRun(out, head, pixels) {
        out.push(head)
        for (var y = root.listCellPx; y < pixels; y += root.listCellPx)
            out.push({ "kind": "gap" })
    }

    function buildTrackCells() {
        var out = []
        var list = root.visibleTracks
        var multi = list.length > 0 && (list[list.length - 1].disc || 1) > 1
        for (var i = 0; i < list.length; i++) {
            var t = list[i]
            var prev = i > 0 ? list[i - 1] : null
            var disc = (i === 0)
                ? (multi ? (t.disc || 1) : 0)
                : (multi && t.disc !== prev.disc ? (t.disc || 1) : 0)
            var work = (i === 0 || t.workHeader !== prev.workHeader)
                ? (t.workHeader || "") : ""
            if (disc > 0)
                root.appendRun(out, { "kind": "disc", "disc": disc }, root.trackHeaderPx)
            if (work !== "") {
                root.appendRun(out, {
                    "kind": "work",
                    "work": work,
                    "composerName": t.workComposerName || "",
                    "composerId": t.workComposerId || ""
                }, root.trackHeaderPx)
            }
            root.appendRun(out, {
                "kind": "track",
                "track": t,
                "trackNumber": i + 1
            }, root.trackRowPx)
        }
        return out
    }

    readonly property var trackCells: buildTrackCells()
    readonly property int trackTapePx: trackCells.length * listCellPx

    function appendRailCells(out, kind, loading, items) {
        var rows = items || []
        if (!loading && rows.length === 0)
            return
        root.appendRun(out, { "kind": "gap" }, 40)
        root.appendRun(out, {
            "kind": loading ? "railSkeleton" : kind,
            "items": rows
        }, root.railSlotPx)
    }

    readonly property var listCells: {
        // Copy: the deferred rail passes rebuild this array without mutating
        // the track-only value used by the header/sidebar overlap calculation.
        var out = root.trackCells.slice(0)
        root.appendRailCells(out, "moreRail", root.moreLoading,
                             root.album.moreFromArtist || [])
        root.appendRailCells(out, "suggestionsRail", root.suggestionsLoading,
                             root.album.suggestions || [])
        root.appendRailCells(out, "similarRail", root.similarLoading,
                             root.album.similarAlbums || [])
        return out
    }

    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            var m = root.coverMap
            m[key] = path
            root.coverMap = Object.assign({}, m)
        }
        // The SETTLED heart from Rust: the flipped value when the write
        // landed, the UNCHANGED one when it failed. The header click writes
        // its optimistic flip into localToggles and nothing used to correct
        // it, so a 404'd un-favorite stayed visibly un-favorited until the
        // user navigated away — LibraryView.qml was this signal's only
        // listener. Key shape is `library_qt::feed_key` (`{kind}:{id}`).
        function onLibraryFavoriteChanged(key, value) {
            var id = (albumHeader && albumHeader.id) ? albumHeader.id : ""
            if (id !== "" && key === "album:" + id)
                root.setToggleState("album", value)
        }
    }
    // Album-blacklist settle / rollback. `blacklistChanged` carries the state
    // the write actually produced — flipped on success, UNCHANGED on failure
    // (blacklist_qt.rs `album_toggle`), so this is both the cross-surface walk
    // (the manager's row `x`, a card's "Block this album") and the rollback for
    // the header menu's optimistic flip below. Same two-arg `{kind}:{id}` shape
    // as pinChanged / libraryFavoriteChanged above; a separate Connections
    // block only because the signal lives on a different singleton.
    Connections {
        target: QbzBlacklist
        function onBlacklistChanged(key, value) {
            var id = (albumHeader && albumHeader.id) ? albumHeader.id : ""
            if (id !== "" && key === "album:" + id)
                root.setToggleState("blocked", value)
        }
    }
    Component.onCompleted: {
        root.applyAlbumDocument()
        syncAlbumState()
        dispatchCovers()
        root.headerLayoutReady = true
    }
    Connections {
        target: QbzAlbum
        function onAlbumJsonChanged() { root.applyAlbumDocument() }
    }
    onAlbumChanged: { syncAlbumState(); dispatchCovers() }
    // The derived binding settles AFTER onAlbumChanged fires (stale race) —
    // redispatch when the header itself updates.
    onAlbumHeaderChanged: { syncAlbumState(); dispatchCovers() }

    // Optimistic heart / pin state. The document is republished once per
    // deferred rail now, and every republish re-parses `album` — a toggle
    // written straight onto the parsed object would silently pop back a
    // second later. Overrides live here and win until the album changes.
    // (Same pattern, same reason, as ArtistView.localToggles.)
    property var localToggles: ({})
    function toggleState(key, fallback) {
        return localToggles[key] !== undefined ? localToggles[key] : fallback === true
    }
    function setToggleState(key, value) {
        var m = localToggles
        m[key] = value
        localToggles = Object.assign({}, m)
    }

    // Per-album view state is reset ONLY when the id actually changes, or a
    // deferred rail landing would yank the user's toggles back mid-read.
    property string loadedAlbumId: ""
    function syncAlbumState() {
        var id = (albumHeader && albumHeader.id) ? albumHeader.id : ""
        if (id === loadedAlbumId)
            return
        loadedAlbumId = id
        localToggles = ({})
        dispatchedCovers = ({})
        // A new album carries no selection over (Slint resets multi-select
        // on every album load/reset, album.rs:692-694).
        setMultiSelect(false)
        // Re-seed the live offline map from the new document (track rows
        // carry their own seeded cacheStatus; this map tracks the LIVE one).
        var seed = ({})
        var ts = album.tracks || []
        for (var i = 0; i < ts.length; i++)
            seed[ts[i].id] = ts[i].cacheStatus || 0
        cacheStates = seed
    }

    // --- Multi-select (AlbumPageView.slint:736-807) -------------------------
    // The selection lives here in QML (LibraryView precedent): select-all and
    // clear never reach Rust, everything else goes down as a JSON id array
    // through QbzAlbum.albumBulkAction.
    property bool multiSelect: false
    property var selected: ({})
    readonly property int selectedCount: Object.keys(root.selected).length
    readonly property bool multiSelectOn: root.multiSelect
    function setMultiSelect(on) {
        root.multiSelect = on
        if (!on) { root.selected = ({}); sel.anchorId = "" }
    }
    /// Excel-style selection lives in ONE place — controls/SelectionModel.qml
    /// holds the anchor and the Shift-range rule; this view keeps owning its
    /// map. `mods` is the mouse event's modifiers, forwarded by the row; a
    /// caller with no event (the checkbox, a keyboard path) may omit it.
    SelectionModel { id: sel }
    function toggleSelected(id, mods) {
        root.selected = sel.next(root.selected, id, root.visibleTracks,
                                 mods === undefined ? Qt.NoModifier : mods)
    }
    /// Selected ids in VISIBLE order — never Object.keys (integer-like keys
    /// iterate in NUMERIC order, not the user's).
    function selectedIdsInOrder() {
        var rows = root.visibleTracks
        var out = []
        for (var i = 0; i < rows.length; i++)
            if (root.selected[rows[i].id] === true) out.push(rows[i].id)
        return out
    }
    function bulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var rows = root.visibleTracks
            for (var i = 0; i < rows.length; i++) m[rows[i].id] = true
            root.selected = m
            return
        }
        if (action === "clear") { root.selected = ({}); sel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder()
        if (ids.length === 0) return
        QbzAlbum.albumBulkAction(albumHeader.id, JSON.stringify(ids), action)
        // The Slint keeps the selection while a picker is still open (a
        // failed write is retried from the same modal) and clears after
        // everything else.
        if (action !== "add-to-playlist" && action !== "add-to-mixtape")
            root.selected = ({})
    }
    // Ctrl+A / Escape hotkey router seam (AppShell duck-types these).
    function selectAll() {
        if (!root.multiSelect) root.setMultiSelect(true)
        root.bulkAction("select-all")
    }
    function exitMultiSelectMode() {
        if (root.multiSelect) root.setMultiSelect(false)
    }

    // --- Offline cache live state ------------------------------------------
    // Row glyphs own their live status (TrackRow's Connections). This map
    // exists for ONE consumer: the ⋯ menu's "Make available offline" /
    // "Refresh offline copy" swap, which needs the all-rows-ready aggregate
    // (Slint recomputes album-fully-cached on every row flip,
    // main.rs:2426-2438).
    property var cacheStates: ({})
    readonly property bool fullyCachedLive: {
        var ts = album.tracks || []
        if (ts.length === 0)
            return false
        var m = cacheStates
        for (var i = 0; i < ts.length; i++) {
            var st = m[ts[i].id]
            if (st === undefined) st = ts[i].cacheStatus || 0
            if (st !== 3)
                return false
        }
        return true
    }
    Connections {
        target: QbzShell
        function onTrackCacheStatusChanged(trackId, status, progress) {
            if (!(trackId in root.cacheStates))
                return
            var m = root.cacheStates
            m[trackId] = status
            root.cacheStates = Object.assign({}, m)
        }
    }

    // Already-requested artwork keys. The document is now published in FOUR
    // passes (primary, then each rail), and every pass re-fires this — resending
    // the whole list each time is pure waste, so only what is new goes out.
    property var dispatchedCovers: ({})
    function dispatchCovers() {
        var urls = []
        if (albumHeader && albumHeader.artUrl) urls.push(albumHeader.artUrl)
        var more = album.moreFromArtist || []
        for (var i = 0; i < more.length; i++) if (more[i].artUrl) urls.push(more[i].artUrl)
        var sug = album.suggestions || []
        for (i = 0; i < sug.length; i++) if (sug[i].artUrl) urls.push(sug[i].artUrl)
        // The Last.fm row's covers ride the same dispatch — without this its
        // cards would render as empty frames.
        var sim = album.similarAlbums || []
        for (i = 0; i < sim.length; i++) if (sim[i].artUrl) urls.push(sim[i].artUrl)

        var seen = dispatchedCovers
        var fresh = []
        for (i = 0; i < urls.length; i++) {
            if (!seen[urls[i]]) {
                seen[urls[i]] = true
                fresh.push(urls[i])
            }
        }
        if (fresh.length > 0) {
            dispatchedCovers = seen
            QbzShell.sidebarArtworkWindow(JSON.stringify(fresh))
        }
    }

    // Ghost CircleAction (secondary, on-surface variant): elevated disc,
    // strong ring, text-primary icon (accent when active).


    // Track list row (TrackRow.slint replica: number cell, no artwork,
    // Sidebar label/award card (SidebarCard).
    component SidebarCard: Rectangle {
        id: sidebarCard
        property string name: ""
        property color gradA: "#6366f1"
        property color gradB: "#8b5cf6"
        property string iconName: "disc"
        property bool compact: false
        signal clicked()
        width: parent ? parent.width : 0
        height: 48
        radius: theme.radiusSm
        color: scArea.containsMouse ? theme.surfaceHover : "transparent"
        Row {
            x: sidebarCard.compact ? Math.round((sidebarCard.width - width) / 2) : 6
            anchors.verticalCenter: parent.verticalCenter
            width: sidebarCard.compact ? 28 : parent.width - 12
            height: parent.height - 12
            spacing: 10
            Rectangle {
                width: 28
                height: 28
                radius: 14
                anchors.verticalCenter: parent.verticalCenter
                gradient: Gradient {
                    orientation: Gradient.Horizontal
                    GradientStop { position: 0.0; color: gradA }
                    GradientStop { position: 1.0; color: gradB }
                }
                QbzIcon {
                    name: iconName
                    width: 13
                    height: 13
                    anchors.centerIn: parent
                    // On the card's fixed brand gradient disc (indigo/violet
                    // or amber), never on a theme surface.
                    tintName: "white"
                }
            }
            Text {
                visible: !sidebarCard.compact
                width: parent.width - 38
                anchors.verticalCenter: parent.verticalCenter
                text: name
                color: theme.textSecondary
                font.pixelSize: 12
                font.weight: theme.weightMedium
                wrapMode: Text.WordWrap
            }
        }
        MouseArea {
            id: scArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: parent.clicked()
            ToolTip.visible: sidebarCard.compact && containsMouse
                                 && sidebarCard.name !== ""
            ToolTip.text: sidebarCard.name
            ToolTip.delay: 350
        }
    }
    component SidebarHeading: Text {
        property bool compact: false
        width: parent ? parent.width : implicitWidth
        color: theme.textMuted
        font.pixelSize: 10
        font.weight: theme.weightSemibold
        font.letterSpacing: 1
        horizontalAlignment: compact ? Text.AlignHCenter : Text.AlignLeft
        wrapMode: compact ? Text.WordWrap : Text.NoWrap
    }

    // Purchase entitlement mark used inside the album metadata flow. Keeping
    // the separator in the component makes the mark a peer of release date,
    // label, genre, track count and duration instead of a detached badge row.
    component PurchaseMetaMark: Row {
        property string label: ""
        property color foreground: "#e0b341"
        spacing: 6
        Text {
            text: "   •   "
            color: parent.foreground
            font.pixelSize: theme.fontBody
        }
        QbzIcon {
            name: "circle-check-big"
            width: 15
            height: 15
            anchors.verticalCenter: parent.verticalCenter
            tintName: "amber"
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: parent.label
            color: parent.foreground
            font.pixelSize: theme.fontLegal
            font.weight: Font.DemiBold
        }
    }

    // Placeholder for a bottom rail that has not resolved yet: the SAME 28px
    // header band, 232px pitch and 246px card band SectionRail uses, so the
    // page does not jump when the real cards replace it. Built out of the
    // shared QbzSkeleton — no local skeleton primitive.
    //
    // Everything it needs is a property, not a file-scope id: an inline
    // `component` does not see the enclosing document's ids (the gotcha
    // QbzSkeleton.qml documents), so `phase` is passed in by the host.
    component RailSkeleton: Column {
        id: railSk
        property bool phase: false
        property int cardCount: 4
        width: parent ? parent.width : 0
        spacing: 12

        Item {
            width: parent.width
            height: 28
            QbzSkeleton {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                variant: "block"
                width: 180
                height: 20
                phase: railSk.phase
            }
        }
        Item {
            width: parent.width
            height: 246
            clip: true
            Row {
                spacing: 32
                Repeater {
                    model: railSk.cardCount
                    // "card" carries its own 200 x (200+42) footprint.
                    delegate: QbzSkeleton {
                        required property int index
                        variant: "card"
                        cellIndex: index
                        phase: railSk.phase
                    }
                }
            }
        }
    }

    // One EXTERNAL LINKS brand icon (AlbumPageView.slint BrandLink): the bare
    // brand SVG in its NATIVE colors — no tint pass, no visible label, the
    // name lives in the hover tooltip (Feishin-style inline links).
    component BrandLink: Rectangle {
        property string iconSource: ""
        property string name: ""
        property string url: ""
        width: 30
        height: 30
        radius: 6
        color: brandArea.containsMouse ? theme.surfaceHover : "transparent"
        Image {
            anchors.centerIn: parent
            source: iconSource
            width: 18
            height: 18
            sourceSize: Qt.size(36, 36)
            fillMode: Image.PreserveAspectFit
            opacity: brandArea.containsMouse ? 1.0 : 0.85
            Behavior on opacity { NumberAnimation { duration: 120 } }
        }
        MouseArea {
            id: brandArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            // Deep link only — the browser does the work, nothing is fetched
            // here and no integration has to be connected.
            onClicked: if (url !== "") Qt.openUrlExternally(url)
            // The Slint BrandLink carries the name in the shared tooltip
            // bubble; the Qt port rides Qt's own ToolTip (LocalMultiSelectBar
            // precedent).
            ToolTip.visible: containsMouse && name !== ""
            ToolTip.text: name
            ToolTip.delay: 350
        }
    }

    // Absolute qrc prefix for the brand SVGs — same rule as QbzIcon: a
    // relative URL resolves against the CONSUMER's document depth.
    readonly property string brandDir: "qrc:/qt/qml/com/blitzfc/qbz/qml/assets/brand/"

    // Whether the right-hand album sidebar has anything to show at all.
    readonly property bool hasSidebar: (albumHeader.label || "") !== ""
                                       || awards.length > 0
                                       || albumHeader.showExternalLinks === true
    // Same contract as ShellState.content-constrained in the Slint view: the
    // album sidebar gives the track table priority only when a right panel is
    // consuming a sub-1366px window. In that state it becomes the 56px icon
    // rail from AlbumPageView.slint; names remain available as tooltips.
    readonly property bool contentConstrained:
        Window.width > 0 && Window.width < 1366
        && (QbzShell.queueOpen || QbzShell.lyricsOpen)
    readonly property bool sidebarCompact: contentConstrained
    readonly property int sidebarPx: sidebarCompact ? 56 : 200
    readonly property int sidebarReservePx: hasSidebar ? sidebarPx + 32 : 0

    // ============================ the page ================================
    ListView {
        id: pageFlick
        anchors.fill: parent
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: root.listCells
        reuseItems: true
        currentIndex: -1
        // The track tape is cheap enough that a fast wheel can consume one
        // viewport before the first heavy card rail has finished incubating.
        // Two viewports give those bottom rails roughly one kinetic-scroll
        // window of lead time. Cache delegates are asynchronous and pooled;
        // this does not restore the old eager footer mount.
        cacheBuffer: Math.max(900, 2 * height)

        header: Item {
            width: pageFlick.width
            height: page.implicitHeight

            // Artwork-tinted header band. FIRST child so it paints under the
            // page, and inside the ListView header so it scrolls with content.
            // Full-bleed on purpose: the page's 32px padding must NOT clip it.
            HeaderGradient {
                x: 0
                y: 0
                width: pageFlick.width
                // .slint:189 `atmo-height: page.y + header-divider.y` — the band
                // ends EXACTLY on the header/track-list divider, whatever height
                // a long editorial description gave the header.
                height: page.y + headerDivider.y + root.atmoReach
                tint: album.headerColor || ""
                atmosphere: album.headerAtmosphere || ""
                active: root.headerAtmoOn
            }

            Column {
                id: page
                width: parent.width
                leftPadding: 32
                rightPadding: 32
                topPadding: 11
                spacing: 0

            // NavButtons is a 0px placeholder in the Slint source.
            Item { width: 1; height: 22 }

            // --- Album header skeleton ----------------------------------
            // Mounted on the primary flag, and the real header is hidden by
            // the same flag: opening album B never renders a half-empty
            // header frame while B's document is in flight.
            Row {
                visible: root.primaryLoading
                width: parent.width - 64
                spacing: root.headerGapPx

                QbzSkeleton {
                    variant: "block"
                    width: root.coverPx
                    height: root.coverPx
                    blockRadius: 12
                    phase: skeletonPhase.on
                }
                Column {
                    width: parent.width - root.coverPx - root.headerGapPx
                    spacing: root.compactHeaderPref ? 4 : 12
                    Item { width: 1; height: root.compactHeaderPref ? 0 : 6 }
                    QbzSkeleton { variant: "block"; width: Math.min(420, parent.width); height: root.compactHeaderPref ? 24 : 30; cellIndex: 0; phase: skeletonPhase.on }
                    QbzSkeleton { visible: !root.compactHeaderPref; variant: "block"; width: Math.min(260, parent.width); height: 18; cellIndex: 1; phase: skeletonPhase.on }
                    QbzSkeleton { variant: "block"; width: Math.min(340, parent.width); height: 14; cellIndex: 2; phase: skeletonPhase.on }
                    Item { width: 1; height: root.compactHeaderPref ? 10 : 14 }
                    Row {
                        spacing: 12
                        Repeater {
                            model: 4
                            delegate: QbzSkeleton {
                                required property int index
                                variant: "circle"
                                width: root.compactHeaderPref ? 28 : 44
                                height: width
                                cellIndex: index
                                phase: skeletonPhase.on
                            }
                        }
                    }
                }
            }

            // --- Album header -------------------------------------------
            Row {
                visible: !root.primaryLoading
                width: parent.width - 64
                spacing: root.headerGapPx

                Rectangle {
                    width: root.coverPx
                    height: root.coverPx
                    radius: 12
                    color: theme.surfaceElevated
                    // No clip: RoundedImage confines itself on both arms; a clip is an
                    // unconditional batch root. coverMenu and CoverLightbox are
                    // view-root siblings, never descendants of this frame.
                    RoundedImage {
                        anchors.fill: parent
                        // A custom cover override (shared custom_artwork
                        // store) beats the url-keyed pipeline image.
                        source: (albumHeader.customCoverPath || "") !== ""
                            ? "file://" + albumHeader.customCoverPath
                            : (root.coverMap[albumHeader.artUrl] || "")
                        radius: 12
                    }
                    // Left-click: the lightbox (NEW — Slint's left-click is
                    // inert). Right-click: the cover menu (Slint
                    // AlbumPageView.slint:300-370).
                    MouseArea {
                        anchors.fill: parent
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        cursorShape: Qt.PointingHandCursor
                        onClicked: function (mouse) {
                            if (mouse.button === Qt.RightButton) {
                                coverMenu.openAtCursor(coverMenuAnchor, mouse.x, mouse.y)
                            } else {
                                coverLightbox.openWith(root.bestCoverSource())
                            }
                        }
                    }
                    Item { id: coverMenuAnchor; anchors.fill: parent }
                }

                Column {
                    width: parent.width - root.coverPx - root.headerGapPx
                    anchors.top: parent.top
                    anchors.topMargin: 4
                    spacing: 0

                    Item {
                        width: parent.width
                        height: Math.max(albumTitle.implicitHeight, 32)
                        Text {
                            id: albumTitle
                            visible: !root.compactHeaderPref
                            anchors.left: parent.left
                            anchors.right: headerModeButton.left
                            anchors.rightMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            text: albumHeader.title || ""
                            color: root.hdrStrong
                            font.pixelSize: theme.fontSection
                            font.weight: theme.weightBold
                            elide: Text.ElideRight
                        }
                        Row {
                            id: compactHeading
                            visible: root.compactHeaderPref
                            anchors.left: parent.left
                            anchors.right: headerModeButton.left
                            anchors.rightMargin: 10
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 0
                            // The title's natural width, measured OUTSIDE the
                            // Text: an elided Text recomputes its implicit size
                            // after laying out at the bound width, so a `width`
                            // that reads its own `implicitWidth` is a binding
                            // loop (Qt reported it on every album open).
                            TextMetrics {
                                id: compactTitleMetrics
                                font.pixelSize: theme.fontSection
                                font.weight: theme.weightBold
                                text: albumHeader.title || ""
                            }
                            Text {
                                id: compactAlbumTitle
                                width: (albumHeader.artist || "") === ""
                                    ? compactHeading.width
                                    : Math.min(Math.ceil(compactTitleMetrics.advanceWidth),
                                               Math.max(80, compactHeading.width * 0.62))
                                text: albumHeader.title || ""
                                color: root.hdrStrong
                                font.pixelSize: theme.fontSection
                                font.weight: theme.weightBold
                                elide: Text.ElideRight
                            }
                            Text {
                                id: compactTitleSeparator
                                visible: (albumHeader.artist || "") !== ""
                                text: "  •  "
                                color: root.hdrBody
                                font.pixelSize: theme.fontSection
                                font.weight: theme.weightBold
                            }
                            Text {
                                width: Math.max(0, compactHeading.width
                                    - compactAlbumTitle.width
                                    - compactTitleSeparator.width)
                                text: albumHeader.artist || ""
                                color: compactArtistArea.containsMouse
                                       && (albumHeader.artistId || "") !== ""
                                    ? root.hdrStrong : root.hdrBody
                                font.pixelSize: theme.fontSection
                                font.weight: theme.weightBold
                                elide: Text.ElideRight
                                MouseArea {
                                    id: compactArtistArea
                                    anchors.fill: parent
                                    enabled: (albumHeader.artistId || "") !== ""
                                    hoverEnabled: true
                                    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onClicked: QbzArtist.openArtist(albumHeader.artistId)
                                }
                            }
                        }
                        QbzIconButton {
                            id: headerModeButton
                            anchors.right: parent.right
                            anchors.top: parent.top
                            anchors.topMargin: -4
                            btnSize: root.compactHeaderPref ? 22 : 26
                            iconSize: root.compactHeaderPref ? 11 : 13
                            name: root.compactHeaderPref ? "maximize-2" : "minimize-2"
                            tintOverride: root.hdrOverlay ? "white" : ""
                            onClicked: root.toggleCompactHeader()
                            HoverHandler { id: headerModeHover }
                            ToolTip.visible: headerModeHover.hovered
                            ToolTip.text: root.compactHeaderPref
                                ? QbzSession.tr("Show full album header", QbzSession.trRev)
                                : QbzSession.tr("Show compact album header", QbzSession.trRev)
                            ToolTip.delay: 350
                        }
                    }
                    Item { width: 1; height: root.compactHeaderPref ? 2 : 4 }
                    // Credited-artist line (links + role suffixes).
                    Flow {
                        visible: !root.compactHeaderPref
                        width: parent.width
                        spacing: 0
                        Repeater {
                            model: albumHeader.credits || []
                            delegate: Row {
                                required property var modelData
                                required property int index
                                spacing: 0
                                Text {
                                    visible: index > 0
                                    text: "  •  "
                                    color: root.hdrBody
                                    font.pixelSize: theme.fontHeading
                                    font.weight: theme.weightBold
                                }
                                Text {
                                    text: modelData[0]
                                    color: creditArea.containsMouse && modelData[1] !== "" ? root.hdrStrong : root.hdrBody
                                    font.pixelSize: theme.fontHeading
                                    font.weight: theme.weightBold
                                    MouseArea {
                                        id: creditArea
                                        anchors.fill: parent
                                        enabled: modelData[1] !== ""
                                        hoverEnabled: true
                                        cursorShape: modelData[1] !== "" ? Qt.PointingHandCursor : Qt.ArrowCursor
                                        onClicked: QbzArtist.openArtist(modelData[1])
                                    }
                                }
                                Text {
                                    visible: modelData[2] !== ""
                                    text: " (" + modelData[2] + ")"
                                    color: root.hdrBody
                                    font.pixelSize: theme.fontHeading
                                }
                            }
                        }
                    }
                    Item { width: 1; height: root.compactHeaderPref ? 2 : 10 }
                    // Meta line (label as a clickable link when navigable).
                    Flow {
                        spacing: 0
                        width: parent.width
                        visible: !root.compactHeaderPref
                                 && (albumHeader.labelId || "") !== ""
                                 && (albumHeader.label || "") !== ""
                        Text {
                            visible: (albumHeader.metaPre || "") !== ""
                            text: (albumHeader.metaPre || "") + "   •   "
                            color: root.hdrBody
                            font.pixelSize: theme.fontBody
                        }
                        Text {
                            text: albumHeader.label || ""
                            color: labelArea.containsMouse ? theme.accent : root.hdrBody
                            font.pixelSize: theme.fontBody
                            MouseArea {
                                id: labelArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzHome.openLabel(albumHeader.labelId)
                            }
                        }
                        Text {
                            visible: (albumHeader.metaPost || "") !== ""
                            text: "   •   " + (albumHeader.metaPost || "")
                            color: root.hdrBody
                            font.pixelSize: theme.fontBody
                        }
                        PurchaseMetaMark {
                            visible: root.purchaseStatusVisible
                            label: root.purchaseStatusText
                            foreground: "#e0b341"
                        }
                    }
                    Flow {
                        visible: !root.compactHeaderPref
                                 && ((albumHeader.labelId || "") === ""
                                     || (albumHeader.label || "") === "")
                        width: parent.width
                        spacing: 0
                        Text {
                            text: albumHeader.infoLine || ""
                            color: root.hdrBody
                            font.pixelSize: theme.fontBody
                        }
                        PurchaseMetaMark {
                            visible: root.purchaseStatusVisible
                            label: root.purchaseStatusText
                            foreground: "#e0b341"
                        }
                    }
                    Flow {
                        visible: root.compactHeaderPref
                        width: parent.width
                        spacing: 0
                        Text {
                            text: albumHeader.compactInfoLine || ""
                            color: root.hdrBody
                            font.pixelSize: theme.fontBody
                        }
                        PurchaseMetaMark {
                            visible: root.purchaseStatusVisible
                            label: root.purchaseStatusText
                            foreground: "#e0b341"
                        }
                    }

                    // Editorial description + Read more.
                    Item {
                        visible: !root.compactHeaderPref
                                 && (albumHeader.description || "") !== ""
                        width: 1
                        height: 12
                    }
                    Text {
                        visible: !root.compactHeaderPref
                                 && (albumHeader.description || "") !== ""
                        width: parent.width
                        text: albumHeader.descriptionShort || ""
                        color: root.hdrBody
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    Item {
                        visible: !root.compactHeaderPref
                                 && (albumHeader.description || "")
                                    !== (albumHeader.descriptionShort || "")
                        width: 1
                        height: 4
                    }
                    Text {
                        visible: !root.compactHeaderPref
                                 && (albumHeader.description || "")
                                    !== (albumHeader.descriptionShort || "")
                        text: QbzSession.tr("Read more", QbzSession.trRev)
                        color: readMoreArea.containsMouse ? theme.accentHover : theme.accent
                        font.pixelSize: theme.fontLegal
                        MouseArea {
                            id: readMoreArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                var shell = root.parent
                                while (shell && shell.openTextModal === undefined) shell = shell.parent
                                if (shell) shell.openTextModal(QbzSession.tr("About this album", QbzSession.trRev), albumHeader.description || "")
                            }
                        }
                    }

                    Item { width: 1; height: root.compactHeaderPref ? 10 : 20 }
                    // Action row — AlbumPageView.slint:504-640. One shared
                    // CircleAction for every button including Play (the
                    // hand-rolled 44px disc it used to be drifted from the
                    // control on ring, hover and glyph tint); the palette arm
                    // follows the header backdrop, exactly like the .slint's
                    // `on-surface: root.hdr-on-surface`.
                    Row {
                        spacing: 12
                        QbzCircleAction {
                            primary: true
                            compactPrimary: root.compactHeaderPref
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            name: "play-fill"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzPlayer.playAlbum(albumHeader.id)
                        }
                        QbzCircleAction {
                            name: "shuffle"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzPlayer.playAlbumShuffled(albumHeader.id)
                        }
                        QbzCircleAction {
                            id: playbackSourceButton
                            visible: root.showPlaybackSource
                            name: "cloud-sync"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            active: (root.purchase.playbackMode || "qobuz") === "purchase"
                                || playbackSourceMenu.opened
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: playbackSourceMenu.openBelowLeft(playbackSourceButton)
                            HoverHandler { id: playbackSourceHover }
                            ToolTip.visible: playbackSourceHover.hovered
                            ToolTip.text: QbzSession.tr("Play with", QbzSession.trRev)
                            ToolTip.delay: 350
                        }
                        QbzCircleAction {
                            readonly property bool favorite: root.toggleState("album", albumHeader.isFavorite)
                            name: favorite ? "heart-filled" : "heart"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            active: favorite
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: {
                                root.setToggleState("album", !favorite)
                                QbzLibrary.libraryToggleFavorite("album", albumHeader.id)
                            }
                        }
                        // Radio: the Qobuz /radio/album endpoint, via the
                        // For-You tile seam (foryou_qt::start_album_radio —
                        // minimal track objects are enriched before play).
                        QbzCircleAction {
                            name: "radio"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            // A radio is a fetch + an enrich + a queue write,
                            // and until the first track resolves nothing on
                            // screen moves. The key is per-album so this disc
                            // spins and the Discover rail's stations do not.
                            loading: QbzHome.radioPending === "album:" + albumHeader.id
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzHome.startAlbumRadio(albumHeader.id)
                        }
                        // Booklet: only when the album carries a PDF goody
                        // (AlbumPageView.slint:575-585); downloads via save-as.
                        QbzCircleAction {
                            visible: albumHeader.hasBooklet === true
                            name: "book-open"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzAlbum.downloadBooklet()
                        }
                        // Mixtape/Collection picker (an album is accepted by
                        // BOTH kinds — myqbz_add_qt's Accepts engine).
                        QbzCircleAction {
                            name: "cassette-tape"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzAlbum.addToMixtape(albumHeader.id)
                        }
                        // Album info: the Credits/Review modal
                        // (AlbumInfoModal.qml, data via album_info_qt.rs).
                        QbzCircleAction {
                            name: "info"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: albumInfo.openFor(albumHeader.id)
                        }
                        QbzCircleAction {
                            id: albumMenuBtn
                            name: "ellipsis"
                            diameterOverride: root.compactHeaderPref ? 28 : 0
                            overlay: root.hdrOverlay
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: function (mouse) { albumMenu.openAtCursor(albumMenuBtn, mouse.x, mouse.y) }
                        }
                    }

                }
            }

            Item { width: 1; height: 20 }
            // Header divider. The gradient band above sizes itself to THIS
            // item's y (.slint:189 atmo-height), so it keeps its id.
            // Kept, deliberately invisible. The gradient band above sizes itself
            // off THIS item's y (.slint:189 atmo-height) and the whole column
            // below is positioned after it, so removing the item would move the
            // page; only the paint is unwanted.
            Rectangle { id: headerDivider; width: parent.width - 64; height: 1; color: "transparent" }
            Item { width: 1; height: 8 }

            // --- Track list + label/awards sidebar ----------------------
            Row {
                id: trackChromeRow
                width: parent.width - 64
                // Usually only the toolbar/header chrome contributes to the
                // ListView header. The sidebar is allowed to paint beside the
                // recycled rows; for a very short album reserve just enough
                // extra height to keep the first full-width rail below it.
                height: Math.max(trackChrome.implicitHeight,
                                 albumSidebar.implicitHeight - root.trackTapePx)
                spacing: 32

                Column {
                    id: trackChrome
                    width: parent.width - root.sidebarReservePx
                    spacing: 0

                    // Track-list placeholder — same flag the spinner used,
                    // now in the shape of the list it is standing in for
                    // (toolbar band + column-header band + 8 rows at the
                    // TrackRow 50px pitch), so nothing shifts on arrival.
                    Column {
                        visible: root.primaryLoading
                        width: parent.width
                        spacing: 0

                        Item { width: 1; height: 52 }
                        Item { width: 1; height: 40 }
                        Repeater {
                            model: 8
                            delegate: Item {
                                required property int index
                                width: parent ? parent.width : 0
                                height: 50
                                QbzSkeleton {
                                    anchors.verticalCenter: parent.verticalCenter
                                    anchors.left: parent.left
                                    anchors.leftMargin: 12
                                    variant: "block"
                                    width: 20
                                    height: 12
                                    cellIndex: index
                                    phase: skeletonPhase.on
                                }
                                QbzSkeleton {
                                    anchors.verticalCenter: parent.verticalCenter
                                    anchors.left: parent.left
                                    anchors.leftMargin: 60
                                    variant: "block"
                                    width: Math.max(90, parent.width * 0.36)
                                    height: 14
                                    cellIndex: index
                                    phase: skeletonPhase.on
                                }
                                QbzSkeleton {
                                    anchors.verticalCenter: parent.verticalCenter
                                    anchors.right: parent.right
                                    anchors.rightMargin: 128
                                    variant: "block"
                                    width: 52
                                    height: 12
                                    cellIndex: index
                                    phase: skeletonPhase.on
                                }
                                QbzSkeleton {
                                    anchors.verticalCenter: parent.verticalCenter
                                    anchors.right: parent.right
                                    anchors.rightMargin: 48
                                    variant: "block"
                                    width: 52
                                    height: 12
                                    cellIndex: index
                                    phase: skeletonPhase.on
                                }
                            }
                        }
                    }

                    // Multi-select bulk bar (controls/QbzMultiSelectBar.qml),
                    // INLINE at the top of the track column — the Local
                    // Library album view's layout (bar in flow, content below)
                    // instead of a floating overlay.
                    QbzMultiSelectBar {
                        id: bulkBar
                        visible: root.multiSelect && !root.primaryLoading
                        width: parent.width
                        selectedCount: root.selectedCount
                        // The reference's full AlbumView inventory
                        // (AlbumPageView.slint:792-807), make-offline included.
                        actions: [
                            { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                            { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                            { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                            { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                            { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                            { "id": "add-to-favorites", "label": QbzSession.tr("Add to Library", QbzSession.trRev), "icon": "heart", "danger": false, "needsSelection": true },
                            { "id": "make-offline", "label": QbzSession.tr("Make available offline", QbzSession.trRev), "icon": "cloud-download", "danger": false, "needsSelection": true },
                            { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
                        ]
                        onAction: function (id) { root.bulkAction(id) }
                    }

                    // Toolbar — quality badge + track search (+ inert select).
                    Row {
                        visible: !QbzAlbum.albumLoading
                        width: parent.width
                        height: 52
                        spacing: 16
                        // AlbumPageView.slint:692 mounts QualityBadgeFull —
                        // the contained chip (format mark + tier label over
                        // the exact bit-depth/rate line), NOT a loose mark
                        // plus a plain "16-bit / 44.1 kHz" string. The 1:1
                        // control already exists; this drew its own.
                        QualityBadgeFull {
                            id: qualityRow
                            anchors.verticalCenter: parent.verticalCenter
                            tier: root.playbackQualityTier()
                            detail: root.playbackQualityDetail()
                        }
                        // Clamped, and the badge slot only counts when the
                        // badge is actually there (QualityBadgeFull hides
                        // itself on an empty tier — an unclamped negative
                        // width is a silent layout trap).
                        Item {
                            width: Math.max(0, parent.width
                                - (qualityRow.visible ? qualityRow.width + 16 : 0)
                                - 168 - 30 - 2 * 16)
                            height: 1
                        }
                        Rectangle {
                            width: 168
                            height: 34
                            radius: 6
                            anchors.verticalCenter: parent.verticalCenter
                            color: theme.surfaceElevated
                            border.width: 1
                            border.color: theme.borderSubtle
                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: 10
                                anchors.rightMargin: 10
                                spacing: 7
                                QbzIcon {
                                    name: "search"
                                    width: 14
                                    height: 14
                                    anchors.verticalCenter: parent.verticalCenter
                                    tintName: "muted"
                                }
                                TextInput {
                                    QbzTextEditMenu { }
                                    width: parent.width - 21
                                    height: parent.height
                                    color: theme.textPrimary
                                    font.pixelSize: 13
                                    verticalAlignment: Text.AlignVCenter
                                    clip: true
                                    onTextEdited: root.trackQuery = text
                                    Text {
                                        visible: parent.text === ""
                                        anchors.fill: parent
                                        text: QbzSession.tr("Search tracks...", QbzSession.trRev)
                                        color: theme.textMuted
                                        font.pixelSize: 13
                                        verticalAlignment: Text.AlignVCenter
                                    }
                                }
                            }
                        }
                        // Multi-select toggle (AlbumPageView.slint:752-787):
                        // accent border + tint while active; leaving the mode
                        // clears the selection.
                        Rectangle {
                            width: 30
                            height: 30
                            radius: 6
                            anchors.verticalCenter: parent.verticalCenter
                            color: selectToggleArea.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                            border.width: 1
                            border.color: root.multiSelect ? theme.accent : theme.borderSubtle
                            QbzIcon {
                                name: "square-check-big"
                                width: 15
                                height: 15
                                anchors.centerIn: parent
                                tintName: root.multiSelect ? "accent" : "secondary"
                            }
                            MouseArea {
                                id: selectToggleArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.setMultiSelect(!root.multiSelect)
                            }
                        }
                    }

                    // Column header — rows/TrackListHeader.qml, i.e. the SAME
                    // rows/TrackCols.qml geometry the TrackRows below use.
                    //
                    // What was here disagreed with the rows on every number:
                    // spacing 16 vs the row's 14, Duration 80 vs 70, Quality
                    // 80 vs 92, and a title width that counted five gaps
                    // where the layout draws six. Those are Slint's own
                    // header numbers (AlbumPageView.slint:811-880) and they
                    // do NOT match primitives/TrackRow.slint — the reference
                    // has the same defect the owner reported here, so the
                    // port keeps the row's numbers and drops the second
                    // hardcoded copy entirely.
                    //
                    // The heart / cloud glyphs stay (they are the only
                    // labelling those two columns get) — and they still fill
                    // the band so `centerIn` centres them, which is the fix
                    // this block used to document at length.
                    TrackListHeader {
                        visible: !QbzAlbum.albumLoading
                        width: parent.width
                        bandHeight: 40
                        labelSpacing: 0.5
                        showDownload: true
                        favoriteGlyph: true
                        downloadGlyph: true
                        downloadActionEnabled: (albumHeader.id || "") !== ""
                            && (root.album.tracks || []).length > 0
                            && QbzSession.offlineMode === 0
                        downloadActionLoading: QbzOffline.collectionPreflightLoading
                            && QbzOffline.collectionPreflightKey === "album:" + (albumHeader.id || "")
                        onDownloadClicked: QbzOffline.cacheAlbum(albumHeader.id)
                    }

                }

                // Label / awards / external-links sidebar (200px).
                Column {
                    id: albumSidebar
                    visible: root.hasSidebar
                    width: root.sidebarPx
                    spacing: 24

                    Column {
                        visible: (albumHeader.label || "") !== ""
                        width: parent.width
                        spacing: 8
                        SidebarHeading {
                            compact: root.sidebarCompact
                            text: QbzSession.tr("LABEL", QbzSession.trRev)
                        }
                        SidebarCard {
                            name: albumHeader.label || ""
                            iconName: "disc"
                            compact: root.sidebarCompact
                            gradA: "#6366f1"
                            gradB: "#8b5cf6"
                            // The label page HAS existed for a long time — the
                            // header chip above (:691) has been opening it all
                            // along. This card carried a "no label view yet"
                            // note and no handler, so the sidebar's LABEL entry
                            // looked clickable (it hovers) and did nothing.
                            onClicked: if ((albumHeader.labelId || "") !== "")
                                QbzHome.openLabel(albumHeader.labelId)
                        }
                    }
                    Column {
                        visible: awards.length > 0
                        width: parent.width
                        spacing: 8
                        SidebarHeading {
                            compact: root.sidebarCompact
                            text: QbzSession.tr("AWARDS", QbzSession.trRev)
                        }
                        Repeater {
                            model: awards
                            delegate: SidebarCard {
                                required property var modelData
                                name: modelData[1]
                                iconName: "award"
                                compact: root.sidebarCompact
                                gradA: "#b45309"
                                gradB: "#eab308"
                                // `modelData` is the (id, name) pair
                                // album_qt publishes. Qobuz omits the id on
                                // some /album/get entries, so with one we open
                                // directly and without one we hand the NAME to
                                // the resolver — 1:1 with the reference's
                                // "open" / "resolve-open" split
                                // (AlbumPageView.slint:1049-1055).
                                onClicked: {
                                    if ((modelData[0] || "") !== "")
                                        QbzHome.openAward(modelData[0], modelData[1] || "")
                                    else if ((modelData[1] || "") !== "")
                                        QbzHome.openAwardByName(modelData[1])
                                }
                            }
                        }
                    }

                    // EXTERNAL LINKS — Last.fm / Discogs / MusicBrainz deep
                    // links for this release. Present whenever the album has
                    // an artist and a title; they are ordinary web URLs, so
                    // they neither require nor touch a connected integration.
                    Column {
                        visible: albumHeader.showExternalLinks === true
                        width: parent.width
                        spacing: 8
                        SidebarHeading {
                            compact: root.sidebarCompact
                            text: QbzSession.tr("EXTERNAL LINKS", QbzSession.trRev)
                        }
                        Row {
                            visible: !root.sidebarCompact
                            spacing: 8
                            BrandLink {
                                visible: (albumHeader.lastfmUrl || "") !== ""
                                iconSource: root.brandDir + "brand-lastfm.svg"
                                name: "Last.fm"
                                url: albumHeader.lastfmUrl || ""
                            }
                            BrandLink {
                                visible: (albumHeader.discogsUrl || "") !== ""
                                iconSource: root.brandDir + "brand-discogs.svg"
                                name: "Discogs"
                                url: albumHeader.discogsUrl || ""
                            }
                            BrandLink {
                                visible: (albumHeader.musicbrainzUrl || "") !== ""
                                iconSource: root.brandDir + "brand-musicbrainz.svg"
                                name: "MusicBrainz"
                                url: albumHeader.musicbrainzUrl || ""
                            }
                        }
                        Column {
                            visible: root.sidebarCompact
                            width: parent.width
                            spacing: 8
                            BrandLink {
                                visible: (albumHeader.lastfmUrl || "") !== ""
                                anchors.horizontalCenter: parent.horizontalCenter
                                iconSource: root.brandDir + "brand-lastfm.svg"
                                name: "Last.fm"
                                url: albumHeader.lastfmUrl || ""
                            }
                            BrandLink {
                                visible: (albumHeader.discogsUrl || "") !== ""
                                anchors.horizontalCenter: parent.horizontalCenter
                                iconSource: root.brandDir + "brand-discogs.svg"
                                name: "Discogs"
                                url: albumHeader.discogsUrl || ""
                            }
                            BrandLink {
                                visible: (albumHeader.musicbrainzUrl || "") !== ""
                                anchors.horizontalCenter: parent.horizontalCenter
                                iconSource: root.brandDir + "brand-musicbrainz.svg"
                                name: "MusicBrainz"
                                url: albumHeader.musicbrainzUrl || ""
                            }
                        }
                    }
                }
            }

            }
        }

        // Real viewport recycling: every delegate root has the SAME height.
        // The child paints over the inert cells reserved by buildTrackCells().
        // cacheBuffer keeps the root alive until the overflowing child is well
        // outside the viewport, and creates the next heavy row asynchronously.
        delegate: DelegateChooser {
            role: "kind"

            DelegateChoice {
                roleValue: "gap"
                delegate: Item {
                    width: ListView.view.width
                    height: root.listCellPx
                }
            }

            DelegateChoice {
                roleValue: "disc"
                delegate: Item {
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx
                    Rectangle {
                        x: 32
                        width: parent.width - 64 - root.sidebarReservePx
                        height: root.trackHeaderPx
                        color: "transparent"
                        Text {
                            anchors.left: parent.left
                            anchors.leftMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("Disc", QbzSession.trRev)
                                  + " " + modelData.disc
                            color: theme.textMuted
                            font.pixelSize: 13
                            font.weight: theme.weightSemibold
                            font.letterSpacing: 0.5
                        }
                    }
                }
            }

            DelegateChoice {
                roleValue: "work"
                delegate: Item {
                    id: workCell
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx
                    Row {
                        x: 32
                        width: parent.width - 64 - root.sidebarReservePx
                        height: root.trackHeaderPx
                        leftPadding: 12
                        rightPadding: 12
                        topPadding: 14
                        bottomPadding: 4
                        spacing: 0
                        Text {
                            text: workCell.modelData.work
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightBold
                        }
                        Text {
                            visible: workCell.modelData.composerName !== ""
                            text: " ("
                            color: theme.textSecondary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightBold
                        }
                        Text {
                            visible: workCell.modelData.composerName !== ""
                            text: workCell.modelData.composerName
                            color: composerArea.containsMouse
                                   && workCell.modelData.composerId !== ""
                                ? theme.textPrimary : theme.textSecondary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightBold
                            MouseArea {
                                id: composerArea
                                anchors.fill: parent
                                enabled: workCell.modelData.composerId !== ""
                                hoverEnabled: true
                                cursorShape: enabled
                                    ? Qt.PointingHandCursor : Qt.ArrowCursor
                                onClicked: QbzArtist.openArtist(
                                    workCell.modelData.composerId)
                            }
                        }
                        Text {
                            visible: workCell.modelData.composerName !== ""
                            text: ")"
                            color: theme.textSecondary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightBold
                        }
                    }
                }
            }

            DelegateChoice {
                roleValue: "track"
                delegate: Item {
                    id: trackCell
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx

                    TrackRow {
                        id: trackDelegate
                        x: 32
                        width: parent.width - 64 - root.sidebarReservePx
                        item: trackCell.modelData.track
                        number: trackCell.modelData.trackNumber
                        zebra: true
                        clickPlays: false
                        artistLink: true
                        qualityStyle: "text"
                        showDownload: true
                        downloadGlyph: true
                        selectMode: root.multiSelect
                        checked: root.selected[item.id] === true
                        onToggleSelect: function (mods) { root.toggleSelected(item.id, mods) }
                        // "Play later" is ON now. It was off because
                        // `enqueue_album_track` had no block-tail arm — "later"
                        // and "queue" both plain-appended, so the two entries
                        // would have done the same thing. That arm exists, so
                        // the entry can.
                        menuShowGoTo: false
                        onPlayRequested: QbzPlayer.playAlbumFrom(albumHeader.id, item.id)
                        onEnqueueRequested: function (m) {
                            QbzPlayer.enqueueAlbumTrack(albumHeader.id, item.id,
                                m === "next" ? "next" : "later")
                        }
                        onMixtapeRequested: QbzMyQbzAdd.open(JSON.stringify([{
                            "itemType": "track", "source": "qobuz",
                            "sourceItemId": item.id, "title": item.title || "",
                            "subtitle": item.artist || "",
                            // The HEADER's url, not `item.artUrl`: album-view
                            // track rows are published with an empty artUrl
                            // (album_qt.rs:90 says so out loud), and every
                            // track on an album shares the album cover anyway.
                            "artworkUrl": (albumHeader && albumHeader.artUrl) || "",
                            "year": null, "trackCount": null
                        }]))
                    }

                    ListView.onPooled: {
                        trackDelegate.recycleActive = false
                        trackDelegate.releaseForReuse()
                    }
                    ListView.onReused: trackDelegate.recycleActive = true
                }
            }

            DelegateChoice {
                roleValue: "railSkeleton"
                delegate: Item {
                    id: railSkeletonCell
                    width: ListView.view.width
                    height: root.listCellPx
                    property bool live: true
                    Loader {
                        x: 32
                        width: parent.width - 64
                        active: railSkeletonCell.live
                        sourceComponent: RailSkeleton {
                            phase: skeletonPhase.on
                            cardCount: root.railSkeletonCount
                        }
                    }
                    ListView.onPooled: live = false
                    ListView.onReused: live = true
                }
            }

            DelegateChoice {
                roleValue: "moreRail"
                delegate: Item {
                    id: moreRailCell
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx
                    property bool live: true
                    Loader {
                        x: 32
                        width: parent.width - 64
                        active: moreRailCell.live
                        sourceComponent: SectionRail {
                            title: QbzSession.tr("From the same artist",
                                                 QbzSession.trRev)
                            items: moreRailCell.modelData.items
                            coverMap: root.coverMap
                            showViewAll: true
                            onViewAllClicked: QbzArtist.openReleases(
                                root.albumHeader.artistId || "",
                                root.albumHeader.artist || "", "album")
                        }
                    }
                    ListView.onPooled: live = false
                    ListView.onReused: live = true
                }
            }

            DelegateChoice {
                roleValue: "suggestionsRail"
                delegate: Item {
                    id: suggestionsRailCell
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx
                    property bool live: true
                    Loader {
                        x: 32
                        width: parent.width - 64
                        active: suggestionsRailCell.live
                        sourceComponent: SectionRail {
                            title: QbzSession.tr("Listening suggestions",
                                                 QbzSession.trRev)
                            items: suggestionsRailCell.modelData.items
                            coverMap: root.coverMap
                        }
                    }
                    ListView.onPooled: live = false
                    ListView.onReused: live = true
                }
            }

            DelegateChoice {
                roleValue: "similarRail"
                delegate: Item {
                    id: similarRailCell
                    required property var modelData
                    width: ListView.view.width
                    height: root.listCellPx
                    property bool live: true
                    Loader {
                        x: 32
                        width: parent.width - 64
                        active: similarRailCell.live
                        sourceComponent: SectionRail {
                            title: QbzSession.tr("Similar albums",
                                                 QbzSession.trRev)
                            items: similarRailCell.modelData.items
                            coverMap: root.coverMap
                        }
                    }
                    ListView.onPooled: live = false
                    ListView.onReused: live = true
                }
            }
        }

        // ListView header/footer instances are eager in Qt. The real rails
        // therefore live in the model above; this footer is intentionally a
        // cheap fixed spacer matching the page's old bottomPadding.
        footer: Item {
            width: ListView.view.width
            height: 100
        }
    }

    // Thin auto-hiding scrollbar (ListScrollbar).
    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: pageFlick; scope: "album" }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: pageFlick
    }

    // Best available cover source for the lightbox: the custom override
    // file when set, else the remote best() URL (mega → … → large).
    function bestCoverSource() {
        var custom = albumHeader.customCoverPath || ""
        if (custom !== "") return "file://" + custom
        return albumHeader.artUrl || ""
    }

    // The cover menu's rows, rebuilt per open (Add vs Change+Remove flips
    // on hasCustomCover; "View cover" is this port's addition).
    function buildCoverMenuModel() {
        var rows = []
        if (albumHeader.hasCustomCover === true) {
            rows.push({ "label": QbzSession.tr("Change cover", QbzSession.trRev), "icon": "image-plus", "action": "add" })
            rows.push({ "label": QbzSession.tr("Remove cover", QbzSession.trRev), "icon": "trash-2", "action": "remove" })
        } else {
            rows.push({ "label": QbzSession.tr("Add cover", QbzSession.trRev), "icon": "image-plus", "action": "add" })
        }
        rows.push({ "label": QbzSession.tr("View cover", QbzSession.trRev), "icon": "eye", "action": "view" })
        rows.push({ "label": QbzSession.tr("Open in browser", QbzSession.trRev), "icon": "external-link", "action": "browser" })
        rows.push({ "label": QbzSession.tr("Save as…", QbzSession.trRev), "icon": "cloud-download", "action": "save" })
        return rows
    }

    function buildAlbumMenuModel() {
        var rows = [
            { "label": QbzSession.tr("Play", QbzSession.trRev), "icon": "play-fill", "action": "play" },
            { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" },
            { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" },
            { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" },
            { "label": root.toggleState("album", albumHeader.isFavorite) ? QbzSession.tr("Remove from Library", QbzSession.trRev) : QbzSession.tr("Add to Library", QbzSession.trRev), "icon": root.toggleState("album", albumHeader.isFavorite) ? "heart-filled" : "heart", "action": "favorite" },
            { "label": root.toggleState("pin", albumHeader.isPinned) ? QbzSession.tr("Unpin", QbzSession.trRev) : QbzSession.tr("Pin", QbzSession.trRev), "icon": root.toggleState("pin", albumHeader.isPinned) ? "pin-filled" : "pin", "action": "pin" },
            { "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "action": "playlist" },
            { "label": QbzSession.tr("Add to mixtape", QbzSession.trRev), "icon": "cassette-tape", "action": "mixtape" },
            { "label": QbzSession.tr("Share Qobuz link", QbzSession.trRev), "icon": "link", "action": "share-qobuz" },
            { "label": QbzSession.tr("Share Album.link", QbzSession.trRev), "icon": "link", "action": "share-albumlink" },
            { "label": root.fullyCachedLive ? QbzSession.tr("Refresh offline copy", QbzSession.trRev) : QbzSession.tr("Make available offline", QbzSession.trRev), "icon": root.fullyCachedLive ? "refresh-cw" : "cloud-download", "action": root.fullyCachedLive ? "recache-album" : "cache-album" }
        ]
        // Separate DRM-free purchase action. Unknown/stale/error/track-only
        // ownership never reaches this branch; Rust computes the gate.
        if (root.purchase.canDownloadPurchase === true)
            rows.push({ "label": QbzSession.tr("Download purchase", QbzSession.trRev),
                        "icon": "cloud-download", "action": "download-purchase" })
        return rows
    }

    // Cover right-click menu (AlbumPageView.slint:300-370) + "View cover",
    // the lightbox entry that is NEW in this port.
    QbzContextMenu {
        id: coverMenu
        menuWidth: 196
        // Rebuilt on every open so Add/Change/Remove track the live flag.
        onAboutToShow: coverMenuRepeater.model = root.buildCoverMenuModel()
        Repeater {
            id: coverMenuRepeater
            model: []
            delegate: Rectangle {
                required property var modelData
                width: parent ? parent.width : 0
                height: 33
                radius: 5
                color: cmiArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    spacing: 8
                    QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: modelData.label
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: cmiArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        coverMenu.close()
                        var a = modelData.action
                        if (a === "add") QbzAlbum.coverAddCustom(albumHeader.id, albumHeader.artUrl)
                        else if (a === "remove") QbzAlbum.coverRemoveCustom(albumHeader.id)
                        else if (a === "view") coverLightbox.openWith(root.bestCoverSource())
                        else if (a === "browser") QbzShell.openExternalUrl(albumHeader.artUrl)
                        else if (a === "save") QbzAlbum.coverSaveAs(albumHeader.id, albumHeader.title, albumHeader.artUrl)
                    }
                }
            }
        }
    }

    CoverLightbox { id: coverLightbox }

    // Album Info (Credits/Review) modal — mounted by its host view, the
    // TrackInfoModal pattern (one AlbumView instance exists at a time; the
    // Popup reparents to the window overlay).
    AlbumInfoModal { id: albumInfo }

    // Album-level source picker. It keeps the shared context-menu chrome and
    // replaces the full-width QbzSelect that used to occupy a second row in
    // the header. The current source is marked in-place; unavailable persisted
    // variants remain visible but cannot be selected again.
    QbzContextMenu {
        id: playbackSourceMenu
        menuWidth: 310
        Repeater {
            model: root.playbackSourceOptions()
            delegate: Rectangle {
                required property var modelData
                required property int index
                readonly property bool selected:
                    index === root.playbackSourceIndex(root.playbackSourceOptions())
                width: parent ? parent.width : 0
                height: 38
                radius: 5
                opacity: modelData.enabled === true ? 1.0 : 0.45
                color: sourceOptionArea.containsMouse && modelData.enabled === true
                    ? theme.surfaceHover : "transparent"
                QbzIcon {
                    visible: parent.selected
                    name: "check"
                    width: 14
                    height: 14
                    x: 8
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "accent"
                }
                Text {
                    x: 30
                    width: parent.width - 38
                    height: parent.height
                    text: modelData.label || ""
                    color: theme.textSecondary
                    font.pixelSize: 13
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                MouseArea {
                    id: sourceOptionArea
                    anchors.fill: parent
                    enabled: modelData.enabled === true
                    hoverEnabled: true
                    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: {
                        playbackSourceMenu.close()
                        root.selectPlaybackSource(modelData)
                    }
                }
            }
        }
    }

    // Album ⋯ menu (AlbumContextMenu subset — playback_qt::enqueue_album owns
    // all three insertion modes since the QoL round gave it a real "later"
    // (#442 block-tail) arm. The offline row swaps on the live all-cached
    // aggregate).
    QbzContextMenu {
        id: albumMenu
        menuWidth: 196
            Repeater {
                model: root.buildAlbumMenuModel()
                delegate: Rectangle {
                    required property var modelData
                    width: parent ? parent.width : 0
                    height: 33
                    radius: 5
                    color: amiArea.containsMouse ? theme.surfaceHover : "transparent"
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 8
                        spacing: 8
                        QbzIcon { name: modelData.icon; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                        Text {
                            height: parent.height
                            width: parent.width - 23
                            text: modelData.label
                            color: theme.textSecondary
                            font.pixelSize: 13
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                    MouseArea {
                        id: amiArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            albumMenu.close()
                            var a = modelData.action
                            if (a === "play") QbzPlayer.playAlbum(albumHeader.id)
                            else if (a === "next") QbzPlayer.enqueueAlbum(albumHeader.id, "next")
                            else if (a === "later") QbzPlayer.enqueueAlbum(albumHeader.id, "later")
                            else if (a === "queue") QbzPlayer.enqueueAlbum(albumHeader.id, "queue")
                            else if (a === "favorite") {
                                root.setToggleState("album", !root.toggleState("album", albumHeader.isFavorite))
                                QbzLibrary.libraryToggleFavorite("album", albumHeader.id)
                            } else if (a === "pin") {
                                root.setToggleState("pin", !root.toggleState("pin", albumHeader.isPinned))
                                QbzLibrary.togglePin("album", albumHeader.id, albumHeader.title, albumHeader.artist, albumHeader.artUrl)
                            } else if (a === "playlist") {
                                // The loaded view's own track ids (Slint
                                // main.rs:12213 collects AlbumState.tracks).
                                var ids = []
                                var ts = root.album.tracks || []
                                for (var i = 0; i < ts.length; i++) {
                                    if (/^\d+$/.test(ts[i].id || "")) ids.push(ts[i].id)
                                }
                                if (ids.length > 0) QbzPlaylistPicker.openForTracks(JSON.stringify(ids))
                            } else if (a === "mixtape") {
                                QbzAlbum.addToMixtape(albumHeader.id)
                            } else if (a === "share-qobuz") {
                                QbzAlbum.shareQobuzLink(albumHeader.id)
                            } else if (a === "share-albumlink") {
                                QbzAlbum.shareAlbumLink(albumHeader.id)
                            } else if (a === "cache-album") {
                                QbzAlbum.albumCacheOffline(albumHeader.id)
                            } else if (a === "recache-album") {
                                QbzAlbum.albumRefreshOffline(albumHeader.id)
                            } else if (a === "download-purchase") {
                                QbzPurchases.openEntitledAlbum(albumHeader.id)
                            }
                        }
                    }
                }
            }

            // AlbumContextMenu.slint:153 — a 1px border-subtle separator, then
            // the album-blacklist toggle (:157-172). The .slint writes it as two
            // `if` arms (Block / Unblock) whose row count is constant; one row
            // with a flipping label is the same single row, and it is the shape
            // ArtistPageView.slint:561-572 uses for the identical toggle. Own
            // Rectangle rather than a sixth Repeater entry because the inline
            // delegate above has no separator arm — `{sep:true}` is CardMenu's
            // vocabulary, not this hand-rolled menu's (ArtistView.qml's
            // overflow menu draws its blacklist row exactly this way).
            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
            Rectangle {
                id: ablkRow
                width: parent.width
                height: 33
                radius: 5
                // Seeded from the header document, which does NOT carry the
                // field yet (album_qt.rs still has to port album.rs:683's
                // `set_is_album_blocked` seed — spec 03 F13). Read defensively:
                // `undefined` folds to false through toggleState's
                // `fallback === true`, so the row is correct the moment the
                // seed lands and never throws before then.
                readonly property bool blocked: root.toggleState("blocked", albumHeader.isAlbumBlocked)
                color: ablkArea.containsMouse ? theme.surfaceHover : "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    spacing: 8
                    QbzIcon { name: "blind-eye"; width: 15; height: 15; anchors.verticalCenter: parent.verticalCenter; tintName: "secondary" }
                    Text {
                        height: parent.height
                        width: parent.width - 23
                        text: ablkRow.blocked
                            ? QbzSession.tr("Unblock album", QbzSession.trRev)
                            : QbzSession.tr("Block this album", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: ablkArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        albumMenu.close()
                        // Optimistic flip (main.rs:12835), then the mutation;
                        // `blacklistChanged` above settles it — or rolls it
                        // back on a write failure (main.rs:12859).
                        root.setToggleState("blocked", !ablkRow.blocked)
                        QbzBlacklist.albumToggle(albumHeader.id, albumHeader.title,
                            albumHeader.artist, albumHeader.artUrl)
                    }
                }
            }
        }
}
