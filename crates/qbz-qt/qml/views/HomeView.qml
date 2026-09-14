// Discover > Home — QML port of crates/qbz-ui/ui/discover/HomeView.slint
// (+ Carousel / AlbumCard / SlimCarousel / SlimCard / ArtistCarousel /
// PlaylistCarousel / HomeSkeleton / OfflinePlaceholder).
//
// Data: QbzHome.homeSectionsJson (one JSON document — see bridge.rs),
// published by src/home_qt.rs; artwork file:// paths resolve through the
// qbz-cache image cache. Section kinds: "album" | "playlist" | "slim" |
// "slimTracks" | "artists" | "pinned" | "mixes" | "radio" | "spotlight" |
// "recentPlaceholder". Rail ORDER + VISIBILITY follow the persisted Discover
// prefs (phase 11, discover_prefs.db).
//
// THREE of those kinds are ORDERING SLOTS with no rows in the document —
// "pinned", "radio", "spotlight". Their rows travel on their own bridge
// properties (pinnedJson / radioStationsJson / spotlightJson) so a per-click
// pin, or the Spotlight's late /artist/page landing, does not republish all
// three tab documents and tear down every rail's delegates. See
// src/foryou_qt.rs.
//
// POC-NOTEs:
// - The genre filter (shared GenreFilterPopup, context "discover") and the
//  section-configurator gear (DiscoverConfigModal) are WIRED: the genre
//  selection feeds get_discover_index and persists to genre_filter.json; the
//  configurator persists to discover_prefs.db and re-renders the tabs from
//  the cached section data. The gear is live on ALL FOUR tabs: on
//  Recommendations the modal shows that tab's own arm instead of a section
//  list (cache window + "Refresh now"), matching the reference.
// - Editor's Picks / For You mount the same rails, ordered by each tab's
//  discover prefs (phase 13). Recommendations is the external-reco engine
//  (src/recommendations_qt.rs), LAZY-loaded the first time the tab becomes
//  visible, and the tab itself is gated on the persisted showRecommendations
//  pref exactly as in Slint.
// - "View all" is LIVE. WHICH rails carry the link is decided HERE, per
//  TAB, by `viewAllKind()` — not in Rust: `home_qt::assemble()` clones ONE
//  candidate list into all three tab documents, and Slint's For You arm for
//  `recentlyPlayedAlbums` has NO link (ForYouView.slint:117-126) while
//  Home's does (HomeView.slint:411). Stamping the decision on the shared
//  candidate would leak the link onto For You. The four destinations:
//    endpoint != ""       -> DiscoverBrowse for that /discover/<module>
//    qobuzPlaylists       -> PlaylistBrowse   (Home + Editor's Picks only)
//    recentlyPlayedAlbums -> Recently Played  (Home only, non-placeholder)
//    mostPlayedAlbums     -> Most Played      (Home + For You)
// - Card clicks / hover actions (play / favorite / more / pin) are live;
//  context menus follow each card's own seam constants.
// - The offline mount mirrors AppShell's ADR-010 seam: OfflineState.offline
//  -> the OfflinePlaceholder replica (exact msgids).

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../cards"
import "../controls"
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

    // ---- mount stopwatch (same category as ContentRouter's) --------------
    // OFF unless QT_LOGGING_RULES="qbz.nav.timing.info=true". The sections
    // count below is the DOCUMENT size, not the number of rails instantiated:
    // the vertical ListView only builds its viewport and cache buffer.
    LoggingCategory {
        id: homeTiming
        name: "qbz.nav.timing"
        defaultLogLevel: LoggingCategory.Warning
    }
    // A property initialiser runs during creation, so this is stamped near the
    // start of the mount rather than at the end of it like Component.onCompleted.
    property double _mountT0: Date.now()
    function _stampViewport(id, count) {
        console.info(homeTiming, "[hometiming] viewport " + id + " ("
                     + count + " document sections) done at +"
                     + (Date.now() - root._mountT0) + "ms")
    }

    // Reparsed whenever Rust republishes the JSON documents (one per
    // Discover tab — phase 13).
    readonly property var sections: JSON.parse(QbzHome.homeSectionsJson)
    readonly property var editorSections: JSON.parse(QbzHome.editorSectionsJson)
    readonly property var forYouSections: JSON.parse(QbzHome.forYouSectionsJson)
    // Track edges replace only these three descriptors. Keeping them outside
    // the tab documents preserves every unrelated rail's delegate model and
    // horizontal scroll position.
    readonly property var recentRails: {
        try {
            return JSON.parse(QbzHome.recentRailsJson)
        } catch (e) {
            return ({})
        }
    }
    // Recommendations (the 4th tab). Lazy: the document stays "[]" until the
    // tab is first shown and src/recommendations_qt.rs publishes into it.
    readonly property var recoSections: JSON.parse(QbzHome.recoSectionsJson)
    // The Pinned rail's ROWS, on their own property. The three documents
    // above carry the "pinned" section as an EMPTY ordering slot only (see
    // home_qt::publish_pinned): a pin is a per-click mutation, and folding
    // its rows into them meant every click re-parsed all three documents and
    // rebuilt every rail's delegates — with each rail's horizontal scroll
    // snapped back to 0. Rebinding THIS re-creates the pinned delegates and
    // nothing else. 1:1 with the Slint split (descriptor for the position,
    // the PinnedState global for the rows).
    readonly property var pinnedItems: {
        try {
            return JSON.parse(QbzHome.pinnedJson)
        } catch (e) {
            return []
        }
    }
    // For You's two other out-of-document rails — same split, same reason as
    // `pinnedItems` (src/foryou_qt.rs): the tab documents carry an EMPTY
    // ordering slot for each and the rows arrive on their own property, so
    // the Spotlight's late /artist/page landing does not re-create every
    // rail's delegates on all three tabs.
    readonly property var radioStations: {
        try {
            return JSON.parse(QbzHome.radioStationsJson)
        } catch (e) {
            return []
        }
    }
    readonly property var spotlight: {
        try {
            return JSON.parse(QbzHome.spotlightJson)
        } catch (e) {
            return ({})
        }
    }
    // Recommendations' two Weekly rails, same split and same reason as the
    // three above: they come from ListenBrainz about a second after the rest
    // of that tab has painted, and folding them into the document made their
    // arrival republish it — which hands every rail a new `model:` array and
    // tears down all nine to deliver two. That was the "it loads, then it
    // flashes and loads again" report. Keyed by section id
    // ("weeklyExploration" / "weeklyJams"); see src/recommendations_qt.rs.
    readonly property var recoWeekly: {
        try {
            return JSON.parse(QbzHome.recoWeeklyJson)
        } catch (e) {
            return ({})
        }
    }
    /// The rows for an out-of-document Weekly slot, or [] before they land.
    function weeklyRows(id) {
        var r = root.recoWeekly[id]
        return r ? r : []
    }
    /// Is this descriptor one of the two slots whose rows live outside the
    /// document? Ids are unique per tab, so this cannot collide with the
    /// other three tabs' sections.
    function isWeeklySlot(id) {
        return id === "weeklyExploration" || id === "weeklyJams"
    }
    // Per-URL cover patches for the two out-of-document For You rails. The
    // documents are NOT republished for artwork (src/foryou_qt.rs), because a
    // republish hands `model:` a new JS array and `QQuickItemView::setModel()`
    // resets the rail's scroll offset AND tears down the QQmlDelegateModel —
    // the "a cover batch landing mid-drag snaps the rail back" report, and this
    // build's only crash signature. So this map is what fills the tiles in: the
    // `coverMap` pattern AlbumView / ArtistView / QueuePanel already use, on a
    // signal only this view listens to.
    //
    // The map is VIEW-LOCAL and this view is NOT durable: AppShell binds
    // `viewLoader.source` to QbzShell.currentView, so opening an album destroys
    // HomeView and the map with it, while the rails' documents are published
    // once per session. That is what `refreshForyouArt()` below is for — on
    // every mount Rust re-hands whatever it has already resolved. Without it the
    // Radio rail and the Spotlight came back blank for the rest of the session
    // and `anyForYouArtPending` never went false again, pinning the pulse Timer.
    property var forYouArt: ({})
    Connections {
        target: QbzHome
        // ONE emit carries the WHOLE landed batch as a `{artUrl: path}` object
        // (src/foryou_qt.rs): merge it, then rebind ONCE. `forYouArt` is a
        // `var`, so only a new object REFERENCE notifies its dependents — an
        // emit per url paid a copy of the growing map plus a sweep over every
        // forYouArtOf(...) binding in both rails, per cover.
        function onForyouArtReady(patchJson) {
            var patch
            try {
                patch = JSON.parse(patchJson)
            } catch (e) {
                return
            }
            var m = root.forYouArt
            var changed = false
            for (var url in patch) {
                if (m[url] !== patch[url]) {
                    m[url] = patch[url]
                    changed = true
                }
            }
            if (changed)
                root.forYouArt = Object.assign({}, m)   // rebind needs a NEW ref
        }
    }
    // Late covers for the three targeted recent rails. Like the For You and
    // Recommendations maps, this patches tiles by URL instead of republishing
    // a section document after an image download.
    property var recentArt: ({})
    Connections {
        target: QbzHome
        function onRecentArtReady(patchJson) {
            var patch
            try {
                patch = JSON.parse(patchJson)
            } catch (e) {
                return
            }
            var m = root.recentArt
            var changed = false
            for (var url in patch) {
                if (m[url] !== patch[url]) {
                    m[url] = patch[url]
                    changed = true
                }
            }
            if (changed)
                root.recentArt = Object.assign({}, m)
        }
    }
    // Re-hand the resolved covers to this (possibly brand-new) instance. Cheap
    // and idempotent: it re-reads the memoized cache paths off the Rust stores,
    // never downloads, never republishes a document, and emits nothing at all
    // when nothing is resolved yet — so the first, cold mount is a no-op and the
    // in-flight download's own emit still fills the map.
    Component.onCompleted: {
        QbzHome.refreshForyouArt()
        QbzHome.refreshRecoArt()
        QbzHome.refreshRecentArt()
    }
    // Recommendations' rails used to get their covers by REPUBLISHING
    // `recoSectionsJson` once per landing batch, which hands `model:` a new JS
    // array — `QQuickItemView::setModel()` then resets the rail's scroll and
    // tears down its QQmlDelegateModel, so every batch rebuilt all nine rails'
    // delegates (the same defect the For You rails were moved off, :161). Rust
    // now emits a url-keyed patch instead and leaves the document alone.
    property var recoArt: ({})
    Connections {
        target: QbzHome
        function onRecoArtReady(patchJson) {
            var patch
            try {
                patch = JSON.parse(patchJson)
            } catch (e) {
                return
            }
            var m = root.recoArt
            var changed = false
            for (var url in patch) {
                if (m[url] !== patch[url]) {
                    m[url] = patch[url]
                    changed = true
                }
            }
            if (changed)
                root.recoArt = Object.assign({}, m)   // rebind needs a NEW ref
        }
    }

    /// Cover for a card that came out of a SECTIONS document: the patch map
    /// first, then whatever the document itself carried.
    ///
    /// The fallback is what makes this safe to put on rails that render the
    /// other tabs too. `recoArt` only ever holds Recommendations urls, so for a
    /// Home / Editor's Picks / For You card the lookup misses and the result is
    /// `item.artPath` — byte for byte what the binding read before. No consumer
    /// of a baked `artPath` can lose its cover to this change.
    function sectionArtOf(item) {
        return (item && item.artUrl && root.recentArt[item.artUrl])
            || (item && item.artUrl && root.recoArt[item.artUrl])
            || (item ? (item.artPath || "") : "")
    }

    // --- Qobuz Playlists: category filter ---------------------------------
    // Client-side over the 40 cached cards; the tags ride in the SAME
    // /discover/index response, so nothing here costs a request. The selection
    // itself lives in Rust (`home_qt`) because this view is destroyed on every
    // navigation — a property here would forget it the moment the user opened
    // an album and came back, where the reference survives (its selection lives
    // in TAB_SECTIONS and its comment says "survives a tab switch").
    readonly property var playlistTagDoc: {
        try {
            return JSON.parse(QbzHome.playlistTagsJson)
        } catch (e) {
            return {}
        }
    }
    readonly property var playlistTags: root.playlistTagDoc.tags || []
    readonly property var playlistTagSel: root.playlistTagDoc.selected || []
    /// The selected tags' display NAMES, for the applied-filters tooltip.
    readonly property var playlistTagNames: {
        var out = []
        var sel = root.playlistTagSel
        var all = root.playlistTags
        for (var i = 0; i < all.length; i++)
            if (sel.indexOf(all[i].slug) >= 0)
                out.push(all[i].name)
        return out
    }
    /// UNION, and an empty selection passes everything (home.rs:79-92).
    function playlistPasses(card) {
        if (root.playlistTagSel.length === 0)
            return true
        var t = (card && card.tags) || []
        for (var i = 0; i < t.length; i++)
            if (root.playlistTagSel.indexOf(t[i]) >= 0)
                return true
        return false
    }
    function filterPlaylists(items) {
        var out = []
        var src = items || []
        for (var i = 0; i < src.length; i++)
            if (root.playlistPasses(src[i]))
                out.push(src[i])
        return out
    }
    // The popup is declared last (z-order); a QML id is document-scoped, so a
    // rail delegate cannot name it. Same bridge-function pattern the genre
    // popup already uses from the toolbar.
    function togglePlaylistTagPopup(btn) {
        plTagPopup.toggle(btn)
    }

    /// The patch map first, then whatever the document happened to carry (a
    /// full Home reload publishes the paths it already has).
    function forYouArtOf(item) {
        return (item && item.artUrl && root.forYouArt[item.artUrl])
            || (item ? (item.artPath || "") : "")
    }
    /// Same predicate as anyItemArtPending, plus the patch map — the rails no
    /// longer get their `artPath` back through a republish, so "artUrl set and
    /// artPath empty" is now true forever and would pin the pulse Timer on.
    function anyForYouArtPending(items) {
        for (var i = 0; i < items.length; i++) {
            var it = items[i]
            if ((it.artUrl || "") !== "" && root.forYouArtOf(it) === "")
                return true
        }
        return false
    }

    property string activeTab: "home"
    // Recommendations remains lazy. The old per-tab Loader called this when
    // its body was constructed; with one virtualized ListView, selecting the
    // tab is the equivalent existence boundary.
    onActiveTabChanged: {
        if (root.activeTab === "recommendations")
            QbzHome.loadRecommendations()
    }

    // Slint gates the 4th tab on the persisted `show_recommendations` pref
    // (discover_prefs.db). The port publishes it inside the settings document
    // (settings_qt SettingsDoc.showRecommendations) — read it straight off the
    // bridge singleton, the NavFlyout precedent.
    readonly property bool showRecommendations: {
        try {
            return JSON.parse(QbzBridge.settingsJson).showRecommendations !== false
        } catch (e) {
            return true
        }
    }
    // Never leave the user parked on a tab that just disappeared.
    onShowRecommendationsChanged: {
        if (!showRecommendations && root.activeTab === "recommendations")
            root.activeTab = "home"
    }

    // --- shared genre filter, "discover" context -------------------------
    // The badge lives in controls/BrowseGenreButton.qml now (the browse
    // pages draw the same control). It reads QbzBridge.genreFilterJson
    // STRAIGHT off the bridge singleton, never off genrePopup: the popup is
    // declared LAST (z-order) and a creation-time binding that dereferences
    // a not-yet-created id registers NO dependency, so it would never
    // re-evaluate. The popup instance is only touched from click handlers,
    // which run long after creation.

    // --- skeleton pulse ---------------------------------------------------
    // ONE 900ms Timer drives EVERY placeholder in this view (QbzSkeleton's
    // preferred drive mode). GATING RULE: freeze on NOT VISIBLE — the view
    // hidden, or the window minimized/hidden. NEVER on lost focus (a tiling
    // desktop keeps windows visible and unfocused).
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    // Runs only while something can actually be shimmering: the first fetch
    // (section skeletons), a card still waiting for its cover, or the grace
    // window in which landed covers are still decoding (artHold).
    readonly property bool skelNeeded: QbzHome.homeLoading || QbzHome.recoLoading
        || root.artPending || root.artHold
    // The sections document the active tab is rendering.
    readonly property var activeSections:
        root.activeTab === "editorPicks" ? root.editorSections
        : root.activeTab === "forYou" ? root.forYouSections
        : root.activeTab === "recommendations" ? root.recoSections
        : root.sections
    /// Replace only a recent-history ordering slot with its live local-store
    /// snapshot. Every other descriptor keeps the original object identity.
    function effectiveSection(section) {
        if (!section)
            return section
        return root.recentRails[section.id] || section
    }
    /// Ordering slots stay in the document even before their out-of-document
    /// rows arrive. Keep the descriptor stable, but do not instantiate its
    /// heavy rail until it has something to show.
    function sectionIsVisible(section, tab) {
        if (!section)
            return false
        return section.kind === "pinned" ? root.pinnedItems.length > 0
            : section.kind === "radio" ? root.radioStations.length > 0
            : section.kind === "spotlight" ? root.spotlight.visible === true
            : root.isWeeklySlot(section.id) ? root.weeklyRows(section.id).length > 0
            : section.kind === "recentPlaceholder" && tab === "forYou" ? false
            : true
    }
    /// The vertical ListView uses zero spacing so a hidden ordering slot leaves
    /// no phantom gap. A visible delegate carries the old 40px Column spacing
    /// only when a later visible section exists.
    function hasVisibleSectionAfter(index) {
        var model = root.activeSections
        for (var i = index + 1; i < model.length; i++) {
            if (root.sectionIsVisible(root.effectiveSection(model[i]), root.activeTab))
                return true
        }
        return false
    }
    readonly property string activeHeaderKind:
        root.activeTab === "home" && QbzHome.homeError !== "" ? "error"
        : ((root.activeTab === "recommendations" ? QbzHome.recoLoading : QbzHome.homeLoading)
           && root.activeSections.length === 0) ? "skeleton"
        : (root.activeTab === "recommendations" && !QbzHome.recoLoading
           && root.activeSections.length === 0) ? "empty"
        : ""
    // Cheap "some card in the mounted rails has artUrl but no artPath yet"
    // probe — recomputed only when a sections document is republished. The
    // pinned rows are probed too: they no longer travel inside the sections
    // documents, and without them a freshly pinned card's placeholder would
    // sit frozen (the pulse Timer runs only while something is pending).
    readonly property bool artPending: root.anyArtPending(root.activeSections)
        || root.anyItemArtPending(root.pinnedItems)
        // The two out-of-document For You rails are probed for the same
        // reason the pinned rows are: without them a Spotlight or Radio tile
        // waiting for its cover would sit on a FROZEN placeholder, because
        // the pulse Timer runs only while something is pending.
        //
        // They use `anyForYouArtPending`, which consults the per-URL patch map
        // as well: their `artPath` never comes back through a republish any
        // more, so the plain `artPath === ""` probe would be true forever and
        // would pin the pulse Timer on. The third term is NEW coverage — the
        // Spotlight DOCUMENT itself (the 140px hero portrait) was never probed.
        || root.anyForYouArtPending(root.radioStations)
        || root.anyForYouArtPending(root.spotlight.albums || [])
        || root.anyForYouArtPending(root.spotlight.visible === true ? [root.spotlight] : [])
    function anyArtPending(model) {
        for (var s = 0; s < model.length; s++) {
            var section = root.effectiveSection(model[s])
            if (root.anyItemArtPending(section.items || []))
                return true
        }
        return false
    }
    // Consults the patch map, NOT a bare `artPath === ""`. A Recommendations
    // cover that arrives as a patch never gets its `artPath` back through a
    // republish any more, so the bare predicate would stay true forever and
    // pin the 900ms skeleton pulse Timer on — the exact trap the For You
    // rails hit (see anyForYouArtPending). Unchanged for every other tab,
    // where `sectionArtOf` just returns `artPath`.
    function anyItemArtPending(items) {
        for (var i = 0; i < items.length; i++) {
            if ((items[i].artUrl || "") !== "" && root.sectionArtOf(items[i]) === "")
                return true
        }
        return false
    }
    // The pulse must OUTLIVE `artPending`. That flag drops when the last
    // PATH lands, but every one of those cards still has a decode and a
    // canvas raster ahead of it and its placeholder is still up (see
    // QbzSkeleton's handover) — without the grace the tiles would freeze
    // mid-shimmer for the rest of the wait.
    property bool artHold: false
    Timer { id: artHoldOff; interval: 1500; onTriggered: root.artHold = false }
    onArtPendingChanged: { root.artHold = true; artHoldOff.restart() }
    Timer {
        interval: 900
        repeat: true
        running: root.visible && root.windowShowing && root.skelNeeded
        onTriggered: root.skelPhase = !root.skelPhase
    }

    // ============================ "View all" ==============================

    /// Which full-list page a rail's "View all" opens on THIS tab, or "" for
    /// no link. Read the header note for why the decision lives here.
    ///   "discover"  -> DiscoverBrowse for `section.endpoint`
    ///   "playlists" -> PlaylistBrowse
    ///   "recent"    -> Recently Played Albums
    ///   "mostplayed"-> Most Played Albums
    function viewAllKind(s, tab) {
        if (!s)
            return ""
        // Generic album carousels + both mostStreamed variants: the section
        // carries the /discover endpoint the page pages through.
        if ((s.endpoint || "") !== "")
            return "discover"
        // HomeView.slint:355 mounts the playlist arm on the Home AND Editor's
        // Picks repeater; discover_prefs has no qobuzPlaylists entry for For
        // You, so this only ever fires on the two tabs that render it.
        if (s.id === "qobuzPlaylists" && (tab === "home" || tab === "editorPicks"))
            return "playlists"
        // Home only (ForYouView.slint's arm has no show-view-all), and never
        // on the empty-history placeholder (kind "recentPlaceholder").
        if (s.id === "recentlyPlayedAlbums" && tab === "home" && s.kind === "album")
            return "recent"
        // HomeView.slint:506 AND ForYouView.slint:152 — both tabs.
        if (s.id === "mostPlayedAlbums")
            return "mostplayed"
        return ""
    }

    /// The page title, when it differs from the rail title. Only the slim
    /// "Most Streamed" rail does: it is titled "Popular albums" on Home but
    /// HomeView.slint:466 hard-codes @tr("Most Streamed") for the page.
    function viewAllTitle(s) {
        if (s.id === "mostStreamed")
            return QbzSession.tr("Most Streamed", QbzSession.trRev)
        return s.title || ""
    }

    function openViewAll(s, tab) {
        var kind = root.viewAllKind(s, tab)
        if (kind === "discover")
            QbzHome.openDiscoverBrowse(s.endpoint || "", root.viewAllTitle(s))
        else if (kind === "playlists")
            QbzHome.openPlaylistBrowse()
        else if (kind === "recent")
            QbzHome.openRecentAlbums()
        else if (kind === "mostplayed")
            QbzHome.openMostPlayedAlbums()
    }

    // ============================ shared components =======================

    // Circular page-control button (Carousel's NavButton).
    // Horizontal album rail (Carousel.slint): header + clipped ListView,
    // page chevrons (per-page step like the Slint paging).
    component AlbumRail: Column {
        id: albumRail
        property var sectionData: ({})
        /// Which Discover tab is rendering this rail — the "View all"
        /// decision is per tab (see viewAllKind).
        property string tabId: "home"
        width: parent ? parent.width : 0
        spacing: 12

        readonly property int perPage: Math.max(1, Math.floor((rail.width + 32) / 232))
        readonly property int step: perPage * 232
        readonly property real maxScroll: Math.max(0, rail.contentWidth - rail.width)

        QbzSectionHeader {
            title: sectionData.title
            leftEnabled: rail.contentX > 1
            rightEnabled: rail.contentX < albumRail.maxScroll - 1
            showViewAll: root.viewAllKind(albumRail.sectionData, albumRail.tabId) !== ""
            onViewAllClicked: root.openViewAll(albumRail.sectionData, albumRail.tabId)
            // Math.MIN here paged LEFT to a permanent 0 (the chevron looked
            // enabled and did nothing once contentX passed the first step);
            // PinnedRail:408 and SlimGrid:341 both use max.
            onPageLeft: rail.contentX = Math.max(0, rail.contentX - albumRail.step)
            onPageRight: rail.contentX = Math.min(albumRail.maxScroll, rail.contentX + albumRail.step)
        }
        Item {
            width: parent.width
            height: 246
            ListView {
                id: rail
                anchors.fill: parent
                orientation: ListView.Horizontal
                spacing: 32
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: sectionData.items
                delegate: Item {
                    required property var modelData
                    required property int index
                    width: 200
                    height: 246

                    AlbumCard {
                        id: railAlbumCard
                        albumId: modelData.id
                        source: modelData.source || ""
                        sources: modelData.sources || []
                        title: modelData.title
                        artist: modelData.artist
                        artistId: modelData.historyArtistLink ? "" : modelData.artistId
                        hostArtistLink: modelData.historyArtistLink === true && (modelData.artist || "").trim() !== ""
                        onArtistRequested: QbzHome.openHistoryAlbumArtist(
                            modelData.id, modelData.source || "", modelData.artist || "", modelData.artistId || "")
                        genre: modelData.genre
                        year: modelData.year
                        qualityTier: modelData.qualityTier
                        qualityDetail: modelData.qualityLabel || modelData.qualityDetail || ""
                        ribbon: modelData.ribbon
                        ribbonKind: modelData.ribbonKind
                        artSource: root.sectionArtOf(modelData)
                        isPinned: modelData.isPinned
                        // Snapshot url the pin payload persists (artPath is
                        // a local cache path — see AlbumCard.artworkUrl).
                        artworkUrl: modelData.artUrl || ""
                        // The POC-NOTE that used to sit here ("Home hearts
                        // are not seeded from fav_cache") is obsolete: the
                        // store IS open and home_qt stamps `isFavorite` on
                        // every card row. While it was hardcoded false, a
                        // Home rail album that IS in the library drew the
                        // hollow heart, read "Add to Library", and the click
                        // REMOVED it — the toggle takes its direction from
                        // the same cache the row is stamped from.
                        isFavorite: modelData.isFavorite === true
                    }
                    // Per-item artwork placeholder: the grey tile shimmers
                    // until THIS card's cover is ON SCREEN, then dissolves
                    // into it — the rail fills in progressively instead of
                    // all at once. A bare Rectangle, so it does not eat the
                    // card's hover/click areas underneath.
                    // AlbumCard seals its RoundedImage away, so the handover
                    // uses the probe arm (`coverSource`): it rides the same
                    // pixmap-cache entry the card is loading and retires on
                    // the DECODE, never on the path merely appearing — the
                    // path lands while the card's canvas is still blank.
                    QbzSkeleton {
                        variant: "art"
                        width: 200
                        height: 200
                        pending: (modelData.artUrl || "") !== ""
                        artworkItem: railAlbumCard.artworkImmediate ? railAlbumCard : null
                        coverSource: railAlbumCard.artworkImmediate ? "" : root.sectionArtOf(modelData)
                        phase: root.skelPhase
                        cellIndex: index
                        // A cover whose download fails republishes the
                        // document with an empty artPath and no further
                        // signal, so the tile must be bounded (same rule and
                        // constant as SearchView's CardArtSkeleton).
                        settleMs: 6000
                    }
                }
            }
            // Soft edge fades (Carousel.slint): content dissolves
            // into the page background at the scrolled edges. Hidden under the
            // app-wide dynamic background — they fade to the OPAQUE
            // surface-main, so over a moving field they read as two dark slabs
            // rather than a dissolve (Carousel.slint:304, :312).
            Rectangle {
                anchors.left: parent.left
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: 56
                visible: !theme.ambientOn
                opacity: rail.contentX > 1 ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                gradient: Gradient {
                    orientation: Gradient.Horizontal
                    GradientStop { position: 0.0; color: theme.surfaceMain }
                    GradientStop { position: 1.0; color: "transparent" }
                }
            }
            Rectangle {
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: 56
                visible: !theme.ambientOn
                opacity: rail.contentX < maxScroll - 1 ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                gradient: Gradient {
                    orientation: Gradient.Horizontal
                    GradientStop { position: 0.0; color: "transparent" }
                    GradientStop { position: 1.0; color: theme.surfaceMain }
                }
            }
        }
    }

    // TrackSlimRow — the TRACK flavour of the slim row. Visually the shared
    // qml/cards/SlimCard.qml (same 60px row, 44px thumb, title/subtitle),
    // but the whole-row click PLAYS the track instead of opening an album.
    // SlimCard hardcodes `QbzAlbum.openAlbum(card.id)`, so mounting it for a
    // track row would navigate to an album id that does not exist — a control
    // that renders and does the WRONG thing. The consolidation (a `kind` /
    // `activated()` seam on SlimCard, then delete this) is in the handoff.
    component TrackSlimRow: Rectangle {
        property var card: ({})
        height: 60
        radius: theme.radiusSm
        color: trArea.containsMouse ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 12
            Rectangle {
                width: 44
                height: 44
                radius: 4
                anchors.verticalCenter: parent.verticalCenter
                color: theme.surfaceElevated
                clip: true
                RoundedImage {
                    anchors.fill: parent
                    source: root.sectionArtOf(card)
                    radius: 4
                }
            }
            Column {
                width: parent.width - 44 - 2 * 12
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    width: parent.width
                    text: card.title || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontLink
                    font.weight: theme.weightMedium
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: card.artist || ""
                    color: theme.textMuted
                    font.pixelSize: 12
                    elide: Text.ElideRight
                }
            }
        }
        MouseArea {
            id: trArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            // The row's ORIGIN goes with the id. A Recently-Played row can be a
            // Qobuz track, a local file or a Plex row, and `playTrack` alone
            // sends every id to the Qobuz catalog — which 404s on the other two
            // (the owner hit it on a Plex id from this exact rail). Empty
            // source = qobuz, which is the legacy history shape.
            onClicked: QbzPlayer.playTrackFrom(card.id, card.source || "")
        }
    }

    // Slim rows mount the shared qml/cards/SlimCard.qml (SlimCard.slint) for
    // ALBUM data and TrackSlimRow for TRACK data — one paging shell, two row
    // flavours (Slint's SlimCarousel is likewise shared by Popular albums and
    // Recently Played Tracks).
    // Popular slim grid (SlimCarousel.slint): 4x3 pages of 12, capped 24.
    component SlimGrid: Column {
        id: sgrid
        property var sectionData: ({})
        property string tabId: "home"
        // true = the rows are TRACKS (click plays), false = ALBUMS (click opens).
        property bool tracks: false
        width: parent ? parent.width : 0
        spacing: 12

        readonly property int perPage: 12
        readonly property int total: Math.min(sectionData.items.length, 2 * perPage)
        readonly property int pageCount: Math.max(1, Math.ceil(total / perPage))
        readonly property real maxScroll: (pageCount - 1) * grid.width

        QbzSectionHeader {
            title: sectionData.title
            leftEnabled: grid.contentX > 1
            rightEnabled: grid.contentX < sgrid.maxScroll - 1
            showViewAll: root.viewAllKind(sgrid.sectionData, sgrid.tabId) !== ""
            onViewAllClicked: root.openViewAll(sgrid.sectionData, sgrid.tabId)
            onPageLeft: grid.contentX = Math.max(0, grid.contentX - grid.width)
            onPageRight: grid.contentX = Math.min(sgrid.maxScroll, grid.contentX + grid.width)
        }
        Item {
            width: parent.width
            height: 3 * 60 + 2 * 8
            clip: true
            Flickable {
                id: grid
                width: parent.width
                height: parent.height
                contentWidth: sgrid.pageCount * width
                contentHeight: height
                boundsBehavior: Flickable.StopAtBounds
                readonly property real cellWidth: (width - 3 * 8) / 4

                Repeater {
                    // The grid is contractually capped at two pages. Model the
                    // cap itself: a Loader existence gate prevented the heavy
                    // row from being built past 24, but the old full-array
                    // model still created an Item + two Components + Loader
                    // shell for every discarded result.
                    model: sgrid.total
                    delegate: Item {
                        id: slimSlot
                        required property int index
                        readonly property var cardData: sgrid.sectionData.items[index]
                        readonly property int slot: index % sgrid.perPage
                        readonly property int pageIdx: Math.floor(index / sgrid.perPage)
                        width: grid.cellWidth
                        height: 60
                        x: pageIdx * grid.width + (slot % 4) * (grid.cellWidth + 8)
                        y: Math.floor(slot / 4) * (60 + 8)

                        // The PinnedRail dispatch pattern: Components declared
                        // in the delegate scope, sharing this capped slot row.
                        Component {
                            id: albumRowComp
                            SlimCard { card: slimSlot.cardData }
                        }
                        Component {
                            id: trackRowComp
                            TrackSlimRow { card: slimSlot.cardData }
                        }
                        Loader {
                            anchors.fill: parent
                            sourceComponent: sgrid.tracks ? trackRowComp : albumRowComp
                        }
                    }
                }
            }
        }
    }

    // Pinned rail (PinnedCarousel.slint) — one 200x246 slot per item, the
    // card picked by the item's own kind: albums reuse AlbumCard, artists
    // render the ArtistGridCard circle, playlists the PlaylistCard square.
    // Fed from the shared per-user pinned_items.db (most-recent first).
    //
    // `sectionData` supplies ONLY the header (title + the rail's position in
    // the tab); the rows come from root.pinnedItems — the dedicated
    // `pinnedJson` property — so a pin/unpin touches this rail and nothing
    // else in the view.
    component PinnedRail: Column {
        id: pinRail
        property var sectionData: ({})
        readonly property var items: root.pinnedItems
        width: parent ? parent.width : 0
        spacing: 12

        readonly property real maxScroll: Math.max(0, rail.contentWidth - rail.width)
        readonly property int perPage: Math.max(1, Math.floor((rail.width + 32) / 232))
        readonly property int step: perPage * 232

        QbzSectionHeader {
            title: sectionData.title
            leftEnabled: rail.contentX > 1
            rightEnabled: rail.contentX < maxScroll - 1
            onPageLeft: rail.contentX = Math.max(0, rail.contentX - step)
            onPageRight: rail.contentX = Math.min(maxScroll, rail.contentX + step)
        }
        Item {
            width: parent.width
            height: 246
            ListView {
                id: rail
                anchors.fill: parent
                orientation: ListView.Horizontal
                spacing: 32
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: pinRail.items
                delegate: Item {
                    required property var modelData
                    required property int index
                    width: 200
                    height: 246

                    Component {
                        id: pAlbum
                        AlbumCard {
                            readonly property bool localPinned:
                                modelData.isLocalAlbum === true
                            localMode: localPinned
                            source: modelData.source || ""
                            sources: modelData.sources || []
                            quickViewAffordance: true
                            pinAffordance: true
                            albumId: modelData.id
                            title: modelData.title
                            artist: modelData.artist
                            hostArtistLink: (modelData.artist || "") !== ""
                            onArtistRequested: QbzHome.openPinnedAlbumArtist(
                                modelData.id, modelData.artist || "")
                            artSource: modelData.artPath || ""
                            isPinned: true
                            // home_qt::pinned_cards stamps the heart per row,
                            // routed by the row's own item_kind.
                            isFavorite: modelData.isFavorite === true
                            // Un-pinning from this rail is fine without it,
                            // but the glyph is a TOGGLE: re-pinning the row
                            // the user just dropped must restore the same
                            // snapshot url, not blank it.
                            artworkUrl: modelData.artUrl || ""
                            pinArtworkUrl: artworkUrl !== "" ? artworkUrl : artSource
                            onOpenRequested: QbzLocal.openAlbum(modelData.id)
                            onPlayRequested: QbzLocal.playAlbum(modelData.id, false)
                            onEnqueueRequested: function (mode) {
                                QbzLocal.enqueue("album", modelData.id, mode)
                            }
                        }
                    }
                    Component {
                        id: pArtist
                        ArtistCard {
                            // The pinned row carries the stored snapshot in
                            // BOTH `artist` and `subtitle` (home_qt
                            // `pinned_cards`) precisely so this card and the
                            // album one can each read their own slot. While
                            // only `artist` was published, this card drew a
                            // blank second line — and, because it hands
                            // `item.subtitle` back to togglePin, re-pinning
                            // from the rail persisted that blank.
                            item: modelData
                            artSource: modelData.artPath || ""
                            isPinned: true
                            artworkUrl: modelData.artUrl || ""
                        }
                    }
                    Component {
                        id: pPlaylist
                        PlaylistCard {
                            // Same story as the artist slot above; the card's
                            // `artworkUrl` default already resolves the row's
                            // `artUrl` snapshot.
                            item: modelData
                            artSource: modelData.artPath || ""
                            isPinned: true
                        }
                    }
                    Loader {
                        id: pinnedCardLoader
                        anchors.fill: parent
                        sourceComponent: modelData.itemKind === "artist" ? pArtist
                            : modelData.itemKind === "playlist" ? pPlaylist : pAlbum
                    }
                    // Square-art slots only — a pinned ARTIST keeps the
                    // designed round gradient+glyph placeholder ArtistCard
                    // already draws (Slint's ArtistGridCard), which reads as
                    // a portrait, not as a missing tile.
                    QbzSkeleton {
                        variant: "art"
                        width: 200
                        height: 200
                        pending: modelData.itemKind !== "artist"
                            && (modelData.artUrl || "") !== ""
                        artworkItem: pinnedCardLoader.item && pinnedCardLoader.item.artworkImmediate ? pinnedCardLoader.item : null
                        coverSource: artworkItem && artworkItem.artworkImmediate ? "" : (modelData.artPath || "")
                        phase: root.skelPhase
                        cellIndex: index
                        settleMs: 6000
                    }
                }
            }
        }
    }

    // Qobuz Mixes rail (QobuzMixesRow.slint) — four static 220px navigation
    // tiles (gradient art + badge + name, description below). The gradient
    // square itself is the shared cards/MixArtwork.qml, because the mix
    // LANDING page draws the same four identities at 224px and the colours
    // must not be able to drift apart (the .slint re-declares them in both
    // files). Tiles are LIVE: `openMix` records the "mix" nav entry and
    // fetches — 1:1 with `media-action("mix", which, "open")`.
    component MixTile: Column {
        id: mixTile
        property string kind: "daily"
        spacing: 8
        width: 220

        MixArtwork {
            id: art
            kind: mixTile.kind
            size: 220
            titleSize: 22
            cornerRadius: 8
            onClicked: QbzHome.openMix(mixTile.kind)
        }
        Text {
            width: 220
            text: art.tileDescription(mixTile.kind)
            color: theme.textMuted
            font.pixelSize: 12
            wrapMode: Text.WordWrap
        }
    }
    component MixesRail: Column {
        property var sectionData: ({})
        width: parent ? parent.width : 0
        spacing: 12
        Text {
            text: sectionData.title
            color: theme.textPrimary
            font.pixelSize: theme.fontSection
            font.weight: theme.weightSemibold
        }
        Row {
            spacing: 32
            MixTile { kind: "daily" }
            MixTile { kind: "weekly" }
            MixTile { kind: "fav" }
            MixTile { kind: "top" }
        }
    }

    // Radio Stations rail (RadioCarousel.slint) — album-seeded radio tiles in
    // a paged horizontal list with the same chevrons every other rail has.
    // 200px card + 32px spacing = the 232px pitch the album rails use.
    //
    // The rows come from `QbzHome.radioStationsJson`, NOT from `sectionData`:
    // the section in the tab document is an empty ordering slot (the `pinned`
    // split — see src/foryou_qt.rs). `sectionData` supplies the title only.
    component RadioRail: Column {
        id: radioRail
        property var sectionData: ({})
        readonly property var items: root.radioStations
        width: parent ? parent.width : 0
        spacing: 12

        readonly property int perPage: Math.max(1, Math.floor((rail.width + 32) / 232))
        readonly property int step: perPage * 232
        readonly property real maxScroll: Math.max(0, rail.contentWidth - rail.width)

        QbzSectionHeader {
            title: radioRail.sectionData.title
            leftEnabled: rail.contentX > 1
            rightEnabled: rail.contentX < radioRail.maxScroll - 1
            onPageLeft: rail.contentX = Math.max(0, rail.contentX - radioRail.step)
            onPageRight: rail.contentX = Math.min(radioRail.maxScroll, rail.contentX + radioRail.step)
        }
        Item {
            width: parent.width
            // RadioCarousel.slint:94 — 234, not the album rails' 246 (the
            // radio card's meta block is one line shorter in the worst case).
            height: 234
            ListView {
                id: rail
                anchors.fill: parent
                orientation: ListView.Horizontal
                spacing: 32
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: radioRail.items
                delegate: Item {
                    required property var modelData
                    required property int index
                    width: 200
                    height: 234

                    RadioCard {
                        id: radioArtworkCard
                        seedTitle: modelData.title
                        seedSubtitle: modelData.artist
                        label: QbzSession.tr("RADIO", QbzSession.trRev)
                        // The patch map, not `artPath`: the rail's document is
                        // published ONCE and covers arrive per-URL.
                        artSource: root.forYouArtOf(modelData)
                        // Per-station key, so the tile that was clicked is the
                        // only one in the rail that spins.
                        loading: QbzHome.radioPending === "album:" + modelData.albumId
                        // Both the card body and the hover disc start the
                        // album radio (the .slint wires `clicked` and `play`
                        // to the same callback).
                        onActivated: QbzHome.startAlbumRadio(modelData.albumId)
                        onPlayRequested: QbzHome.startAlbumRadio(modelData.albumId)
                    }
                    // The card's artwork is an INSET 126px display, so the
                    // placeholder is that square, not the whole tile.
                    QbzSkeleton {
                        variant: "art"
                        width: 126
                        height: 126
                        x: 37
                        y: 29
                        blockRadius: 4
                        pending: (modelData.artUrl || "") !== ""
                        artworkItem: radioArtworkCard.artworkImmediate ? radioArtworkCard : null
                        coverSource: radioArtworkCard.artworkImmediate ? "" : root.forYouArtOf(modelData)
                        phase: root.skelPhase
                        cellIndex: index
                        settleMs: 6000
                    }
                }
            }
        }
    }

    // Spotlight (Spotlight.slint) — one favourite artist: header, a hero
    // (140px round portrait + ARTIST + name + play / open-artist discs), and
    // a draggable row mixing a TOP TRACKS card, a RADIO card and the
    // artist's albums. Rows come from `QbzHome.spotlightJson`; `sectionData`
    // is the ordering slot only.
    component SpotlightRail: Column {
        id: spot
        property var sectionData: ({})
        readonly property var doc: root.spotlight
        readonly property var albums: root.spotlight.albums || []
        readonly property bool hasTopTracks: root.spotlight.hasTopTracks === true
        width: parent ? parent.width : 0
        spacing: 12

        // Header — the .slint hardcodes the pair, not sectionData.title.
        Column {
            spacing: 2
            Text {
                text: QbzSession.tr("Spotlight", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: theme.fontSection
                font.weight: theme.weightSemibold
            }
            Text {
                text: QbzSession.tr("Shine a light on one of your favourite artists.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }
        }

        // Hero.
        Row {
            spacing: 16
            Rectangle {
                width: 140
                height: 140
                radius: 70
                color: theme.surfaceElevated
                clip: true
                QbzIcon {
                    // The REMOTE url, not the local path — 1:1 with
                    // Spotlight.slint:91-98 (`spotlight-image-url == ""`).
                    // Gating on artPath flashed this glyph on every cold
                    // cache, because the path is empty until the download
                    // lands; the artist HAS a portrait, it just is not here
                    // yet, and the placeholder for that is the surface fill
                    // (Slint's hero is an unconditional Image over the
                    // surface-elevated box, so it simply draws nothing).
                    visible: (spot.doc.artUrl || "") === ""
                    name: "user"
                    width: 40
                    height: 40
                    anchors.centerIn: parent
                    tintName: "muted"
                }
                RoundedImage {
                    anchors.fill: parent
                    source: root.forYouArtOf(spot.doc)
                    radius: 70
                }
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzArtist.openArtist(spot.doc.artistId || "")
                }
            }
            Column {
                anchors.verticalCenter: parent.verticalCenter
                spacing: 8
                Text {
                    visible: (spot.doc.category || "") !== ""
                    text: QbzSession.tr("ARTIST", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.5
                }
                Text {
                    text: spot.doc.name || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontTitle
                    font.weight: theme.weightBold
                }
                // Spotlight.slint's local CircleButton is 44px for BOTH
                // discs (not the shared primitive's 44/32) — a 32px disc next
                // to the 140px portrait reads wrong, which is presumably why.
                // QbzCircleAction's size is a plain binding, so a caller can
                // override it without a new knob on the shared control.
                Row {
                    spacing: 10
                    QbzCircleAction {
                        visible: spot.hasTopTracks
                        name: "play-fill"
                        primary: true
                        onClicked: QbzHome.playArtistTopTracks(spot.doc.artistId || "")
                    }
                    QbzCircleAction {
                        name: "user"
                        width: 44
                        height: 44
                        onClicked: QbzArtist.openArtist(spot.doc.artistId || "")
                    }
                }
            }
        }

        Item { width: 1; height: 4 }

        // Content row — one draggable strip, like the other carousels.
        Item {
            width: parent.width
            height: 270
            ListView {
                id: spotRow
                anchors.fill: parent
                orientation: ListView.Horizontal
                spacing: 24
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                // The two radio cards are prepended to the album rows, so the
                // strip is ONE model and the drag/scroll covers all of it (the
                // .slint puts them in the same HorizontalLayout).
                model: {
                    var out = []
                    if (spot.hasTopTracks)
                        out.push({ "slot": "top" })
                    out.push({ "slot": "radio" })
                    for (var i = 0; i < spot.albums.length; i++)
                        out.push({ "slot": "album", "album": spot.albums[i] })
                    return out
                }
                delegate: Item {
                    required property var modelData
                    required property int index
                    width: 200
                    height: 246

                    // The PinnedRail dispatch pattern: Components declared in
                    // the delegate scope, so `modelData` resolves.
                    Component {
                        id: topCard
                        RadioCard {
                            seedTitle: QbzSession.tr("Top tracks", QbzSession.trRev)
                            seedSubtitle: QbzSession.tr("By {}", QbzSession.trRev)
                                .replace("{}", spot.doc.name || "")
                            label: QbzSession.tr("TOP TRACKS", QbzSession.trRev)
                            artSource: root.forYouArtOf(spot.doc)
                            onActivated: QbzHome.playArtistTopTracks(spot.doc.artistId || "")
                            onPlayRequested: QbzHome.playArtistTopTracks(spot.doc.artistId || "")
                        }
                    }
                    Component {
                        id: radioSeedCard
                        RadioCard {
                            seedTitle: spot.doc.name || ""
                            seedSubtitle: QbzSession.tr("Qobuz Radio Station", QbzSession.trRev)
                            label: QbzSession.tr("RADIO", QbzSession.trRev)
                            artSource: root.forYouArtOf(spot.doc)
                            // The smart pool builder is the slowest radio in
                            // the app; without this the tile went back to its
                            // resting state the instant the pointer left.
                            loading: QbzHome.radioPending
                                === "artist:" + (spot.doc.artistId || "")
                            onActivated: QbzHome.startArtistRadio(spot.doc.artistId || "")
                            onPlayRequested: QbzHome.startArtistRadio(spot.doc.artistId || "")
                        }
                    }
                    Component {
                        id: albumCardComp
                        AlbumCard {
                            albumId: modelData.album.id
                            source: modelData.album.source || ""
                            sources: modelData.album.sources || []
                            title: modelData.album.title
                            artist: modelData.album.artist
                            artistId: modelData.album.artistId
                            genre: modelData.album.genre
                            year: modelData.album.year
                            qualityTier: modelData.album.qualityTier
                            qualityDetail: modelData.album.qualityLabel || modelData.album.qualityDetail || ""
                            artSource: root.forYouArtOf(modelData.album)
                            artworkUrl: modelData.album.artUrl || ""
                            isPinned: modelData.album.isPinned === true
                            isFavorite: modelData.album.isFavorite === true
                        }
                    }
                    Loader {
                        anchors.fill: parent
                        sourceComponent: modelData.slot === "top" ? topCard
                            : modelData.slot === "radio" ? radioSeedCard
                            : albumCardComp
                    }
                }
            }
        }
    }

    // ONE vertical section delegate. The page-level ListView below owns the
    // model and therefore only asks for the rails in (or near) its viewport.
    // This used to be a Column + Repeater: every section in the active tab
    // existed at once, so every horizontal rail eagerly built its first screen
    // of cards even when the rail was several viewports below the fold.
    component SectionRail: Item {
        id: railSlot
        required property var modelData
        required property int index
        /// "home" | "editorPicks" | "forYou" | "recommendations" — forwarded
        /// to the rail so its "View all" resolves per tab.
        property string railTab: "home"
        readonly property var effectiveData: root.effectiveSection(modelData)
        readonly property bool shown: root.sectionIsVisible(effectiveData, railTab)
        width: parent ? parent.width : 0
        // ListView spacing would reserve a gap for zero-height hidden ordering
        // slots. Carry the 40px gap inside each visible slot instead, and only
        // when another visible section follows it.
        height: shown && railLoader.item
            ? railLoader.height + (root.hasVisibleSectionAfter(index) ? 40 : 0)
            : 0

        Loader {
                id: railLoader
                property var modelData: railSlot.modelData
                property string railTab: railSlot.railTab
                // ListView owns the delegate's x-position and forces it back
                // to the content origin. Put the horizontal inset on the
                // child it does not position: 32px on both sides, exactly the
                // old padded Column geometry.
                x: 32
                width: parent.width - 64
                height: item ? item.implicitHeight : 0
                // The pinned slot is always in the document (it is where the
                // discover prefs put the rail); it renders only once the
                // store has rows — the Slint `PinnedState.items.length > 0`
                // gate. `active`, not only `visible`, is the existence gate:
                // an empty out-of-document slot builds no rail subtree.
                // The out-of-document rails are always IN the document (that
                // is where the discover prefs put them) and render only once
                // their own store has rows — the Slint
                // `PinnedState.items.length > 0` /
                // `ForYouState.radio-stations.length > 0` /
                // `spotlight-visible` gates. A Column skips invisible children
                // entirely, spacing included, so an empty one leaves no gap.
                active: railSlot.shown
                visible: active
                sourceComponent: sectionData.kind === "album" ? albumRailComp
                    : sectionData.kind === "playlist" ? playlistRailComp
                    : sectionData.kind === "slim" ? slimGridComp
                    : sectionData.kind === "slimTracks" ? trackGridComp
                    : sectionData.kind === "artists" ? artistRailComp
                    : sectionData.kind === "pinned" ? pinnedRailComp
                    : sectionData.kind === "mixes" ? mixesRailComp
                    : sectionData.kind === "radio" ? radioRailComp
                    : sectionData.kind === "spotlight" ? spotlightRailComp
                    : recentComp
                // A Weekly slot arrives with EMPTY items and is filled from
                // `recoWeekly` here. Object.assign builds a new descriptor
                // only for those two ids; every other rail returns the very
                // same `modelData` reference, so a Weekly landing re-evaluates
                // all nine bindings but re-creates only the two delegates
                // whose value actually changed.
                property var sectionData: root.isWeeklySlot(railSlot.effectiveData.id)
                    ? Object.assign({}, railSlot.effectiveData,
                                    { "items": root.weeklyRows(railSlot.effectiveData.id) })
                    : railSlot.effectiveData

                Component {
                    id: pinnedRailComp
                    PinnedRail { sectionData: parent.sectionData }
                }
                Component {
                    id: albumRailComp
                    AlbumRail { sectionData: parent.sectionData; tabId: parent.railTab }
                }
                Component {
                    id: playlistRailComp
                    Column {
                        id: plRail
                        property var sectionData: parent.sectionData
                        property string tabId: parent.railTab
                        width: parent ? parent.width : 0
                        spacing: 12

                        readonly property int perPage: Math.max(1, Math.floor((plList.width + 32) / 232))
                        readonly property int step: perPage * 232
                        readonly property real maxScroll: Math.max(0, plList.contentWidth - plList.width)

                        // The rail used to draw a bare Text: no chevrons (so
                        // a 40-playlist rail's tail was unreachable) and no
                        // "View all". PlaylistCarousel.slint:109-115 has both.
                        //
                        // The CATEGORY FILTER on this title line
                        // (HomeView.slint:360-374) is now ported too, and with
                        // it the empty-result line. The POC-NOTE that used to
                        // sit here said neither was, and pointed at the
                        // PlaylistBrowse page as the substitute — that page's
                        // tag filter is a DIFFERENT one (single-select and
                        // server-side), so the note was directing the reader to
                        // a control that does not do this job.
                        //
                        // Only the Qobuz-Playlists rail offers it: it is the
                        // only playlist section the index ships tags for, and
                        // the button hides itself when the set is empty.
                        readonly property bool tagsOffered:
                            plRail.sectionData.id === "qobuzPlaylists"
                            && root.playlistTags.length > 0
                        readonly property var visibleItems:
                            root.playlistTagSel.length > 0 && plRail.tagsOffered
                                ? root.filterPlaylists(plRail.sectionData.items)
                                : plRail.sectionData.items

                        QbzSectionHeader {
                            title: plRail.sectionData.title
                            leftEnabled: plList.contentX > 1
                            rightEnabled: plList.contentX < plRail.maxScroll - 1
                            showViewAll: root.viewAllKind(plRail.sectionData, plRail.tabId) !== ""
                            onViewAllClicked: root.openViewAll(plRail.sectionData, plRail.tabId)
                            onPageLeft: plList.contentX = Math.max(0, plList.contentX - plRail.step)
                            onPageRight: plList.contentX = Math.min(plRail.maxScroll, plList.contentX + plRail.step)
                            leading: plRail.tagsOffered ? plTagFilterComp : null
                        }
                        Component {
                            id: plTagFilterComp
                            PlaylistTagFilterButton {
                                count: root.playlistTagSel.length
                                selectedNames: root.playlistTagNames
                                onClicked: root.togglePlaylistTagPopup(this)
                            }
                        }
                        // Zero results with a filter on: say so, instead of
                        // showing an empty band the user cannot explain
                        // (HomeView.slint:393-397). The rail stays mounted so
                        // the filter is still reachable to clear.
                        Text {
                            visible: plRail.visibleItems.length === 0
                                     && root.playlistTagSel.length > 0
                            width: parent.width
                            text: QbzSession.tr("No playlists match the selected categories.", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 13
                            wrapMode: Text.WordWrap
                        }
                        ListView {
                            id: plList
                            visible: plRail.visibleItems.length > 0
                            width: parent.width
                            height: visible ? 246 : 0
                            orientation: ListView.Horizontal
                            spacing: 32
                            clip: true
                            boundsBehavior: Flickable.StopAtBounds
                            model: plRail.visibleItems
                            delegate: PlaylistCard {
                                item: modelData
                                artSource: root.sectionArtOf(modelData)
                                // The card's own default is `false`; the
                                // pin state travels on the row (home_qt
                                // `map_playlist`), so it has to be handed
                                // over or the glyph lies on every rail.
                                isPinned: modelData.isPinned === true
                            }
                        }
                    }
                }
                Component {
                    id: slimGridComp
                    SlimGrid { sectionData: parent.sectionData; tabId: parent.railTab }
                }
                Component {
                    id: trackGridComp
                    SlimGrid { sectionData: parent.sectionData; tabId: parent.railTab; tracks: true }
                }
                Component {
                    id: artistRailComp
                    Column {
                        id: arRail
                        property var sectionData: parent.sectionData
                        width: parent ? parent.width : 0
                        spacing: 12

                        readonly property int perPage: Math.max(1, Math.floor((arList.width + 32) / 232))
                        readonly property int step: perPage * 232
                        readonly property real maxScroll: Math.max(0, arList.contentWidth - arList.width)

                        // ArtistCarousel.slint:141-147 pages with chevrons; a
                        // bare Text left the tail of an 18-artist rail
                        // unreachable. No "View all" — the Slint artist arms
                        // (topArtists / artistsToFollow) carry none.
                        QbzSectionHeader {
                            title: arRail.sectionData.title
                            leftEnabled: arList.contentX > 1
                            rightEnabled: arList.contentX < arRail.maxScroll - 1
                            onPageLeft: arList.contentX = Math.max(0, arList.contentX - arRail.step)
                            onPageRight: arList.contentX = Math.min(arRail.maxScroll, arList.contentX + arRail.step)
                        }
                        ListView {
                            id: arList
                            width: parent.width
                            height: 246
                            orientation: ListView.Horizontal
                            spacing: 32
                            clip: true
                            boundsBehavior: Flickable.StopAtBounds
                            model: arRail.sectionData.items
                            delegate: ArtistCard {
                                item: modelData
                                artSource: root.sectionArtOf(modelData)
                                // Same hand-over as the playlist rail —
                                // home_qt `map_fav_artist` publishes the
                                // pin state and the card defaults to false.
                                isPinned: modelData.isPinned === true
                                artworkUrl: modelData.artUrl || ""
                            }
                        }
                    }
                }
                Component {
                    id: mixesRailComp
                    MixesRail { sectionData: parent.sectionData }
                }
                Component {
                    id: radioRailComp
                    RadioRail { sectionData: parent.sectionData }
                }
                Component {
                    id: spotlightRailComp
                    SpotlightRail { sectionData: parent.sectionData }
                }
                Component {
                    id: recentComp
                    QbzEmptyState {
                        property var sectionData: parent.sectionData
                        title: sectionData.title
                        body: sectionData.hint
                    }
                }
        }
    }

    // HomeSkeleton.slint's SkeletonRow, 1:1 on the shared QbzSkeleton
    // control: a 180x22 title bar over five 200px card placeholders,
    // spacing 32/12/8. `phase` comes from the ONE Timer on the view root.
    component SkeletonRow: Column {
        id: skelRow
        property bool phase: false
        width: parent ? parent.width : 0
        spacing: 12
        QbzSkeleton { variant: "block"; width: 180; height: 22; phase: skelRow.phase }
        Row {
            spacing: 32
            Repeater {
                model: 5
                delegate: QbzSkeleton {
                    required property int index
                    variant: "card"
                    width: 200
                    phase: skelRow.phase
                    cellIndex: index
                }
            }
        }
    }
    // Two SkeletonRows = the whole HomeSkeleton component. Mounted by each
    // Discover tab while its sections document is still empty.
    component TabSkeleton: Column {
        id: tabSkel
        property bool phase: false
        width: parent ? parent.width : 0
        spacing: 40
        SkeletonRow { phase: tabSkel.phase }
        SkeletonRow { phase: tabSkel.phase }
    }

    // ============================ offline gate ============================
    // (OfflinePlaceholder.slint replica; mounted INSTEAD of the view.)
    QbzOfflinePlaceholder {
        visible: QbzSession.offline
        anchors.centerIn: parent
        // The induced-only "Open Settings" arm (Slint) is wired:
        showSettingsAction: true
        onSettingsClicked: QbzShell.navigateTo("settings")
    }

    // ============================ the view ================================
    Column {
        anchors.fill: parent
        spacing: 0
        visible: !QbzSession.offline

        // --- Toolbar (fixed 56px) ---------------------------------------
        Item {
            width: parent.width
            height: 56

            // Thin-bar tier: surface-main @ bar-alpha (0.3) under the app-wide
            // dynamic background, opaque surface-main otherwise (HomeView.slint:182).
            // The toolbar had NO fill of its own — with the background off the
            // view root's surface-main showed through and it looked right, but
            // the view root goes transparent under the background, so the bar
            // lost its backing exactly when it needed one.
            Rectangle {
                anchors.fill: parent
                // Rounded at the TOP because this bar is full-bleed at y=0 of
                // the content pane, and under the dynamic background the pane's
                // own rounding cannot reach it: Qt's `clip` is a rectangular
                // scissor that ignores `radius`, and AppShell hides its bezel
                // nubs while the field is meant to show through the corners.
                // So a full-bleed pane child rounds ITSELF — AppShell.qml says
                // exactly this ("there is no mask that can do it for it here")
                // and this bar was the counterexample the owner spotted in
                // Discover. Invisible with the background off: the view root
                // paints the same colour underneath.
                topLeftRadius: theme.radiusMd
                topRightRadius: theme.radiusMd
                color: root.ambientOn ? theme.surfaceMainA30 : theme.surfaceMain
            }

            Row {
                // Slint left-controls: x 32 + NavButtons (now a 0px
                // placeholder) + 16px spacing -> the pill starts at 48.
                x: 48
                y: 25 - height / 2
                spacing: 16

                QbzTabBar {
                    // The 4th tab is present only while the pref is on (1:1
                    // with the Slint `if SettingsState.show-recommendations`).
                    tabs: {
                        var t = [
                            { "id": "home", "label": QbzSession.tr("Home", QbzSession.trRev) },
                            { "id": "editorPicks", "label": QbzSession.tr("Editor's Picks", QbzSession.trRev) },
                            { "id": "forYou", "label": QbzSession.tr("For You", QbzSession.trRev) },
                        ]
                        if (root.showRecommendations)
                            t.push({ "id": "recommendations", "label": QbzSession.tr("Recommendations", QbzSession.trRev) })
                        return t
                    }
                    activeId: root.activeTab
                    // Data is per-tab JSON (no refetch on switch); scroll
                    // resets to top.
                    onSelected: function (id) {
                        root.activeTab = id
                        homeFlick.contentY = 0
                    }
                }
            }

            // Genre filter + configurator gear (HomeView.slint right-controls).
            Row {
                x: parent.width - width - 32
                y: 25 - height / 2
                height: 32
                spacing: 6

                // GenreButton — now the SHARED controls/BrowseGenreButton
                // (the browse pages draw the same control; this was a
                // verbatim copy of it). HomeView.slint:85 is the 32px
                // variant, BrowseHeaderTools.slint:108 the 34px one.
                BrowseGenreButton {
                    context: "discover"
                    btnHeight: 32
                    onClicked: genrePopup.toggle()
                }

                // GearButton — per-tab show/hide + reorder of the Discover
                // sections, and on Recommendations the cache window +
                // "Refresh now" instead (that tab has no orderable sections).
                //
                // It used to be DISABLED on Recommendations, on the grounds
                // that the external reco engine was not ported and the modal
                // would open empty. The engine is ported, and the modal now
                // carries that tab's own arm — so the gear is live on all four
                // and `gearEnabled` is gone with the condition it existed for.
                Rectangle {
                    id: gearBtn
                    width: 32
                    height: 32
                    radius: 4
                    color: gearArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        name: "home-gear"
                        width: 20
                        height: 20
                        anchors.centerIn: parent
                        tintName: gearArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: gearArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: configModal.open(root.activeTab)
                    }
                }
            }
        }
        Rectangle {
            width: parent.width
            height: 1
            color: theme.borderSubtle
        }

        // --- Scrollable sections -----------------------------------------
        Item {
            width: parent.width
            height: parent.height - 57

            // The vertical viewport is the model owner. A Column + Repeater
            // built every section in the active tab and, through each
            // horizontal ListView, at least one screenful of cards per section.
            // ListView keeps that cost proportional to this viewport while
            // preserving the same synchronous first paint.
            ListView {
                id: homeFlick
                anchors.fill: parent
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: root.activeSections
                reuseItems: true
                spacing: 0
                // A rail is a burst of 6-12 cards plus their artwork textures.
                // Half a viewport left that burst inside the first fast wheel
                // gesture: the user's custom Recently bands were mounted just
                // as they crossed the screen, then felt progressively better
                // after QML/pixmap/texture caches warmed. ListView creates its
                // cache delegates asynchronously, so a 1.25-viewport runway
                // moves that finite work ahead of the gesture without bringing
                // back the old all-tabs/all-sections synchronous mount.
                cacheBuffer: Math.max(720, Math.round(height * 1.25))

                delegate: SectionRail {
                    width: homeFlick.width
                    railTab: root.activeTab
                }

                // Loading, error and empty states remain inline at the top of
                // the same scroll surface. Only the selected arm exists.
                // Explicit header/footer spacers reproduce the old 32px
                // Column padding without ListView margins changing contentY's
                // zero-origin contract (ScrollMemory and QbzScrollBar both
                // consume that coordinate).
                header: Item {
                    width: homeFlick.width
                    height: 32 + discoverHeader.height
                    Loader {
                        id: discoverHeader
                        x: 32
                        y: 32
                        width: homeFlick.width - 64
                        property string kind: root.activeHeaderKind
                        active: kind !== ""
                        visible: active
                        height: active && item ? item.implicitHeight : 0
                        sourceComponent: kind === "error" ? homeErrorHeader
                            : kind === "empty" ? recoEmptyHeader
                            : tabSkeletonHeader
                    }
                }
                footer: Item { width: homeFlick.width; height: 32 }

                Component {
                    id: tabSkeletonHeader
                    TabSkeleton {
                        phase: root.skelPhase
                    }
                }

                Component {
                    id: homeErrorHeader
                    Item {
                        implicitHeight: errorBox.implicitHeight
                            + (homeFlick.count > 0 ? 40 : 0)
                        Rectangle {
                            id: errorBox
                            width: parent.width
                            implicitHeight: errorColumn.implicitHeight + 28
                            height: implicitHeight
                            radius: theme.radiusSm
                            color: theme.surfaceElevated
                            border.width: 1
                            border.color: theme.borderSubtle
                            Column {
                                id: errorColumn
                                anchors.centerIn: parent
                                spacing: 10
                                Text {
                                    text: QbzHome.homeError
                                    color: theme.textSecondary
                                    font.pixelSize: 13
                                }
                                Rectangle {
                                    width: retryText.implicitWidth + 28
                                    height: 32
                                    radius: 6
                                    color: retryArea.containsMouse
                                        ? theme.surfaceElevatedHover : theme.surfaceElevated
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
                                        onClicked: QbzHome.reloadHome()
                                    }
                                }
                            }
                        }
                    }
                }

                Component {
                    id: recoEmptyHeader
                    QbzEmptyState {
                        title: QbzSession.tr("No recommendations yet", QbzSession.trRev)
                        body: QbzSession.tr("Connect Last.fm or ListenBrainz in Settings, or play more music, to get personalized recommendations.", QbzSession.trRev)
                        actionLabel: QbzSession.tr("Open Settings", QbzSession.trRev)
                        onActionClicked: QbzShell.navigateTo("settings")
                    }
                }

                Component.onCompleted:
                    root._stampViewport(root.activeTab, root.activeSections.length)
            }

            // Back/forward scroll memory and the custom gutter both accept a
            // ListView because it is a Flickable. The per-tab scope is
            // unchanged.
            ScrollMemory { target: homeFlick; scope: "home:" + root.activeTab }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: homeFlick
            }
        }
    }

    // ============================ overlays ================================
    // Declared LAST: in QML declaration order IS z-order, so these sit above
    // the toolbar and the rails and keep their own presses (ADR-009 is also
    // satisfied explicitly by their z). Both are hidden + disabled while
    // closed, so they never eat a click meant for the view.

    // Shared Filter-by-genre popup, "discover" context: the selection feeds
    // get_discover_index, so toggling one re-fetches the index.
    GenreFilterPopup {
        id: genrePopup
        anchors.fill: parent
        context: "discover"
        // Under the toolbar (56px + the 1px divider + a 5px gap).
        anchorTop: 62
        anchorRight: 32
    }

    // Per-tab section configurator (the gear).
    DiscoverConfigModal {
        id: configModal
        anchors.fill: parent
    }

    // Qobuz-Playlists category filter. Its trigger lives inside the flickable
    // at a y that depends on the scroll and on the rail order, so the popup
    // captures the anchor by mapToItem AT OPEN rather than pinning to a fixed
    // offset the way the genre popup can.
    PlaylistTagFilterPopup {
        id: plTagPopup
        anchors.fill: parent
        tags: root.playlistTags
        selected: root.playlistTagSel
        onToggled: function (slug) { QbzHome.togglePlaylistTag(slug) }
        onCleared: {
            QbzHome.clearPlaylistTags()
            plTagPopup.close()
        }
    }
}
