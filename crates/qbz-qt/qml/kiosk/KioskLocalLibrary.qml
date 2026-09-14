// KioskLocalLibrary — the kiosk Local Library host.
//
// ── WHAT CHANGED AND WHY ──────────────────────────────────────────────────
// The K0 audit found this view reading LEGACY JSON documents while Full UI had
// already switched the same surfaces to the paged native models. That is not a
// styling difference, it is why the panel showed empty Artists and Tracks tabs
// on a machine whose library is fine: `local_albums_json` / `local_artists_json`
// / `local_tracks_json` are published by the legacy readers only, and once
// `albums/artists/tracks_native_active` is true (the production default) Rust
// stops republishing them. It also mounted `Repeater`s over the whole document
// with a Loader per row — 20 416 items and 10 053 Loaders on the Albums tab at
// the 10 000-album fixture, 1.19 s to idle; 20 487 / 10 075 on Artists at
// 1.87 s (evidence/runtime-baseline/partial-initial.jsonl).
//
// This file is now only a HOST. It owns:
//   1. the tab set, its default and its history;
//   2. the four legacy documents (still the authority for Folders, Genres and
//      as the automatic fallback when a catalog session fails);
//   3. the artwork window REGISTRY — the one policy that cannot be split per
//      surface, because eviction is only correct against every live surface at
//      once (the desktop `_windows` finding, reproduced);
//   4. the nav geometry it publishes into QbzKioskNav;
//   5. the media FUNNEL (quality / format / source / favorites) — the SAME
//      persisted state as the desktop view, seeded from `QbzLocal.albumsFilter`
//      and written through the desktop's own setters (owner report 2026-09-07:
//      a funnel set on the desktop kept filtering the kiosk with no control to
//      see or clear it);
//   6. the "Open" entry point (folder / audio CD / SACD image) that starts an
//      ephemeral session — the desktop reaches it through LocalChrome's
//      toolbar and the nav flyout, neither of which the kiosk mounts, so the
//      Ephemeral tab was unreachable from the kiosk (owner report 2026-09-07).
// Every tab body is its own file, mounted behind `Loader.active`, and each one
// consumes the authoritative reader for its surface.
//
// ── DEFAULT TAB ───────────────────────────────────────────────────────────
// Albums, always — contract §2.2. A fresh mount, a NavRail entry and a
// programmatic navigation with no tab all land on Albums, and the persisted
// tab ORDER is deliberately not consulted for the default: it may put Tracks
// first, and three different defaults in shell, settings and view is the
// defect that rule exists to close. Back/Forward is the exception and the
// point: a restored entry keeps the tab that entry actually recorded, Tracks
// included — the past is not rewritten.
//
// ── HISTORY ───────────────────────────────────────────────────────────────
// There is no second stack. `KioskNavigation` writes this view's opaque state
// onto the ONE `QbzShell` history, records a tab destination through
// `recordKioskTab` BEFORE the tab changes (so the entry that is pushed carries
// the OUTGOING state) and restores it on the way back.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root

    color: "transparent"

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    /// Content inset. 16 keeps a card clear of the NavRail at 800px.
    property real pad: 16


    // =====================================================================
    // Tabs
    // =====================================================================
    readonly property var knownTabs: ["genres", "albums", "artists", "folders", "tracks"]

    /// The user's own tab order drives the STRIP. It deliberately does not
    /// drive the DEFAULT (see the header).
    readonly property var orderedTabs: {
        var stored
        try {
            stored = JSON.parse(QbzBridge.settingsJson).localTabOrder
        } catch (e) {
            stored = null
        }
        var out = []
        var i
        if (Array.isArray(stored)) {
            for (i = 0; i < stored.length; i++)
                if (root.knownTabs.indexOf(stored[i]) >= 0 && out.indexOf(stored[i]) < 0)
                    out.push(stored[i])
        }
        // Anything the stored order omits still has to be reachable.
        for (i = 0; i < root.knownTabs.length; i++)
            if (out.indexOf(root.knownTabs[i]) < 0)
                out.push(root.knownTabs[i])
        return out
    }

    readonly property bool ephemeralActive: QbzLocal.localEphemeralActive

    /// The strip's contents. The open session appends its own tab, exactly as
    /// it does on the desktop, and it is the only tab that can VANISH while
    /// the user is standing on it.
    readonly property var visibleTabs: root.ephemeralActive
        ? root.orderedTabs.concat(["ephemeral"]) : root.orderedTabs

    function tabLabel(id) {
        if (id === "genres") return root.t("Genres")
        if (id === "albums") return root.t("Albums")
        if (id === "artists") return root.t("Artists")
        if (id === "folders") return root.t("Folders")
        if (id === "tracks") return root.t("Tracks")
        // The open session names itself; Rust computes the label so this view
        // and the nav flyout cannot call the same thing two different things.
        return QbzLocal.localEphemeralLabel !== ""
            ? QbzLocal.localEphemeralLabel : root.t("Open")
    }

    /// ALBUMS. Not `orderedTabs[0]`.
    property string activeTab: "albums"

    /// Opaque per-tab state carried on the shared history entry.
    property string selectedArtist: ""
    property string selectedGenre: ""

    // =====================================================================
    // The media funnel
    // =====================================================================
    // One quality/format/source funnel follows the user across Albums,
    // Artists, Genres and Tracks; Favorites is an Albums/Genres-only predicate
    // that survives visits to the other tabs. Mirrors LocalLibraryView.qml
    // :323-383 — same seed, same writers, same split into the shared "common"
    // part — so the two shells cannot disagree about what is filtered.
    property var albumsFilter: root.parseFilter(QbzLocal.albumsFilter)
    readonly property var commonFilter: {
        var out = Object.assign({}, root.albumsFilter || ({}))
        delete out.favorite
        return out
    }
    readonly property bool favoriteOnly: root.albumsFilter.favorite === true
    /// The funnel the ACTIVE tab sees.
    readonly property var filter: (root.activeTab === "albums" || root.activeTab === "genres")
        ? root.albumsFilter : root.commonFilter
    /// The descriptor handed to the native readers (which know no favorites).
    readonly property string filterJson: JSON.stringify(root.commonFilter)
    readonly property int filterCount: {
        var n = 0
        for (var k in root.filter)
            if (root.filter[k])
                n++
        return n
    }
    function parseFilter(json) {
        if (!json || json === "")
            return ({})
        try {
            return JSON.parse(json) || ({})
        } catch (e) {
            return ({})
        }
    }
    /// The ONE writer (LocalLibraryView.qml:361-383). Persists through the
    /// desktop's setters and re-queries whatever the active tab reads.
    function setFilter(f) {
        var value = f || ({})
        var favorite = (root.activeTab === "albums" || root.activeTab === "genres")
            ? value.favorite === true : root.favoriteOnly
        var common = Object.assign({}, value)
        delete common.favorite
        var albumValue = Object.assign({}, common)
        if (favorite)
            albumValue.favorite = true
        root.albumsFilter = albumValue
        QbzLocal.setAlbumsFilterJson(Object.keys(albumValue).length === 0
            ? "" : JSON.stringify(albumValue))
        // Tracks owns a server-side descriptor; its setter resets the paged
        // query, so it is only called while Tracks is the visible consumer.
        // A later tab switch installs the shared state (loadActiveTab).
        if (root.activeTab === "tracks")
            QbzLocal.tracksSetFilterJson(JSON.stringify(common))
        else
            root.loadActiveTab()
    }
    function toggleFilter(key) {
        var f = Object.assign({}, root.filter)
        f[key] = !f[key]
        if (!f[key])
            delete f[key]
        root.setFilter(f)
    }
    function clearFilter() { root.setFilter({}) }
    // A later republish still wins (a media server connected or removed
    // prunes the saved funnel). Guarded on a real difference so writing the
    // property back does not bounce.
    Connections {
        target: QbzLocal
        function onAlbumsFilterChanged() {
            var next = root.parseFilter(QbzLocal.albumsFilter)
            if (JSON.stringify(next) !== JSON.stringify(root.albumsFilter))
                root.albumsFilter = next
        }
    }

    // The client-side predicates for the LEGACY documents, ported from
    // LocalLibraryView.qml (sourceBucket :795, applyFilter :810,
    // artistMatchesFilter :862). The native readers filter in Rust from the
    // same descriptor.
    function sourceBucket(word) {
        var source = (word || "local").toLowerCase()
        if (source === "" || source === "user")
            return "local"
        if (source === "qobuz_purchase" || source === "qobuz_download")
            return "offline"
        if (source === "navidrome" || source === "gonic"
                || source === "airsonic" || source === "astiga")
            return "subsonic"
        return source
    }
    function applyFilter(rows, selectedFilter) {
        var selected = selectedFilter || ({})
        if (Object.keys(selected).length === 0)
            return rows
        var qAny = selected.dsd || selected.hires || selected.cd || selected.lossy
        var fAny = selected.flac || selected.alac || selected.ape || selected.wav
            || selected.mp3 || selected.aac || selected.other
        var sAny = selected.local || selected.offline || selected.plex
            || selected.jellyfin || selected.subsonic
        var known = { "flac": 1, "alac": 1, "ape": 1, "wav": 1, "mp3": 1, "aac": 1 }
        var out = []
        for (var i = 0; i < rows.length; i++) {
            var r = rows[i]
            if (selected.favorite === true && r.isFavorite !== true)
                continue
            if (qAny || fAny || sAny) {
                // Test every active section against ONE physical copy: a
                // logical album can carry DSD on one copy and FLAC on
                // another, and DSD+FLAC must not manufacture a parent whose
                // filtered detail has zero tracks.
                var variants = r.mediaVariants || []
                if (variants.length === 0) {
                    var sources = r.sources && r.sources.length > 0
                        ? r.sources : [r.source || "local"]
                    variants = []
                    for (var si = 0; si < sources.length; si++)
                        variants.push({ "qualityTier": r.qualityTier || "",
                                        "format": r.format || "",
                                        "source": sources[si] })
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
                    var sok = !sAny || selected[root.sourceBucket(variant.source)] === true
                    if (qok && fok && sok) {
                        mediaMatches = true
                        break
                    }
                }
                if (!mediaMatches)
                    continue
            }
            out.push(r)
        }
        return out
    }
    function artistMatchesFilter(artist, selected) {
        if (!selected || Object.keys(selected).length === 0)
            return true
        var qAny = selected.dsd || selected.hires || selected.cd || selected.lossy
        var fAny = selected.flac || selected.alac || selected.ape || selected.wav
            || selected.mp3 || selected.aac || selected.other
        var sAny = selected.local || selected.offline || selected.plex
            || selected.jellyfin || selected.subsonic
        var known = { "flac": 1, "alac": 1, "ape": 1, "wav": 1, "mp3": 1, "aac": 1 }
        var i, value
        if (qAny) {
            var qualityOk = false
            for (i = 0; i < (artist.qualityTiers || []).length; i++) {
                value = (artist.qualityTiers[i] || "").toLowerCase()
                if ((selected.dsd && value === "dsd")
                        || (selected.hires && (value === "hires" || value === "max"))
                        || (selected.cd && value === "cd")
                        || (selected.lossy && (value === "mp3" || value === "lossy"))) {
                    qualityOk = true
                    break
                }
            }
            if (!qualityOk)
                return false
        }
        if (fAny) {
            var formatOk = false
            for (i = 0; i < (artist.formats || []).length; i++) {
                value = (artist.formats[i] || "").toLowerCase()
                if (selected[value] === true
                        || (selected.other === true && known[value] !== 1)) {
                    formatOk = true
                    break
                }
            }
            if (!formatOk)
                return false
        }
        if (sAny) {
            var sourceOk = false
            for (i = 0; i < (artist.sources || []).length; i++) {
                if (selected[root.sourceBucket(artist.sources[i])] === true) {
                    sourceOk = true
                    break
                }
            }
            if (!sourceOk)
                return false
        }
        return true
    }

    function tabAvailable(tab) {
        if (tab === "ephemeral")
            return root.ephemeralActive
        return root.knownTabs.indexOf(tab) >= 0
    }

    /// THE recording setter. Everything that represents a user destination —
    /// the strip, the router handshake, `open ephemeral` — goes through it.
    function activateTab(tab) {
        if (!tab || tab === root.activeTab || !root.tabAvailable(tab))
            return
        // BEFORE the assignment: the entry that gets pushed has to carry the
        // state of the tab being LEFT, which is what Back restores.
        nav.recordTab(tab)
        root.activeTab = tab
    }

    /// A tab change that is NOT a destination: the ephemeral fallback, and the
    /// restoration of a history entry. Neither may push an entry.
    function setTabSilently(tab) {
        if (!tab || tab === root.activeTab)
            return
        root.activeTab = tab
    }

    // The router's ONE external tab seam (NavFlyout, the kiosk NavRail through
    // `navigateToTab`, and `open ephemeral`). ContentRouter writes this
    // property; `sequence` makes re-selecting the same tab re-apply.
    property var tabNavigationRequest: ({})
    onTabNavigationRequestChanged: {
        if (root.tabNavigationRequest && root.tabNavigationRequest.tab)
            root.activateTab(root.tabNavigationRequest.tab)
    }

    function selectArtist(name) { root.selectedArtist = name || "" }
    function selectGenre(key) { root.selectedGenre = key || "" }

    /// A local album card opens the routed local album page. Both halves are
    /// required: `openAlbum` only LOADS the document — the old kiosk called it
    /// alone, so a card tap loaded an album nobody ever navigated to and read
    /// as a dead control.
    function openAlbum(id) {
        if (!id)
            return
        QbzLocal.openAlbum(id)
        QbzShell.navigateTo("localalbum")
    }

    /// LocalLibraryView.qml:700-707. Tracks owns a server-side descriptor:
    /// installing the shared funnel IS its load. Albums under "Favorites
    /// only" reads the legacy document (the native reader knows no
    /// favorites — LocalLibraryView.qml:341-344, :515).
    function loadActiveTab() {
        if (root.activeTab === "tracks")
            QbzLocal.tracksSetFilterJson(JSON.stringify(root.commonFilter))
        else if (root.activeTab === "albums" && root.favoriteOnly)
            QbzLocal.loadTab("albums-legacy")
        else
            QbzLocal.loadTab(root.activeTab)
    }

    onActiveTabChanged: {
        root.loadActiveTab()
        root.publishNav(root._navColumns, 0)
        root.artworkRefresh()
    }

    /// The open session is the one tab that can disappear under the user (the
    /// disc is ejected, the folder is closed). Falling back to Albums is the
    /// desktop lifecycle, and it is SILENT: closing a session is not a
    /// navigation, and Back must not be able to resurrect it.
    onEphemeralActiveChanged: {
        if (!root.ephemeralActive && root.activeTab === "ephemeral")
            root.setTabSilently("albums")
        root.publishNav(root._navColumns, root._navItems)
    }

    Component.onCompleted: {
        // KioskNavigation has already completed (children complete first), so
        // a restored entry has set `activeTab` by now and this is its load.
        root.loadActiveTab()
        root.publishNav(1, 0)
    }

    // =====================================================================
    // History — one stack, this view's opaque state on it
    // =====================================================================
    KioskNavigation {
        id: nav
        route: "local"
        snapshot: ({
            "activeTab": root.activeTab,
            "selectedArtist": root.selectedArtist,
            "selectedGenre": root.selectedGenre
        })
        onRestore: function (saved) {
            if (!saved)
                return
            var tab = typeof saved.activeTab === "string" ? saved.activeTab : root.activeTab
            // NO RESURRECTION: an entry recorded while a session was open must
            // not reopen a session that has since been closed.
            if (!root.tabAvailable(tab))
                tab = "albums"
            root.activeTab = tab
            root.selectedArtist = typeof saved.selectedArtist === "string"
                ? saved.selectedArtist : ""
            root.selectedGenre = typeof saved.selectedGenre === "string"
                ? saved.selectedGenre : ""
        }
    }

    // =====================================================================
    // Nav geometry (QbzKioskNav)
    // =====================================================================
    // The mounted tab body is the only thing that knows its own column count
    // and item count, so it publishes them through here. The leading `tabs`
    // entries are the strip.
    property int _navColumns: 1
    property int _navItems: 0
    function publishNav(columns, items) {
        root._navColumns = Math.max(1, columns || 1)
        root._navItems = Math.max(0, items || 0)
        // `visibleTabs` is a derived binding, and an early publish (the
        // ephemeral flag arriving during construction) can reach here before
        // it has been evaluated. A geometry publish is not worth a TypeError.
        var tabs = root.visibleTabs ? root.visibleTabs.length : 0
        if (tabs === 0)
            return
        QbzKioskNav.publishNav(tabs, root._navColumns, tabs + root._navItems, false)
    }

    // Enter on a tab entry drives the same switch a tap does. The
    // `index < tabs` test is what keeps this handler and the mounted body's
    // disjoint: both see the same pulse.
    Connections {
        target: QbzKioskNav
        function onActivateSeqChanged() {
            if (!QbzKioskNav.navActive || QbzKioskNav.zone !== "content")
                return
            if (QbzKioskNav.index < 0 || QbzKioskNav.index >= QbzKioskNav.tabs)
                return
            var id = root.visibleTabs[QbzKioskNav.index]
            if (id !== undefined)
                root.activateTab(id)
        }
    }

    // =====================================================================
    // Legacy documents
    // =====================================================================
    // Still authoritative for Folders and for the Genres browser (whose
    // `load_tab("genres")` arm deliberately routes to `load_albums_legacy`),
    // and still the automatic fallback for Albums/Artists/Tracks when a
    // catalog session fails. A tab body reads these ONLY when its native
    // reader is inactive.
    function parseDoc(json, fallback) {
        if (json === "")
            return fallback
        try {
            return JSON.parse(json)
        } catch (e) {
            console.warn("[qbz-qt] kiosk local: bad document — " + e)
            return fallback
        }
    }
    // The legacy album/artist documents are read THROUGH the funnel: Albums
    // and Genres see the favorites predicate, the Artists drill-down the
    // shared common part (LocalLibraryView.qml:916, :1021, :1213).
    readonly property var albums: ((root.activeTab === "albums" && (!QbzLocal.localAlbumsNativeActive || root.favoriteOnly)) || root.activeTab === "genres" || (root.activeTab === "artists" && !QbzLocal.localArtistsNativeActive))
        ? root.applyFilter(root.parseDoc(QbzLocal.localAlbumsJson, []),
                           root.activeTab === "artists" ? root.commonFilter : root.albumsFilter)
        : []
    readonly property var artists: {
        if (!(root.activeTab === "artists" && !QbzLocal.localArtistsNativeActive))
            return []
        var rows = root.parseDoc(QbzLocal.localArtistsJson, [])
        var out = []
        for (var i = 0; i < rows.length; i++)
            if (root.artistMatchesFilter(rows[i], root.commonFilter))
                out.push(rows[i])
        return out
    }
    readonly property var folders: (root.activeTab === "folders") ? root.parseDoc(QbzLocal.localFoldersJson, []) : []
    readonly property var tracks: (root.activeTab === "tracks" && !QbzLocal.localTracksNativeActive) ? root.parseDoc(QbzLocal.localTracksJson, []) : []
    readonly property var ephemeral: (root.activeTab === "ephemeral") ? root.parseDoc(QbzLocal.localEphemeralJson, null) : null

    readonly property bool trackArtwork: QbzLocal.localTrackArtwork

    // =====================================================================
    // Artwork window registry
    // =====================================================================
    // Covers are id-keyed: a surface reports the artKeys of the rows it has
    // MOUNTED, Rust answers one `localArtworkReady` per resolved key, and the
    // union of every live window plus one window of margin is what survives
    // eviction. It lives here rather than per tab because two cover surfaces
    // can be alive at once (the Artists list and its drill-down grid), and a
    // per-surface keep-set makes each one delete the other's covers.
    property var artMap: ({})
    property var _artInbox: ({})
    property var _windows: ({})
    property var _pending: ({})
    // `real`, not `int`: Date.now() is ~1.7e12.
    property real _lastReportMs: 0

    signal artworkRefresh()

    function artPathOf(key) { return root.artMap[key] || "" }
    function artWanted(key) { return (key || "") !== "" }

    // Arrivals stream in one at a time. Rebinding `artMap` per arrival is
    // quadratic in the window, so they are coalesced into one rebind per
    // frame — the covers still appear progressively at 16ms granularity.
    Timer {
        id: artFlush
        interval: 16
        repeat: false
        onTriggered: {
            var next = Object.assign({}, root.artMap, root._artInbox)
            root._artInbox = ({})
            // A rebind needs a NEW object reference: a same-ref assignment is
            // not a change in QML.
            root.artMap = next
        }
    }

    Connections {
        target: QbzLocal
        function onLocalArtworkReady(key, path) {
            root._artInbox[key] = path
            if (!artFlush.running)
                artFlush.start()
        }
    }

    function applyWindow(key, rows, first, last) {
        if (!rows || rows.length === 0) {
            delete root._windows[key]
            return
        }
        last = Math.min(last, rows.length - 1)
        first = Math.max(0, first)
        if (first > last) {
            delete root._windows[key]
            return
        }
        root._windows[key] = { "rows": rows, "first": first, "last": last }
    }

    /// A surface that unmounts, scrolls off or has nothing to show stops
    /// holding its covers.
    function releaseWindow(key) {
        var k = key || "default"
        if (root._windows[k] === undefined && root._pending[k] === undefined)
            return
        delete root._windows[k]
        delete root._pending[k]
        root.flushWindows()
    }

    /// Evict against the UNION of every live window, then request what is
    /// still missing. A key already resolved is never re-sent: `artMap` IS the
    /// resolved set and a re-request costs Rust a stat per key.
    function flushWindows() {
        var keep = ({})
        var k, w, rows, i, ak, span, lo, hi
        for (k in root._windows) {
            w = root._windows[k]
            rows = w.rows
            span = w.last - w.first + 1
            lo = Math.max(0, w.first - span)
            hi = Math.min(rows.length - 1, w.last + span)
            for (i = lo; i <= hi; i++) {
                ak = rows[i] ? rows[i].artKey : ""
                if (ak)
                    keep[ak] = true
            }
        }
        var map = root.artMap
        var changed = false
        for (k in map) {
            if (!keep[k]) {
                delete map[k]
                changed = true
            }
        }
        // Evict the not-yet-flushed arrivals too, or a cover that landed for a
        // row we have just scrolled past would be re-added by the next flush.
        for (k in root._artInbox)
            if (!keep[k])
                delete root._artInbox[k]
        if (changed)
            root.artMap = Object.assign({}, map)

        var missing = []
        var seen = ({})
        for (k in root._windows) {
            w = root._windows[k]
            rows = w.rows
            for (i = w.first; i <= w.last; i++) {
                ak = rows[i] ? rows[i].artKey : ""
                if (!ak || seen[ak])
                    continue
                seen[ak] = true
                if (map[ak] !== undefined || root._artInbox[ak] !== undefined)
                    continue
                missing.push(ak)
            }
        }
        if (missing.length > 0)
            QbzLocal.artworkWindow(JSON.stringify(missing))
    }

    // Rate-limited to one resolution pass per 180ms, but LEADING EDGE: the
    // limiter exists so a flick cannot fire a pass per pixel, and a view that
    // has just mounted is not flicking. Pending reports are keyed by SURFACE,
    // so two surfaces reporting inside the same window both survive.
    Timer {
        id: windowDebounce
        interval: 180
        repeat: false
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
            if (any)
                root.flushWindows()
        }
    }
    function queueWindowReport(rows, first, last, key) {
        var k = key || "default"
        if (!windowDebounce.running
                && Date.now() - root._lastReportMs >= windowDebounce.interval) {
            root._lastReportMs = Date.now()
            root.applyWindow(k, rows, first, last)
            root.flushWindows()
        } else {
            root._pending[k] = { "rows": rows, "first": first, "last": last }
            if (!windowDebounce.running)
                windowDebounce.start()
        }
    }

    // =====================================================================
    // Tab strip
    // =====================================================================
    // 64px — the contract's primary touch target — and the whole cell is the
    // hit area, not the text's bounding box. The strip scrolls horizontally so
    // a six-tab set with a long session name stays reachable at 800px.
    Rectangle {
        id: tabStrip

        anchors.left: root.left
        anchors.right: root.right
        anchors.top: root.top
        height: 64
        color: theme.surfaceMain

        Flickable {
            id: tabScroll
            anchors.fill: parent
            anchors.leftMargin: root.pad
            anchors.rightMargin: root.pad + stripActions.width + 8
            contentWidth: tabsRow.width
            contentHeight: height
            clip: true
            flickableDirection: Flickable.HorizontalFlick
            boundsBehavior: Flickable.StopAtBounds

            Row {
                id: tabsRow
                height: tabScroll.height
                spacing: 6

                // A FIXED tab list, never a data collection: at most the five
                // ordered tabs plus the open session.
                Repeater {
                    model: root.visibleTabs

                    delegate: Rectangle {
                        id: tab
                        required property string modelData
                        required property int index

                        readonly property bool active: root.activeTab === tab.modelData
                        readonly property bool navFocused: QbzKioskNav.navActive
                            && QbzKioskNav.zone === "content"
                            && QbzKioskNav.index === tab.index

                        width: Math.max(88, tabText.implicitWidth + 28)
                        height: tabsRow.height
                        color: tab.active
                            ? Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.10)
                            : "transparent"
                        radius: theme.radiusSm
                        border.width: tab.navFocused ? 2 : 0
                        border.color: theme.accent

                        Text {
                            id: tabText
                            anchors.centerIn: parent
                            width: Math.min(implicitWidth, 220)
                            text: root.tabLabel(tab.modelData)
                            color: tab.active ? theme.textPrimary : theme.textMuted
                            font.pixelSize: 16
                            font.weight: theme.weightSemibold
                            horizontalAlignment: Text.AlignHCenter
                            elide: Text.ElideRight
                            maximumLineCount: 1
                        }

                        Rectangle {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.leftMargin: 10
                            anchors.rightMargin: 10
                            anchors.bottom: parent.bottom
                            anchors.bottomMargin: 6
                            height: 3
                            radius: 2
                            color: tab.active ? theme.accent : "transparent"
                        }

                        MouseArea {
                            anchors.fill: parent
                            onClicked: root.activateTab(tab.modelData)
                        }
                    }
                }
            }
        }

        // The strip's right cluster: Open (starts an ephemeral session) and
        // the funnel. Two 64px touch targets, always mounted — the funnel
        // button is the only place the kiosk shows that a filter is on.
        Row {
            id: stripActions
            anchors.right: parent.right
            anchors.rightMargin: root.pad
            anchors.verticalCenter: parent.verticalCenter
            height: 64
            spacing: 4

            Rectangle {
                id: openBtn
                width: 64
                height: 64
                radius: theme.radiusSm
                color: openArea.containsMouse ? theme.surfaceHover : "transparent"
                Accessible.role: Accessible.Button
                Accessible.name: root.t("Open")
                QbzIcon {
                    name: "folder-open"
                    width: 26
                    height: 26
                    anchors.centerIn: parent
                    tintName: "textPrimary"
                }
                MouseArea {
                    id: openArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: openMenu.openBelowRight(openBtn)
                }
            }

            Rectangle {
                id: filterBtn
                width: 64
                height: 64
                radius: theme.radiusSm
                color: filterArea.containsMouse ? theme.surfaceHover : "transparent"
                border.width: root.filterCount > 0 ? 1 : 0
                border.color: theme.accent
                Accessible.role: Accessible.Button
                Accessible.name: root.t("Quality, format and source filters")
                QbzIcon {
                    name: "list-filter"
                    width: 26
                    height: 26
                    anchors.centerIn: parent
                    tintName: root.filterCount > 0 ? "accent" : "textPrimary"
                }
                Rectangle {
                    visible: root.filterCount > 0
                    x: parent.width - width - 8
                    y: 8
                    width: 20
                    height: 20
                    radius: 10
                    color: theme.accent
                    Text {
                        anchors.centerIn: parent
                        text: root.filterCount
                        color: theme.accentGlyphColor
                        font.pixelSize: 12
                        font.weight: theme.weightBold
                    }
                }
                MouseArea {
                    id: filterArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: root.openFilterSheet()
                }
            }
        }

        Rectangle {
            anchors.left: tabStrip.left
            anchors.right: tabStrip.right
            y: tabStrip.height - 1
            height: 1
            color: theme.borderSubtle
        }
    }

    // The Open menu: LocalChrome.qml:262-282's entries and actions, verbatim
    // (the audio-CD row is dropped on Windows for the same reason it is
    // there). The session opens globally and the router lands on the
    // Ephemeral tab (ContentRouter.qml:606-611).
    CardMenu {
        id: openMenu
        kioskHost: true
        menuWidth: 260
        entries: [
            { "label": root.t("Open folder…"), "icon": "folder-open", "action": "folder" }
        ].concat(QbzShell.isWindows ? [] : [
            { "label": root.t("Open audio CD"), "icon": "disc", "action": "cd" }
        ]).concat([
            { "label": root.t("Open SACD image…"), "icon": "disc-3", "action": "sacd" }
        ])
        onPicked: function (action) {
            if (action === "folder")
                QbzLocal.ephemeralOpen()
            else if (action === "cd")
                QbzLocal.ephemeralOpenCd()
            else if (action === "sacd")
                QbzLocal.ephemeralOpenSacd()
        }
    }

    /// The funnel sheet: LocalFilterPopup.qml's sections and chips at touch
    /// density, centred in the window and clamped inside it.
    function openFilterSheet() {
        var win = filterSheet.parent
        if (win) {
            filterSheet.x = Math.max(8, (win.width - filterSheet.width) / 2)
            filterSheet.y = Math.max(8, Math.min(88, win.height - filterSheet.height - 8))
        }
        filterSheet.open()
    }

    component FunnelChip: Rectangle {
        id: chip
        property string label: ""
        property bool active: false
        signal toggled()
        height: 44
        width: chipLabel.implicitWidth + 32
        radius: theme.radiusSm
        border.width: 1
        border.color: chip.active ? theme.accent : theme.borderSubtle
        color: chip.active ? theme.accent
             : chipArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
        Text {
            id: chipLabel
            anchors.centerIn: parent
            text: chip.label
            color: chip.active ? theme.accentGlyphColor : theme.textSecondary
            font.pixelSize: 15
            font.weight: chip.active ? theme.weightSemibold : theme.weightRegular
        }
        MouseArea {
            id: chipArea
            anchors.fill: parent
            hoverEnabled: true
            onClicked: chip.toggled()
        }
    }

    component FunnelHeading: Text {
        color: theme.textMuted
        font.pixelSize: 13
        font.weight: theme.weightSemibold
    }

    Popup {
        id: filterSheet
        parent: Overlay.overlay
        width: Math.min(560, (parent ? parent.width : 800) - 16)
        height: Math.min(sheetColumn.height + 32, (parent ? parent.height : 480) - 16)
        padding: 16
        closePolicy: Popup.CloseOnPressOutside | Popup.CloseOnEscape
        background: Rectangle {
            color: theme.surfaceCard
            radius: 10
            border.width: 1
            border.color: theme.borderSubtle
        }
        contentItem: Flickable {
            contentWidth: width
            contentHeight: sheetColumn.height
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            Column {
                id: sheetColumn
                width: parent.width
                spacing: 10

                Row {
                    width: parent.width
                    height: 44
                    Text {
                        width: parent.width - clearBtn.width
                        height: parent.height
                        text: root.t("Filter")
                        color: theme.textPrimary
                        font.pixelSize: 18
                        font.weight: theme.weightSemibold
                        verticalAlignment: Text.AlignVCenter
                    }
                    Rectangle {
                        id: clearBtn
                        visible: root.filterCount > 0
                        width: visible ? clearText.implicitWidth + 32 : 0
                        height: 44
                        radius: theme.radiusSm
                        color: clearArea.containsMouse ? theme.surfaceHover : "transparent"
                        Text {
                            id: clearText
                            anchors.centerIn: parent
                            text: root.t("Clear")
                            color: theme.accent
                            font.pixelSize: 15
                            font.weight: theme.weightMedium
                        }
                        MouseArea {
                            id: clearArea
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: root.clearFilter()
                        }
                    }
                }

                FunnelHeading {
                    visible: root.activeTab === "albums" || root.activeTab === "genres"
                    text: root.t("Favorites")
                }
                Flow {
                    visible: root.activeTab === "albums" || root.activeTab === "genres"
                    width: parent.width
                    spacing: 8
                    FunnelChip {
                        label: root.t("Favorites only")
                        active: root.filter.favorite === true
                        onToggled: root.toggleFilter("favorite")
                    }
                }

                FunnelHeading { text: root.t("Quality") }
                Flow {
                    width: parent.width
                    spacing: 8
                    FunnelChip { label: "DSD"; active: root.filter.dsd === true; onToggled: root.toggleFilter("dsd") }
                    FunnelChip { label: root.t("Hi-Res"); active: root.filter.hires === true; onToggled: root.toggleFilter("hires") }
                    FunnelChip { label: root.t("CD"); active: root.filter.cd === true; onToggled: root.toggleFilter("cd") }
                    FunnelChip { label: root.t("Lossy"); active: root.filter.lossy === true; onToggled: root.toggleFilter("lossy") }
                }

                FunnelHeading { text: root.t("Format") }
                Flow {
                    width: parent.width
                    spacing: 8
                    FunnelChip { label: "FLAC"; active: root.filter.flac === true; onToggled: root.toggleFilter("flac") }
                    FunnelChip { label: "ALAC"; active: root.filter.alac === true; onToggled: root.toggleFilter("alac") }
                    FunnelChip { label: "APE"; active: root.filter.ape === true; onToggled: root.toggleFilter("ape") }
                    FunnelChip { label: "WAV"; active: root.filter.wav === true; onToggled: root.toggleFilter("wav") }
                    FunnelChip { label: "MP3"; active: root.filter.mp3 === true; onToggled: root.toggleFilter("mp3") }
                    FunnelChip { label: "AAC"; active: root.filter.aac === true; onToggled: root.toggleFilter("aac") }
                    FunnelChip { label: root.t("Other"); active: root.filter.other === true; onToggled: root.toggleFilter("other") }
                }

                FunnelHeading { text: root.t("Source") }
                Flow {
                    width: parent.width
                    spacing: 8
                    FunnelChip { label: root.t("Local"); active: root.filter.local === true; onToggled: root.toggleFilter("local") }
                    FunnelChip { label: root.t("Offline cache"); active: root.filter.offline === true; onToggled: root.toggleFilter("offline") }
                    FunnelChip { visible: QbzLocal.plexAvailable; label: "Plex"; active: root.filter.plex === true; onToggled: root.toggleFilter("plex") }
                    FunnelChip { visible: QbzLocal.mediaHasJellyfin; label: "Jellyfin"; active: root.filter.jellyfin === true; onToggled: root.toggleFilter("jellyfin") }
                    FunnelChip { visible: QbzLocal.mediaHasSubsonic; label: "Subsonic"; active: root.filter.subsonic === true; onToggled: root.toggleFilter("subsonic") }
                }
            }
        }
    }

    // =====================================================================
    // Content — ONE tab exists at a time
    // =====================================================================
    // `Loader.active`, never `visible: false`: a hidden item still costs what
    // it mounted, and the Tracks body is the one that makes that matter. This
    // is also what keeps the Tracks model out of an Albums visit — nothing
    // subscribes to its page misses and nothing queries it until its own tab
    // is the mounted one.
    Item {
        id: content

        anchors.left: root.left
        anchors.right: root.right
        anchors.top: tabStrip.bottom
        anchors.bottom: root.bottom
        clip: true

        /// The open session renders with no indexed library at all — it is
        /// content from OUTSIDE the index, so gating it on `localAvailable`
        /// would hide the pane from exactly the people the feature is for.
        readonly property bool ephemeralShowing:
            root.activeTab === "ephemeral" && root.ephemeralActive

        KioskEmptyState {
            anchors.fill: parent
            visible: !QbzLocal.localAvailable && !content.ephemeralShowing
            // An EXISTING msgid, present in all eight catalogues — this lane
            // may not add translations, so no new msgid is introduced.
            text: root.t("No folders yet. Add a folder to build your local library.")
        }

        Loader {
            anchors.fill: parent
            active: QbzLocal.localAvailable && root.activeTab === "genres"
            sourceComponent: KioskLocalGenresTab { view: root }
        }
        Loader {
            anchors.fill: parent
            active: QbzLocal.localAvailable && root.activeTab === "albums"
            sourceComponent: KioskLocalAlbumsTab { view: root }
        }
        Loader {
            anchors.fill: parent
            active: QbzLocal.localAvailable && root.activeTab === "artists"
            sourceComponent: KioskLocalArtistsTab { view: root }
        }
        Loader {
            anchors.fill: parent
            active: QbzLocal.localAvailable && root.activeTab === "folders"
            sourceComponent: KioskLocalFoldersTab { view: root }
        }
        Loader {
            anchors.fill: parent
            active: QbzLocal.localAvailable && root.activeTab === "tracks"
            sourceComponent: KioskLocalTracksTab { view: root }
        }
        Loader {
            anchors.fill: parent
            active: content.ephemeralShowing
            sourceComponent: KioskLocalEphemeralTab { view: root }
        }

        // Retry. The kiosk primitive draws an error message and no action, and
        // a route that can only say "it failed" is a route the user has to
        // leave. One affordance for every retryable tab, mounted over the
        // message, 64px tall.
        readonly property string activeError: root.activeTab === "albums"
                ? QbzLocal.localAlbumsError
            : root.activeTab === "artists" && QbzLocal.localArtistsNativeActive ? QbzLocal.localArtistsNativeError
            : root.activeTab === "tracks" && QbzLocal.localTracksNativeActive ? QbzLocal.localTracksNativeError
            : root.activeTab === "genres" ? QbzLocal.localAlbumsError
            : ""

        Rectangle {
            id: retry
            visible: content.activeError !== ""
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.verticalCenter: parent.verticalCenter
            anchors.verticalCenterOffset: 56
            width: Math.max(140, retryText.implicitWidth + 44)
            height: 64
            radius: theme.radiusSm
            color: theme.surfaceElevated
            border.width: 1
            border.color: theme.borderSubtle

            Text {
                id: retryText
                anchors.centerIn: parent
                text: root.t("Retry")
                color: theme.textPrimary
                font.pixelSize: 16
                font.weight: theme.weightSemibold
            }

            MouseArea {
                anchors.fill: parent
                onClicked: root.loadActiveTab()
            }
        }
    }
}
