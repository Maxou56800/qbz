// Library view — QML port of crates/qbz-ui/ui/favorites/FavoritesView.slint
// + the Library "All" mixed feed (library_all.rs semantics).
//
// Data: QbzLibrary.libraryJson (ONE JSON document — the full merged feed;
// tabs/search/sort/source-filters derive HERE in JS, measured per the
// phase brief) + libraryCountsJson (tab badges). Artwork is id-keyed
// through the libraryArtworkReady signal into `artMap` (never a
// wrong-cover race); windows are reported as artKeys via
// libraryArtworkWindow, and artMap entries far outside the viewport are
// pruned (the Slint eviction policy, QML-side).
//
// THIS FILE IS THE STATE + ORCHESTRATION HALF. The bodies live next door
// (track rule 2 — it used to be 1,881 lines):
//   library/LibraryToolbar.qml      the two-row Library header
//   library/FeedListRow.qml         the All feed's mixed LIST row
//   library/LibraryAlbumsList.qml   Albums in LIST mode + its bulk bar
//   library/LibraryArtistsPanel.qml Artists in SIDEPANEL mode
// Each takes `view: root`, the seam views/local/*.qml already uses.
//
// Remaining delta: while offline, the generic placeholder mounts instead of
// Slint's rail of playable cached favourites. The offline cache itself and
// the Library "All" local scope (favourites vs the whole local library) are
// both live; this is a missing Library presentation, not a missing engine.
//
// Filter by genre IS wired, on the All / Tracks / Albums toolbars — the three
// places the Slint FavoritesView draws it (FavoritesView.slint:609, :652,
// :864). Each tab now has an independent context. Filtering runs over the feed,
// exactly like library_all.rs::derive: an item shows when its (lowercased)
// genre contains one of the selected genre names (+ their sub-genres), so rows
// with no genre at all (artist / label / playlist) drop out while a genre is
// selected. The selection lives in the shared per-context store and persists
// to genre_filter.json.
//
// Toolbar choices PERSIST (favorites_prefs.rs parity): albums view/sort/group,
// tracks group, playlists view, artists group/view and the All hard-drive
// toggle all go through `setPref` into the same favorites_ui.json the shipping
// Slint build writes. Search queries stay transient — deliberately, there too.

import "../assets/release-sort.js" as ReleaseSort
import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"
import "library"

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

    // ============================ state ==================================
    // Everything that logs through this category MUST defer the call (see the
    // `Qt.callLater` in `visibleRows` and `parseFeed`). Declaring it first is
    // NOT enough and moving it here did not silence the warning: a category is
    // finalised the first time it is USED, so a `console.info` running during
    // component creation pins it before Qt applies `defaultLogLevel`, which
    // then warns ("cannot be changed after the component is completed") and —
    // the part that matters — leaves the level UNSET, i.e. this logging on by
    // default for every user. Deferring past creation is what actually fixes
    // it. Both call sites below run during creation, which is why both defer.
    LoggingCategory {
        id: libTiming
        name: "qbz.nav.timing"
        defaultLogLevel: LoggingCategory.Warning
    }

    property string activeTab: "all"
    property string pendingReleaseRemovalId: ""

    readonly property var settingsDoc: {
        try { return JSON.parse(QbzBridge.settingsJson) }
        catch (e) { return ({}) }
    }
    // Independent opt-in from Queue and Local Library. TrackRow already owns
    // the required number → artwork → title geometry, so this only arms it.
    readonly property bool libraryTrackArtwork:
        settingsDoc.libraryTrackArtwork === true
    onLibraryTrackArtworkChanged: Qt.callLater(root.listWindowReport)

    readonly property var counts: JSON.parse(QbzLibrary.libraryCountsJson)
    // Full merged feed (parsed once per publish — timed for the report).
    readonly property var feed: parseFeed(QbzLibrary.libraryJson)
    // The local catalog includes disk, download cache and self-hosted servers.
    function isLocalFeedItem(item) {
        return item.group === "local" || ["local", "plex", "jellyfin", "subsonic",
            "navidrome", "gonic", "airsonic", "astiga"].indexOf(item.source) >= 0
    }

    function hideableLocalAlbum(item) {
        // Matched Qobuz/download entries stay in the library when local is hidden.
        var sources = item.sources || []
        var isDownload = item.source === "offline" || item.source === "qobuz_download"
            || item.source === "qobuz_purchase" || sources.indexOf("offline") >= 0
            || sources.indexOf("qobuz_purchase") >= 0
        return item.kind === "album" && item.source !== "qobuz"
            && isLocalFeedItem(item) && !isDownload
    }

    readonly property bool showSourceBadges: visibleRows.some(function (item) {
        return isLocalFeedItem(item) || (item.sources || []).some(function (source) {
            return source !== "qobuz"
        })
    })

    function parseFeed(json) {
        var t = Date.now()
        var f = JSON.parse(json)
        var totals = { "tracks": 0, "albums": 0, "artists": 0,
                       "playlists": 0, "labels": 0 }
        // Tracks / Albums now expose both favourite and purchased sources.
        // A purchased entity can also be favourited and therefore appear in
        // the merged feed twice; count the entity, not its source rows.
        var tabSeen = ({})
        var tabPurchased = ({})
        var tabFavorites = ({})
        var tabFollowing = ({})
        // Normalize the strings once per Rust publish. Library > All can hold
        // the complete local library, so repeating toLowerCase() in every
        // search pass and every sort comparison turns one key press into a
        // large allocation storm. These private fields stay on the parsed JS
        // rows; they do not enlarge the Rust JSON transport.
        for (var i = 0; i < f.length; i++) {
            var it = f[i]
            it._feedOrder = i
            // Local database IDs can coincide with Qobuz IDs.
            var membershipKey = (isLocalFeedItem(it)
                ? it.source + ":" : "") + it.kind + ":" + it.id
            it._membershipKey = membershipKey
            if (it.group === "purchases") tabPurchased[membershipKey] = true
            if (it.kind === "playlist" ? it.isFavorite === true
                : it.group === "favorites" || (isLocalFeedItem(it) && it.isFavorite === true))
                tabFavorites[membershipKey] = true
            if (it.group === "following") tabFollowing[membershipKey] = true
            it._titleLc = String(it.title || "").toLowerCase()
            it._artistLc = String(it.artist || "").toLowerCase()
            it._albumLc = String(it.album || "").toLowerCase()
            it._genreLc = String(it.genre || "").toLowerCase()
            if (it.kind === "track" || it.kind === "album") {
                if (it.group === "favorites" || it.group === "purchases"
                    || (it.kind === "album" && isLocalFeedItem(it))) {
                    var tabKey = membershipKey
                    if (it.group === "purchases") tabPurchased[tabKey] = true
                    else if (it.group === "favorites" || it.isFavorite === true)
                        tabFavorites[tabKey] = true
                    if (!tabSeen[tabKey]) {
                        tabSeen[tabKey] = true
                        totals[it.kind + "s"]++
                    }
                }
            } else if (it.kind === "artist") totals.artists++
            else if (it.kind === "playlist" && !tabSeen[membershipKey]) {
                totals.playlists++
                tabSeen[membershipKey] = true
            }
            else if (it.kind === "label") totals.labels++
        }
        // The toolbar emptiness gate used to make a second full pass over the
        // same feed. Carry the result on the Array itself (outside `length`),
        // so all consumers keep the exact same row array and model identity.
        f._tabTotals = totals
        f._tabPurchased = tabPurchased
        f._tabFavorites = tabFavorites
        f._tabFollowing = tabFollowing
        // Was a bare console.log, i.e. printed on every publish for every user
        // forever. It belongs to the same investigation as the derive timing
        // right below it, so it now rides the same category and is silent
        // unless that category is turned on.
        var line = "[libtiming] JSON.parse feed: " + (Date.now() - t)
                 + "ms (" + f.length + " items, " + json.length + " bytes)"
        Qt.callLater(function () { console.info(libTiming, line) })
        return f
    }

    // Decoded-cover map {artKey: file://path}, fed by the signal.
    property var artMap: ({})
    // Artwork completions commonly arrive in a burst. Rebinding artMap for
    // every file made every mounted card re-evaluate once per neighbour — an
    // O(window²) cold paint on Library > All. Flush one merged map per frame,
    // and ignore global library-art signals that are not in this body's keep
    // band (the sidepanel has its own URL-keyed inbox).
    property var _artInbox: ({})
    property var _artKeep: ({})
    property var _artRequested: ({})
    Timer {
        id: artFlush
        interval: 16
        repeat: false
        onTriggered: {
            if (Object.keys(root._artInbox).length === 0)
                return
            root.artMap = Object.assign({}, root.artMap, root._artInbox)
            root._artInbox = ({})
        }
    }

    // --- skeleton pulse ---------------------------------------------------
    // ONE 900ms Timer drives EVERY placeholder in this view (the loading
    // grid/list AND the per-item cover tiles). GATING RULE: freeze on NOT
    // VISIBLE — the view hidden, or the window minimized/hidden. NEVER on
    // lost focus (a tiling desktop keeps windows visible and unfocused).
    property bool skelPhase: false
    // Covers requested for the current window that have not arrived yet, and
    // the window's first index (the per-item placeholders count their cap
    // from it, so the cap follows the viewport instead of the model).
    property int artPending: 0
    property int artWindowFirst: 0
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    Timer {
        interval: 900
        repeat: true
        // Ticks only while something can actually be shimmering.
        running: root.visible && root.windowShowing
            && (QbzLibrary.libraryLoading || root.artPending > 0)
        onTriggered: root.skelPhase = !root.skelPhase
    }

    // All-tab derive state (LibraryAllState semantics).
    property string search: ""
    // Group modes (favorites.rs derive_tracks / derive_albums / artists).
    // Grouping REORDERS rows; for the tracks list it also injects separator
    // rows — see visibleItems / withGroupHeaders.
    property string tracksGroup: "off"   // "off" | "album" | "artist" | "name"
    property string albumsGroup: "off"   // "off" | "alpha" | "artist" | "year" | "decade"
    property string artistsGroup: "off"  // "off" | "alpha"
    property string sortBy: "date"      // "date" | "title" | "artist"
    property bool sortAsc: false        // date: false = newest first
    // Each tab retains its own source choices across navigation.
    readonly property bool showPurchases: activeTab === "albums"
        ? QbzLibrary.sessionAlbumsShowPurchases : activeTab === "tracks"
        ? QbzLibrary.sessionTracksShowPurchases : QbzLibrary.sessionShowPurchases
    readonly property bool showFavorites: activeTab === "albums"
        ? QbzLibrary.sessionAlbumsShowFavorites : activeTab === "tracks"
        ? QbzLibrary.sessionTracksShowFavorites : QbzLibrary.sessionShowFavorites
    readonly property bool showFollowing: QbzLibrary.sessionShowFollowing
    readonly property bool showLocal: !QbzLibrary.sessionHideLocalAlbums
    readonly property bool hiresOnly: activeTab === "albums"
        ? QbzLibrary.sessionAlbumsHiresOnly : activeTab === "tracks"
        ? QbzLibrary.sessionTracksHiresOnly : false
    function setHiresOnly(on) {
        if (activeTab === "albums") {
            QbzLibrary.sessionAlbumsHiresOnly = on
            QbzLibrary.setLibraryPref("albumsHiresOnly", String(on))
        } else if (activeTab === "tracks") {
            QbzLibrary.sessionTracksHiresOnly = on
            QbzLibrary.setLibraryPref("tracksHiresOnly", String(on))
        }
    }
    // Only bumped when a live heart change can alter membership in the
    // current Tracks / Albums source filter. Kept separate from `feed` so the
    // normal card-heart path still avoids rebuilding a large scrolled model.
    property int favoriteFilterRev: 0
    property string viewMode: "grid"    // All tab: "grid" | "list"
    property string albumsView: "grid"    // "grid" | "list"
    property string playlistsView: "grid" // "grid" | "list"
    property string artistsView: "grid"   // "grid" | "sidepanel"

    function setShowPurchases(on) {
        if (activeTab === "albums") {
            QbzLibrary.sessionAlbumsShowPurchases = on
            QbzLibrary.setLibraryPref("albumsShowPurchases", String(on))
        } else if (activeTab === "tracks") {
            QbzLibrary.sessionTracksShowPurchases = on
            QbzLibrary.setLibraryPref("tracksShowPurchases", String(on))
        } else {
            QbzLibrary.sessionShowPurchases = on
            QbzLibrary.setLibraryPref("allShowPurchases", String(on))
        }
    }
    function setShowFavorites(on) {
        if (activeTab === "albums") {
            QbzLibrary.sessionAlbumsShowFavorites = on
            QbzLibrary.setLibraryPref("albumsShowFavorites", String(on))
        } else if (activeTab === "tracks") {
            QbzLibrary.sessionTracksShowFavorites = on
            QbzLibrary.setLibraryPref("tracksShowFavorites", String(on))
        } else {
            QbzLibrary.sessionShowFavorites = on
            QbzLibrary.setLibraryPref("allShowFavorites", String(on))
        }
    }
    function setShowFollowing(on) {
        QbzLibrary.sessionShowFollowing = on
        QbzLibrary.setLibraryPref("allShowFollowing", String(on))
    }

    // --- persisted toolbar choices (library_prefs.rs) --------------------
    // Seeded from the bridge document rather than BOUND to it: these are
    // user-editable properties, and a binding would fight every click. The
    // Rust setter never republishes the document, so there is no loop.
    function applyPrefs() {
        var p
        try {
            p = JSON.parse(QbzLibrary.libraryPrefsJson)
        } catch (e) {
            return
        }
        // The default "{}" reaches here before `boot` seeds the real one.
        if (!p || p.albumsView === undefined) return
        root.albumsView = p.albumsView
        root.albumsSort = p.albumsSort
        root.labelsSort = p.labelsSort || "default"
        root.artistsSort = p.artistsSort || "default"
        root.playlistsSort = p.playlistsSort || "default"
        root.tracksSort = p.tracksSort || "default"
        root.albumsGroup = p.albumsGroup
        root.tracksGroup = p.tracksGroup
        root.playlistsView = p.playlistsView
        root.artistsGroup = p.artistsGroup
        root.artistsView = p.artistsView
    }
    Component.onCompleted: root.applyPrefs()
    Connections {
        target: QbzLibrary
        function onLibraryPrefsJsonChanged() { root.applyPrefs() }
    }

    /// Write one toolbar choice AND persist it. The toolbar calls this instead
    /// of assigning the property, so the two can never drift apart.
    function setPref(key, value) {
        if (key === "albumsView") {
            root.albumsView = value
            // Multi-select is LIST-only (FavoritesView.slint:514); leaving
            // list mode with a live selection would strand the bulk bar.
            if (value !== "list") root.setAlbumsMultiSelect(false)
        } else if (key === "albumsSort") root.albumsSort = value
        else if (key === "tracksSort") root.tracksSort = value
        else if (key === "playlistsSort") root.playlistsSort = value
        else if (key === "artistsSort") root.artistsSort = value
        else if (key === "labelsSort") root.labelsSort = value
        else if (key === "albumsGroup") root.albumsGroup = value
        else if (key === "tracksGroup") root.tracksGroup = value
        else if (key === "playlistsView") root.playlistsView = value
        else if (key === "artistsGroup") root.artistsGroup = value
        else if (key === "artistsView") root.artistsView = value
        else {
            console.warn("[qbz-qt] LibraryView.setPref: unknown key " + key)
            return
        }
        QbzLibrary.setLibraryPref(key, String(value))
    }
    /// Match the other session filters: false means no restriction.
    function setShowLocal(on) {
        QbzLibrary.sessionHideLocalAlbums = !on
        QbzLibrary.setLibraryPref("allHideLocal", String(!on))
    }

    // Genre selections are independent for each Library tab, too.
    readonly property string genreContext: activeTab === "albums" ? "library-albums"
        : activeTab === "tracks" ? "library-tracks" : "library-all"
    // Read STRAIGHT off the bridge singleton, not off libGenrePopup: the
    // popup is declared LAST (z-order) and a creation-time binding that
    // dereferences a not-yet-created id registers NO dependency, so it would
    // never re-evaluate. The popup instance is only touched from click
    // handlers, which run long after creation.
    readonly property var genreDoc: {
        try {
            return JSON.parse(QbzBridge.genreFilterJson)
        } catch (e) {
            return {}
        }
    }
    readonly property int genreCount: (genreDoc.counts || {})[genreContext] || 0
    // Selected genre NAMES (+ sub-genres), lowercased once here — the port's
    // FeedItem keeps the display casing (the Slint model lowercases at build
    // time instead, library_all.rs:540). Reading this inside visibleItems()
    // is what makes the grid/list model re-derive when a chip is toggled.
    readonly property var genreNames: {
        var src = (genreDoc.names || {})[genreContext] || []
        var out = []
        for (var i = 0; i < src.length; i++)
            out.push(String(src[i]).toLowerCase())
        return out
    }
    /// The toolbar's genre buttons reach the popup through here — it is
    /// declared LAST in this file and cannot be named from a child component.
    function toggleGenrePopup() { libGenrePopup.toggle() }

    /// Active restrictions for the genre tooltip.
    readonly property var filterSummaryGroups: {
        var tr = QbzSession.trRev
        var active = []
        if (root.showPurchases) active.push(QbzSession.tr("Purchases only", tr))
        if (root.showFavorites) active.push(QbzSession.tr("Favorites only", tr))
        if (root.activeTab === "all" && root.showFollowing)
            active.push(QbzSession.tr("Following only", tr))
        if (root.hiresOnly) active.push(QbzSession.tr("Hi-Res only", tr))
        if (root.activeTab === "all" && !root.showLocal)
            active.push(QbzSession.tr("Hide local albums from library", tr))
        return active.length ? [{ group: QbzSession.tr("Filters", tr), values: active }] : []
    }

    // Other-tab state.
    property string tabSearch: ""
    property string tracksSort: "default"
    property string playlistsSort: "default"
    property string artistsSort: "default"
    property string labelsSort: "default"
    readonly property string activeSort: activeTab === "all" ? sortBy + (sortAsc ? "-asc" : "-desc")
        : activeTab === "albums" ? albumsSort : activeTab === "tracks" ? tracksSort
        : activeTab === "playlists" ? playlistsSort : activeTab === "artists" ? artistsSort : labelsSort

    function pickSort(field) {
        var same = ReleaseSort.fieldOf(activeSort) === field
        var asc = same ? !ReleaseSort.ascending(activeSort)
            : ["default", "date", "updated", "duration", "track-count", "release-date"].indexOf(field) < 0
        if (activeTab === "all") {
            // "all" has no server "default" sort key of its own — its resting
            // order IS date-added descending, so the explicit Default row maps
            // back to exactly that.
            if (field === "default") {
                sortBy = "date"
                sortAsc = false
            } else {
                sortBy = field
                sortAsc = asc
            }
        } else {
            setPref(activeTab + "Sort", field === "default"
                ? (asc ? "default-reverse" : "default") : field + (asc ? "-asc" : "-desc"))
        }
    }

    property string albumsSort: "default" // default|title-asc|title-desc|artist-asc|oldest|newest
    property string playlistsSubTab: "all"

    /// The All tab's search field debounces; the toolbar routes through here
    /// so the timer stays next to the window report it feeds.
    function setAllSearch(v) {
        root.search = v
        allSearchDebounce.restart()
    }

    // ONE shared per-tab query, reset on entry — that IS the reference.
    // In Slint every entry into one of the five tabs runs
    // navigate_favorites -> load_favorites -> apply_favorites, which assigns
    // "" to that tab's search (favorites.rs:570/595/614/627/649) AND, because
    // the ExpandableSearch lives inside an `if` branch, destroys and
    // re-creates it COLLAPSED. So a tab is never re-entered with a stale
    // filter and five independent queries would be wrong. The port has one
    // feed and no per-tab reload, so the reset is explicit here.
    // The All tab is deliberately NOT reset: LibraryAllState.search is never
    // cleared anywhere in the reference, so its feed stays filtered behind a
    // collapsed magnifier — quirk included.
    onActiveTabChanged: {
        tabSearch = ""
        if (toolbar) toolbar.closeTabSearch()
        // A selection cannot survive leaving its own tab: the bulk bar would
        // still be armed over rows that are no longer on screen.
        setTracksMultiSelect(false)
        setAlbumsMultiSelect(false)
    }

    // Per-tab emptiness gate. Slint gates each per-tab toolbar (search AND
    // genre/sort/sub-tab/view toggles) on the tab's FULL loaded set
    // (FavoritesView.slint:593/636/707/722/747) — searching down to zero
    // results must NOT hide it, so this counts the feed and never
    // visibleItems(). Same keep-rules as visibleItems() minus the playlists
    // sub-tab split, which makes the playlists count the favorites-OR-
    // following the reference gate wants (counts.playlists is favorites
    // only, library_qt.rs:479).
    readonly property var tabTotals: {
        return feed._tabTotals || { "tracks": 0, "albums": 0, "artists": 0,
                                    "playlists": 0, "labels": 0 }
    }
    readonly property bool tabHasItems:
        activeTab === "all" || (tabTotals[activeTab] || 0) > 0

    // ------------------------- derive (JS) ------------------------------
    // THE model, derived once and held here. Both views bind to it and both
    // window reports read IT — never `grid.model` / `list.model` back off
    // the view, which is a hard crash, not a style point:
    //
    //   QQuickItemView::model() (libQt6Quick 6.11.1, +0x145dfe..+0x145e2c)
    //   reads THREE things off the private and guards only two of them:
    //     0x588  the QPointer's ExternalRefCountData guard  -> null-checked,
    //            and its strongref at +4 is checked too
    //     0x7e0  bit 0, the "this view owns a delegate model" flag
    //     0x590  the QPointer's raw VALUE -> handed to
    //            QQmlDelegateModel::model() with NO test at all
    //   So the getter is safe against a QPointer whose guard says "gone" and
    //   unsafe against the window where the guard still says "alive" while
    //   the value slot no longer refers to a usable object. setModel() opens
    //   exactly that window: it tears the old QQmlDelegateModel down, and its
    //   own repositioning emits contentYChanged from inside the teardown,
    //   straight into onContentYChanged and straight back into the getter.
    //   `this` arrives null (gdb: rsi=0), +8 is the first member read, SIGSEGV
    //   at address 8 in QQmlDelegateModel::model()+5. Reproduced standalone
    //   under qml6 6.11.1 (same fault ip, same dmesg Code bytes); it is the
    //   same fault the app took on 2026-07-27/28/29 going Library -> Labels
    //   with the grid scrolled — the tab switch re-derives the model AND the
    //   clamp to the shorter list moves contentY. With contentY already 0
    //   nothing is emitted and nothing crashes, which is why it was
    //   intermittent rather than every switch.
    //
    //   PRECONDITION, precisely, because the wrong one invites the wrong fix:
    //   the old delegate model is being DESTROYED, not momentarily unset. It
    //   does not come back a moment later. So the repair for any site with
    //   this shape is NOT "defer the readback one event-loop turn" (Qt.
    //   callLater / a 0ms Timer) — by then there is still no old model and,
    //   worse, the handler has been decoupled from the state it was meant to
    //   report. The only repair is to not ask the view for its model.
    //
    //   onModelChanged is NOT the entry: it fires after the swap is complete
    //   and the same readback there survives. Bisected, not assumed.
    //
    //   Only `.model` is unguarded. `count`, `contentHeight`, `currentIndex`,
    //   `indexAt()` and `itemAtIndex()` all null-check and are safe in the
    //   same handler — which is why the LocalLibrary tabs, which cache their
    //   array in `entries` and only ever ask the view for indices, never had
    //   this bug. Same shape here.
    //
    // Deriving once also replaces two independent passes over the feed (the
    // grid and the list each used to call visibleItems() on every change)
    // with one, and guarantees both views and the debounce Timer see the
    // SAME array instance.
    // Named `visibleRows`, not `visible` — root is a Rectangle and `visible`
    // is taken.
    // Timed like `parseFeed` above, and for the same reason: this runs a full
    // pass plus a sort over the WHOLE merged feed, so it is the one derive in
    // the app whose cost scales with the size of the user's library. The log
    // rides the shared route-timing category — it is silent unless
    // QT_LOGGING_RULES="qbz.nav.timing.info=true".
    readonly property var visibleRows: {
        // Explicit dependency for the narrow live-membership invalidation
        // above; the value itself is not part of the derive.
        var _favoriteFilterRev = root.favoriteFilterRev
        var _t = Date.now()
        var _rows = visibleItems()
        var _line = "[libtiming] derive tab=" + activeTab + " feed=" + feed.length
                  + " rows=" + _rows.length + " in " + (Date.now() - _t) + "ms"
        Qt.callLater(function () { console.info(libTiming, _line) })
        return _rows
    }

    // Emit counts and geometry, never search text or library contents. This
    // distinguishes an empty derive from a mounted view that cannot paint.
    function reportSurfaceState() {
        if (!root.visible || !content.ready) return
        console.info("[library-view] tab=" + root.activeTab
            + " feed=" + root.feed.length + " rows=" + root.visibleRows.length
            + " searchActive=" + (root.tabSearch.length > 0)
            + " artistsView=" + root.artistsView
            + " grid=" + content.showGrid + " gridCount=" + grid.count
            + " viewport=" + grid.width + "x" + grid.height
            + " contentY=" + grid.contentY)
    }
    onVisibleRowsChanged: Qt.callLater(root.reportSurfaceState)
    onVisibleChanged: Qt.callLater(root.reportSurfaceState)

    /// A-Z bucket for a title: its first letter uppercased, "#" for anything
    /// that is not a letter (a leading digit, "*", punctuation). Matches the
    /// jump strip's own buckets, so a letter in the strip always has a
    /// separator to land on.
    function alphaKey(title) {
        var t = (title || "").trim()
        if (t.length === 0) return "#"
        var c = t.charAt(0).toUpperCase()
        return (c >= "A" && c <= "Z") ? c : "#"
    }

    /// Inject a `group-header` pseudo-row before each run of rows sharing a
    /// group key. The rows are ALREADY ordered by that key (see the `by(...)`
    /// sorts above), so this is one linear pass.
    ///
    /// Album and artist grouping match the reference's separators. NAME
    /// grouping gets them too — the reference draws only the A-Z jump strip
    /// there and no separators, which the owner asked to close (2026-07-31):
    /// a strip that jumps to a letter with nothing labelling it leaves the
    /// user guessing where a bucket starts.
    function withGroupHeaders(rows, mode) {
        var out = []
        var last = null
        var n = 1
        for (var i = 0; i < rows.length; i++) {
            var r = rows[i]
            var key = mode === "album" ? (r.album || "")
                : mode === "artist" ? (r.artist || "")
                : alphaKey(r.title)
            if (key !== last) {
                out.push({ "kind": "group-header", "title": key, "id": "hdr:" + mode + ":" + key })
                last = key
            }
            // The visible track NUMBER keeps running across groups (the
            // reference numbers 1,2,3… straight through its separators), so it
            // cannot be the model index once headers share the model. Carried
            // on a shallow COPY — the feed's row objects are shared with the
            // grid and the artwork map and must not be mutated.
            out.push(Object.assign({}, r, { "_no": n }))
            n++
        }
        return out
    }

    function matchesSources(isPurchased, isFavorite, isFollowing) {
        return (!showPurchases || isPurchased)
            && (!showFavorites || isFavorite)
            && (activeTab !== "all" || !showFollowing || isFollowing)
    }

    function visibleItems() {
        var items = []
        var i
        if (activeTab === "all") {
            var needle = search.toLowerCase()
            // Genre gate (library_all.rs::derive): empty = no filter;
            // otherwise the item's genre must contain one of the selected
            // names, so genre-less kinds are excluded.
            var genres = root.genreNames
            // The dominant path: All really means all, newest first. Rust has
            // already merged every source in that order, so do not copy and
            // rescan a potentially 10k-row array merely to return it intact.
            if (needle === "" && genres.length === 0
                && !showPurchases && !showFavorites && !showFollowing && showLocal
                && sortBy === "date" && !sortAsc)
                return feed
            var allSeen = ({})
            for (i = 0; i < feed.length; i++) {
                var it = feed[i]
                if (!showLocal && hideableLocalAlbum(it)) continue
                var memberKey = it._membershipKey
                if (!matchesSources(feed._tabPurchased[memberKey] === true,
                                    feed._tabFavorites[memberKey] === true,
                                    feed._tabFollowing[memberKey] === true)) continue
                if (needle !== ""
                    && it._titleLc.indexOf(needle) < 0
                    && it._artistLc.indexOf(needle) < 0) continue
                if (genres.length > 0) {
                    var itemGenre = it._genreLc
                    if (itemGenre === "") continue
                    var genreHit = false
                    for (var gi = 0; gi < genres.length; gi++) {
                        if (itemGenre.indexOf(genres[gi]) >= 0) { genreHit = true; break }
                    }
                    if (!genreHit) continue
                }
                if (showPurchases || showFavorites || showFollowing) {
                    if (allSeen[memberKey]) continue
                    allSeen[memberKey] = true
                }
                items.push(it)
            }
            items = ReleaseSort.sortRows(items, root.activeSort)
            return items
        }
        var tabNeedle = tabSearch.toLowerCase()
        function hit(it) {
            return tabNeedle === ""
                || it._titleLc.indexOf(tabNeedle) >= 0
                || it._artistLc.indexOf(tabNeedle) >= 0
        }
        var sourceTab = activeTab === "tracks" || activeTab === "albums"
        var sourceKind = activeTab === "tracks" ? "track" : "album"
        var tabGenres = sourceTab ? root.genreNames : []
        // Every enabled "only" filter restricts the result. With none enabled,
        // show all entities. Prefer purchase metadata for an entity in both
        // sources, and render it once.
        var purchased = feed._tabPurchased || ({})
        var favorites = feed._tabFavorites || ({})
        var tabSeen = ({})
        for (i = 0; i < feed.length; i++) {
            var x = feed[i]
            var keep = false
            if (sourceTab && x.kind === sourceKind) {
                var sourceKey = x._membershipKey
                var isPurchased = purchased[sourceKey] === true
                var isFavorite = favorites[sourceKey] === true
                var included = matchesSources(isPurchased, isFavorite, false)
                var localRelease = sourceKind === "album" && isLocalFeedItem(x)
                if (localRelease && !showLocal && hideableLocalAlbum(x)) included = false
                // One representative row per included entity. A purchase row
                // wins even in Favorites-only so its quality badge survives.
                keep = included && (localRelease || (isPurchased && x.group === "purchases")
                    || (!isPurchased && x.group === "favorites"))
                if (keep) {
                    if (tabSeen[sourceKey]) keep = false
                    else tabSeen[sourceKey] = true
                }
            } else if (activeTab === "artists") keep = x.kind === "artist"
            else if (activeTab === "labels") keep = x.kind === "label"
            else if (activeTab === "playlists") {
                keep = x.kind === "playlist"
                    && (playlistsSubTab === "all" || x.playlistCreator === playlistsSubTab)
                if (keep) {
                    if (tabSeen["playlist:" + x.id]) keep = false
                    else tabSeen["playlist:" + x.id] = true
                }
            }
            if (keep && tabGenres.length > 0) {
                var tg = x._genreLc
                keep = false
                for (var tgi = 0; tgi < tabGenres.length; tgi++) {
                    if (tg !== "" && tg.indexOf(tabGenres[tgi]) >= 0) { keep = true; break }
                }
            }
            if (keep && sourceTab && hiresOnly && x.qualityTier !== "hires") keep = false
            if (keep && hit(x)) items.push(x)
        }
        if (activeTab === "albums")
            items = ReleaseSort.sortAlbums(items, albumsSort, albumsGroup)
        else items = ReleaseSort.sortRows(items, root.activeSort)
        // --- Group-by (favorites.rs::derive_tracks / derive_albums) ------
        // Grouping REORDERS the rows so a group's entries sit together. The
        // TRACKS list also gets separator rows (see withGroupHeaders); the
        // albums and artists GRIDS do not — a uniform-cell GridView has no
        // room for a full-width header, which is exactly why the reference
        // scrolls those two PROPORTIONALLY from the A-Z strip instead of
        // landing on a separator (FavoritesView.slint:1996, :2008).
        var cachedKey = function (item, key) {
            if (key === "title") return item._titleLc
            if (key === "artist") return item._artistLc
            if (key === "album") return item._albumLc
            return String(item[key] || "").toLowerCase()
        }
        var by = function (keys) {
            items.sort(function (a, b) {
                for (var k = 0; k < keys.length; k++) {
                    var av = cachedKey(a, keys[k]), bv = cachedKey(b, keys[k])
                    if (av !== bv) return av < bv ? -1 : 1
                }
                return 0
            })
        }
        if (activeTab === "tracks" && tracksGroup !== "off") {
            if (tracksSort !== "default") {
                var groupKey = tracksGroup === "name" ? "title" : tracksGroup
                items.sort(function (a, b) {
                    var av = cachedKey(a, groupKey), bv = cachedKey(b, groupKey)
                    if (tracksGroup === "name") { av = av.charAt(0); bv = bv.charAt(0) }
                    return av !== bv ? (av < bv ? -1 : 1) : ReleaseSort.compareItems(a, b, tracksSort)
                })
            } else if (tracksGroup === "album") by(["album", "title"])
            else if (tracksGroup === "artist") by(["artist", "album", "title"])
            else if (tracksGroup === "name") by(["title"])
            items = withGroupHeaders(items, tracksGroup)
        } else if (activeTab === "artists" && artistsGroup === "alpha" && artistsView === "grid") {
            items.sort(function (a, b) {
                var av = root.alphaKey(a.title), bv = root.alphaKey(b.title)
                return av !== bv ? (av < bv ? -1 : 1)
                    : artistsSort === "default" ? ReleaseSort.textCompare(a.title, b.title)
                    : ReleaseSort.compareItems(a, b, artistsSort)
            })
        }
        return items
    }

    // ------------------------ A-Z jump strip -----------------------------
    // FavoritesView.slint:1680-1691 (tracks, name grouping — index-accurate
    // because the rows are uniform), :1988-1999 (albums, alpha grouping) and
    // :2000-2011 (artists, alpha grouping).
    //
    // ONE deliberate improvement over the reference on the two GRIDS: it
    // scrolls proportionally (`ord / (jumps-1) * maxScroll`) because a Slint
    // Flickable cannot address a cell; a QML GridView can, so the strip lands
    // on the letter's FIRST card instead of near it. Same strip, same
    // buckets, an exact landing.
    readonly property bool dateGrouping: activeTab === "albums"
        && (albumsGroup === "year" || albumsGroup === "decade")
    readonly property bool alphaActive:
        dateGrouping ||
        (activeTab === "tracks" && tracksGroup !== "off")
        || (activeTab === "albums" && albumsGroup === "alpha")
        || (activeTab === "artists" && artistsGroup === "alpha" && artistsView === "grid")
    readonly property var alphaJumps: {
        if (!root.alphaActive) return []
        var rows = root.visibleRows
        var out = []
        var last = null
        for (var i = 0; i < rows.length; i++) {
            // The tracks list already carries a separator per bucket; jumping
            // to the SEPARATOR (not the first track) is what puts the letter
            // itself at the top of the viewport.
            if (activeTab === "tracks") {
                if (rows[i].kind !== "group-header") continue
                var trackKey = root.alphaKey(rows[i].title)
                if (trackKey === last) continue
                out.push({ "letter": trackKey, "index": i })
                last = trackKey
                continue
            }
            var key = root.dateGrouping
                ? ReleaseSort.groupLabel(rows[i], root.albumsGroup)
                : root.alphaKey(rows[i].title)
            if (key !== last) {
                out.push({ "letter": key, "index": i })
                last = key
            }
        }
        return out
    }
    readonly property bool alphaVisible: root.alphaActive && root.alphaJumps.length > 0
    function alphaJumpTo(index) {
        if (activeTab === "tracks") list.positionViewAtIndex(index, ListView.Beginning)
        else if (activeTab === "albums" && albumsView === "list") albumsList.positionAt(index)
        else grid.positionViewAtIndex(index, GridView.Beginning)
    }

    // ------------------------ multi-select -------------------------------
    // FavoritesView.slint:465-523 (the two toggles) + :1570 / :1734 (the two
    // bars). The SELECTION lives here, in QML, exactly like the Local Library
    // grid/table bars: "select-all" and "clear" never reach Rust, everything
    // else goes down as a JSON id array (src/library_bulk.rs).
    property bool tracksMultiSelect: false
    property var tracksSelected: ({})
    readonly property int tracksSelectedCount: Object.keys(root.tracksSelected).length
    property bool albumsMultiSelect: false
    property var albumsSelected: ({})
    readonly property int albumsSelectedCount: Object.keys(root.albumsSelected).length

    function setTracksMultiSelect(on) {
        root.tracksMultiSelect = on
        if (!on) root.tracksSelected = ({})
    }
    function setAlbumsMultiSelect(on) {
        root.albumsMultiSelect = on
        if (!on) root.albumsSelected = ({})
    }
    // A rebind needs a NEW object reference — mutating the map in place
    // notifies nothing (the same rule artMap and every row's `favorite`
    // property follow).
    /// Excel-style selection — controls/SelectionModel.qml holds the anchor
    /// and the Shift-range rule; this view keeps owning its two maps.
    ///
    /// TWO models, because tracks and albums are two independent selections
    /// with two independent anchors, and the rows here are MIXED: `visibleRows`
    /// carries both kinds, so each range is taken over the visible rows OF ITS
    /// OWN KIND. Ranging over the mixed list would let a shift-click on a
    /// track sweep up the albums drawn between it and the anchor.
    SelectionModel { id: trackSel }
    SelectionModel { id: albumSel }
    function _rowsOfKind(kind) {
        return root.visibleRows.filter(function (r) { return r.kind === kind })
    }
    function toggleTrackSelected(id, mods) {
        root.tracksSelected = trackSel.next(root.tracksSelected, id,
                                            root._rowsOfKind("track"),
                                            mods === undefined ? Qt.NoModifier : mods)
    }
    function toggleAlbumSelected(id, mods) {
        root.albumsSelected = albumSel.next(root.albumsSelected, id,
                                            root._rowsOfKind("album"),
                                            mods === undefined ? Qt.NoModifier : mods)
    }

    /// Selected ids in VISIBLE order — never `Object.keys(map)`. Qobuz ids are
    /// numeric strings, and a JS object iterates integer-like keys in ASCENDING
    /// NUMERIC order, so the keys of the map are not the order the user sees
    /// and "play next" would insert a re-sorted block.
    function selectedIdsInOrder(kind, map) {
        var rows = root.visibleRows
        var out = []
        for (var i = 0; i < rows.length; i++)
            if (rows[i].kind === kind && map[rows[i].id] === true) out.push(rows[i].id)
        return out
    }

    function tracksBulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var rows = root.visibleRows
            for (var i = 0; i < rows.length; i++)
                if (rows[i].kind === "track") m[rows[i].id] = true
            root.tracksSelected = m
            return
        }
        if (action === "clear") { root.tracksSelected = ({}); trackSel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder("track", root.tracksSelected)
        if (ids.length === 0) return
        QbzLibrary.libraryBulkAction("track", JSON.stringify(ids), action)
        // The Slint clears after an enqueue and after remove-selected, and
        // KEEPS the selection while a picker is still open (a failed write is
        // retried from the same modal) — local_bulk.rs::apply says the same.
        if (action !== "add-to-playlist" && action !== "add-to-mixtape")
            root.tracksSelected = ({})
    }
    function albumsBulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var rows = root.visibleRows
            for (var i = 0; i < rows.length; i++)
                if (rows[i].kind === "album") m[rows[i].id] = true
            root.albumsSelected = m
            return
        }
        if (action === "clear") { root.albumsSelected = ({}); albumSel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder("album", root.albumsSelected)
        if (ids.length === 0) return
        QbzLibrary.libraryBulkAction("album", JSON.stringify(ids), action)
        root.albumsSelected = ({})
    }

    // --- Ctrl+A / Escape hotkeys interface (2026-08-03 hotkeys-port §4.6) --
    // The duck-typed seam the AppShell router calls on QbzShell's
    // selectAllRequested / exitMultiSelectRequested. Only the tracks and
    // albums tabs have multi-select here; on any other tab selectAll() is a
    // deliberate no-op (the capability reporter still counts this view, so
    // Ctrl+A consumes without visible effect on the All/Artists/Playlists
    // tabs — those have no selection model to fill).
    readonly property bool multiSelectOn: root.tracksMultiSelect || root.albumsMultiSelect
    function selectAll() {
        if (root.activeTab === "tracks") {
            if (!root.tracksMultiSelect) root.setTracksMultiSelect(true)
            root.tracksBulkAction("select-all")
        } else if (root.activeTab === "albums") {
            if (!root.albumsMultiSelect) root.setAlbumsMultiSelect(true)
            root.albumsBulkAction("select-all")
        }
    }
    function exitMultiSelectMode() {
        if (root.tracksMultiSelect) root.setTracksMultiSelect(false)
        if (root.albumsMultiSelect) root.setAlbumsMultiSelect(false)
    }

    // --------------------- windowed artwork -----------------------------
    // Report the mounted window as artKeys; prune far-away covers.
    property var _artSent: ({})
    function reportWindow(visibleArray, first, last, visibleFirst) {
        if (visibleArray.length === 0) {
            root._artKeep = ({})
            root._artRequested = ({})
            root._artSent = ({})
            root._artInbox = ({})
            root.artPending = 0
            if (Object.keys(root.artMap).length > 0)
                root.artMap = ({})
            return
        }
        last = Math.min(last, visibleArray.length - 1)
        if (first > last) return
        var keys = []
        var keep = {}
        var requested = {}
        var span = last - first + 1
        var keepLo = Math.max(0, first - span)
        var keepHi = Math.min(visibleArray.length - 1, last + span)
        // A group-header pseudo-row has no artwork of its own; skipping it here
        // keeps `undefined` out of the keep-set and the pending count.
        for (var i = keepLo; i <= keepHi; i++)
            if (visibleArray[i].kind !== "group-header") keep[visibleArray[i].artKey] = true
        var m = artMap
        var inbox = root._artInbox
        var pending = 0
        var now = Date.now()
        // Visible and forward rows enter the existing worker queue first.
        var start = Math.max(first, Math.min(last, visibleFirst === undefined ? first : visibleFirst))
        for (var offset = 0; offset <= last - first; offset++) {
            i = start + offset
            if (i > last) i = first + i - last - 1
            if (visibleArray[i].kind === "group-header") continue
            var k = visibleArray[i].artKey
            if (visibleArray[i].imageUrl !== "") {
                requested[k] = true
                if (!m[k] && !inbox[k]) {
                    pending++
                    // Scrolling must not launch the same pending download
                    // again every 180 ms. Permit a later retry after failure.
                    if (!root._artSent[k] || now - root._artSent[k] >= 30000) {
                        root._artSent[k] = now
                        keys.push(k)
                    }
                }
            }
        }
        root._artKeep = keep
        root._artRequested = requested
        // Drives the skeleton Timer + the per-item cap (see the pulse block).
        artWindowFirst = first
        artPending = pending
        var changed = false
        for (var key in m) {
            if (!keep[key]) { delete m[key]; changed = true }
        }
        // A late completion for the previous window may already be queued but
        // not published. Drop it here so the frame flush cannot resurrect a
        // cover that this report just evicted.
        for (key in inbox) {
            if (!keep[key]) delete inbox[key]
        }
        for (key in root._artSent) {
            if (!keep[key]) delete root._artSent[key]
        }
        if (changed) artMap = Object.assign({}, m)
        if (keys.length > 0) QbzLibrary.libraryArtworkWindow(JSON.stringify(keys))
    }

    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            // This is a global signal. The selected-artist pane, Home and
            // several detail routes publish URL keys through it too; accepting
            // those here wakes the whole Library grid for an image it cannot
            // display. Only the current keep band belongs in this map.
            if (root._artKeep[key] !== true)
                return
            var known = root.artMap[key] || root._artInbox[key] || ""
            if (known === path)
                return
            if (known === "" && root._artRequested[key] === true
                && root.artPending > 0)
                root.artPending--
            root._artInbox[key] = path
            if (!artFlush.running) artFlush.start()
        }
        // Favourite state settled in the store. Patched IN PLACE, and
        // `feedChanged()` is deliberately NOT called — that is the WHOLE fix
        // for "clicking a heart in Library > All throws me back to the top".
        //
        //   feedChanged() -> `visibleRows` re-derives -> a NEW array reaches
        //   `model:` -> QQuickItemView::setModel() -> the view resets its
        //   scroll offset to 0 and rebuilds every delegate.
        //
        // Scrolled to row 40 in a 3,000-row feed, one heart click sent the
        // user back to row 0. Ordinarily nothing on screen needs the
        // re-derive: the grid cards each listen to this same signal for their
        // own key
        // (cards/AlbumCard.qml, TrackCard.qml, PlaylistCard.qml,
        // ArtistCard.qml) and the list rows do the same
        // (library/FeedListRow.qml).
        // The in-place mutation is for the delegates built LATER — when the
        // user scrolls, re-filters or switches tab — and it reaches them
        // because `visibleRows` pushes the very same row objects. The narrow
        // exception is a Tracks / Albums combination whose membership
        // depends on the heart; `favoriteFilterRev` re-derives only then.
        //
        // Same reasoning as onPinChanged below; the two are now symmetric.
        function onLibraryFavoriteChanged(key, value) {
            var f = root.feed
            for (var i = 0; i < f.length; i++) {
                // A purchased favourite has one row per source; patch both.
                if (f[i].artKey === key) {
                    f[i].isFavorite = value
                    if (value) f._tabFavorites[f[i]._membershipKey] = true
                    else delete f._tabFavorites[f[i]._membershipKey]
                }
            }
            // Keep the entity-level source set live even when a newly-added
            // favourite has no `favorites` row until the next API refresh.
            if (f._tabFavorites) {
                if (value) f._tabFavorites[key] = true
                else delete f._tabFavorites[key]
            }
            var sourceTab = root.activeTab === "tracks"
                || root.activeTab === "albums"
            // Favourite membership affects the model in every combination
            // except Purchases-only. Re-derive only in those cases; elsewhere
            // the mounted delegates consume this same signal directly and
            // retain the existing no-scroll-reset path.
            if ((sourceTab || root.activeTab === "all") && root.showFavorites)
                root.favoriteFilterRev++
        }
        // Pin state settled in the store. The row is patched IN PLACE and the
        // feed is deliberately NOT re-signalled: `feedChanged()` re-derives
        // `visibleRows`, which swaps the GridView/ListView model and tears
        // down every delegate — per pin click. The glyphs on screen do not
        // need it, because each card listens to this same signal for its own
        // key (cards/AlbumCard.qml); the mutation here is for the delegates
        // built LATER, when the user scrolls or re-filters. `visibleRows`
        // pushes the very same row objects, so both see the new value.
        function onPinChanged(key, value) {
            var f = root.feed
            for (var i = 0; i < f.length; i++) {
                if (f[i].artKey === key) { f[i].isPinned = value; break }
            }
        }
    }

    // Debounced window reporting (180ms, library_all.rs throttle).
    //
    // `pendingRows` is the array the band indexes into. It is null for every
    // body that scrolls `visibleRows` itself (grid, list, albums list,
    // playlists list) and non-null ONLY for a body whose mounted order is not
    // `visibleRows` — today that is the Artists sidepanel rail, which sorts
    // A-Z and interleaves letter headers, so its ListView index N is NOT
    // `visibleRows[N]`. Reporting the rail against `visibleRows` would name
    // the wrong artists; reporting nothing (what it did until now) left
    // `artMap` empty for every row it mounts and every rail avatar fell to
    // the placeholder.
    Timer {
        id: windowDebounce
        interval: 180
        onTriggered: root.reportWindow(
            pendingRows ? pendingRows : root.visibleRows, pendingFirst, pendingLast, pendingVisibleFirst)
        property int pendingFirst: 0
        property int pendingLast: 0
        property int pendingVisibleFirst: 0
        property var pendingRows: null
    }
    /// `rows` is optional — omit it and the band is read off `visibleRows`.
    function queueWindowReport(first, last, rows, visibleFirst) {
        windowDebounce.pendingFirst = first
        windowDebounce.pendingLast = last
        windowDebounce.pendingVisibleFirst = visibleFirst === undefined ? first : visibleFirst
        windowDebounce.pendingRows = rows === undefined ? null : rows
        // Coalesce to the latest window without postponing it until scrolling
        // stops. The existing 180 ms cap still bounds bridge traffic.
        if (!windowDebounce.running) windowDebounce.start()
    }

    // All-tab search settle -> re-report the first window so the filtered
    // head gets covers. Lives at ROOT scope (its callee already does) so it
    // survives the search control it used to be parented to.
    Timer {
        id: allSearchDebounce
        interval: 250
        onTriggered: root.queueWindowReport(0, 59)
    }

    // ------------------- per-row play (PARITY-DEBT #5) -------------------
    // Every track row in this view queues the list the user is LOOKING AT,
    // anchored on the clicked row — it does NOT play one track and stop.
    //
    // Slint: playback.rs:3518-3524
    //   order_by_visible(FavoritesState.tracks-visible,   // rendered order
    //                    favorites::play_tracks(),        // FAV_CURRENT
    //                    clicked_id) -> play_tracks(tracks, idx)
    // and, when order_by_visible returns None (the clicked row does not
    // resolve inside the visible list), the single-track fallback at :3620.
    //
    // The visible ORDER is the whole point: it carries the tab, the search,
    // the sort and the genre/source filters. In this port that order exists
    // only here (the feed is derived in JS), so it goes down as a JSON id
    // array and `library_qt::order_by_visible` resolves it against the feed.
    // `visibleRows` is THE derived model both views bind to — never read
    // `grid.model` / `list.model` back off a view (see the note on it).
    function visibleTrackIds() {
        var rows = root.visibleRows
        var ids = []
        for (var i = 0; i < rows.length; i++)
            if (rows[i].kind === "track") ids.push(rows[i].id)
        return ids
    }
    function playTrackInContext(trackId) {
        QbzLibrary.libraryPlayVisible(JSON.stringify(visibleTrackIds()), trackId)
    }

    // Track context-menu model (TrackCard.slint track-menu) + dispatch —
    // shared by the LIST rows here (the grid card carries its own copy in
    // TrackCard.qml).
    // `favorite` is passed in rather than read off `item`: the row owns the
    // live state (see FeedListRow.favorite), `item.isFavorite` is only its
    // seed.
    function trackMenuModel(item, favorite, pulledDead) {
        var m = []
        var releaseGone = item.releaseUnavailable === true
        if (!pulledDead) {
            m.push({ "label": QbzSession.tr("Play", QbzSession.trRev), "icon": "play-fill", "action": "play" })
            m.push({ "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" })
            m.push({ "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" })
            m.push({ "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" })
        }
        if (item.artistId !== "") m.push({ "label": QbzSession.tr("Go to artist", QbzSession.trRev), "icon": "user", "action": "go-artist" })
        if (item.albumId !== "" && !releaseGone)
            m.push({ "label": QbzSession.tr("Go to album", QbzSession.trRev), "icon": "disc", "action": "go-album" })
        // A dead favourite is still a favourite. Suppressing this row was the
        // bug: it made the tombstone impossible to remove.
        m.push({ "label": favorite ? QbzSession.tr("Remove from Library", QbzSession.trRev) : QbzSession.tr("Add to Library", QbzSession.trRev),
                 "icon": favorite ? "heart-filled" : "heart", "action": "favorite" })
        if (releaseGone) {
            m.push({ "label": QbzSession.tr("Look for better replacement", QbzSession.trRev),
                     "icon": "search", "action": "find-release" })
            m.push({ "label": QbzSession.tr("Remove all favorites from this release", QbzSession.trRev),
                     "icon": "trash-2", "action": "remove-release-favorites" })
        }
        return m
    }
    function askRemoveReleaseFavorites(item) {
        if (!item || (item.albumId || "") === "")
            return
        root.pendingReleaseRemovalId = item.albumId
        releaseRemovalConfirm.open()
    }
    // Takes the ROW, not the item — "favorite" has to go through the row's
    // own property (a write to `item.isFavorite` notifies nothing).
    function trackAction(row, a) {
        var item = row.item
        if ((a === "play" || a === "next" || a === "later" || a === "queue")
                && row.pulledDead)
            return
        if (a === "play") root.playTrackInContext(item.id)
        else if (a === "next") QbzPlayer.enqueueTrack(item.id, "next")
        else if (a === "later") QbzPlayer.enqueueTrack(item.id, "later")
        else if (a === "queue") QbzPlayer.enqueueTrack(item.id, "queue")
        else if (a === "go-artist") QbzArtist.openArtist(item.artistId)
        else if (a === "go-album") QbzAlbum.openAlbum(item.albumId)
        else if (a === "favorite") row.toggleFavorite()
        else if (a === "find-release") QbzTrackReplace.openRelease(JSON.stringify({
            "targetKind": "track",
            "albumId": item.albumId || "",
            "albumTitle": item.album || "",
            "trackId": item.id || "",
            "trackTitle": item.title || "",
            "artist": item.artist || item.albumArtist || "",
            "albumArtist": item.albumArtist || item.artist || "",
            "isrc": item.isrc || "",
            "durationSecs": item.durationSecs || 0
        }))
        else if (a === "remove-release-favorites") root.askRemoveReleaseFavorites(item)
    }

    // ============================ view ===================================

    // Offline gate (OfflinePlaceholder replica; the Slint offline RAIL is
    // out of scope — see header note).
    QbzOfflinePlaceholder {
        visible: QbzSession.offline
        anchors.centerIn: parent
        showSettingsAction: true
        onSettingsClicked: QbzShell.navigateTo("settings")
    }

    Column {
        anchors.fill: parent
        spacing: 0
        visible: !QbzSession.offline

        LibraryToolbar {
            id: toolbar
            width: parent.width
            view: root
        }
        Rectangle {
            width: parent.width
            height: 1
            color: theme.borderSubtle
        }

        // ===================== SCROLLING CONTENT =========================
        Item {
            id: content
            width: parent.width
            height: parent.height - toolbar.height - 1
            clip: true

            // The tracks bulk bar is pinned above the list (the Slint scrolls
            // it away with the page; here the list IS the scrolling view).
            readonly property int barInset:
                (root.activeTab === "tracks" && root.tracksMultiSelect) ? 52 : 0

            // Loading skeleton — a deliberate ADDITION: the Slint mounts a
            // bare centred 36px LoadingSpinner here (FavoritesView.slint:
            // 956/1012), which says "busy" but not "this is the shape of
            // what is coming". The placeholders take the exact footprint of
            // the cells that will replace them (200x246 grid cells / 44-50px
            // rows), so the first paint reads as the page filling in.
            //
            // COST: the layer only exists while `libraryLoading` — the
            // Repeater models collapse to 0 otherwise, so nothing is mounted
            // and nothing animates once the feed lands. The mounted count is
            // capped to the viewport (never the model), and QbzSkeleton caps
            // ANIMATED instances at 48; the pulse itself is one shared bool.
            Item {
                id: skelLayer
                anchors.fill: parent
                anchors.leftMargin: 32
                anchors.rightMargin: 32
                anchors.topMargin: 16
                visible: QbzLibrary.libraryLoading && QbzLibrary.libraryError === ""
                // Which shape the tab is about to render.
                readonly property bool listShape:
                    (root.activeTab === "all" && root.viewMode === "list")
                    || root.activeTab === "tracks"
                    || (root.activeTab === "albums" && root.albumsView === "list")
                    || (root.activeTab === "playlists" && root.playlistsView === "list")
                readonly property int cols: Math.max(1, Math.floor(width / 220))
                readonly property int rows: Math.max(1, Math.ceil(height / 266))
                readonly property int rowHeight: root.activeTab === "tracks" ? 50
                    : root.activeTab === "albums" ? 64
                    : root.activeTab === "playlists" ? 60 : 44

                Grid {
                    columns: skelLayer.cols
                    columnSpacing: 20
                    rowSpacing: 20
                    Repeater {
                        model: (skelLayer.visible && !skelLayer.listShape)
                            ? Math.min(48, skelLayer.cols * skelLayer.rows) : 0
                        delegate: QbzSkeleton {
                            required property int index
                            variant: "card"
                            // The real cell exactly: GridView cellWidth 220 /
                            // cellHeight 266 = 200x246 + the 20px gaps.
                            width: 200
                            height: 246
                            phase: root.skelPhase
                            cellIndex: index
                        }
                    }
                }
                Column {
                    width: parent.width
                    spacing: 6
                    Repeater {
                        model: (skelLayer.visible && skelLayer.listShape)
                            ? Math.min(48, Math.ceil(skelLayer.height / (skelLayer.rowHeight + 6))) : 0
                        delegate: QbzSkeleton {
                            required property int index
                            variant: "row"
                            width: skelLayer.width
                            height: skelLayer.rowHeight
                            phase: root.skelPhase
                            cellIndex: index
                        }
                    }
                }
            }

            // Error + retry.
            Column {
                visible: !QbzLibrary.libraryLoading && QbzLibrary.libraryError !== ""
                anchors.centerIn: parent
                spacing: 10
                Text {
                    text: QbzSession.tr("Couldn't load your Library.", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                }
                Text {
                    text: QbzLibrary.libraryError
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                }
                Rectangle {
                    width: retryText.implicitWidth + 28
                    height: 32
                    radius: 6
                    anchors.horizontalCenter: parent.horizontalCenter
                    color: retryArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                    border.width: 1
                    border.color: theme.borderSubtle
                    Text {
                        id: retryText
                        anchors.centerIn: parent
                        text: QbzSession.tr("Retry", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontLegal
                    }
                    MouseArea {
                        id: retryArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzLibrary.reloadLibrary()
                    }
                }
            }

            readonly property bool ready:
                !QbzLibrary.libraryLoading && QbzLibrary.libraryError === ""
            onReadyChanged: Qt.callLater(root.reportSurfaceState)
            // Which surface owns the tab right now — one predicate per body,
            // so no two can mount at once.
            readonly property bool showAlbumsList:
                content.ready && root.activeTab === "albums" && root.albumsView === "list"
            readonly property bool showPlaylistsList:
                content.ready && root.activeTab === "playlists" && root.playlistsView === "list"
            readonly property bool showArtistsPanel:
                content.ready && root.activeTab === "artists" && root.artistsView === "sidepanel"
            readonly property bool showList:
                content.ready && ((root.activeTab === "all" && root.viewMode === "list")
                                  || root.activeTab === "tracks")
            readonly property bool showGrid:
                content.ready && !content.showList && !content.showAlbumsList
                && !content.showPlaylistsList && !content.showArtistsPanel

            // ============ GRID (all tabs; All in grid mode) ==============
            // HEIGHT-GATED, and that is the load-bearing part — see the
            // `showGrid`/`showList`/... predicates above, which already say
            // "one predicate per body, so no two can mount at once". They did
            // not achieve that: `visible: false` stops a view from PAINTING,
            // but a QQuickItemView that still has a viewport and a model keeps
            // refilling it. Measured 2026-08-17 with a per-view creation
            // counter: on the Albums tab the grid built 51 delegates and the
            // hidden list built 47 alongside it; on Tracks the list built 47
            // and the hidden GRID built 51 — the expensive direction, since a
            // FeedGridCell carries a cover. Half of every Library mount was
            // being spent on the body that was not showing.
            //
            // Collapsing the height to 0 is what actually stops the refill.
            // The obvious alternative — `model: visible ? root.visibleRows :
            // []` — is the one thing this file spends a paragraph warning
            // against (see onLibraryFavoriteChanged): handing `model` a new
            // value goes through QQuickItemView::setModel(), which resets the
            // scroll offset to 0 and rebuilds every delegate. The height gate
            // leaves `model` untouched.
            GridView {
                id: grid
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.leftMargin: 32
                anchors.rightMargin: root.alphaVisible ? (root.dateGrouping ? 98 : 52) : 32
                anchors.topMargin: 16
                height: grid.visible ? parent.height - 16 : 0
                visible: content.showGrid && root.activeTab !== "tracks"
                cellWidth: 220
                cellHeight: 266
                cacheBuffer: 266 * 2
                // Mixed cards are expensive to construct. FeedGridCell resets
                // optimistic heart/pin bindings and closes transient popups at
                // the reuse edge, so native pooling is safe here.
                reuseItems: true
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: root.visibleRows

                onContentYChanged: root.gridWindowReport(true)
                onModelChanged: root.gridWindowReport()
                onWidthChanged: root.gridWindowReport()
                onVisibleChanged: root.gridWindowReport()
                // onHeightChanged IS THE ONE THAT WAS MISSING, and its absence
                // is why this grid loaded exactly ONE ROW of covers and then
                // nothing until the user scrolled.
                //
                // `height` is gated on `visible` two lines up — that is the
                // 2026-08-17 fix that stops a hidden body from refilling, and
                // it stays. But it means the height is 0 right up until the
                // body appears, and QML gives no ordering between "the
                // `visible` change handler runs" and "the `height` binding that
                // depends on `visible` re-evaluates". When the handler wins,
                // `gridWindowReport` reads height 0 and computes
                // `lastRow = ceil(0/266) + 1 = 1`, i.e. a window of exactly
                // `cols` items — one row, six covers on the owner's window, and
                // that is precisely what the screencast shows. Nothing fires
                // again until `onContentYChanged`, which is why a nudge of the
                // scrollbar "fixed" it.
                //
                // The control case is in this same tree: `LibraryAlbumsList`
                // reports on `onHeightChanged` and does not have the defect.
                // The width already had its handler; the height never got one
                // because before the visibility gate the height never changed.
                onHeightChanged: root.gridWindowReport()
                Component.onCompleted: root.gridWindowReport()

                delegate: FeedGridCell {
                    required property var modelData
                    required property int index
                    view: root
                    item: modelData
                    // Viewport-relative so the skeleton's 48-instance cap
                    // follows the window, not the (possibly 10k-item) model.
                    cellIndex: Math.max(0, index - root.artWindowFirst)
                }
            }

            // ============ LIST (All list mode + Tracks tab) ==============
            // Height-gated for the reason spelled out on the grid above.
            ListView {
                id: list
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.leftMargin: 32
                anchors.rightMargin: root.alphaVisible ? (root.dateGrouping ? 98 : 52) : 32
                anchors.topMargin: 10 + content.barInset
                height: list.visible ? parent.height - (10 + content.barInset) : 0
                visible: content.showList
                spacing: root.activeTab === "all" ? 2 : 0
                cacheBuffer: 50 * 10
                reuseItems: true
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: root.visibleRows
                onContentYChanged: root.listWindowReport()
                onModelChanged: root.listWindowReport()
                onVisibleChanged: root.listWindowReport()
                // Same missing trigger, same `height: visible ? … : 0` gate,
                // same one-window-of-stale-zero — see the grid above.
                onHeightChanged: root.listWindowReport()
                Component.onCompleted: root.listWindowReport()

                delegate: Loader {
                    required property var modelData
                    required property int index
                    width: list.width
                    height: modelData && modelData.kind === "group-header"
                        ? 34
                        : 50
                    Component {
                        id: listHeaderComp
                        Item {
                            Text {
                                x: 2
                                anchors.verticalCenter: parent.verticalCenter
                                width: parent.width - 4
                                text: modelData.title
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightSemibold
                                elide: Text.ElideRight
                            }
                        }
                    }
                    Component {
                        id: trackRowComp
                        TrackRow {
                            item: {
                                var art = root.libraryTrackArtwork
                                    ? (root.artMap[modelData.artKey] || "") : ""
                                return art === "" ? modelData
                                    : Object.assign({}, modelData, { "artPath": art })
                            }
                            number: modelData._no || (index + 1)
                            showArtwork: root.libraryTrackArtwork
                            artPending: root.libraryTrackArtwork
                                && (modelData.imageUrl || "") !== ""
                                && (root.artMap[modelData.artKey] || "") === ""
                            skelPhase: root.skelPhase
                            artSettleMs: 2500
                            selectMode: root.tracksMultiSelect
                            checked: root.tracksSelected[modelData.id] === true
                            onToggleSelect: function (mods) { root.toggleTrackSelected(modelData.id, mods) }
                            onPlayRequested: root.playTrackInContext(item.id)
                            onEnqueueRequested: function (m) { QbzPlayer.enqueueTrack(item.id, m) }
                            // MyQBZ "Add to mixtape" — the HOST builds the
                            // AddItem array (TrackRow does not know
                            // itemType/source).
                            //
                            // The source comes off the ROW: every feed row
                            // carries its own word (library_qt.rs:54 —
                            // "qobuz" | "local" | "plex"), so a Plex or local
                            // row can never be stored as a Qobuz id again.
                            // "plex" is folded into "local" because the store
                            // has no Plex source: `AddItem.source` is
                            // "qobuz" | "local" and `source_from_str`
                            // (myqbz_add_qt.rs:85-90) maps everything that is
                            // not "local" to Qobuz — passing "plex" verbatim
                            // would silently become "qobuz" again.
                            //
                            // Today this list only ever holds Qobuz rows (the
                            // Tracks tab keeps `kind === "track" && group ===
                            // "favorites"` and the local layer lands in group
                            // "local"), so this is a no-op until that filter
                            // changes — which is the point: it stays right
                            // when it does.
                            onMixtapeRequested: QbzMyQbzAdd.open(JSON.stringify([{
                                "itemType": "track",
                                "source": item.source === "local" || item.source === "plex"
                                    ? "local" : "qobuz",
                                "sourceItemId": item.id, "title": item.title || "",
                                "subtitle": item.artist || "", "artworkUrl": item.imageUrl || "",
                                "year": null, "trackCount": null
                            }]))
                        }
                    }
                    Component {
                        id: feedRowComp
                        FeedListRow {
                            view: root
                            item: modelData
                            rowIndex: Math.max(0, index - root.artWindowFirst)
                            displayIndex: index
                        }
                    }
                    sourceComponent: (modelData && modelData.kind === "group-header")
                        ? listHeaderComp
                        : (root.activeTab === "tracks" ? trackRowComp : feedRowComp)
                    function releaseLoadedRow() {
                        if (!item)
                            return
                        if (item.recycleActive !== undefined)
                            item.recycleActive = false
                        if (typeof item.releaseForReuse === "function")
                            item.releaseForReuse()
                    }
                    function activateLoadedRow() {
                        if (item && item.recycleActive !== undefined)
                            item.recycleActive = true
                    }
                    onLoaded: activateLoadedRow()
                    ListView.onPooled: releaseLoadedRow()
                    ListView.onReused: activateLoadedRow()
                }
            }

            // ============ Albums LIST mode ===============================
            LibraryAlbumsList {
                id: albumsList
                visible: content.showAlbumsList
                anchors.fill: parent
                anchors.leftMargin: 32
                anchors.rightMargin: root.alphaVisible ? (root.dateGrouping ? 98 : 52) : 32
                anchors.topMargin: 16
                view: root
                rows: content.showAlbumsList ? root.visibleRows : []
            }

            // ============ Playlists LIST mode ============================
            // FavoritesView.slint:1935 — the same ViewToggle the albums tab
            // has, over primitives/PlaylistListRow.
            ListView {
                id: playlistsList
                visible: content.showPlaylistsList
                anchors.fill: parent
                anchors.leftMargin: 32
                anchors.rightMargin: 32
                anchors.topMargin: 16
                clip: true
                spacing: 2
                cacheBuffer: 60 * 8
                reuseItems: true
                boundsBehavior: Flickable.StopAtBounds
                model: content.showPlaylistsList ? root.visibleRows : []

                // Gated on visibility for the same reason gridWindowReport and
                // listWindowReport are: `artMap` is ONE map, a report PRUNES
                // outside its band, and this body still fires while hidden —
                // `model` flips to [] on every tab switch (onModelChanged) and
                // `anchors.fill` makes every window resize an onHeightChanged.
                // Ungated, a resize on the Artists sidepanel evicted the rail's
                // avatars and requested a `visibleRows` band the rail is not
                // even showing (its order is A-Z, not feed order).
                function report() {
                    if (!playlistsList.visible) return
                    var first = Math.max(0, Math.floor(playlistsList.contentY / 62) - 4)
                    var last = Math.ceil((playlistsList.contentY + playlistsList.height) / 62) + 4
                    root.queueWindowReport(first, Math.min(root.visibleRows.length - 1, last))
                }
                onContentYChanged: playlistsList.report()
                onModelChanged: playlistsList.report()
                onHeightChanged: playlistsList.report()
                onVisibleChanged: playlistsList.report()
                Component.onCompleted: playlistsList.report()

                delegate: PlaylistListRow {
                    required property var modelData
                    required property int index
                    width: playlistsList.width
                    item: modelData
                    rowIndex: index
                    artSource: root.artMap[modelData.artKey] || ""
                    covers: modelData.playlistOwnImage === true ? [] : (modelData.covers || [])
                }
            }

            // ============ Artists SIDEPANEL mode =========================
            // Behind a Loader so LEAVING the mode really destroys it: the
            // panel drops its Rust-side selection in Component.onDestruction,
            // and it also stops rebuilding its rail on every feed change while
            // the user is on another tab.
            Loader {
                anchors.fill: parent
                active: content.showArtistsPanel
                sourceComponent: LibraryArtistsPanel { view: root }
            }

            // Declared AFTER every scrolling body: declaration order IS
            // z-order, and this bar overlays the top of the content area.
            // ---- Tracks multi-select bar (FavoritesView.slint:1570) -----
            QbzMultiSelectBar {
                id: tracksBar
                visible: root.activeTab === "tracks" && root.tracksMultiSelect && content.ready
                anchors.top: parent.top
                anchors.topMargin: 10
                anchors.left: parent.left
                anchors.leftMargin: 32
                anchors.right: parent.right
                anchors.rightMargin: 32
                selectedCount: root.tracksSelectedCount
                // The reference's inventory MINUS "Make available offline":
                // the cache engine is live, but the Library bulk-download
                // action has no bridge seam yet. A bulk bar must not render a
                // control that no-ops. Everything else is wired.
                // "heart-crack" is likewise not in this port's icon set, so
                // "Remove from Library" uses the same heart + danger pair
                // views/local/LocalTracksTab.qml already ships.
                actions: [
                    { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                    { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                    { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                    { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                    { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                    { "id": "add-to-mixtape", "label": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), "icon": "cassette-tape", "danger": false, "needsSelection": true },
                    { "id": "remove-selected", "label": QbzSession.tr("Remove from Library", QbzSession.trRev), "icon": "heart", "danger": true, "needsSelection": true },
                    { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true },
                ]
                onAction: function (id) { root.tracksBulkAction(id) }
            }

            // A-Z jump strip, right edge (FavoritesView.slint:1683 / :1992 /
            // :2004 all pin it at `parent.width - self.width - 20px`).
            QbzAlphaStrip {
                visible: root.alphaVisible
                anchors.right: parent.right
                anchors.rightMargin: 12
                anchors.top: parent.top
                anchors.topMargin: 16
                anchors.bottom: parent.bottom
                jumps: root.alphaJumps
                width: root.dateGrouping ? 64 : 18
                completeAlphabet: !root.dateGrouping
                onJump: function (ordinal, index) { root.alphaJumpTo(index) }
            }

            // Thin auto-hiding scrollbars (ListScrollbar replica).
            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: grid; scope: "library:" + root.activeTab }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: root.alphaVisible ? (root.dateGrouping ? 80 : 34) : 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: grid
                visible: grid.visible && grid.contentHeight > grid.height
            }
            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: list; scope: "library:" + root.activeTab }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: root.alphaVisible ? (root.dateGrouping ? 80 : 34) : 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: list
                visible: list.visible && list.contentHeight > list.height
            }
            // Back/forward scroll memory (controls/ScrollMemory.qml): reports
            // this container's offset while it is the live page, and restores it
            // when a back/forward step arms this route.
            ScrollMemory { target: playlistsList; scope: "library:" + root.activeTab }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: playlistsList
                visible: playlistsList.visible
                    && playlistsList.contentHeight > playlistsList.height
            }
        }
    }

    // Both run from onContentYChanged, i.e. from INSIDE QQuickItemView::
    // setModel() — see the visibleRows comment. Everything they touch on the
    // view (width/height/contentY/cellWidth/cellHeight) is a plain qreal on
    // the private; the array comes from root, never from the view.
    //
    // Both early-return while their body is hidden. `artMap` is ONE map and a
    // report PRUNES everything outside the reported band, so a hidden body
    // that still fires (the grid's rightMargin changes when `alphaVisible`
    // flips; the list's `onModelChanged` fires on every tab switch because its
    // `model` is bound to `visibleRows` unconditionally) would evict the
    // covers of whichever body is actually on screen — the Artists sidepanel
    // rail above all, since it is the one body whose band is NOT a
    // `visibleRows` band. `onVisibleChanged` keeps the gate from swallowing
    // the first report of a body that becomes visible after its model landed.
    property string _gridMotionBand: ""
    property double _gridMotionReportedAt: 0
    function gridWindowReport(motionOnly) {
        if (!grid.visible) return
        var cols = Math.max(1, Math.floor(grid.width / grid.cellWidth))
        var visibleFirst = Math.max(0, Math.floor(grid.contentY / grid.cellHeight))
        // Match the two rows of delegates already held by cacheBuffer.
        var firstRow = Math.max(0, visibleFirst - 2)
        var lastRow = Math.ceil((grid.contentY + grid.height) / grid.cellHeight) + 2
        var m = root.visibleRows
        var band = firstRow + ":" + lastRow + ":" + visibleFirst + ":" + cols
        var now = Date.now()
        // Do not restart the bridge timer for every pixel in the same band.
        // Non-motion notifications bypass this guard; a periodic motion
        // refresh also preserves the existing failed-request retry behavior.
        if (motionOnly === true && root._gridMotionBand === band
                && now - root._gridMotionReportedAt < 1000) return
        root._gridMotionBand = band
        root._gridMotionReportedAt = now
        queueWindowReport(firstRow * cols, Math.min(m.length - 1, lastRow * cols - 1), undefined, visibleFirst * cols)
    }
    function listWindowReport() {
        if (!list.visible) return
        // With the Library track-art opt-in disabled this list renders no
        // images; release its window instead of decoding invisible covers.
        if (root.activeTab === "tracks" && !root.libraryTrackArtwork) {
            root.reportWindow([], 0, -1)
            return
        }
        // Variable-height group headers and the All tab's 2px zebra gutter
        // make arithmetic row pitches approximate. Ask the native ListView
        // for the real visible indices and keep a four-row artwork runway.
        var first = list.indexAt(4, list.contentY + 1)
        var last = list.indexAt(4, list.contentY + Math.max(1, list.height) - 1)
        if (first < 0)
            first = Math.max(0, Math.floor(list.contentY / 50))
        if (last < 0)
            last = Math.ceil((list.contentY + list.height) / 50)
        first = Math.max(0, first - 4)
        last += 4
        queueWindowReport(first, Math.min(root.visibleRows.length - 1, last))
    }

    // ============================ overlay =================================
    // Declared LAST: declaration order IS z-order, so the popup sits above
    // the toolbar and the grid and keeps its own presses. Hidden AND
    // disabled while closed, so it never eats a click meant for the view.
    // Each Library tab and Discover carry independent selections and counts.
    GenreFilterPopup {
        id: libGenrePopup
        anchors.fill: parent
        context: root.genreContext
        // Under the 56px toolbar, right-aligned like the Slint overlay.
        anchorTop: 62
        anchorRight: 32
    }

    // Release-wide cleanup is destructive enough to confirm: it can remove an
    // album heart plus several track hearts in one server batch. Mounted after
    // every view overlay so declaration order and QbzConfirmModal's own z=3100
    // keep it above the feed and filter popup.
    QbzConfirmModal {
        id: releaseRemovalConfirm
        anchors.fill: parent
        title: QbzSession.tr("Remove all favorites from this release?", QbzSession.trRev)
        body: QbzSession.tr("Remove the album and every track from this release from your Library?",
                            QbzSession.trRev)
        confirmLabel: QbzSession.tr("Remove", QbzSession.trRev)
        danger: true
        onConfirmed: {
            var albumId = root.pendingReleaseRemovalId
            root.pendingReleaseRemovalId = ""
            if (albumId !== "")
                QbzLibrary.libraryRemoveReleaseFavorites(albumId)
        }
        onCancelled: root.pendingReleaseRemovalId = ""
    }
}
