// Shared route-to-view mapping for Desktop and Kiosk.
// Kiosk-specific views and explicit kioskHost initial properties implement
// the 2026-09-07 hardening contract; Desktop retains its existing route files.
// Keep this file in qml/shell: paths are relative to this directory.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../kiosk"

Item {
    id: root

    // false = desktop shell (AppShell), true = kiosk shell (KioskShell).
    property bool kiosk: false
    // Each shared view declares kioskHost:false; only this shell opts in.
    readonly property var kioskHostRoutes: [
        "settings", "metadataeditor", "purchases", "purchase-album",
        "queue-view", "offlinemanager", "libraryfolders", "blacklist",
        "recentalbums", "mostplayedalbums", "label", "labelreleases",
        "artistreleases", "awardalbums", "discoverbrowse", "playlistbrowse",
        "playlistmanager", "musician", "scene", "discobuilder",
        "award", "mix"
    ]


    // Capture only the explicit live tab when switching into Kiosk. The
    // ordinary desktop mount and navigation path do not consume this seam.
    Connections {
        target: QbzShell
        function onKioskProfileChanged() {
            if (!QbzShell.kioskProfile || root.kiosk || !viewLoader.item) return
            var item = viewLoader.item
            if (QbzShell.currentView === "search" && typeof item.tab === "number") {
                QbzShell.reportNavState("search", JSON.stringify({
                    activeTab: String(item.tab), query: item.query || ""
                }))
            } else if (typeof item.activeTab === "string") {
                var state = typeof item.navigationStateJson === "string"
                    ? item.navigationStateJson : JSON.stringify({activeTab: item.activeTab})
                QbzShell.reportNavState(QbzShell.currentView, state)
            }
        }
    }

    // THE PAGE'S TAB, LIVE (2026-09-13). Any mounted view with an `activeTab`
    // string that does not report a richer state of its own (Local Library
    // does, and restores it itself) reports its tab on every change: nav_qt
    // stamps it on the history entry, so Back/Forward land on the tab the
    // user left, and page_restore_qt keeps it in the per-profile page
    // document, so "Where you left off" reopens on it. Never during the
    // two-phase route commit, when the item on screen is still the outgoing
    // view while `currentView` already names the next one.
    Connections {
        target: viewLoader.item
        ignoreUnknownSignals: true
        function onActiveTabChanged() { root._reportTab() }
    }
    function _reportTab() {
        var item = viewLoader.item
        if (!item || root._armed || root.kiosk)
            return
        if (typeof item.activeTab !== "string" || typeof item.navigationStateJson === "string")
            return
        QbzShell.reportNavState(QbzShell.currentView, JSON.stringify({ "activeTab": item.activeTab }))
    }
    // The other direction: a Back/Forward destination whose entry carries a
    // tab (armed by nav_qt::step before `currentView` is published) mounts
    // on it. Views that restore their own state consume the arming
    // themselves and are left alone.
    function _restoreTab() {
        var item = viewLoader.item
        if (!item || root.kiosk || QbzShell.restoreStateScope === "")
            return
        if (QbzShell.restoreStateScope !== QbzShell.currentView)
            return
        if (typeof item.activeTab !== "string" || typeof item.restoreNavigationState === "function")
            return
        var saved = null
        try { saved = JSON.parse(QbzShell.stateRestore) } catch (e) { saved = null }
        QbzShell.restoreStateScope = ""
        if (saved && typeof saved.activeTab === "string" && saved.activeTab !== "")
            item.activeTab = saved.activeTab
    }

    // The mounted view. The replacement for AppShell's four `viewLoader.item`
    // references (multi-select: the two Ctrl+A / Escape routers and the two
    // duck-typed reporters) — a QML `id` is scoped to its own document, so
    // those cannot reach into this file and need a public surface.
    readonly property var currentItem: viewLoader.item

    // SYNCHRONOUS — and the reasoning for the failed experiment is kept here so
    // nobody re-runs it.
    //
    // The dead-click complaint ("nothing is happening after the click… ah, it
    // WAS loading, I thought it had died") is real: a route change instantiates
    // the whole view on the UI thread before anything can paint. `asynchronous:
    // true` plus a router-level placeholder looked like the answer, and it made
    // the app MUCH worse — the owner's words were "como si mi conexión fuera de
    // 56kbps" and "nos regresó 10 años atrás".
    //
    // WHY it was worse, because this is the part that is not obvious: async
    // incubation is TIME-SLICED. It yields to the UI thread between slices, so
    // it trades a short freeze for a much longer wall-clock build, and the
    // Loader stays in `Loading` until the ENTIRE tree exists — for Discover >
    // Home that means all 413 cards. A 1s freeze became a five-second skeleton.
    // On a top-level tab the user expects to be instant, that trade is simply
    // the wrong one.
    //
    // WHAT THE REAL FIX TURNED OUT TO BE — and this paragraph used to say the
    // opposite, so read it before acting on a memory of it. It claimed the
    // answer was to "keep the top-level tabs alive across navigation". That is
    // wrong, and it was wrong on the evidence available at the time: BOTH
    // references destroy and re-create the view exactly like this Loader does.
    // Slint gates every view on `if NavState.view == ...` (AppShell.slint:426),
    // which destroys the element tree; Tauri used `{#if activeView === 'home'}`
    // (+page.svelte:6286), which removes it from the DOM — and Tauri even
    // REFETCHES from the network on mount and still transitions instantly.
    // Neither gets its speed from caching the view.
    //
    // What they both do is keep the MOUNT proportional to one viewport, and
    // that is where this port had diverged. Measured 2026-08-17 by driving the
    // real app (`QBZ_QT_NAV_BENCH`, the probe below, and a frame-by-frame read
    // of a screen recording): a click on Discover froze the UI thread for
    // 1.52 s — not slow, FROZEN, zero pixels changing anywhere in the window —
    // where the Slint build painted the same page, artwork included, in a
    // single frame. Inside that time HomeView was building all FOUR of its
    // Discover tabs, because `visible: false` hides an item and does not stop
    // QML from instantiating it, so 19 of 28 section rails belonged to tabs
    // nobody was looking at. Slint runs ONE repeater whose model is chosen by
    // a ternary (HomeView.slint:321) and mounts the rest behind `if`.
    //
    // So the rule is about the SIZE of a mount, not its lifetime: a view may
    // be rebuilt freely as long as it only builds what is on screen. That rule
    // is now enforced statically — qml_eager_tab_audit.py runs in qt-run.sh
    // and fails the build on a heavy per-tab body gated only by `visible:`.
    // Synchronous stays, and is correct — what changed on 2026-08-21 is
    // WHEN it runs, not how. See THE TWO-PHASE ROUTE COMMIT below.

    // Route-change stopwatch. OFF by default (a LoggingCategory at Warning
    // emits nothing at Info), so the only cost on a normal run is one
    // Date.now() per route change. Turn it on with:
    //
    //     QT_LOGGING_RULES="qbz.nav.timing.info=true" ./target/release/qbz
    //
    // It answers the question no static gate can: of the wall-clock between
    // the click and a usable page, how much is QML instantiation (`built`)
    // versus everything the event loop still owes afterwards (`to-idle`).
    LoggingCategory {
        id: navTiming
        name: "qbz.nav.timing"
        defaultLogLevel: LoggingCategory.Warning
    }
    property double _routeT0: 0

    // ── THE TWO-PHASE ROUTE COMMIT (the dead click) ────────────────────────
    //
    // MEASURED 2026-08-21, by driving the real app under Xvfb and diffing the
    // captured frames: a click on Discover > Home left the window showing the
    // OLD page — flyout still open, not one pixel changing anywhere — for
    // 590 ms. The route change is synchronous (read the Loader's header above, which
    // is still correct), so the click handler, the flyout close, the sidebar
    // highlight, the page blank AND the whole mount all happened inside ONE
    // turn of the event loop, and the first frame that could be presented was
    // the one that already had the new page in it. Nothing the user did was
    // acknowledged until everything was finished.
    //
    // The breakdown of that 590 ms is worth keeping, because only ONE of the
    // three parts is what it looks like:
    //     navigate_to (Rust, on the GUI thread)     7.6 ms
    //     bridge queue hop                          1   ms
    //     the QML mount (`built`)                 263   ms
    //     post-mount work before the first frame  ~290  ms   <-- `to-idle`
    // `built` is under half of it. The other half is everything the mount
    // DEFERS — layout and the first delegate refills — which the GUI thread
    // pays before it can reach the event loop again and let a frame out. So
    // `to-idle`, not `built`, is the number that matches the freeze.
    //
    // THE SPLIT: arm on the route change, commit on the next PRESENTED frame.
    // Frame N carries the cheap half — flyout gone, nav highlight moved, page
    // blanked — and only then does frame N+1 pay for the mount. The click is
    // acknowledged in one frame instead of in `to-idle`.
    //
    // `frameSwapped` and not a zero-interval Timer: the timer fires on the
    // next event-loop iteration, which is NOT the same thing as "after a frame
    // reached the screen" — the render pass is scheduled through the event
    // loop too, so a Timer can easily win the race and put the mount back in
    // the same frame, which is the bug. `frameSwapped` is emitted after the
    // buffer swap (verified against this Qt build with a standalone scene),
    // and its queued delivery lands the handler on the GUI thread.
    //
    // Cost: ONE frame (~16 ms) of added latency to the content, in exchange
    // for the whole `to-idle` window becoming visibly alive.

    /// The file the CURRENT route wants. Pure — no side effects — so that the
    /// "two ids, one file" cases (recentalbums / mostplayedalbums) simply do
    /// not change it, and the arm below never fires for them. That is the same
    /// guard the old `_fade.path` holder carried, expressed as the identity of
    /// the thing being watched rather than as a manual comparison.
    readonly property string _wantedPath: root._pathFor(QbzShell.currentView)

    /// What the Loader has actually been given. Assigned, never bound: it
    /// trails `_wantedPath` by exactly one presented frame.
    property string _loaderPath: ""

    /// A route is waiting for its frame.
    property bool _armed: false

    function _pathFor(v) {
        return v === "home"
                ? (root.kiosk ? "../kiosk/KioskDiscover.qml" : "../views/HomeView.qml")
            : v === "library"
                ? (root.kiosk ? "../kiosk/KioskLibrary.qml" : "../views/LibraryView.qml")
            : v === "local"
                ? (root.kiosk ? "../kiosk/KioskLocalLibrary.qml" : "../views/LocalLibraryView.qml")
            : v === "localalbum" ? (root.kiosk ? "../kiosk/KioskLocalAlbum.qml" : "../views/LocalAlbumView.qml")
            : v === "metadataeditor" ? "../controls/TagEditorModal.qml"
            : v === "album"
                ? (root.kiosk ? "../kiosk/KioskAlbum.qml" : "../views/AlbumView.qml")
            : v === "artist"
                ? (root.kiosk ? "../kiosk/KioskArtist.qml" : "../views/ArtistView.qml")
            : v === "settings" ? "../settings/SettingsView.qml"
            : v === "search"
                ? (root.kiosk ? "../kiosk/KioskSearch.qml" : "../views/SearchView.qml")
            : v === "queue-view" ? "../views/QueueView.qml"
            : v === "playlist" ? (root.kiosk ? "../kiosk/KioskPlaylist.qml" : "../views/PlaylistView.qml")
            : v === "discoverbrowse" ? "../views/DiscoverBrowseView.qml"
            : v === "playlistbrowse" ? "../views/PlaylistBrowseView.qml"
            : v === "recentalbums" ? "../views/PlayHistoryView.qml"
            : v === "mostplayedalbums" ? "../views/PlayHistoryView.qml"
            : v === "label" ? "../views/LabelView.qml"
            : v === "labelreleases" ? "../views/LabelReleasesView.qml"
            // Artist page > a release section > "See discography", and the
            // album page's "From the same artist" View all. Same two-file
            // contract as the rest of this chain (nav_qt.rs:9-19):
            // artist_releases_qt::open records the id, this arm mounts it.
            // Forgetting the arm is NOT a crash — the ternary falls through
            // to "" and the pane goes blank with nothing logged, i.e. the
            // dead "See discography" in a new costume.
            : v === "artistreleases" ? "../views/ArtistReleasesView.qml"
            // Artist page > Network > Origin > the location link, and the
            // header ⋯ > "Artist Scene". Both doors call QbzScene.open(...),
            // which records this id (artist_scene_qt.rs:430).
            : v === "scene" ? "../views/ArtistSceneView.qml"
            // A credited musician who did NOT resolve to a Qobuz artist:
            // musician_qt.rs:362 records this on the `contextual` branch only.
            // `weak`/`none` are a global modal, not a route — see AppShell.
            : v === "musician" ? "../views/MusicianPageView.qml"
            // For You > Qobuz Mixes. The whole chain existed — HomeView's tile
            // -> QbzHome.openMix -> foryou_qt -> nav_qt records the "mix" view
            // — and MixView.qml is written and registered in build.rs, but
            // this arm was missing, so every mix tile navigated to a view
            // nobody mounted and the content pane simply went blank.
            : v === "mix" ? "../views/MixView.qml"
            // MyQBZ. Mixtapes and Collections are ONE file — the two pages
            // differ only in their filter row and their empty state, so
            // MyQbzGridView carries a `kind` discriminator (see the Binding
            // below, which is how it gets set). The kiosk does the same with
            // ONE component on both routes (KioskShell.slint:512,517).
            : (v === "mixtapes" || v === "collections")
                ? (root.kiosk ? "../kiosk/KioskMyQBZ.qml" : "../views/myqbz/MyQbzGridView.qml")
            : v === "mixtapedetail" ? (root.kiosk ? "../kiosk/KioskMyQBZDetailLite.qml" : "../views/myqbz/MyQbzDetailView.qml")
            : v === "discobuilder" ? "../views/myqbz/DiscoBuilderView.qml"
            // Settings > Blacklist > Manage. Not reachable from the sidebar —
            // blacklist_qt::open_manager records the route.
            : v === "blacklist" ? "../views/BlacklistManagerView.qml"
            // Settings > Local Library > Library folders > Manage. Same
            // shape as the blacklist arm above: not a sidebar section, and
            // the "library-open-folders" settings action records the route.
            : v === "libraryfolders" ? "../views/LibraryFoldersView.qml"
            // Settings > Offline > Manage offline cache > Open manager. Same
            // shape again; offline_manager_qt::open() records the route.
            : v === "offlinemanager" ? "../views/OfflineManagerView.qml"
            // Awards — reached from the album sidebar's laurel, an "Other
            // awards" card or the landing page's all-awards dropdown. Both
            // routes are recorded by award_qt (open_award / open_albums); the
            // id lives in the controller, not in the route, exactly like
            // "label" / "labelreleases" next to them.
            : v === "award" ? "../views/AwardView.qml"
            : v === "awardalbums" ? "../views/AwardAlbumsView.qml"
            // Sidebar > Playlists ⋯ > Manage playlists. Not a sidebar
            // section — playlist_manager_qt::navigate() records the route.
            : v === "playlistmanager" ? "../views/PlaylistManagerView.qml"
            // Purchases — the opt-in Qobuz store surface (Settings >
            // Appearance > Navigation > Show Purchases, default OFF). The
            // sidebar's direct row records "purchases"; a purchased album's
            // card records "purchase-album" through
            // QbzPurchases.openAlbum(id), which holds the id — the route
            // carries none, exactly like "localalbum".
            //
            // BOTH arms land in the same edit on purpose. This chain's failure
            // mode is a blank pane logged nowhere, and Purchases is the one
            // feature nobody on this team can smoke-test (the owner's region
            // does not sell it, so the account returns an empty list forever):
            // a missing arm here would first be seen by a stranger.
            : v === "purchases" ? "../views/PurchasesView.qml"
            : v === "purchase-album" ? "../views/PurchaseAlbumView.qml"
            // KIOSK-ONLY route (nav_qt.rs:46-51): the NavRail's fifth tile,
            // the full-screen player. The desktop shell has no equivalent —
            // its transport is the persistent bar — so outside kiosk this id
            // falls through to "" and the pane is blank, which is the desktop
            // behaviour before this router existed.
            : (root.kiosk && v === "nowplaying") ? "../kiosk/KioskNowPlaying.qml" : ""
    }

    /// ARM. Runs the instant the route resolves to a DIFFERENT file: blank the
    /// page and start the stopwatch, but do not touch the Loader yet.
    // `on_Wanted...`, NOT `onWanted...`: QML derives a change handler's name by
    // upper-casing the property's FIRST letter, and for `_wantedPath` that
    // first letter is the underscore — so the handler is `on_WantedPathChanged`
    // (the same shape `theme/RoundedImage.qml` already uses for
    // `on_EffectiveSourceChanged` and `on_DprChanged`).
    //
    // Worth the comment because of HOW it fails: `onWantedPathChanged` is not a
    // syntax error, qmlcachegen compiles it without a word, and the app starts
    // fine — the first route is armed by Component.onCompleted below. Every
    // navigation AFTER that silently does nothing. Proven against this Qt build
    // with a standalone scene before it could ship.
    on_WantedPathChanged: {
        root._routeT0 = Date.now()
        root._armed = true
        // An unmapped route unloads the Loader; there is nothing to reveal, so
        // there is nothing to blank either.
        if (root._wantedPath !== "" && !QbzShell.reduceMotion) {
            // Stop first: a second navigation inside the 300 ms would
            // otherwise have a running render-thread Animator writing the
            // property back from underneath this assignment.
            fadeIn.stop()
            viewLoader.opacity = 0
            fadeGuard.restart()
        }
        commitGuard.restart()
    }

    function _commit() {
        if (!root._armed)
            return
        commitGuard.stop()
        // ORDER MATTERS. `_armed` is what keeps the two Bindings below OFF the
        // OUTGOING view (see their `when`), and the Loader builds the new item
        // SYNCHRONOUSLY inside this assignment — so clearing the flag first
        // would re-evaluate those `when` clauses one statement too early, while
        // `viewLoader.item` is still the view on its way out. Assign, then
        // disarm.
        root._loaderPath = root._wantedPath
        if (root.kiosk) {
            var properties = root.kioskHostRoutes.indexOf(QbzShell.currentView) >= 0 ? {kioskHost: true} : {}
            viewLoader.setSource(root._loaderPath, properties)
        }
        root._armed = false
    }

    /// The frame that carries the acknowledgement has reached the screen —
    /// build the view now.
    Connections {
        target: root.Window.window
        ignoreUnknownSignals: true
        function onFrameSwapped() { root._commit() }
    }

    /// Backstop, and NOTHING ELSE. A window that is minimized or hidden
    /// presents no frames at all, and a route that never commits is a
    /// permanently blank content pane — the worst failure this file can
    /// produce. So there has to be a timer; the only question is how long.
    ///
    /// IT WAS 32 ms AND THAT WAS A RACE, measured 2026-08-21 by capturing two
    /// navigations frame by frame: Discover > Home got its acknowledgement
    /// frame at +90 ms, and Library > All got NONE — one step at +290 ms with
    /// nothing before it, i.e. the old dead click, on the same build, minutes
    /// apart. The path is render-at-the-next-vsync (16.67 ms on this driver,
    /// confirmed with QSG_INFO=1) plus a QUEUED delivery of `frameSwapped`
    /// back to the GUI thread, so the honest range is ~16-33 ms and a 32 ms
    /// guard wins the coin flip often enough to make navigation feel
    /// inconsistent — which is worse than being slow, because the user cannot
    /// learn it.
    ///
    /// 250 ms puts the guard far outside that range: `frameSwapped` wins on
    /// every visible window, and the only thing the timer still covers is the
    /// case it was written for, where nobody can see the delay anyway.
    Timer {
        id: commitGuard
        interval: 250
        repeat: false
        onTriggered: root._commit()
    }

    /// THE FIRST ROUTE. `on_WantedPathChanged` covers every navigation, but the
    /// startup view arrives as the INITIAL evaluation of the binding, and
    /// whether that counts as a change is not something to bet the whole
    /// content pane on: if it does not fire, `_loaderPath` stays "" and the
    /// app opens to a blank page with nothing logged. Arm it explicitly.
    Component.onCompleted: {
        if (root._loaderPath !== root._wantedPath) {
            root._armed = true
            commitGuard.restart()
        }
    }

    // --- Page fade ---------------------------------------------------------
    // COSMETIC, FIXED DURATION, AND IT NEVER WAITS FOR CONTENT. Only the
    // REVEAL is animated: the page snaps to transparent when the route is
    // ARMED (the frame that acknowledges the click), and fades back in from
    // the moment the view has been built. There is no fade-OUT because there
    // is nothing to fade out of — the acknowledgement frame is where the old
    // page leaves, and animating that would put a 300 ms wait in front of a
    // navigation the whole two-phase commit exists to make feel instant.
    //
    // OpacityAnimator, not NumberAnimation, and this is the whole trick: an
    // Animator runs on the RENDER thread, so it keeps advancing while the GUI
    // thread is still paying off everything the mount deferred (layout, the
    // first delegate refills). Measured on this shell with the thread blocked:
    // OpacityAnimator advanced 0.53 where NumberAnimation advanced 0.11. The
    // shared QbzShell.pulseMs cannot serve this: the pulse arrives through
    // `ui(...)` onto the Qt event loop (shell_bridge.rs:966), i.e. the very
    // thread the mount is blocking, so a pulse-driven fade would freeze during
    // exactly the milliseconds it exists to cover.
    //
    // GPU doctrine (qt-frontend/2026-08-11-scenegraph-batches §9) forbids
    // CONTINUOUS animation off the shared pulse, because each dirty frame
    // presents the whole window at ~1.2% GPU. This one is bounded and
    // user-initiated: one navigation buys ~18 presents at 60 Hz, and at rest
    // it writes NOTHING (the Animator is stopped and opacity is a flat 1).
    //
    // 300 ms. The contract's first proposal was 120-150 ms; at 140 the owner
    // could not see it at all. Part of that was the duration and part was a
    // real defect (see the note on WHAT fades, below) — 300 fixes the half
    // that is duration, and is still one bounded animation per navigation.
    property int fadeMs: 300

    // WHAT FADES, and why this is the page and not a veil over it.
    //
    // The first cut of this faded a Rectangle laid over the content, coloured
    // from the pane it sits in. That is wrong here, and invisibly so: AppShell
    // paints the pane `surface-main` normally but `surface-main @ 0.22` while
    // the ambient background is on (AppShell.qml:324, QbzTheme.qml:38), and the
    // ambient background is on whenever a track is loaded (QbzTheme.qml:132).
    // So in the configuration the app actually runs in, a veil at FULL opacity
    // covered 22% of the page and the fade read as a flicker. Fading the page
    // itself has no such failure mode, needs no colour at all, and keeps the
    // ambient art visible underneath — which a veil opaque enough to work would
    // have blacked out for the length of the fade.
    //
    // No `layer.enabled`: group opacity would cost an FBO the size of the
    // content pane, allocated at the exact moment the mount is already the most
    // expensive thing on screen. Per-node alpha lets overlapping children show
    // through each other, which is the honest trade — and at 300 ms on a page
    // that is arriving rather than sitting still, it is not visible.
    //
    // reduceMotion (shell_bridge.rs:217) skips the whole thing: the page simply
    // appears, which is the behaviour before this block existed.

    // Watchdog. `onLoaded` is the only thing that brings the page back, and an
    // invisible content pane is the worst failure this file can produce, so a
    // missed signal (a view that fails to instantiate, a source that resolves
    // to nothing) must not be able to leave it at zero.
    Timer {
        id: fadeGuard
        interval: 1500
        repeat: false
        onTriggered: viewLoader.opacity = 1
    }

    OpacityAnimator {
        id: fadeIn
        target: viewLoader
        from: 0.0
        to: 1.0
        duration: root.fadeMs
        easing.type: Easing.OutCubic
    }

    // TABS FADE TOO. A tab switch inside a view (Discover's four, Library's,
    // Local Library's) never touches the Loader's `source`, so none of the
    // machinery above fires — the page changed under the user with a hard cut
    // while a route change faded. Same transition, same duration, for what is
    // the same thing from where the user sits.
    //
    // `activeTab` is the ONE name every tabbed view uses for this (the Binding
    // further down already routes the nav flyout through it), so watching it
    // here covers all of them from one place instead of a copy per view.
    // `ignoreUnknownSignals` is what lets the untabbed views mount unharmed:
    // without it a Connections whose target has no such signal is an error.
    //
    // No watchdog and no opacity assignment: a tab body is built synchronously
    // inside the view, in this same turn, so by the time a frame renders there
    // is something to reveal — `restart()` alone sets the page to `from` (0)
    // and animates it back. The `running` guard keeps a tab that is stamped
    // right after a route change from restarting the fade that route already
    // started.
    Connections {
        target: viewLoader.item
        ignoreUnknownSignals: true
        function onActiveTabChanged() {
            if (!QbzShell.reduceMotion && !fadeIn.running)
                fadeIn.restart()
        }
    }

    Loader {
        id: viewLoader
        anchors.fill: parent
        source: root.kiosk ? "" : root._loaderPath
        visible: !root.kiosk || !root._armed

        onLoaded: {
            // The reveal starts in the same turn the view finished building,
            // i.e. before a single frame of it has been presented. The render
            // thread picks the Animator up at that first frame, so the fade is
            // exactly as long as it says it is no matter how busy the GUI
            // thread still is.
            fadeGuard.stop()
            if (QbzShell.reduceMotion) {
                viewLoader.opacity = 1
            } else {
                fadeIn.restart()
            }
            var built = Date.now() - root._routeT0
            console.info(navTiming, "[navtiming] " + QbzShell.currentView
                         + " built=" + built + "ms at=" + Date.now())
            // One more turn of the event loop: everything the mount deferred
            // (layout, the first delegate refills, the pending property
            // updates) has run by the time this fires, so the delta is the
            // honest "how long until the page stops owing work" number.
            var t0 = root._routeT0
            var view = QbzShell.currentView
            Qt.callLater(function () {
                console.info(navTiming, "[navtiming] " + view
                             + " to-idle=" + (Date.now() - t0) + "ms")
            })
        }
    }

    // Kiosk acknowledges the route with cheap static geometry during the
    // existing one-frame commit split. No remote model or image is mounted.
    Loader {
        anchors.fill: parent
        active: root.kiosk && root._armed
        sourceComponent: Component {
            KioskSkeleton {
                loading: true
                kind: ["album", "localalbum", "nowplaying"].indexOf(QbzShell.currentView) >= 0
                    ? "list" : "grid"
            }
        }
    }

    // MyQbzGridView serves BOTH the "mixtapes" and "collections" routes
    // and needs to know which. A Loader cannot pass properties through a
    // declarative `source:` ternary, and the view's own default is
    // "mixtape" — so without this the Collections route would render the
    // Mixtapes page, permanently, with nothing logged.
    //
    // A Binding rather than `onLoaded`/`setSource`: it applies the moment
    // `item` exists (the Loader creates it synchronously, so still before
    // the first render pass), it re-applies if the route flips
    // mixtapes <-> collections while the same instance is mounted, and it
    // keeps the whole router declarative. Same shape as the seeded-field
    // pattern at controls/QbzLineEdit.qml:174-179.
    //
    // RestoreNone: the target is DESTROYED on unload, and the default
    // RestoreBindingOrValue would try to write the old value back into a
    // dying object.
    //
    // It applies in KIOSK too, and must: KioskMyQBZ serves the same two
    // routes as one component (KioskShell.slint:512,517), so without the
    // discriminator Collections would render the Mixtapes page there for
    // exactly the same reason.
    //
    // `!root._armed` IS LOAD-BEARING ON BOTH BINDINGS BELOW, and it is newer
    // than the rest of these comments. The two-phase commit means
    // `QbzShell.currentView` is the NEW route for one frame while
    // `viewLoader.item` is still the OLD view — so without it, every
    // navigation applies the incoming route's discriminators to the OUTGOING
    // view.
    //
    // That is not theoretical: it shipped for one build and a frame capture
    // caught it. Library -> Discover > Home wrote `activeTab = "home"` onto
    // the LIBRARY view (which has an `activeTab`, so the `typeof` guard passes
    // happily), whose tabs are all/tracks/albums/… — the body emptied, the tab
    // bar stayed with nothing selected, and the `onActiveTabChanged` that
    // followed restarted the page fade and brought the OUTGOING page back up
    // over the blank. Visible as a flash of the previous section, mid-flight.
    //
    // The `typeof` guards stay as well: they are what keeps the `kind` Binding
    // from warning against views that have no such property at all.
    Binding {
        target: viewLoader.item
        property: "kind"
        value: QbzShell.currentView === "collections" ? "collection" : "mixtape"
        when: !root._armed
              && viewLoader.item !== null
              && (QbzShell.currentView === "mixtapes"
                  || QbzShell.currentView === "collections")
              && typeof viewLoader.item.kind === "string"
        restoreMode: Binding.RestoreNone
    }

    // NavFlyout landing tab (Discover > For You, Library > Albums, Local >
    // Folders...). The request rides the bridge (QbzShell.navigateToTab ->
    // navTab/navTabView/navTabSeq) because a QML id cannot cross documents;
    // this Binding is the ONLY writer. It replaces NavFlyout's depth-capped
    // scene search (findTabHost), which the ContentRouter extraction (kiosk
    // port D3) silently outran — every flyout entry landed on the section's
    // default tab instead of its own.
    //
    // `navTabSeq` in the value expression forces re-application when the
    // same entry is clicked twice in a row (a bare navTab would not
    // renotify). `navTabView === currentView` keeps a cross-view request
    // from being stamped on the outgoing view before the route lands. The
    // typeof guard leaves non-tabbed views alone rather than warning about
    // a non-existent property. RestoreNone, like the `kind` Binding above:
    // the target is destroyed on unload. An in-view tab-bar click writes
    // `activeTab` directly and simply wins until the next flyout click —
    // this Binding does not refire without a dependency change.
    /// Does the MOUNTED view have an `activeTab` at all?
    ///
    /// Captured in a HANDLER, never inside the Binding's `when`. Reading
    /// `typeof viewLoader.item.activeTab` from `when` makes the condition
    /// depend on the very property the Binding WRITES, and QML reports the
    /// cycle on every flyout navigation:
    ///
    ///   QML Binding: Binding loop detected for property "when"
    ///
    /// (Pre-existing — reproduced on the pre-2026-08-21 binary by navigating
    /// through a nav flyout, where it logs from the same Binding at its own
    /// line number.) A handler runs once per item change and registers no
    /// dependency, so the guard survives and the cycle does not.
    property bool _itemTabbed: false
    property bool _itemTabRequest: false
    Connections {
        target: viewLoader
        function onItemChanged() {
            root._itemTabbed = viewLoader.item !== null
                && typeof viewLoader.item.activeTab === "string"
            root._itemTabRequest = viewLoader.item !== null
                && typeof viewLoader.item.tabNavigationRequest !== "undefined"
            root._restoreTab()
        }
    }

    // Opening ad-hoc media is a GLOBAL navigation action. The old listener
    // lived only inside LocalLibraryView, so opening a folder/disc from
    // Settings or another section completed successfully but left the user on
    // the old page. The sequence changes only for an explicit user open (not
    // boot rehydrate); navigateToTab avoids duplicate history when Local
    // Library is already mounted and applies the tab after a fresh mount.
    Connections {
        target: QbzLocal
        function onLocalEphemeralOpenSeqChanged() {
            QbzShell.navigateToTab("local", "ephemeral")
        }
    }

    Binding {
        target: viewLoader.item
        property: root._itemTabRequest ? "tabNavigationRequest" : "activeTab"
        when: !root._armed
              && root._itemTabbed
              && QbzShell.navTab !== ""
              && QbzShell.navTabView === QbzShell.currentView
        value: root._itemTabRequest
            ? ({ "tab": QbzShell.navTab, "sequence": QbzShell.navTabSeq })
            : (QbzShell.navTabSeq, QbzShell.navTab)
        restoreMode: Binding.RestoreNone
    }
}
