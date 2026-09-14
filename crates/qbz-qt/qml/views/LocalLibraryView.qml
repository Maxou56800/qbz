// Local Library — composition root for the QML port of
// crates/qbz-ui/ui/locallibrary/LocalLibraryView.slint (the shipping
// behaviour; that file is the ONLY reference).
//
// This file owns FOUR things and nothing else:
//   1. state — the Slint LocalLibraryState/LibAlbumFilterState fields;
//   2. the derived documents (search / sort / filter / grouping / A-Z), in
//      JS, because QbzLocal publishes ONE JSON document per surface (the
//      library_qt.rs transport rationale);
//   3. the fixed chrome (title row, tab bar, toolbar, divider) and which tab
//      body is mounted;
//   4. the ARTWORK WINDOW REGISTRY (`_windows` + `artMap`, below). This one
//      is here — and pushes the file past the 500-line guideline — because it
//      is the one policy that CANNOT be split per surface: several cover
//      surfaces are mounted at once (rail + grid, subfolders + track rows)
//      and eviction is only correct when it is decided against all of them
//      together. Splitting it per body is exactly the bug it replaced.
// Every body, rail, pane, popup and row lives in qml/views/local/.
//
// CHROME (identical to FavoritesView / LibraryView, per the Slint header):
//   row 1  "Local Library" title + settings gear + Plex sync
//   row 2  segmented tab bar (Albums / Artists / Folders / Tracks, with
//          count badges) on the left, the per-tab toolbar floated right
//   1px divider, then ONE content area.
//
// PERFORMANCE (this is the surface with the documented Slint freeze at 16K+
// tracks):
//   - Tracks is SERVER-paginated (500/page, appended on scroll);
//   - every list/grid is a windowing view (ListView over a chunk model),
//     never a Repeater over the full array — including the grouped arms and
//     the artist rail;
//   - the folder tree is a FLAT array windowed by a ListView, levels fetched
//     lazily on expand;
//   - artwork is id-keyed (artKey), requested only for the MOUNTED window,
//     resolved to a 256px thumbnail in Rust, and evicted from `artMap` once
//     it falls a full window away from the viewport.
//
// The local ALBUM detail is no longer inline: it is a routed page,
// views/LocalAlbumView.qml, exactly as the Slint routes to
// album/LocalAlbumView.slint.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../theme"
import "local"

Rectangle {
    id: root

    // Transparent while the ambient background is active (the frosted
    // content panel shows through — AppShell's contentFrame owns the fill).
    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    // Round to the AppShell content-frame bezel (QML clips are rectangular).
    radius: 12

    QbzTheme { id: theme }

    // Force construction before Rust publishes the first bounded page. The
    // hand-written singleton owns the resident LRU; keeping this reference at
    // the composition root also gives every Tracks delegate one stable model.
    readonly property var nativeTracksModel: QbzLocalTracks
    readonly property var nativeAlbumsModel: QbzLocalAlbums
    readonly property var nativeArtistsModel: QbzLocalArtists
    readonly property var nativeArtistAlbumsModel: QbzLocalArtistAlbums

    // ============================ state ==================================
    // One app-wide order drives this bar, both nav flyouts and logged-off
    // startup. Rust seeds the key synchronously before the full settings
    // snapshot, so the first frame never flashes a different default.
    readonly property var localTabOrder: {
        try {
            var stored = JSON.parse(QbzBridge.settingsJson).localTabOrder
            if (Array.isArray(stored) && stored.length > 0) return stored
        } catch (e) { /* construction fallback below */ }
        return ["genres", "albums", "artists", "folders", "tracks"]
    }
    readonly property string localDefaultTab:
        localTabOrder.length > 0 ? localTabOrder[0] : "genres"
    readonly property string genreFiltersPosition: {
        try {
            var value = JSON.parse(QbzBridge.settingsJson).genreFiltersPosition || "top"
            if (["top", "right", "left", "bottom"].indexOf(value) >= 0) return value
        } catch (e) { /* construction fallback below */ }
        return "top"
    }
    property string activeTab: localDefaultTab
    // Router requests go through the same history path as the in-view bar.
    // The sequence makes reselecting a flyout tab after an in-view change work.
    property var tabNavigationRequest: ({})
    onTabNavigationRequestChanged: {
        if (tabNavigationRequest.tab) activateTab(tabNavigationRequest.tab)
    }
    function activateTab(tab) {
        if (tab === activeTab) return
        if (localTabOrder.indexOf(tab) < 0 && !(tab === "ephemeral" && ephemeralActive)) return
        if (_navigationReady && !_restoringNavigationState && QbzShell.currentView === "local")
            QbzShell.recordLocalTab(tab, navigationStateJson)
        activeTab = tab
    }
    // Session-only file reachability. Keys are `source:id`; values are the
    // user-facing reason. This is deliberately not persisted: a moved file or
    // unavailable NAS must remain retryable and a rescan remains authoritative.
    property var localTrackErrors: ({})
    function localTrackErrorKey(source, id) {
        return (source || "local") + ":" + String(id || "")
    }

    // Albums tab.
    property string albumsSearch: ""
    property string albumsSort: "artist-asc"
    property string albumsGroup: "off"      // off | alpha | artist
    property string albumsView: "grid"      // grid | list
    property bool albumsMultiSelect: false
    property var albumsSelected: ({})
    readonly property int albumsSelectedCount: root.albumsNativeViewActive
        ? QbzLocal.localAlbumsNativeSelectedCount
        : Object.keys(albumsSelected).length

    // Folders tab.
    property string foldersMode: "tree"     // flat | tree
    property string foldersSearch: ""
    property string foldersSort: "artist-asc"
    property string foldersGroup: "off"
    property string foldersGridView: "grid"
    property string treeSearch: ""
    property bool treeSelectMode: false
    property string selectedFolder: ""
    property string folderDetailSearch: ""
    property string folderDetailView: "grid"
    property int treeRailWidth: 272

    // Artists tab.
    property string artistsSearch: ""
    property string artistsSort: "name-asc"
    property string selectedArtist: ""

    // Tracks tab.
    property string tracksSearch: ""
    // off | album | artist | name — PERSISTED, so it is owned by the bridge
    // exactly like the Tracks sort and the album-identity mode, not by a QML
    // default. It used to be a plain `property string tracksGroup: "off"`:
    // `locallibrary_ui.json` carried the user's choice across restarts and
    // nothing ever read it back into the UI (PARITY-DEBT #13).
    readonly property string tracksGroup: QbzLocal.localTracksGroup
    property bool tracksMultiSelect: false
    property var tracksSelected: ({})
    readonly property int tracksSelectedCount: QbzLocal.localTracksNativeActive
        ? QbzLocal.localTracksNativeSelectedCount
        : Object.keys(tracksSelected).length

    // Genres column browser. Selections are maps so Ctrl/Cmd toggles stay
    // O(1), and an empty map is the explicit All state.
    property string genresSearch: ""
    property string genreYearsSearch: ""
    property string genreArtistsSearch: ""
    property string genreAlbumsSearch: ""
    property var selectedGenres: ({})
    property var selectedGenreYears: ({})
    property var selectedGenreArtists: ({})
    property var selectedGenreAlbums: ({})
    // Which leading Library Explorer facet is mounted. Genre remains the
    // default; "both" deliberately adds a fourth column for larger screens.
    // Seeded from locallibrary_ui.json; navigation-state restoration may
    // temporarily reassign it while rebuilding a Back/Forward snapshot.
    property string explorerColumns: QbzLocal.localExplorerColumns // genre | year | both
    readonly property int selectedGenreCount: Object.keys(selectedGenres).length
    readonly property int selectedGenreYearCount: Object.keys(selectedGenreYears).length
    readonly property int selectedGenreArtistCount: Object.keys(selectedGenreArtists).length
    readonly property int selectedGenreAlbumCount: Object.keys(selectedGenreAlbums).length
    property bool genresBrowserCollapsed: false
    property string genresView: "details"   // grid | list | details
    property string genresSort: "title-asc"

    // Routed history state. Scroll is handled independently by ScrollMemory;
    // this document restores the controls that determine what that offset
    // means before the tab asks for data.
    property bool _restoringNavigationState: false
    property bool _navigationReady: false
    readonly property string navigationStateJson: JSON.stringify({
        activeTab: root.activeTab,
        albumsSearch: root.albumsSearch,
        albumsSort: root.albumsSort,
        albumsGroup: root.albumsGroup,
        albumsView: root.albumsView,
        albumsFilter: root.albumsFilter,
        artistsFilter: root.artistsFilter,
        tracksFilter: root.tracksFilter,
        genresFilter: root.genresFilter,
        foldersMode: root.foldersMode,
        foldersSearch: root.foldersSearch,
        foldersSort: root.foldersSort,
        foldersGroup: root.foldersGroup,
        foldersGridView: root.foldersGridView,
        treeSearch: root.treeSearch,
        selectedFolder: root.selectedFolder,
        folderDetailSearch: root.folderDetailSearch,
        folderDetailView: root.folderDetailView,
        treeRailWidth: root.treeRailWidth,
        artistsSearch: root.artistsSearch,
        artistsSort: root.artistsSort,
        selectedArtist: root.selectedArtist,
        tracksSearch: root.tracksSearch,
        genresSearch: root.genresSearch,
        genreYearsSearch: root.genreYearsSearch,
        genreArtistsSearch: root.genreArtistsSearch,
        genreAlbumsSearch: root.genreAlbumsSearch,
        selectedGenres: root.selectedGenres,
        selectedGenreYears: root.selectedGenreYears,
        selectedGenreArtists: root.selectedGenreArtists,
        selectedGenreAlbums: root.selectedGenreAlbums,
        explorerColumns: root.explorerColumns,
        genresBrowserCollapsed: root.genresBrowserCollapsed,
        genresView: root.genresView,
        genresSort: root.genresSort
    })
    onNavigationStateJsonChanged: {
        if (root._navigationReady && !root._restoringNavigationState
                && QbzShell.currentView === "local")
            QbzShell.reportNavState("local", root.navigationStateJson)
    }
    function restoreNavigationState() {
        var state = QbzShell.restoreStateScope === "local" && QbzShell.stateRestore !== ""
            ? QbzShell.stateRestore : QbzShell.localNavigationState()
        if (state === "") return
        var saved
        try { saved = JSON.parse(state) }
        catch (e) { QbzShell.restoreStateScope = ""; return }
        root._restoringNavigationState = true
        function text(name, fallback) {
            return typeof saved[name] === "string" ? saved[name] : fallback
        }
        root.activeTab = text("activeTab", root.activeTab)
        root.albumsSearch = text("albumsSearch", root.albumsSearch)
        root.albumsSort = text("albumsSort", root.albumsSort)
        root.albumsGroup = text("albumsGroup", root.albumsGroup)
        root.albumsView = text("albumsView", root.albumsView)
        if (saved.albumsFilter && typeof saved.albumsFilter === "object")
            root.albumsFilter = saved.albumsFilter
        else if (saved.filter && typeof saved.filter === "object")
            root.albumsFilter = saved.filter
        if (saved.artistsFilter && typeof saved.artistsFilter === "object")
            root.artistsFilter = saved.artistsFilter
        if (saved.tracksFilter && typeof saved.tracksFilter === "object")
            root.tracksFilter = saved.tracksFilter
        if (saved.genresFilter && typeof saved.genresFilter === "object")
            root.genresFilter = saved.genresFilter
        // Older history entries kept four independent funnels. Adopt the one
        // belonging to the restored tab as the shared quality/format/source
        // state, while preserving the Albums/Genres-only favorite predicate.
        var restoredFilter = root.activeTab === "tracks" ? root.tracksFilter
            : root.activeTab === "artists" ? root.artistsFilter
            : root.activeTab === "genres" ? root.genresFilter
            : root.albumsFilter
        if (root.albumsFilter.favorite === true
                || root.genresFilter.favorite === true) {
            restoredFilter = Object.assign({}, restoredFilter)
            restoredFilter.favorite = true
        }
        root.synchronizeFilters(root.activeTab, restoredFilter, false)
        root.foldersMode = text("foldersMode", root.foldersMode)
        root.foldersSearch = text("foldersSearch", root.foldersSearch)
        root.foldersSort = text("foldersSort", root.foldersSort)
        root.foldersGroup = text("foldersGroup", root.foldersGroup)
        root.foldersGridView = text("foldersGridView", root.foldersGridView)
        root.treeSearch = text("treeSearch", root.treeSearch)
        root.selectedFolder = text("selectedFolder", root.selectedFolder)
        root.folderDetailSearch = text("folderDetailSearch", root.folderDetailSearch)
        root.folderDetailView = text("folderDetailView", root.folderDetailView)
        if (typeof saved.treeRailWidth === "number") root.treeRailWidth = saved.treeRailWidth
        root.artistsSearch = text("artistsSearch", root.artistsSearch)
        root.artistsSort = text("artistsSort", root.artistsSort)
        root.selectedArtist = text("selectedArtist", root.selectedArtist)
        root.tracksSearch = text("tracksSearch", root.tracksSearch)
        root.genresSearch = text("genresSearch", root.genresSearch)
        root.genreYearsSearch = text("genreYearsSearch", root.genreYearsSearch)
        root.genreArtistsSearch = text("genreArtistsSearch", root.genreArtistsSearch)
        root.genreAlbumsSearch = text("genreAlbumsSearch", root.genreAlbumsSearch)
        if (saved.selectedGenres && typeof saved.selectedGenres === "object")
            root.selectedGenres = saved.selectedGenres
        if (saved.selectedGenreYears && typeof saved.selectedGenreYears === "object")
            root.selectedGenreYears = saved.selectedGenreYears
        if (saved.selectedGenreArtists && typeof saved.selectedGenreArtists === "object")
            root.selectedGenreArtists = saved.selectedGenreArtists
        if (saved.selectedGenreAlbums && typeof saved.selectedGenreAlbums === "object")
            root.selectedGenreAlbums = saved.selectedGenreAlbums
        var savedExplorerColumns = text("explorerColumns", root.explorerColumns)
        if (["genre", "year", "both"].indexOf(savedExplorerColumns) >= 0)
            root.explorerColumns = savedExplorerColumns
        if (typeof saved.genresBrowserCollapsed === "boolean")
            root.genresBrowserCollapsed = saved.genresBrowserCollapsed
        root.genresView = text("genresView", root.genresView)
        root.genresSort = text("genresSort", root.genresSort)
        QbzShell.restoreStateScope = ""
        root._restoringNavigationState = false
        QbzShell.reportNavState("local", root.navigationStateJson)
    }
    Connections {
        target: QbzShell
        function onStateRestoreChanged() {
            // Back/Forward between tabs keeps this component mounted: there
            // is no Component.onCompleted to consume the destination state.
            if (root._navigationReady && QbzShell.currentView === "local"
                    && QbzShell.restoreStateScope === "local" && QbzShell.stateRestore !== "") {
                root.restoreNavigationState()
                root.loadActiveTab()
            }
        }
    }
    Timer {
        id: navigationSeed
        interval: 0
        onTriggered: {
            // Initial persisted state, pending artist and flyout landing tab
            // are one arrival, not separate history entries.
            root._navigationReady = true
            QbzShell.reportNavState("local", root.navigationStateJson)
        }
    }

    // One quality/format/source funnel follows the user across Albums,
    // Artists, Genres and Tracks. Favorites remains an Albums/Genres-only
    // predicate, but it survives visits to tabs where that chip is absent.
    property bool filterOpen: false
    // SEEDED FROM THE BRIDGE, not defaulted to `{}`. This view is DESTROYED on
    // every navigation away (see the bridge's `albums_filter` doc), so a
    // view-local default meant the funnel reset the moment the user visited
    // Discover — and never survived a restart at all. `QbzLocal.albumsFilter`
    // outlives the view and mirrors `ui_prefs.json`.
    property var albumsFilter: root.parseFilter(QbzLocal.albumsFilter)
    property var artistsFilter: root.commonFilter(albumsFilter)
    property var tracksFilter: root.commonFilter(albumsFilter)
    property var genresFilter: Object.assign({}, albumsFilter)
    // Live heart fan-out. Album JSON/native pages are immutable snapshots;
    // this tiny id map updates every mounted card and the Favorites-only
    // predicate without republishing thousands of album rows (and therefore
    // without moving the scroll position).
    property var favoriteOverrides: ({})
    readonly property bool albumsNativeViewActive:
        QbzLocal.localAlbumsNativeActive
        && albumsFilter.favorite !== true
        && activeTab === "albums"
    readonly property var filter: activeTab === "artists" ? artistsFilter
        : activeTab === "tracks" ? tracksFilter
        : activeTab === "genres" ? genresFilter
        : albumsFilter
    function parseFilter(json) {
        if (!json || json === "") return ({})
        try { return JSON.parse(json) || ({}) } catch (e) { return ({}) }
    }
    function commonFilter(value) {
        var out = Object.assign({}, value || ({}))
        delete out.favorite
        return out
    }
    function synchronizeFilters(sourceTab, value, persist) {
        var wasFavoriteOnly = albumsFilter.favorite === true
        var favorite = sourceTab === "albums" || sourceTab === "genres"
            ? value.favorite === true : wasFavoriteOnly
        var common = commonFilter(value)
        var albumValue = Object.assign({}, common)
        var genreValue = Object.assign({}, common)
        if (favorite) {
            albumValue.favorite = true
            genreValue.favorite = true
        }
        albumsFilter = albumValue
        artistsFilter = Object.assign({}, common)
        tracksFilter = Object.assign({}, common)
        genresFilter = genreValue
        if (!persist) return
        QbzLocal.setAlbumsFilterJson(Object.keys(albumValue).length === 0
            ? "" : JSON.stringify(albumValue))
        // This setter resets the Tracks query, so call it only while Tracks
        // is the visible consumer. A later tab switch applies the already
        // shared state before loading page one.
        if (activeTab === "tracks")
            QbzLocal.tracksSetFilterJson(JSON.stringify(common))
        if (!wasFavoriteOnly && favorite && activeTab === "albums")
            QbzLocal.loadTab("albums-legacy")
    }
    // A LATER republish still wins: the gates and the saved funnel are
    // published together when a media server is connected or removed, and the
    // pruning that happens there (a tick whose chip is now hidden) has to
    // reach a view that is already open. Guarded on a real difference so
    // writing the property back does not bounce.
    Connections {
        target: QbzLocal
        function onAlbumsFilterChanged() {
            var next = root.parseFilter(QbzLocal.albumsFilter)
            if (JSON.stringify(next) !== JSON.stringify(root.albumsFilter))
                root.synchronizeFilters("albums", next, false)
        }
        function onLocalAlbumFavoriteChanged(id, favorite) {
            var next = Object.assign({}, root.favoriteOverrides)
            next[id] = favorite
            root.favoriteOverrides = next
        }
        function onLocalTrackAvailability(source, id, message) {
            var next = Object.assign({}, root.localTrackErrors)
            var key = root.localTrackErrorKey(source, id)
            if (message === "") delete next[key]
            else next[key] = message
            root.localTrackErrors = next
        }
    }
    readonly property int filterCount: {
        var n = 0
        for (var k in filter) if (filter[k]) n++
        return n
    }
    /// The ONE writer. Both mutators funnel through it, so the four tabs
    /// cannot drift into different source/format/quality states.
    function setFilter(f) {
        synchronizeFilters(activeTab, f, true)
    }
    function toggleFilter(key) {
        var f = Object.assign({}, filter)
        f[key] = !f[key]
        if (!f[key]) delete f[key]
        setFilter(f)
    }
    function clearFilter() { setFilter({}) }

    /// The funnel's state as the applied-filters tooltip wants it: grouped
    /// in the popup's own order, each holding the labels of the keys that are
    /// on. Built HERE because this is where the state lives — the toolbar only
    /// draws the trigger, and `LocalFilterPopup` only draws chips.
    ///
    /// The labels are the popup's, verbatim, so the bubble reads exactly like
    /// the control the user set. Two are not translated there either: FLAC /
    /// ALAC / APE / WAV / MP3 / AAC and "Plex" are proper names.
    readonly property var filterSummaryGroups: {
        function pick(keys, labels) {
            var out = []
            for (var i = 0; i < keys.length; i++)
                if (root.filter[keys[i]] === true)
                    out.push(labels[i])
            return out
        }
        var tr = QbzSession.trRev
        return [
            { group: QbzSession.tr("Favorites", tr),
              values: pick(["favorite"], [QbzSession.tr("Favorites only", tr)]) },
            { group: QbzSession.tr("Quality", tr),
              values: pick(["dsd", "hires", "cd", "lossy"],
                           ["DSD", QbzSession.tr("Hi-Res", tr), QbzSession.tr("CD", tr),
                            QbzSession.tr("Lossy", tr)]) },
            { group: QbzSession.tr("Format", tr),
              values: pick(["flac", "alac", "ape", "wav", "mp3", "aac", "other"],
                           ["FLAC", "ALAC", "APE", "WAV", "MP3", "AAC",
                            QbzSession.tr("Other", tr)]) },
            { group: QbzSession.tr("Source", tr),
              values: pick(["local", "offline", "plex", "jellyfin", "subsonic"],
                           [QbzSession.tr("Local", tr),
                            QbzSession.tr("Offline cache", tr), "Plex",
                            "Jellyfin", "Subsonic"]) }
        ]
    }

    // Per-row artwork on the track lists — the Slint gates this on
    // AppearanceState.local-library-track-artwork for the freeze reason and
    // its default is OFF.
    readonly property bool trackArtwork: QbzLocal.localTrackArtwork

    readonly property bool ephemeralActive: QbzLocal.localEphemeralActive

    /// Badge count for the ephemeral tab. Reads the SAME document the pane
    /// renders, so the number on the tab and the rows behind it are one fact.
    readonly property int ephemeralTrackCount:
        ephemeral && ephemeral.trackCount ? ephemeral.trackCount : 0

    /// The ephemeral tab's LABEL — the name of the thing you opened, with no
    /// verb in front of it. Computed in Rust (`local_ephemeral::display_label`)
    /// and read here, so this view and `NavFlyout` cannot name it differently:
    /// the flyout does not parse the ephemeral document and should not start.
    ///
    /// A verb was tried and rejected: "Now Playing" / "Playing: …" is false the
    /// moment a folder sits open while something else plays, and the tab would
    /// contradict the now-playing bar on the same screen.
    readonly property string ephemeralLabel: QbzLocal.localEphemeralLabel

    /// The ephemeral tab is the only one that can VANISH while the user stands
    /// on it (the folder is closed, the disc is ejected). Without this the view
    /// keeps `activeTab === "ephemeral"` with no tab in the bar and no Loader
    /// active: a blank content area and no way back except another tab. The
    /// user's first ordered surface is the same fallback every other Local
    /// Library entry point uses.
    onEphemeralActiveChanged: {
        if (!ephemeralActive && activeTab === "ephemeral")
            activeTab = root.localDefaultTab
    }

    /// Opening something must LAND you on it. While the pane was an arm of the
    /// Folders tab this was implicit — the arm simply took the tab over.
    ///
    /// This watches the open SEQUENCE, not `ephemeralActive`. The flag is
    /// already `true` when you open a SECOND folder over a first, so a handler
    /// on the flag fires exactly once in the life of the app and then never
    /// again — which is precisely the reported symptom: the view stayed on
    /// whatever tab you were reading. A sequence changes on every open.
    ///
    /// It also excludes the boot restore by construction:
    /// `local_ephemeral::scan` bumps it only when a runtime is passed, and only
    /// `open`/`open_path` pass one. A remembered folder therefore never steals
    /// the tab you actually navigated to.
    Connections {
        target: QbzLocal
        function onLocalEphemeralOpenSeqChanged() { root.activateTab("ephemeral") }
    }

    function loadTabForView(tab) {
        QbzLocal.loadTab(tab === "albums" && root.albumsFilter.favorite === true
            ? "albums-legacy" : tab)
    }

    // ---------------------------- documents ------------------------------
    function parseDoc(json, fallback) {
        if (json === "") return fallback
        try {
            return JSON.parse(json)
        } catch (e) {
            console.warn("[qbz-qt] local: bad document — " + e)
            return fallback
        }
    }
    readonly property var counts: parseDoc(QbzLocal.localCountsJson, ({}))
    readonly property var albums: parseDoc(QbzLocal.localAlbumsJson, [])
    readonly property var artists: parseDoc(QbzLocal.localArtistsJson, [])
    readonly property var folders: parseDoc(QbzLocal.localFoldersJson, [])
    readonly property var tree: parseDoc(QbzLocal.localTreeJson, [])
    readonly property var tracks: parseDoc(QbzLocal.localTracksJson, [])
    readonly property var folderDetail: parseDoc(QbzLocal.localDetailJson, null)
    readonly property var ephemeral: parseDoc(QbzLocal.localEphemeralJson, null)

    // Decoded-cover map {artKey: "file://…"} — fed by the id-keyed signal.
    property var artMap: ({})
    property var _artKeep: ({})
    // Keep the high-density path unchanged until it passes the CPU budget.
    // This remains reactive when the window moves between displays.
    readonly property bool immediateArtwork: QbzLocal.artworkImmediateEnabled()
        && Screen.devicePixelRatio <= 1
    readonly property bool artTimingEnabled: QbzLocal.artworkTimingEnabled()
    property var _artTraceWindows: ({})
    property int _artTraceSequence: 0

    function traceArtReport(rows, first, last, surface) {
        if (!root.artTimingEnabled) return
        var keys = []
        for (var i = Math.max(0, first); i <= Math.min(last, rows.length - 1); i++)
            if (rows[i] && rows[i].artKey) keys.push(rows[i].artKey)
        var signature = keys.join("|")
        var old = root._artTraceWindows[surface]
        if (old && old.signature === signature) return
        if (old) {
            for (var j = 0; j < old.keys.length; j++)
                QbzLocal.artworkTiming("window-end", old.keys[j], old.id)
        }
        var id = surface + ":" + (++root._artTraceSequence)
        root._artTraceWindows[surface] = {id: id, keys: keys, signature: signature}
        for (var k = 0; k < keys.length; k++)
            QbzLocal.artworkTiming("report", keys[k], id)
    }

    function traceArt(stage, key) {
        if (!root.artTimingEnabled) return
        for (var surface in root._artTraceWindows) {
            var w = root._artTraceWindows[surface]
            if (w.keys.indexOf(key) >= 0)
                QbzLocal.artworkTiming(stage, key, w.id)
        }
    }

    // Covers arrive ONE AT A TIME (src/local_artwork.rs phase 2 streams each
    // thumbnail through the id-keyed signal the moment it resolves). Rebinding
    // `artMap` per arrival is quadratic in the window: each arrival copied the
    // whole map and re-evaluated the art binding of EVERY mounted cell, so a
    // 50-cover window did ~2500 binding evaluations and 50 map copies during
    // exactly the seconds the grid is trying to stay smooth. Arrivals are
    // coalesced into ONE rebind per frame instead — the covers still appear
    // progressively (16ms granularity is invisible), at O(n) total cost.
    property var _artInbox: ({})

    // ORDERED REVEAL. The cold-thumbnail pool (local_artwork.rs:163) starts
    // work in window order but FINISHES out of order, because decode cost
    // varies per cover — so covers used to pop in scattered across the grid.
    // Ordering the emission in Rust instead would be a mistake: one slow
    // decode at index 3 would hold 4..N behind it, which ADDS latency rather
    // than hiding it.
    //
    // So arrivals are applied behind a reveal FRONT that walks down the
    // window at a fixed rate. A cover that lands ahead of the front waits for
    // it; a cover that lands behind it (the slow ones) is applied at once,
    // having already been passed. The front advances every tick unconditionally,
    // which is what makes a stall impossible: a key is delayed at most
    // (ordinal / step) ticks no matter what the pool does, and a key with no
    // ordinal at all is released immediately.
    //
    // `_artOrder` is rebuilt per window request (see flushWindows), so the
    // wipe restarts from the top of whatever is on screen now.
    property var _artOrder: ({})
    property var _artHeld: ({})
    property int _artFront: 0
    /// Keys the front passes per 16ms tick — about one grid row, which reads
    /// as a wipe rather than a sweep. A ~50-cover window resolves in ~130ms.
    property int artRevealStep: 6

    Timer {
        id: artFlush
        interval: 16
        repeat: false
        onTriggered: {
            if (root.immediateArtwork) {
                // One rebind for this batch, with no decorative reveal front.
                root.artMap = Object.assign({}, root.artMap, root._artHeld, root._artInbox)
                root._artHeld = ({})
                root._artInbox = ({})
                return
            }
            var k
            // Everything that arrived this tick joins whatever is still held.
            for (k in root._artInbox) root._artHeld[k] = root._artInbox[k]
            root._artInbox = ({})

            root._artFront += root.artRevealStep

            var m = Object.assign({}, root.artMap)
            var released = false
            var stillHeld = false
            for (k in root._artHeld) {
                var ord = root._artOrder[k]
                // No ordinal means this key is not part of the current window
                // (a late arrival from a window we have scrolled past). Holding
                // it would be holding it forever.
                if (ord === undefined || ord < root._artFront) {
                    m[k] = root._artHeld[k]
                    delete root._artHeld[k]
                    released = true
                } else {
                    stillHeld = true
                }
            }
            // A rebind needs a NEW object reference (same-ref assignment is
            // not a change in QML).
            if (released) root.artMap = m
            // Keep ticking while anything waits, or the front stops moving and
            // the covers behind it never land.
            if (stillHeld) artFlush.start()
        }
    }
    Connections {
        target: QbzLocal
        function onLocalArtworkReady(key, path) {
            root.traceArt("path", key)
            root.nativeTracksModel.setArtwork(key, path)
            root.nativeAlbumsModel.setArtwork(key, path)
            root.nativeArtistsModel.setArtwork(key, path)
            root.nativeArtistAlbumsModel.setArtwork(key, path)
            if (root.immediateArtwork && !root._artKeep[key]) return
            root._artInbox[key] = path
            if (!artFlush.running) artFlush.start()
            // An arrival is live evidence that the pass is still running, so
            // it keeps every placeholder's settle countdown suspended. Without
            // this the bound is a fixed timeout from the REPORT, and a cold
            // local library resolves slower than that (see artPassMs).
            root.artPulse = true
            artPulseOff.restart()
        }
    }
    Connections {
        target: root.nativeTracksModel
        function onPageMiss(page, generation) {
            QbzLocal.tracksNativePageMiss(page, generation)
        }
    }
    Connections {
        target: root.nativeAlbumsModel
        function onPageMiss(page, generation) {
            QbzLocal.albumsNativePageMiss(page, generation)
        }
    }
    Connections {
        target: root.nativeArtistsModel
        function onPageMiss(page, generation) {
            QbzLocal.artistsNativePageMiss(page, generation)
        }
    }
    Connections {
        target: root.nativeArtistAlbumsModel
        function onPageMiss(page, generation) {
            QbzLocal.artistAlbumsNativePageMiss(page, generation)
        }
    }

    // A name route from LocalAlbumView ("go to artist" on a local album:
    // local/Plex artists have no catalog id) lands the view on the Artists
    // tab with that artist selected.
    function consumePendingArtist() {
        var pending = QbzLocal.localPendingArtist
        if (pending === "") return
        activateTab("artists")
        selectedArtist = pending
        QbzLocal.clearPendingArtist()
    }

    // A TAB route from the cortinilla's local "View more" links. Same shape
    // as consumePendingArtist above: the property change is the trigger, so
    // it must be released after it is applied or the same link cannot fire
    // twice in a row.
    function consumePendingRoute() {
        var raw = QbzLocal.localPendingRoute
        if (raw === "") return
        var route
        try { route = JSON.parse(raw) } catch (e) { QbzLocal.clearPendingRoute(); return }
        if (route.tab) activateTab(route.tab)
        // The query pre-filters the TRACKS tab only; the albums and artists
        // tabs have no search box of their own.
        if (route.tab === "tracks" && route.query) {
            // Set the view's own search state too, not just the query, or the
            // box would render empty while the list is filtered.
            root.tracksSearch = route.query
            QbzLocal.tracksSearch(route.query)
        }
        QbzLocal.clearPendingRoute()
    }

    Connections {
        target: QbzLocal
        function onLocalPendingArtistChanged() { root.consumePendingArtist() }
        function onLocalPendingRouteChanged() { root.consumePendingRoute() }
    }

    // Mount: load the default tab. Tab switches load on demand (each tab is
    // one query; the Albums/Folders/Artists sets are bounded).
    Component.onCompleted: {
        restoreNavigationState()
        consumePendingArtist()
        consumePendingRoute()
        loadActiveTab()
        navigationSeed.restart()
    }
    function loadActiveTab() {
        if (root.activeTab === "tracks")
            QbzLocal.tracksSetFilterJson(JSON.stringify(root.tracksFilter))
        else
            root.loadTabForView(root.activeTab)
        if (root.activeTab === "tracks" && root.tracksSearch !== "")
            QbzLocal.tracksSearch(root.tracksSearch)
    }
    Component.onDestruction: {
        if (QbzLocal.localAlbumsNativeActive) QbzLocal.albumsNativeClearSelection()
        if (QbzLocal.localTracksNativeActive) QbzLocal.tracksNativeClearSelection()
    }
    onActiveTabChanged: {
        // Tracks owns a server-side descriptor; install the shared funnel
        // before its first page is queried. The setter performs that load.
        if (!root._restoringNavigationState) {
            if (activeTab === "tracks")
                QbzLocal.tracksSetFilterJson(JSON.stringify(root.tracksFilter))
            else
                root.loadTabForView(activeTab)
        }
        // The tab that just appeared has covers to ask for and the one that
        // left has covers to let go of. Each surface answers for itself.
        artworkRefresh()
    }

    /// "Re-report your window." Broadcast for the state changes no single
    /// surface can observe on its own (the tab switch). A surface that is
    /// hidden when it hears this releases its slot instead.
    signal artworkRefresh()

    // The folder-detail cover set is SMALL and fully mounted, so the whole
    // set is one window report (the grids/lists window themselves).
    //
    // BOTH of its cover sections contribute. The report used to carry the
    // subfolder cards only, while LocalFolderDetail ALSO binds `artMap` on
    // the folder's direct track rows — those rows asked for a key nobody had
    // requested, so their 36px cell stayed blank forever. The track half is
    // gated on `trackArtwork` (default OFF), which is why this is a derived
    // property: flipping the setting re-derives it and re-reports by itself.
    readonly property var folderDetailArtRows: {
        if (!folderDetail) return []
        var out = (folderDetail.subfolders || []).slice()
        if (trackArtwork && folderDetail.tracks) {
            for (var i = 0; i < folderDetail.tracks.length; i++)
                out.push(folderDetail.tracks[i])
        }
        return out
    }
    function reportFolderDetailWindow() {
        var rows = folderDetailArtRows
        if (rows.length === 0) { releaseWindow("folder-detail"); return }
        queueWindowReport(rows, 0, rows.length - 1, "folder-detail")
    }
    onFolderDetailArtRowsChanged: reportFolderDetailWindow()

    function reportEphemeralWindow() {
        var rows = (ephemeral && ephemeral.albums) ? ephemeral.albums : []
        if (rows.length === 0) { releaseWindow("ephemeral"); return }
        queueWindowReport(rows, 0, rows.length - 1, "ephemeral")
    }
    onEphemeralChanged: reportEphemeralWindow()

    // ---------------------- derived (JS, per-tab) -------------------------
    function sortRows(rows, key) {
        var field = key.indexOf("year") === 0 ? "year"
                  : key.indexOf("title") === 0 ? "title" : "artist"
        var desc = key.slice(-4) === "desc"
        var out = rows.slice()
        out.sort(function (a, b) {
            var av = (a[field] || "").toString().toLowerCase()
            var bv = (b[field] || "").toString().toLowerCase()
            if (av === bv) {
                var at = (a.title || "").toLowerCase()
                var bt = (b.title || "").toLowerCase()
                return at < bt ? -1 : at > bt ? 1 : 0
            }
            return av < bv ? -1 : 1
        })
        if (desc) out.reverse()
        return out
    }

    function filterRows(rows, needle) {
        var q = needle.trim().toLowerCase()
        if (q === "") return rows
        var out = []
        for (var i = 0; i < rows.length; i++) {
            var r = rows[i]
            if ((r.title || "").toLowerCase().indexOf(q) >= 0
                || (r.artist || "").toLowerCase().indexOf(q) >= 0) out.push(r)
        }
        return out
    }

    // Quality / format / source chips. Each SECTION is an OR within itself
    // and an AND across sections — an empty section means "any".
    function sourceBucket(word) {
        var source = (word || "local").toLowerCase()
        // library.db stores ordinary scanner rows as `user`; every Local
        // Library surface exposes that storage word through the Local chip.
        if (source === "" || source === "user") return "local"
        if (source === "qobuz_purchase" || source === "qobuz_download")
            return "offline"
        if (source === "navidrome" || source === "gonic"
            || source === "airsonic" || source === "astiga")
            return "subsonic"
        return source
    }

    function applyFilter(rows, selectedFilter) {
        var selected = selectedFilter || ({})
        if (Object.keys(selected).length === 0) return rows
        var qAny = selected.dsd || selected.hires || selected.cd || selected.lossy
        var fAny = selected.flac || selected.alac || selected.ape || selected.wav
            || selected.mp3 || selected.aac || selected.other
        var sAny = selected.local || selected.offline || selected.plex
            || selected.jellyfin || selected.subsonic
        var known = { "flac": 1, "alac": 1, "ape": 1, "wav": 1, "mp3": 1, "aac": 1 }
        var out = []
        for (var i = 0; i < rows.length; i++) {
            var r = rows[i]
            if (selected.favorite === true && !root.albumFavorite(r)) continue
            if (qAny || fAny || sAny) {
                // Test all active media sections against ONE physical copy.
                // Independent aggregate arrays are not enough: a logical
                // album can carry DSD on its purchased copy and FLAC on a
                // different local copy. DSD+FLAC must not manufacture a
                // parent whose filtered detail has zero tracks.
                var variants = r.mediaVariants || []
                if (variants.length === 0) {
                    var sources = r.sources && r.sources.length > 0
                        ? r.sources : [r.source || "local"]
                    variants = []
                    for (var si = 0; si < sources.length; si++) {
                        variants.push({ "qualityTier": r.qualityTier || "",
                                      "format": r.format || "",
                                      "source": sources[si] })
                    }
                }
                var mediaMatches = false
                for (var vi = 0; vi < variants.length; vi++) {
                    var variant = variants[vi]
                    var tier = (variant.qualityTier || "").toLowerCase()
                    var fmt = (variant.format || "").toLowerCase()
                    var qok = !qAny
                        || (selected.dsd && tier === "dsd")
                        || (selected.hires && (tier === "hires" || tier === "max"))
                        || (selected.cd && tier === "cd")
                        || (selected.lossy && (tier === "mp3" || tier === "lossy"))
                    var fok = !fAny || selected[fmt] === true
                        || (selected.other === true && known[fmt] !== 1)
                    // `sourceBucket` folds qobuz_purchase into Offline while
                    // its raw word remains available to the gold badge.
                    var sok = !sAny
                        || selected[sourceBucket(variant.source)] === true
                    if (qok && fok && sok) {
                        mediaMatches = true
                        break
                    }
                }
                if (!mediaMatches) continue
            }
            out.push(r)
        }
        return out
    }

    function albumFavorite(album) {
        if (!album || !album.id) return false
        var override = root.favoriteOverrides[album.id]
        return override === undefined ? album.isFavorite === true : override === true
    }

    function toggleAlbumFavorite(album, artworkUrl) {
        if (!album || !album.id || album.favoriteable !== true) return
        var sources = album.sources && album.sources.length > 0
            ? album.sources : [album.sourceRaw || album.source || ""]
        QbzLocal.albumToggleFavorite(
            album.id,
            album.title || "",
            album.artist || "",
            artworkUrl || "",
            JSON.stringify(sources))
    }

    // A-Z / by-artist grouping — [{ letter, items }] plus the alpha jumps the
    // strips consume (the strip derives its own indices from the groups).
    function groupRows(rows, mode) {
        if (mode === "off") return []
        var buckets = {}
        var order = []
        for (var i = 0; i < rows.length; i++) {
            var r = rows[i]
            var key
            if (mode === "artist") {
                key = (r.artist || "").trim() || "—"
            } else {
                var c = ((r.title || "").trim().slice(0, 1) || "#").toUpperCase()
                key = (c >= "A" && c <= "Z") ? c : "#"
            }
            if (!buckets[key]) { buckets[key] = []; order.push(key) }
            buckets[key].push(r)
        }
        order.sort()
        var out = []
        for (i = 0; i < order.length; i++) {
            out.push({ "letter": order[i], "items": buckets[order[i]] })
        }
        return out
    }

    // Once F1 owns the Albums tab, never run the legacy O(n) JS pipeline in
    // parallel. The old document may still be resident because Artists uses
    // it until F2; it is evaluated only while that legacy surface is active.
    readonly property var albumsVisible: root.albumsNativeViewActive ? []
        : applyFilter(sortRows(filterRows(albums, albumsSearch), albumsSort), albumsFilter)
    readonly property var albumsGrouped: groupRows(albumsVisible, albumsGroup)

    readonly property var foldersVisible:
        sortRows(filterRows(folders, foldersSearch), foldersSort)
    readonly property var foldersGrouped: groupRows(foldersVisible, foldersGroup)

    // Legacy and native readers both publish rows in the final global group
    // order. Keeping this as a direct alias makes every 500-row legacy page an
    // immutable append; re-sorting the accumulated JSON here moved the current
    // track hundreds of indices whenever a later page belonged ahead of it.
    readonly property var tracksVisible: tracks

    function artistMatchesFilter(artist, selected) {
        if (!selected || Object.keys(selected).length === 0) return true
        var qAny = selected.dsd || selected.hires || selected.cd || selected.lossy
        var fAny = selected.flac || selected.alac || selected.ape || selected.wav
            || selected.mp3 || selected.aac || selected.other
        var sAny = selected.local || selected.offline || selected.plex
            || selected.jellyfin || selected.subsonic
        var known = { "flac": 1, "alac": 1, "ape": 1, "wav": 1,
                      "mp3": 1, "aac": 1 }
        var i, value
        if (qAny) {
            var qualityOk = false
            for (i = 0; i < (artist.qualityTiers || []).length; i++) {
                value = (artist.qualityTiers[i] || "").toLowerCase()
                if ((selected.dsd && value === "dsd")
                    || (selected.hires && (value === "hires" || value === "max"))
                    || (selected.cd && value === "cd")
                    || (selected.lossy && (value === "mp3" || value === "lossy"))) {
                    qualityOk = true; break
                }
            }
            if (!qualityOk) return false
        }
        if (fAny) {
            var formatOk = false
            for (i = 0; i < (artist.formats || []).length; i++) {
                value = (artist.formats[i] || "").toLowerCase()
                if (selected[value] === true
                    || (selected.other === true && known[value] !== 1)) {
                    formatOk = true; break
                }
            }
            if (!formatOk) return false
        }
        if (sAny) {
            var sourceOk = false
            for (i = 0; i < (artist.sources || []).length; i++) {
                if (selected[sourceBucket(artist.sources[i])] === true) {
                    sourceOk = true; break
                }
            }
            if (!sourceOk) return false
        }
        return true
    }

    readonly property var artistsVisible: {
        if (QbzLocal.localArtistsNativeActive && activeTab === "artists") return []
        var q = artistsSearch.trim().toLowerCase()
        var out = []
        for (var i = 0; i < artists.length; i++) {
            var artist = artists[i]
            if (q !== "" && (artist.name || "").toLowerCase().indexOf(q) < 0)
                continue
            if (!artistMatchesFilter(artist, artistsFilter)) continue
            out.push(artist)
        }
        out.sort(function(a, b) {
            var key = root.artistsSort
            var year = key.indexOf("year-") === 0
            var av = year ? Number(a.year || 0) : (a.name || "").toLowerCase()
            var bv = year ? Number(b.year || 0) : (b.name || "").toLowerCase()
            if (year && av === 0) return 1
            if (year && bv === 0) return -1
            var cmp = av < bv ? -1 : av > bv ? 1 : 0
            return key.slice(-4) === "desc" ? -cmp : cmp
        })
        return out
    }
    readonly property int artistsVisibleCount: artistsVisible.length
    readonly property var artistsGrouped: {
        if (QbzLocal.localArtistsNativeActive && activeTab === "artists") return []
        var rows = artistsVisible
        var buckets = {}
        var order = []
        for (var i = 0; i < rows.length; i++) {
            var c = ((rows[i].name || "").trim().slice(0, 1) || "#").toUpperCase()
            var key = (c >= "A" && c <= "Z") ? c : "#"
            if (!buckets[key]) { buckets[key] = []; order.push(key) }
            buckets[key].push(rows[i])
        }
        order.sort()
        var out = []
        for (i = 0; i < order.length; i++) {
            out.push({ "letter": order[i], "items": buckets[order[i]] })
        }
        return out
    }

    // --------------------- Genres column browser -------------------------
    // Empty selection maps are the explicit All state. Multiple selections
    // are OR-ed within one column and AND-ed across the chained columns.
    readonly property var genreBaseAlbums: applyFilter(albums, genresFilter)

    function albumArtistNames(album) {
        if (album.artists && album.artists.length > 0)
            return album.artists
        var values = []
        var seen = {}
        var raw = (album.allArtists || "").trim()
        var parts = raw === "" ? [album.artist || ""] : raw.split(",")
        if ((album.artist || "").trim() !== "") parts.unshift(album.artist)
        for (var i = 0; i < parts.length; i++) {
            var display = (parts[i] || "").trim()
            var key = display.toLowerCase()
            if (display !== "" && !seen[key]) { seen[key] = true; values.push(display) }
        }
        return values
    }

    function albumHasSelectedGenre(album) {
        if (explorerColumns === "year") return true
        if (selectedGenreCount === 0) return true
        var genres = album.genres || []
        for (var i = 0; i < genres.length; i++)
            if (selectedGenres[(genres[i] || "").toLowerCase()] === true) return true
        return false
    }

    function albumHasSelectedYear(album) {
        if (explorerColumns === "genre") return true
        if (selectedGenreYearCount === 0) return true
        var years = album.years && album.years.length > 0
            ? album.years : [album.year || ""]
        for (var i = 0; i < years.length; i++)
            if (selectedGenreYears[String(years[i])] === true) return true
        return false
    }

    function albumHasSelectedArtist(album) {
        if (selectedGenreArtistCount === 0) return true
        var names = albumArtistNames(album)
        for (var i = 0; i < names.length; i++)
            if (selectedGenreArtists[names[i].toLowerCase()] === true) return true
        return false
    }

    readonly property var genreNames: {
        var values = {}, display = {}
        for (var i = 0; i < genreBaseAlbums.length; i++) {
            var genres = genreBaseAlbums[i].genres || []
            for (var j = 0; j < genres.length; j++) {
                var name = (genres[j] || "").trim(), key = name.toLowerCase()
                if (name !== "" && !values[key]) { values[key] = true; display[key] = name }
            }
        }
        var out = []
        var q = genresSearch.trim().toLowerCase()
        for (var key in values)
            if (q === "" || display[key].toLowerCase().indexOf(q) >= 0)
                out.push({ "key": key, "label": display[key] })
        out.sort(function(a, b) { return a.label.localeCompare(b.label) })
        return out
    }

    readonly property var genreYearOptions: {
        var values = ({})
        for (var i = 0; i < genreBaseAlbums.length; i++) {
            var album = genreBaseAlbums[i]
            if (!albumHasSelectedGenre(album)) continue
            var years = album.years && album.years.length > 0
                ? album.years : [album.year || ""]
            for (var j = 0; j < years.length; j++) {
                var key = String(years[j] || "").trim()
                if (key !== "") values[key] = true
            }
        }
        var out = [], q = genreYearsSearch.trim().toLowerCase()
        for (var key in values)
            if (q === "" || key.toLowerCase().indexOf(q) >= 0)
                out.push({ "key": key, "label": key })
        out.sort(function(a, b) { return Number(b.key) - Number(a.key) })
        return out
    }

    readonly property var genreArtistOptions: {
        var values = {}, display = {}
        for (var i = 0; i < genreBaseAlbums.length; i++) {
            var album = genreBaseAlbums[i]
            if (!albumHasSelectedGenre(album) || !albumHasSelectedYear(album)) continue
            var names = albumArtistNames(album)
            for (var j = 0; j < names.length; j++) {
                var key = names[j].toLowerCase()
                if (!values[key]) { values[key] = true; display[key] = names[j] }
            }
        }
        var out = [], q = genreArtistsSearch.trim().toLowerCase()
        for (var key in values)
            if (q === "" || display[key].toLowerCase().indexOf(q) >= 0)
                out.push({ "key": key, "label": display[key] })
        out.sort(function(a, b) { return a.label.localeCompare(b.label) })
        return out
    }

    readonly property var genreAlbumOptions: {
        var out = [], q = genreAlbumsSearch.trim().toLowerCase()
        for (var i = 0; i < genreBaseAlbums.length; i++) {
            var album = genreBaseAlbums[i]
            if (!albumHasSelectedGenre(album) || !albumHasSelectedYear(album)
                    || !albumHasSelectedArtist(album)) continue
            if (q !== "" && (album.title || "").toLowerCase().indexOf(q) < 0) continue
            out.push({ "key": album.id, "label": album.title || "", "album": album })
        }
        out.sort(function(a, b) { return a.label.localeCompare(b.label) })
        return out
    }

    readonly property var genreAlbumsVisible: {
        var rows = []
        for (var i = 0; i < genreAlbumOptions.length; i++) {
            var option = genreAlbumOptions[i]
            if (selectedGenreAlbumCount === 0 || selectedGenreAlbums[option.key] === true)
                rows.push(option.album)
        }
        return sortRows(rows, genresSort)
    }

    // The Explorer facets follow the spreadsheet rule — a plain click picks
    // one value, Ctrl adds or removes one, Shift picks the run from the last
    // click, Ctrl+Shift adds that run (SelectionModel.nextExclusive) — each
    // column over its values AS LISTED (sorted, and filtered by its own
    // search box). One anchor per column; a column whose values are rebuilt
    // by a change upstream starts without one.
    SelectionModel { id: genreFacetSel; idKey: "key" }
    SelectionModel { id: yearFacetSel; idKey: "key" }
    SelectionModel { id: artistFacetSel; idKey: "key" }
    SelectionModel { id: albumFacetSel; idKey: "key" }
    function toggleGenre(key, modifiers) {
        selectedGenres = nextFacetSelection(genreFacetSel, selectedGenres, key, modifiers, genreNames)
        selectedGenreYears = ({})
        selectedGenreArtists = ({})
        selectedGenreAlbums = ({})
        yearFacetSel.anchorId = ""
        artistFacetSel.anchorId = ""
        albumFacetSel.anchorId = ""
    }
    function toggleGenreYear(key, modifiers) {
        selectedGenreYears = nextFacetSelection(yearFacetSel, selectedGenreYears, key, modifiers,
                                                genreYearOptions)
        selectedGenreArtists = ({})
        selectedGenreAlbums = ({})
        artistFacetSel.anchorId = ""
        albumFacetSel.anchorId = ""
    }
    function toggleGenreArtist(key, modifiers) {
        selectedGenreArtists = nextFacetSelection(artistFacetSel, selectedGenreArtists, key,
                                                  modifiers, genreArtistOptions)
        selectedGenreAlbums = ({})
        albumFacetSel.anchorId = ""
    }
    function toggleGenreAlbum(key, modifiers) {
        selectedGenreAlbums = nextFacetSelection(albumFacetSel, selectedGenreAlbums, key, modifiers,
                                                 genreAlbumOptions)
    }
    function setExplorerColumns(mode) {
        if (["genre", "year", "both"].indexOf(mode) < 0 || mode === explorerColumns)
            return
        explorerColumns = mode
        QbzLocal.setExplorerColumns(mode)
        // A hidden facet must never keep filtering the visible chain.
        if (mode === "genre") selectedGenreYears = ({})
        else if (mode === "year") selectedGenres = ({})
        selectedGenreArtists = ({})
        selectedGenreAlbums = ({})
    }
    readonly property int explorerFacetCount: explorerColumns === "both" ? 4 : 3
    readonly property int explorerLeadingCount: explorerColumns === "year"
        ? genreYearOptions.length : genreNames.length
    /// `key` "" is the column's "All" row: it clears the column.
    function nextFacetSelection(rule, current, key, modifiers, options) {
        if (key === "") {
            rule.anchorId = ""
            return ({})
        }
        return rule.nextExclusive(current, key, options || [],
                                  modifiers === undefined ? Qt.NoModifier : modifiers)
    }

    // The selected artist's albums.
    //
    // The MATCH lives in Rust — `QbzLocal.artistAlbumIds` ->
    // local_artist_match::album_matches_artist, the port of
    // local_library.rs:3368-3390 — because it is the same normalized-name rule
    // the Artists merge uses and there must be exactly one of it. It compares
    // NORMALIZED PARTS for exact equality against the primary artist, every
    // `allArtists` comma part and every split-credit part (`,&/;`, feat/ft/
    // featuring/with), plus the "various artists" special case.
    //
    // What it replaced: a lowercase equality on `artist` OR a SUBSTRING
    // `indexOf` on `allArtists`. That listed "Airbourne" and "Blair" under
    // "Air", and an album credited "A & B" never appeared under "B"
    // (PARITY-DEBT #8).
    //
    // The ids come back rather than the rows, so the pane renders the very
    // objects this view already parsed, in the published order.
    readonly property var artistAlbums: {
        if (selectedArtist === "") return []
        if (QbzLocal.localArtistsNativeActive) return []
        // The rail filters artist aggregates; the detail must also filter
        // physical album copies, including when the selected artist stays.
        var rows = applyFilter(albums, artistsFilter)
        var ids = {}
        try {
            var arr = JSON.parse(QbzLocal.artistAlbumIds(selectedArtist))
            for (var j = 0; j < arr.length; j++) ids[arr[j]] = true
        } catch (e) {
            console.warn("[qbz-qt] local: bad artist album ids — " + e)
            return []
        }
        var out = []
        for (var i = 0; i < rows.length; i++) {
            if (ids[rows[i].id]) out.push(rows[i])
        }
        return out
    }

    readonly property var detailSubfolders: {
        if (!folderDetail) return []
        var q = folderDetailSearch.trim().toLowerCase()
        if (q === "") return folderDetail.subfolders
        var out = []
        for (var i = 0; i < folderDetail.subfolders.length; i++) {
            if ((folderDetail.subfolders[i].name || "").toLowerCase().indexOf(q) >= 0)
                out.push(folderDetail.subfolders[i])
        }
        return out
    }

    // --------------------- windowed artwork ------------------------------
    // Report the mounted window as artKeys and prune covers a full window
    // away. Same policy as LibraryView (the Slint eviction, QML-side), with
    // one correction the port needed: eviction is derived from EVERY live
    // surface at once, not from the one array the current call carries.
    //
    // WHY A REGISTRY AND NOT A SINGLE WINDOW. This view mounts TWO cover
    // surfaces side by side more often than not: the Artists tab is the
    // avatar rail plus that artist's album grid, and Folders tree mode is the
    // subfolder cards plus the folder's track rows. The old per-call eviction
    // built `keep` from its own rows, so each surface deleted the other's
    // covers on every report and both settled on grey squares. `_windows`
    // holds the last window of every live surface keyed by a stable surface
    // id, and the keep-set is their union.
    property var _windows: ({})

    /// Record (or drop) one surface's window. An empty or degenerate window
    /// RELEASES the slot rather than pinning covers the surface no longer
    /// shows.
    function applyWindow(key, rows, first, last) {
        if (!rows || rows.length === 0) { delete root._windows[key]; return }
        last = Math.min(last, rows.length - 1)
        first = Math.max(0, first)
        if (first > last) { delete root._windows[key]; return }
        root._windows[key] = { "rows": rows, "first": first, "last": last }
    }

    /// A surface that unmounts, or scrolls/tabs off screen, stops holding its
    /// covers. Cheap no-op when the surface was never registered.
    function releaseWindow(key) {
        var k = key || "default"
        if (root.artTimingEnabled && root._artTraceWindows[k]) {
            var traced = root._artTraceWindows[k]
            for (var i = 0; i < traced.keys.length; i++)
                QbzLocal.artworkTiming("window-end", traced.keys[i], traced.id)
            delete root._artTraceWindows[k]
        }
        if (root._windows[k] === undefined && root._pending[k] === undefined) return
        delete root._windows[k]
        delete root._pending[k]
        flushWindows()
    }

    /// Evict against the union of every live window, then request what is
    /// still missing. Keys already resolved are NOT re-sent: `artMap` IS the
    /// resolved set and a re-request costs Rust a `stat` per key
    /// (local_artwork.rs phase 1), which a scroll would pay per pass.
    function flushWindows() {
        var keep = {}
        var k, w, rows, i, ak, span, lo, hi
        for (k in root._windows) {
            w = root._windows[k]
            rows = w.rows
            span = w.last - w.first + 1
            lo = Math.max(0, w.first - span)
            hi = Math.min(rows.length - 1, w.last + span)
            for (i = lo; i <= hi; i++) {
                ak = rows[i] ? rows[i].artKey : ""
                if (ak) keep[ak] = true
            }
        }
        var m = root.artMap
        root._artKeep = keep
        var changed = false
        for (k in m) if (!keep[k]) { delete m[k]; changed = true }
        // The not-yet-flushed arrivals are evicted too, or a cover that landed
        // for a row we have just scrolled away from would be re-added by the
        // next flush and defeat the eviction.
        for (k in root._artInbox) if (!keep[k]) delete root._artInbox[k]
        for (k in root._artHeld) if (!keep[k]) delete root._artHeld[k]
        if (changed) root.artMap = Object.assign({}, m)

        // Ordinals for the ordered reveal, assigned over the window in DISPLAY
        // order so the front walks top to bottom. Built for every key in the
        // window, not just the missing ones, so a cover that resolves while
        // the wipe is mid-flight still knows where it sits.
        var order = {}
        var ord = 0
        var keys = []
        var seen = {}
        for (k in root._windows) {
            w = root._windows[k]
            rows = w.rows
            for (i = w.first; i <= w.last; i++) {
                ak = rows[i] ? rows[i].artKey : ""
                if (!ak || seen[ak]) continue
                seen[ak] = true
                if (!root.immediateArtwork && order[ak] === undefined) order[ak] = ord++
                if (m[ak] !== undefined || root._artInbox[ak] !== undefined) continue
                keys.push(ak)
            }
        }
        // Restart the wipe at the top of what is on screen NOW.
        root._artOrder = order
        root._artFront = 0
        if (keys.length > 0) {
            if (root.artTimingEnabled)
                for (var n = 0; n < keys.length; n++) root.traceArt("request", keys[n])
            QbzLocal.artworkWindow(JSON.stringify(keys))
        }
    }

    /// Immediate single-surface report — the leading edge and the rate-limit
    /// flush both land here.
    function reportWindow(rows, first, last, key) {
        applyWindow(key || "default", rows, first, last)
        flushWindows()
    }

    // ------------------------- skeleton pulse ----------------------------
    // ONE 900ms Timer drives EVERY placeholder in this view AND in every
    // tab body under views/local/ (they read `view.skelPhase`) — N
    // placeholders, 1 timer, which is QbzSkeleton's preferred drive mode.
    // GATING RULE: freeze on NOT VISIBLE — the view hidden, or the window
    // minimized/hidden. NEVER on lost focus (a tiling desktop keeps windows
    // visible and unfocused).
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    readonly property bool anyTabLoading: QbzLocal.localAlbumsLoading
        || QbzLocal.localArtistsLoading || QbzLocal.localFoldersLoading
        || QbzLocal.localTreeLoading || QbzLocal.localTracksLoading
        || QbzLocal.localTracksLoadingMore || QbzLocal.localDetailLoading
        || QbzLocal.localEphemeralLoading

    // THE CLEANUP RULE FOR LOCAL ARTWORK. src/local_artwork.rs
    // (`resolve_window_blocking`) DROPS keys with no cover — "the QML map
    // only ever grows with real hits" — so a local album with no embedded
    // art NEVER produces an artMap entry. A per-item placeholder gated only
    // on "artMap has no path yet" would therefore shimmer forever on every
    // artless album. `artSettleMs` is the bound: each placeholder fades out
    // by itself, revealing the card's own empty tile. Per-item handover
    // (QbzSkeleton.handedOver) still wins whenever the cover DOES arrive.
    //
    // The countdown does NOT start at mount — it starts when the artwork pass
    // goes quiet (`artPulse` below). A FIRST visit to a cold library decodes
    // every cover from scratch and takes seconds; a countdown from mount used
    // to expire a beat BEFORE the covers landed, which is the other half of
    // "the skeleton disappears before the art is rendered".
    readonly property int artSettleMs: 1200
    /// How long after the last sign of life a resolution pass is still
    /// presumed in flight. Re-armed by every window report AND by every cover
    /// that arrives, so a slow first visit extends it by itself instead of
    /// needing a pessimistic constant.
    readonly property int artPassMs: 2500
    // The pulse runs while an artwork window can still resolve, and is also
    // what holds off every placeholder's settle countdown.
    property bool artPulse: false
    Timer {
        id: artPulseOff
        interval: root.artPassMs
        onTriggered: root.artPulse = false
    }
    Timer {
        interval: 900
        repeat: true
        running: (root.anyTabLoading || root.artPulse)
            && root.visible && root.windowShowing
        onTriggered: root.skelPhase = !root.skelPhase
    }

    /// Does THIS row want a cover at all? An empty artKey means the row has
    /// no artwork slot and there is nothing to wait for.
    ///
    /// NOTE — this REPLACED `artPending(key)`, which asked "is the path still
    /// missing". That predicate is not the placeholder gate and never was:
    /// it goes false when the PATH lands, while the art needs a further load
    /// + canvas raster before it is on screen. The placeholder now hands over
    /// on QbzSkeleton's `coverSource`/`coverReady` (see QbzSkeleton.qml), and
    /// this function only answers "is a cover expected here".
    function artWanted(key) { return (key || "") !== "" }
    /// The decoded cover for a row, "" until it lands.
    function artPathOf(key) { return root.artMap[key] || "" }

    // Reporting is rate-limited to one pass per 180ms, but LEADING-EDGE: the
    // limiter exists to stop a scroll from firing a resolution pass per pixel,
    // and a view that has just mounted is not scrolling. Trailing-only cost
    // every first paint a flat 180ms before Rust was even asked for a cover —
    // pure latency on the one report the user is actually waiting for.
    //
    // TWO THINGS THIS USED TO SWALLOW, both of which read as "grey squares
    // until I scroll":
    //   - ONE pending slot for the whole view. Two surfaces reporting inside
    //     the same 180ms (the artist rail and the artist's album grid do
    //     exactly that on every selection) meant the second overwrote the
    //     first and one pane was never requested at all. Pending reports are
    //     keyed by surface now and ALL of them flush.
    //   - `restart()` on every call. That is a reset-on-change debounce: a
    //     continuous flick (or a burst of mount-time reports — rows arrive,
    //     the column count settles, the tab becomes visible) pushed the
    //     trailing edge out by another 180ms each time, so the pass fired
    //     only once everything stopped moving. It is a rate limiter now:
    //     `start()`, never `restart()`.
    Timer {
        id: windowDebounce
        interval: 180
        onTriggered: {
            root._lastReportMs = Date.now()
            var pending = root._pending
            root._pending = ({})
            var any = false
            for (var k in pending) {
                var w = pending[k]
                root.applyWindow(k, w.rows, w.first, w.last)
                any = true
            }
            if (any) root.flushWindows()
        }
    }
    /// Surface id -> the window waiting for the next flush.
    property var _pending: ({})
    // `real` is a double in QML — Date.now() (~1.7e12) needs the range.
    property real _lastReportMs: 0
    function queueWindowReport(rows, first, last, key) {
        var k = key || "default"
        if (rows && root.artTimingEnabled) root.traceArtReport(rows, first, last, k)
        if (!windowDebounce.running
            && Date.now() - root._lastReportMs >= windowDebounce.interval) {
            root._lastReportMs = Date.now()
            root.reportWindow(rows, first, last, k)
        } else {
            root._pending[k] = { "rows": rows, "first": first, "last": last }
            if (!windowDebounce.running) windowDebounce.start()
        }
        // Covers can now land: keep the shared pulse (and every placeholder's
        // settle hold) alive until artPassMs after the LAST sign of life —
        // this report, or the next cover that arrives.
        root.artPulse = true
        artPulseOff.restart()
    }

    // ============================ actions ================================
    function openAlbum(id) {
        QbzLocal.openAlbumFiltered(id, JSON.stringify(root.filter || {}))
        QbzShell.navigateTo("localalbum")
    }
    function selectFolder(path) {
        selectedFolder = path
        folderDetailSearch = ""
        QbzLocal.selectFolder(path)
    }
    function selectArtist(name) { selectedArtist = name }

    function toggleAlbumsMultiSelect() {
        albumsMultiSelect = !albumsMultiSelect
        if (!albumsMultiSelect) {
            albumsSelected = ({}); albumSel.anchorId = ""
            if (QbzLocal.localAlbumsNativeActive) QbzLocal.albumsNativeClearSelection()
        }
    }
    /// Excel-style selection — controls/SelectionModel.qml holds the anchor
    /// and the Shift-range rule; this view keeps owning its maps. Ranges run
    /// over the VISIBLE rows, because the toolbar's filter is a view filter
    /// and `select-all` right below already means the filtered set.
    SelectionModel { id: albumSel }
    function toggleAlbumSelected(id, mods) {
        albumsSelected = albumSel.next(albumsSelected, id, albumsVisible,
                                       mods === undefined ? Qt.NoModifier : mods)
    }
    function toggleNativeAlbumSelected(index, mods) {
        QbzLocal.albumsNativeToggleSelect(
            index, (mods & Qt.ShiftModifier) !== 0)
    }
    function albumsBulkAction(action) {
        if (QbzLocal.localAlbumsNativeActive) {
            if (action === "clear") QbzLocal.albumsNativeClearSelection()
            else if (action === "select-all") QbzLocal.albumsNativeSelectAll()
            else QbzLocal.albumsNativeBulkAction(action)
            return
        }
        if (action === "clear") { albumsSelected = ({}); albumSel.anchorId = ""; return }
        if (action === "select-all") {
            var s = {}
            for (var i = 0; i < albumsVisible.length; i++) s[albumsVisible[i].id] = true
            albumsSelected = s
            return
        }
        QbzLocal.bulkAction("album", JSON.stringify(Object.keys(albumsSelected)), action)
    }

    function toggleTracksMultiSelect() {
        tracksMultiSelect = !tracksMultiSelect
        if (!tracksMultiSelect) {
            tracksSelected = ({}); trackSel.anchorId = ""
            if (QbzLocal.localTracksNativeActive) QbzLocal.tracksNativeClearSelection()
        }
    }
    SelectionModel { id: trackSel }
    function toggleTrackSelected(id, mods) {
        tracksSelected = trackSel.next(tracksSelected, id, tracksVisible,
                                       mods === undefined ? Qt.NoModifier : mods)
    }
    function tracksBulkAction(action) {
        if (QbzLocal.localTracksNativeActive) {
            if (action === "clear") QbzLocal.tracksNativeClearSelection()
            else if (action === "select-all") QbzLocal.tracksNativeSelectAll()
            else QbzLocal.tracksNativeBulkAction(action)
            return
        }
        if (action === "clear") { tracksSelected = ({}); trackSel.anchorId = ""; return }
        if (action === "select-all") {
            var s = {}
            for (var i = 0; i < tracksVisible.length; i++) s[tracksVisible[i].id] = true
            tracksSelected = s
            return
        }
        QbzLocal.bulkAction("track", JSON.stringify(Object.keys(tracksSelected)), action)
    }

    // --- Ctrl+A / Escape hotkeys interface (2026-08-03 hotkeys-port §4.6) --
    // The duck-typed seam the AppShell router calls. Only the albums and
    // tracks tabs have multi-select (the folder tree rail's select mode is
    // Rust-side with no select-all arm — not covered); on the artists /
    // folders tabs selectAll() is a deliberate no-op.
    readonly property bool multiSelectOn: root.albumsMultiSelect || root.tracksMultiSelect
    function selectAll() {
        if (root.activeTab === "albums") {
            if (!root.albumsMultiSelect) root.albumsMultiSelect = true
            root.albumsBulkAction("select-all")
        } else if (root.activeTab === "tracks") {
            if (!root.tracksMultiSelect) root.tracksMultiSelect = true
            root.tracksBulkAction("select-all")
        }
    }
    function exitMultiSelectMode() {
        // Same "leaving drops the selection" contract the toggle buttons
        // carry (toggleAlbumsMultiSelect / toggleTracksMultiSelect).
        if (root.albumsMultiSelect) {
            root.albumsMultiSelect = false
            root.albumsSelected = ({})
            if (QbzLocal.localAlbumsNativeActive) QbzLocal.albumsNativeClearSelection()
        }
        if (root.tracksMultiSelect) {
            root.tracksMultiSelect = false
            root.tracksSelected = ({})
            if (QbzLocal.localTracksNativeActive) QbzLocal.tracksNativeClearSelection()
        }
    }

    function toggleTreeSelectMode() {
        treeSelectMode = !treeSelectMode
        QbzLocal.treeSetSelectMode(treeSelectMode)
    }

    // ============================ view ===================================

    Column {
        anchors.fill: parent
        spacing: 0

        // ---- Fixed chrome: title row, tab row, divider (91px) -----------
        LocalChrome {
            width: parent.width
            view: root
        }

        // ===================== CONTENT =================================
        Item {
            id: contentArea
            width: parent.width
            height: parent.height - 91
            clip: true

            // The `Open` pane is showing. It is the ONE body that renders with
            // no indexed library (see the Loader below), so it is also the one
            // the empty state has to stand down for.
            readonly property bool ephemeralShowing:
                root.activeTab === "ephemeral" && root.ephemeralActive

            // "Nothing indexed yet" (no db / no registered folder).
            //
            // ALSO gated on the Open pane, and that second half is not
            // hypothetical: on a machine with no library at all — a fresh
            // install, or the owner's Mac mini — opening a CD drew the disc
            // AND this empty state through it, both fully painted, because
            // each was individually correct about its own condition. Reported
            // as a transparency bug; it was two views agreeing to be visible.
            QbzEmptyState {
                visible: !QbzLocal.localAvailable && !contentArea.ephemeralShowing
                anchors.centerIn: parent
                iconName: "folder-plus"
                title: QbzSession.tr("No local library yet", QbzSession.trRev)
                body: QbzSession.tr("Add a music folder to scan your local files.", QbzSession.trRev)
                actionLabel: QbzSession.tr("Open Local Library settings", QbzSession.trRev)
                onActionClicked: QbzShell.navigateTo("settings")
            }

            // ONE TAB EXISTS AT A TIME. These four were sibling mounts gated
            // on `visible:`, which hides an item but does not stop QML from
            // building it — so every entry into Local Library instantiated all
            // four tab bodies and their views' first screenful of delegates,
            // to show one. The same divergence measured in HomeView.qml (see
            // the Home tab there for the numbers and for why `asynchronous` is
            // NOT the answer); the Tracks tab is the one that makes it matter
            // here, being the 16K-row view.
            //
            // `active` also carries the tab's own precondition: with no local
            // library there is nothing to build, and the empty state above is
            // what shows instead.
            Loader {
                anchors.fill: parent
                active: QbzLocal.localAvailable && root.activeTab === "albums"
                sourceComponent: LocalAlbumsTab { view: root }
            }
            Loader {
                anchors.fill: parent
                active: QbzLocal.localAvailable && root.activeTab === "artists"
                sourceComponent: LocalArtistsTab { view: root }
            }
            Loader {
                anchors.fill: parent
                active: QbzLocal.localAvailable && root.activeTab === "genres"
                sourceComponent: LocalGenresTab { view: root }
            }
            Loader {
                anchors.fill: parent
                active: QbzLocal.localAvailable && root.activeTab === "folders"
                sourceComponent: LocalFoldersTab { view: root }
            }
            Loader {
                anchors.fill: parent
                active: QbzLocal.localAvailable && root.activeTab === "tracks"
                sourceComponent: LocalTracksTab { view: root }
            }
            // The `Open` tab, previously the ephemeral ARM of the Folders tab.
            //
            // NOT gated on `localAvailable`, unlike its four siblings: an
            // ephemeral session is content from OUTSIDE the indexed library,
            // so a user with an empty (or unscanned) library must still be
            // able to open a folder or a disc and play it. Gating it would
            // hide the pane behind the "no local library" empty state for
            // exactly the people the feature exists for.
            Loader {
                anchors.fill: parent
                active: contentArea.ephemeralShowing
                sourceComponent: LocalEphemeralPane { view: root }
            }
        }
    }

    // Albums filter popup — sibling of the content column so it FLOATS over
    // it instead of taking a layout slot (the Slint mount, :2470).
    LocalFilterPopup {
        anchors.fill: parent
        visible: root.filterOpen
            && ["albums", "artists", "genres", "tracks"].indexOf(root.activeTab) >= 0
        view: root
    }

}
