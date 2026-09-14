// App shell — the QML port of crates/qbz-ui/ui/shell/AppShell.slint's
// chrome: HeaderBar (top, 42px) / { Sidebar | content frame | queue
// column } / NowPlayingBarSmall (bottom).
//
// The window chrome is surface-card throughout; the content area is a
// ROUNDED surface-main panel inset 8px left/right/bottom (0 top — it
// butts the header), the "Slack-style bezel on all four corners" of
// AppShell.slint:358-390. The recovery affordance and logout live in the
// header (offline badge flyout + app menu), like the Slint shell.
//
// The artwork-derived ambient background IS implemented, in BOTH of the
// reference's modes (AppShell.slint:206-242): 1 = Ambient, the album-triad
// metaball field (AmbientField) under a tunable dark scrim; 2 = Blurred art,
// the ImmersiveAtmosphere cover look, which carries its own `dim` and takes no
// scrim. `theme.ambientOn` says "a mode is picked and a track is loaded";
// `QbzShell.ambientMode` says WHICH. Until 2026-08-11 mode 2 rendered as mode 1
// (ambient_qt.rs said so out loud), so the two settings looked identical.
//
// THE LAYERING, which is the part that has to be exact. Three tiers, not one:
//   chrome  (Sidebar / HeaderBar / NPB / queue column / the CONTENT FRAME)
//           -> surface-card @ 0.5
//   panel   (the inset content pane) -> surface-main @ 0.22 + a 1px hairline,
//           composited ON TOP OF the frame, because in the reference it is the
//           frame's CHILD (AppShell.slint:358-414)
//   views   -> transparent, with their thin bars at surface-main @ 0.3 and
//           their controls at surface-elevated @ 0.5
// The frame tier was missing here: `contentFrame` was a sibling of the shell
// root, so the 8px gutter around the pane showed the RAW field instead of
// card @ 0.5, and the pane composited over the raw field too. Measured on the
// owner's side-by-side: Qt's sidebar (27,28,34) against its gutter (50,52,64) —
// a bright seam at x=240 — where Slint's sidebar (79,72,23) and gutter
// (82,74,20) are the same surface and read as one continuous frame.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../immersive"
import "../theme"

Rectangle {
    id: root
    // OPAQUE base (the Slint root background is opaque surface-main too —
    // invisible while the ambient field mounts over it; this IS the D4
    // no-track fallback). The translucent chrome comes from each chrome
    // piece's OWN background going surface-card @ 0.5 above the field.
    color: theme.surfaceCard

    // App-wide dynamic background (phase 14): active when the mode pref is
    // on AND a track is loaded (D4: no track -> opaque theme restored). The
    // predicate itself now lives on the theme object so the ~25 surfaces that
    // read it cannot drift apart (they did: the carousel fades and the header
    // search field never got it).
    readonly property bool ambientOn: theme.ambientOn
    // Window-level history navigation: mouse side buttons + two-finger
    // horizontal touchpad swipe (#653 parity). Declared FIRST so the whole UI
    // hit-tests on top of it — a scroll reaches it only when nothing above
    // consumed it, which is what keeps horizontal carousels scrolling instead
    // of navigating. Never consumes a wheel event; accepts only the two side
    // buttons.
    NavGestureLayer {
        anchors.fill: parent
    }

    // --- The background layers (AppShell.slint:206-242) --------------------
    // The bottom-most visual layer, declared before every chrome surface so
    // they all paint above it. Mode 1 and mode 2 are DIFFERENT looks and are
    // mutually exclusive; neither is mounted while a mode is off or nothing is
    // playing, which is the D4 "opaque theme restored" case.
    readonly property bool ambientModeOn: root.ambientOn && QbzShell.ambientMode === 1
    readonly property bool blurredModeOn: root.ambientOn && QbzShell.ambientMode === 2

    // --- The polarity-aware legibility veil (owner, 2026-08-31) ------------
    // The old scrim was a fixed BLACK layer at ambientDim — tuned for dark
    // themes, where dark ground + light text is exactly right. Over a LIGHT
    // theme the same dark scrim pushed the field to MID luminance, the worst
    // possible ground for dark text (the weak grey ramp died completely).
    // Light themes therefore veil with their own near-white surfaceMain, a
    // notch stronger, and the in-shader/in-atmosphere darkening is turned
    // OFF there (darkening and then whitening would just grey the field).
    // Dark themes keep the exact previous look.
    readonly property real veilStrength: theme.isDark
        ? QbzShell.ambientDim
        : Math.min(0.72, QbzShell.ambientDim + 0.2)

    // Mode 1 — the album-triad metaball field, plus the veil that keeps text
    // legible over a bright album palette (QBZ_BG_DIM, default 0.35).
    AmbientField {
        anchors.fill: parent
        visible: root.ambientModeOn
        running: root.ambientModeOn
        dim: theme.isDark ? QbzShell.ambientDim : 0.0
    }
    Rectangle {
        anchors.fill: parent
        visible: root.ambientModeOn
        color: theme.isDark ? "#000000" : theme.surfaceMain
        opacity: root.veilStrength
    }

    // Mode 2 — Blurred art: the SAME ImmersiveAtmosphere the immersive view
    // and the album/artist headers use, at window size (AppShell.slint:221-231
    // reuses the identical component). `animated` follows the transport, so a
    // paused player holds the static pose instead of drifting forever, and the
    // fallback is the plain cover for a track whose atmosphere bitmap has not
    // been generated yet. On light themes its internal dark dim is disabled
    // and the veil below provides the legibility layer instead (the
    // atmosphere's baked gradient scrim stays — it reads as depth under the
    // light veil, not as darkness).
    ImmersiveAtmosphere {
        anchors.fill: parent
        visible: root.blurredModeOn
        source: root.blurredModeOn ? QbzImmersive.atmosphereUrl : ""
        fallbackSource: root.blurredModeOn ? QbzPlayer.npArtworkPath : ""
        animated: root.blurredModeOn && QbzPlayer.npPlaying
        dim: theme.isDark ? QbzShell.ambientDim : 0.0
    }
    Rectangle {
        anchors.fill: parent
        visible: root.blurredModeOn && !theme.isDark
        color: theme.surfaceMain
        opacity: root.veilStrength
    }

    // Mode 3 — Wallpaper: the window reads as translucent to the desktop:
    // ALWAYS the part of the wallpaper that lies under it, wherever it is
    // moved (X11, macOS and Windows say where the window is; KDE Plasma
    // Wayland says so through its window-management protocol; anywhere
    // else a centred crop stands in), lightly blurred so nothing in the
    // picture competes with the text, yet still recognisable. Static: it
    // costs a frame only when the window moves or resizes
    // (shell/WallpaperField.qml).
    // Mode 4 — Wallpaper, blurred: Blurred art with the wallpaper in the
    // cover's place — the SAME atmosphere pass, the SAME drift while the
    // transport plays, the same still pose when it does not.
    // Neither waits for a playing track (QbzTheme.ambientOn).
    readonly property bool wallpaperModeOn: root.ambientOn && QbzShell.ambientMode === 3
    readonly property bool wallpaperBlurOn: root.ambientOn && QbzShell.ambientMode === 4
    WallpaperField {
        anchors.fill: parent
        visible: root.wallpaperModeOn
        source: root.wallpaperModeOn ? QbzShell.wallpaperUrl : ""
        hostWindow: root.hostWindow
        blur: 0.6
        dim: theme.isDark ? QbzShell.ambientDim : 0.0
    }
    Rectangle {
        anchors.fill: parent
        visible: root.wallpaperModeOn && !theme.isDark
        color: theme.surfaceMain
        opacity: root.veilStrength
    }
    ImmersiveAtmosphere {
        anchors.fill: parent
        visible: root.wallpaperBlurOn
        source: root.wallpaperBlurOn ? QbzShell.wallpaperAtmosphereUrl : ""
        animated: root.wallpaperBlurOn && QbzPlayer.npPlaying
        dim: theme.isDark ? QbzShell.ambientDim : 0.0
    }
    Rectangle {
        anchors.fill: parent
        visible: root.wallpaperBlurOn && !theme.isDark
        color: theme.surfaceMain
        opacity: root.veilStrength
    }
    // The wallpaper can change while QBZ runs: re-resolve when the window
    // comes back to the front (wallpaper_qt memoises path + mtime, so a
    // wallpaper that did not change republishes nothing).
    Connections {
        target: root.hostWindow
        ignoreUnknownSignals: true
        function onActiveChanged() {
            if (root.hostWindow && root.hostWindow.active && QbzShell.ambientMode >= 3)
                QbzShell.refreshWallpaper()
        }
    }

    // The host ApplicationWindow (custom chrome: drag / maximize / resize).
    property var hostWindow: null

    // §1.4.1 + §4.1 (2026-08-03 hotkeys-port contract): the shell root is
    // BOTH the dispatcher host and the fallback focus item — Qt delivers key
    // events only to activeFocusItem and propagates up its parent chain, and
    // the shell tree had ZERO focus items, so without this a fresh launch
    // has activeFocusItem == null and the pipeline receives NOTHING until
    // the first click (the round-1 BLOCKER; H0 is the RFB proof). The
    // marker lets a modal anywhere in the tree walk up and re-focus this
    // root on close (§1.4.2/§1.4.3).
    focus: true
    readonly property bool isQbzShellRoot: true

    // The assert half of §1.4.1: declarative `focus: true` alone did NOT
    // survive the splash -> shell Loader swap in a WM-less (VNC) session —
    // activeFocusItem stayed null and the dispatcher received nothing
    // until the first click (measured 2026-08-03, RFB H0 first pass). Grab
    // once at mount; every other lifecycle arm (immersive close, modal
    // closes) hands focus BACK here.
    Component.onCompleted: {
        root.forceActiveFocus()
        Qt.callLater(function () { QbzAbout.updatesLaunch() })
    }

    // THE ONE key entry (§1.1 route (b), divergence K1): NOTHING is handled
    // locally. The ordered pipeline (capture steal, search-dropdown Up/Down
    // steal, central text-input gate, Ctrl+A, binding dispatch, the §1.2
    // Escape stack) lives in Rust behind QbzHotkeys.keyPressed; the QML side
    // only computes the gate (§1.4.4). Null activeFocusItem passes the gate
    // (the semantically right case). Every existing Keys. handler accepts
    // only what it owns. Shared controls accept their Space/Enter events
    // before they bubble here; in particular a focused Settings toggle owns
    // Space, so toggling it cannot also trigger global play/pause (§4.1).
    InputFocusDismiss { id: inputFocusDismiss }
    Connections {
        target: QbzShell
        function onCurrentViewChanged() { inputFocusDismiss.dismiss() }
    }

    Keys.onPressed: function (event) {
        var w = root.Window.window
        var afi = w !== null ? w.activeFocusItem : null
        var textInputFocused = (afi instanceof TextInput) || (afi instanceof TextEdit)
        event.accepted = QbzHotkeys.keyPressed(event.key, event.modifiers,
                                               event.text, textInputFocused)
    }

    // --- Hotkeys routers (§4.3 / §4.6) -------------------------------------
    // nav.search (Ctrl+f, divergence K6 — EXCEEDS Slint, whose focus_search
    // only flipped cortinilla_open with no visible effect on an empty
    // query): the Rust dispatch emits this signal and the field lands
    // focused and ready to type.
    Connections {
        target: QbzHotkeys
        function onFocusSearchRequested() { header.focusSearch() }
        // playback.favorite — the player bar's own heart call, empty-id guard
        // included. The now-playing track id is a QbzPlayer property, so the
        // Rust dispatch signals and QML answers rather than resolving the id
        // twice (PlayerBar.qml:769-773).
        function onFavoriteRequested() {
            var id = QbzPlayer.npTrackId
            if (id !== "") QbzQueue.queueToggleFavorite("track", id)
        }
    }

    // The §4.6 Ctrl+A + multi-select Escape-exit seam. Selection state is
    // in-view QML JS (library_bulk.rs:8: "select-all / clear never reach
    // Rust"), so the QbzShell signals route to the mounted view's
    // duck-typed interface: selectAll() / exitMultiSelectMode() /
    // multiSelectOn. Implemented by LibraryView, LocalLibraryView and
    // LocalAlbumView (MyQbzDetailView is EXIT-ONLY — its selection lives in
    // Rust with no select-all arm); views without multi-select (Artist,
    // Playlist, Mix, Label, Offline) match nothing — PARITY-DEBT (K4).
    Connections {
        target: QbzShell
        function onSelectAllRequested() {
            if (contentRouter.currentItem !== null
                    && typeof contentRouter.currentItem.selectAll === "function")
                contentRouter.currentItem.selectAll()
        }
        function onExitMultiSelectRequested() {
            if (contentRouter.currentItem !== null
                    && typeof contentRouter.currentItem.exitMultiSelectMode === "function")
                contentRouter.currentItem.exitMultiSelectMode()
        }
    }
    // The reporters: their QbzShell mirrors feed the Rust Ctrl+A
    // consumption predicate (capable) and the §1.2 Escape stack arm 6
    // (active). Duck-typed, so a view without the interface reports
    // capable=false and Ctrl+A falls through to the binding lookup (the
    // Slint select_all_active_surface false case).
    Binding {
        target: QbzShell
        property: "multiSelectCapable"
        value: contentRouter.currentItem !== null
               && typeof contentRouter.currentItem.selectAll === "function"
        restoreMode: Binding.RestoreNone
    }
    Binding {
        target: QbzShell
        property: "multiSelectActive"
        value: contentRouter.currentItem !== null
               && contentRouter.currentItem.multiSelectOn === true
        restoreMode: Binding.RestoreNone
    }

    QbzTheme { id: theme }

    HeaderBar {
        id: header
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: theme.headerHeight
        hostWindow: root.hostWindow
        onReportIssueRequested: reportIssueModal.open = true
        // Square corners (phase 12: the window is opaque; any rounding is
        // the compositor's business).
    }

    OrbitBanner {
        id: orbitBanner
        anchors.top: header.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        remoteActive: QbzOrbit.enabled && QbzOrbit.controllingRemote
        connectionLost: QbzOrbit.connectionLost
        hostName: QbzOrbit.hostName
    }

    // The bottom chrome band (NowPlayingBar.qml's root): full width, flush
    // on the window's bottom edge, the same tier and paint as the HeaderBar
    // above. The bar layout inside it keeps the content pane's gutter to the
    // window's left, right and bottom edges, the same on every platform —
    // and the band sizes itself: the mode-aware layout height (Small
    // collapses to one header-tall row; New/Classic/Large keep the full
    // 112px, AppShell.slint:396) plus that gutter.
    NowPlayingBar {
        id: npb
        // The small seek thumb/hit area extends above the bar by a few pixels.
        // Only Small needs this overlap. Large's cover dock is a later
        // sibling and must paint over the full-width player background.
        z: QbzShell.npbMode === 2 ? 1 : 0
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        // The shared hover-tooltip overlay (declared further down — id
        // references resolve at completion). All four modes consume it for
        // Shuffle/Repeat state; the full bar also uses it for Qobuz Connect.
        tooltip: tooltipOverlay
    }

    Sidebar {
        id: sidebar
        anchors.left: parent.left
        anchors.top: orbitBanner.bottom
        anchors.bottom: npb.top
        // The shared hover-tooltip overlay (declared last, below). The sidebar
        // clips its own overflow, so the collapsed rail's name bubble HAS to be
        // rendered out here — same reason Slint mounts SidebarTooltip at the
        // AppShell level rather than inside Sidebar.slint.
        tooltip: tooltipOverlay
        // Animated 3-state width lives inside the component.
        resizing: sidebarHandle.active
    }

    // QueueView temporarily takes the queue drawer's visual slot without
    // rewriting the user's logical toggle. Leaving the view therefore
    // restores the exact sidebar state that was active before entering it;
    // Lyrics remains independent and may stay open beside the full view.
    readonly property bool queueSidebarVisible: QbzShell.queueOpen
        && QbzShell.currentView !== "queue-view"

    // Right-side panel column — Queue and/or Lyrics, stacked vertically in
    // a shared 300px column (Feishin-style, AppShell.slint:684-707). Each
    // is toggled from its bar button and closed from its own X; the column
    // is visible when either is open, animated 0 <-> 300 (160ms).
    Rectangle {
        id: queueColumn
        anchors.right: parent.right
        anchors.top: orbitBanner.bottom
        anchors.bottom: npb.top
        // #771: the open width is the user's (300..600, persisted).
        width: (root.queueSidebarVisible || QbzShell.lyricsOpen)
            ? QbzShell.queuePanelWidth : 0
        clip: true
        color: root.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard

        Behavior on width {
            enabled: !queueHandle.active
            NumberAnimation { duration: 160; easing.type: Easing.InOutQuad }
        }

        // Loader-gated: while the drawer is closed the panel is not built.
        // Its rows are paginated (PAGE_SIZE 40) so this one was never the
        // freeze, but a closed panel should not be constructed on principle —
        // and the audit cannot know about a cap that lives in Rust.
        Loader {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.top: parent.top
            height: QbzShell.lyricsOpen
                ? (root.queueSidebarVisible ? parent.height / 2 : 0)
                : parent.height
            active: root.queueSidebarVisible
            visible: active
            sourceComponent: QueuePanel { }
        }
        Rectangle {
            visible: root.queueSidebarVisible && QbzShell.lyricsOpen
            anchors.left: parent.left
            anchors.right: parent.right
            y: QbzShell.lyricsOpen && root.queueSidebarVisible ? parent.height / 2 : 0
            height: 1
            color: theme.borderSubtle
        }
        LyricsPanel {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: QbzShell.lyricsOpen
                ? (root.queueSidebarVisible ? parent.height / 2 - 1 : parent.height)
                : 0
            visible: QbzShell.lyricsOpen
        }
    }

    // --- #771 resize handles ------------------------------------------------
    // Siblings of the panels, not children: Sidebar clips and the column
    // animates, and a handle inside either would be clipped or dragged along.
    // Each sits on the border the user SEES — the content PANE's edge, 8px
    // past the panel (the bezel between them is panel-coloured, so the
    // panel's own edge is invisible) — nudged 1-2px toward its panel, above
    // the content frame in z. The bridge (`panel_resize.rs`) decides
    // clamp/snap/close; these only report the pointer, and pulse their notch
    // when a drag lands on a snap point.
    PanelResizeHandle {
        id: sidebarHandle
        // No edge to grab once the sidebar is closed; the header button
        // reopens it, as before.
        visible: QbzShell.sidebarState !== 2
        // Pane edge at sidebar.width + 8; strip 245..249 at 240, centre 247.
        x: sidebar.width + 5
        anchors.top: orbitBanner.bottom
        anchors.bottom: npb.top
        z: 5
        onDragged: function (px) { QbzShell.sidebarDrag(px) }
        onReleased: QbzShell.sidebarDragEnd()
    }
    PanelResizeHandle {
        id: queueHandle
        visible: queueColumn.width > 0
        // Pane edge at queueColumn.x - 8; strip x-9..x-4, centre x-6.5.
        x: queueColumn.x - 9
        anchors.top: orbitBanner.bottom
        anchors.bottom: npb.top
        z: 5
        // The column hangs off the window's RIGHT edge: a pointer at x means
        // a width of (window width - x).
        onDragged: function (px) { QbzShell.queuePanelDrag(root.width - px) }
        onReleased: QbzShell.queuePanelDragEnd()
        // Parked at the maximum with the QUEUE in the column: there is more
        // past here (push through -> Listen List). Lyrics-only: nothing.
        beyond: active && root.queueSidebarVisible
            && QbzShell.queuePanelWidth === theme.queuePanelWidth * 2
        beyondSide: -1
    }
    // Snap feedback: the bridge clamps and snaps; a width landing EXACTLY on
    // a range end, or a state snap, while the pointer is down is the moment
    // the notch pulses. A drag inside the range changes the width on every
    // move and never hits these equalities twice in a row.
    Connections {
        target: QbzShell
        function onSidebarWidthChanged() {
            if (sidebarHandle.active
                    && (QbzShell.sidebarWidth === theme.sidebarOpenWidth
                        || QbzShell.sidebarWidth === theme.sidebarOpenWidth * 2))
                sidebarHandle.pulse()
        }
        function onSidebarStateChanged() {
            if (sidebarHandle.active)
                sidebarHandle.pulse()
        }
        function onQueuePanelWidthChanged() {
            if (queueHandle.active
                    && (QbzShell.queuePanelWidth === theme.queuePanelWidth
                        || QbzShell.queuePanelWidth === theme.queuePanelWidth * 2))
                queueHandle.pulse()
        }
    }

    // Content FRAME — the chrome slab the content pane is inset INTO
    // (AppShell.slint:357-362). It is the same tier as the sidebar, the header
    // and the player bar, and under the dynamic background it takes the same
    // surface-card @ 0.5: the 8px gutter it leaves around the pane is the
    // "bezel", and it must read as one continuous frame with the sidebar, not
    // as a bright band of raw background between them.
    //
    // It also has to be the pane's PARENT, not its sibling. In the reference
    // the pane is a child of this rectangle, so surface-main @ 0.22 composites
    // over card @ 0.5 over the field; as a sibling it composited over the raw
    // field and the pane came out roughly 1.8x brighter than the reference's.
    // Measured at y=700 on the owner's capture, Slint's pane is exactly
    // 0.78 x its gutter (51,48,34 against 65,62,44) — that ratio only falls out
    // if the gutter is already under the pane.
    //
    // With the background OFF this is surface-card, i.e. the same colour the
    // shell root paints, so the geometry and the look are byte-identical to
    // what shipped before.
    Rectangle {
        id: contentFrame
        anchors.left: sidebar.right
        anchors.right: queueColumn.left
        anchors.top: orbitBanner.bottom
        anchors.bottom: npb.top
        color: root.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard

        // Content PANE — the rounded, inset surface-main panel (Radius.md,
        // 8px gaps left/right/bottom, flush to the header: the reference gives
        // the frame no top padding so the pane butts the header band).
        Rectangle {
            id: contentPane
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            anchors.bottomMargin: 8
            radius: theme.radiusMd
            // Frosted content panel while the ambient background is active
            // (AppShell.slint: surface-main @ 0.22 + 1px #ffffff@0.10
            // hairline), over the frame's card @ 0.5.
            color: root.ambientOn ? theme.surfaceMainA22 : theme.surfaceMain
            border.width: root.ambientOn ? 1 : 0
            border.color: theme.frostBorder
            clip: true
            // QBZ_PANE_LAYER: collapse the pane into one cached texture instead
            // of redrawing its whole (all-alpha, unbatchable) draw list on every
            // whole-window repaint. Measurement knob, default off — see
            // settings_qt::pane_layer.
            layer.enabled: QbzShell.paneLayer

            // The view-mount chain — every route arm, every hard-won comment
            // and the MyQBZ `kind` discriminator now live in ContentRouter.qml,
            // which the KIOSK shell mounts too (kiosk-port contract §4.2 / D3).
            // The Slint kiosk copies AppShell's mount block verbatim and calls
            // that copy debt in its own header (KioskShell.slint:10-17); this
            // is the named fix, so the two shells cannot drift.
            //
            // `kiosk` defaults false, i.e. this instance routes every id to the
            // desktop view it always did. Reached from elsewhere in this
            // document through `contentRouter.currentItem` (the multi-select
            // seam above) — a QML `id` does not cross documents, so
            // `viewLoader` is no longer nameable here and that public property
            // is the replacement.
            ContentRouter {
                id: contentRouter
                anchors.fill: parent
            }
        }
    }

    // --- Bezel corners ----------------------------------------------------
    // `clip: true` on contentPane is a RECTANGULAR scissor in Qt Quick — it
    // is a glScissor / QPainter setClipRect and it does NOT follow `radius`.
    // The radius only shapes that Rectangle's own fill, so anything that
    // paints opaquely at the frame's corners paints straight over the bezel
    // and the panel reads square. Slint has no such split: its `clip: true`
    // on a Rectangle clips to the ROUNDED shape (AppShell.slint:409-414 sets
    // border-radius + clip on the same element and the album atmosphere
    // inside it is cut by the arc), which is why the identical structure is
    // correct there and wrong here. The visible offender is the album/artist
    // header band (controls/HeaderGradient.qml, full-bleed at y:0), but the
    // defect is the frame's, not the band's: the band scrolls, so rounding
    // the band itself would only hold at scroll offset 0 — measured, the
    // corners go square again the moment the page scrolls and the band's body
    // reaches the top edge. The mask therefore belongs here, once, and it
    // covers every present and future view.
    //
    // SIBLINGS of the pane, not children of it. Inside it they sat
    // at z:0 with the view Loader, so any overlay a VIEW mounts painted over
    // them and the corners read square while it was open — the pane-filling
    // modals are exactly that: DiscoverConfigModal z:3100 (HomeView:1130),
    // GenreFilterPopup z:3000 (Home/Library/DiscoverBrowse/PlaylistBrowse),
    // TrackInfoModal z:3000 (rows/TrackRow.qml:565, and TrackRow mounts
    // inside views). Raising the nubs to z:3200 would only win until the next
    // modal, because ADR-009 fixes in-pane modals at z >= 3000 and says
    // nothing about a ceiling. Out here the question does not arise: the mask
    // describes the FRAME's silhouette, so nothing the frame contains can
    // reach it, whatever z it picks. Window-level overlays declared after
    // this block still paint over the nubs — Cortinilla, the shared text
    // modal, the drag ghost, ArtPreviewOverlay — and that is correct: a
    // window-wide scrim covers the pane corners too. SidebarNowPlayingDock is
    // also later but lives inside the 240px sidebar (x:16, w:208) and never
    // touches the frame.
    //
    // Mechanism: four small Canvas nubs, each filling its corner square with
    // the shell colour and punching the quarter-disc back out.
    //
    // DOCTRINE CORRECTION (2026-07-29). This paragraph used to justify the
    // Canvas by claiming "ShaderEffect / OpacityMask render NOTHING on the
    // software path, and `layer.enabled` would cost an FBO every frame".
    // Both halves are FALSE as stated: effects need shaders, and this port
    // runs on the GPU (OpenGL RHI, measured via QSG_INFO 2026-07-29) — the
    // "renders nothing" note came from an offscreen session, which forces the
    // software renderer by definition. A `layer` FBO is also rendered once and
    // CACHED for static content, not re-rendered per frame. Where a software
    // path is genuinely possible, detect it with `GraphicsInfo.api` rather
    // than assuming it (theme/RoundedImage.qml does exactly that and is the
    // canonical statement of this rule — read its "HOW THE ROUNDING IS DONE"
    // header before touching any masking code).
    //
    // The Canvas here is still the right call, but on its own merits, not that
    // one: the nubs are 4 x 12x12 px rastered ONCE and repainted only when the
    // theme colour or the radius changes, whereas masking the frame would mean
    // an FBO the size of the whole content pane, re-rendered on every window
    // resize and invalidated by anything animating inside the pane.
    //
    // The fill is `contentFrame.color` because that is literally what shows
    // through the bezel — the frame IS the 8px gutter, and with the background
    // off it is surface-card, the same colour as the HeaderBar above and the
    // NPB gap below. (It was `root.color` before the frame existed; identical
    // value, but now it tracks the surface it is actually cutting into rather
    // than one that happens to match.)
    //
    // Gated on the ambient background being OFF, and that is not a
    // shortcut: with the dynamic background active the pane goes
    // translucent (surface-main @ 0.22 + hairline) and the field
    // is SUPPOSED to show through the corners, so an opaque nub would be
    // the regression. The same flag also suppresses the album/artist
    // atmosphere (AlbumView.qml:71 `headerAtmoOn = pref && !ambientOn`,
    // AlbumPageView.slint:168) — the two states are complementary, never
    // both.
    //
    // WHAT COVERS THE AMBIENT-ON CASE, since this block cannot: nothing that
    // paints opaquely may reach the frame's corners while the field is
    // showing through them, and by construction almost nothing does — every
    // view root goes `color: "transparent"` under ambient (HomeView.qml:53 and
    // its twelve siblings), and the ones that stay opaque round their own fill
    // (`radius: 12`, the same trick, applied by the offender instead of by a
    // mask). Sweeping the pane-level overlay set (ADR-009's z >= 3000) turned
    // up ONE that did neither: controls/DiscoverConfigModal.qml's scrim, a
    // full-bleed #bf000000 Rectangle at z:3100.
    // It now carries `radius: theme.radiusMd` too. GenreFilterPopup's backdrop
    // is a bare MouseArea (paints nothing) and TrackInfoModal / CastPicker are
    // parented to the window Overlay, where covering the pane corners is the
    // correct behaviour. A future full-bleed opaque pane child must round
    // itself the same way — there is no mask that can do it for it here.
    // Positions are the PANE's rect in shell coordinates. The pane is a child
    // of the frame now, so its own x/y are frame-local (8, 0) and have to be
    // offset by the frame's origin — reading contentPane.x directly here would
    // put every nub 8px to the left of the corner it is meant to cut.
    readonly property real _paneX: contentFrame.x + contentPane.x
    readonly property real _paneY: contentFrame.y + contentPane.y

    BezelCorner { corner: 0; fill: contentFrame.color; r: contentPane.radius
                  visible: !root.ambientOn
                  x: root._paneX; y: root._paneY }
    BezelCorner { corner: 1; fill: contentFrame.color; r: contentPane.radius
                  visible: !root.ambientOn
                  x: root._paneX + contentPane.width - width; y: root._paneY }
    BezelCorner { corner: 2; fill: contentFrame.color; r: contentPane.radius
                  visible: !root.ambientOn
                  x: root._paneX + contentPane.width - width
                  y: root._paneY + contentPane.height - height }
    BezelCorner { corner: 3; fill: contentFrame.color; r: contentPane.radius
                  visible: !root.ambientOn
                  x: root._paneX
                  y: root._paneY + contentPane.height - height }

    // The bezel nub itself. Kept as an inline component (no outer `id` is
    // referenced inside it — inline components do not share the document's
    // scope, so the colour and the radius arrive as properties).
    //
    // `corner`: 0 = top-left, 1 = top-right, 2 = bottom-right, 3 = bottom-left.
    // The arc centre is the corner of the r x r square that points INTO the
    // panel, so the punched-out quarter-disc is the panel side and the painted
    // remainder is the sliver outside the arc — the exact pixels Qt's
    // rectangular clip fails to cut.
    component BezelCorner: Canvas {
        id: nub
        property int corner: 0
        property color fill: "#000000"
        property int r: 12

        width: nub.r
        height: nub.r
        // Purely decorative: never take a click meant for the view underneath.
        enabled: false
        // Same pair as RoundedImage/PlaylistCollage — CPU raster, and Immediate
        // so a repaint cannot land on a pixmap whose Canvas is already gone.
        renderTarget: Canvas.Image
        renderStrategy: Canvas.Immediate

        onFillChanged: nub.requestPaint()
        onRChanged: nub.requestPaint()
        onCornerChanged: nub.requestPaint()

        onPaint: {
            var ctx = nub.getContext("2d")
            if (!ctx || nub.r <= 0)
                return
            ctx.reset()
            ctx.clearRect(0, 0, nub.width, nub.height)
            ctx.fillStyle = nub.fill
            ctx.fillRect(0, 0, nub.width, nub.height)
            // destination-out = QPainter's CompositionMode_DestinationOut, so
            // the disc erases the fill instead of drawing over it. Its edge is
            // antialiased against transparency, which is what makes the nub
            // blend into the arc rather than staircase along it.
            ctx.globalCompositeOperation = "destination-out"
            ctx.beginPath()
            ctx.arc(nub.corner === 0 || nub.corner === 3 ? nub.r : 0,
                    nub.corner === 0 || nub.corner === 1 ? nub.r : 0,
                    nub.r, 0, 2 * Math.PI)
            ctx.fill()
            ctx.globalCompositeOperation = "source-over"
        }
    }

    // --- Large NPB (mode 3) cover dock (phase 18) -------------------------
    // The L's vertical arm: the square now-playing cover + FFT band, pinned
    // flush to the window bottom-left over the sidebar
    // (SidebarNowPlayingDock.slint, AppShell.slint:747). Only while Large is
    // ACTIVE (mode 3 + sidebar open).
    //
    // The height comes from QbzShell.largeDockHeight (it changes with the
    // band's eye toggle) and Sidebar.qml reserves the same value minus the
    // bar height — pinning here with a literal would desync the two.
    readonly property bool largeActive: QbzShell.npbMode === 3 && QbzShell.sidebarState === 0
    SidebarNowPlayingDock {
        visible: root.largeActive
        // Equal 16px gutters inside the 240px sidebar; the cover is square,
        // so this width also sets the art size.
        x: 16
        width: 208
        // Flush with the bar LAYOUT's bottom edge, which sits a gutter above
        // the window's (the band's inset) — the dock lifts with it.
        y: npb.y + npb.height - npb.gutter - QbzShell.largeDockHeight
        ambientOn: root.ambientOn
    }

    // Search cortinilla (phase 15): the live-search dropdown overlay.
    // Declared here, ABOVE the shell but BELOW the four siblings that follow
    // (toast host, immersive, art preview, tooltip) — declaration order is
    // z-order. It is NOT the last child, and does not need to be: immersive
    // dismisses the cortinilla on open, and the other three are transient
    // layers that are meant to sit above it.
    Cortinilla {
        anchors.fill: parent
        headerBar: header
    }

    // --- Shared text modal (phase 16) ---------------------------------------
    // The AppShell-level modal layer (the Slint AppShell modals mount at
    // window level, ADR-009): the scrim covers the WHOLE window — sidebar /
    // header / NPB included — and the panel centers on the WINDOW, not on
    // the content frame. Views reach it via `openTextModal(title, body)`.
    property bool textModalOpen: false
    property string textModalTitle: ""
    property string textModalBody: ""
    function openTextModal(title, body) {
        textModalTitle = title
        textModalBody = body
        textModalOpen = true
    }

    // Bodies arrive as plain text with blank lines between paragraphs (the
    // biography and review sources). Rendered as rich text so the line height
    // can breathe — TextEdit has no lineHeight — and the paragraphs keep
    // their gap; single newlines stay line breaks.
    function textModalHtml(body) {
        var esc = function (s) {
            return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
        }
        var paras = String(body || "").replace(/\r\n/g, "\n").split(/\n{2,}/)
        var out = []
        for (var i = 0; i < paras.length; i++) {
            var p = paras[i].trim()
            if (p === "") continue
            out.push("<p style=\"line-height:150%; margin-top:0; margin-bottom:14px\">"
                     + esc(p).replace(/\n/g, "<br>") + "</p>")
        }
        return out.join("")
    }
    property bool textModalCopied: false
    Timer {
        id: textModalCopiedTimer
        interval: 1400
        onTriggered: root.textModalCopied = false
    }
    QbzClipboard { id: textModalClipboard }

    Rectangle {
        visible: root.textModalOpen
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.textModalOpen = false
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
        Rectangle {
            anchors.centerIn: parent
            // At least 75% x 65% of the window (2026-09-13): the old 560 x 460
            // card crammed a biography into a column the reader had to squint
            // at. Floors for small windows, ceilings that keep a margin.
            width: Math.min(root.width - 32, Math.max(Math.round(root.width * 0.75), 560))
            height: Math.min(root.height - 40, Math.max(Math.round(root.height * 0.65), 400))
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
            Column {
                anchors.fill: parent
                anchors.margins: 28
                spacing: 16
                Row {
                    width: parent.width
                    spacing: 8
                    Text {
                        width: parent.width - 28 - 28 - 16
                        text: root.textModalTitle
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                        anchors.verticalCenter: parent.verticalCenter
                    }
                    // Copy the whole body (plain text) to the clipboard; the
                    // glyph turns into a check for a moment as the receipt.
                    Rectangle {
                        width: 28
                        height: 28
                        color: tmCopyArea.containsMouse ? theme.surfaceHover : "transparent"
                        radius: 6
                        anchors.verticalCenter: parent.verticalCenter
                        QbzIcon {
                            name: root.textModalCopied ? "check" : "copy"
                            width: 16
                            height: 16
                            anchors.centerIn: parent
                            tintName: root.textModalCopied ? "accent"
                                : (tmCopyArea.containsMouse ? "textPrimary" : "muted")
                        }
                        MouseArea {
                            id: tmCopyArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                if (textModalClipboard.copy(root.textModalBody)) {
                                    root.textModalCopied = true
                                    textModalCopiedTimer.restart()
                                }
                            }
                        }
                    }
                    Rectangle {
                        width: 28
                        height: 28
                        color: tmCloseArea.containsMouse ? theme.surfaceHover : "transparent"
                        radius: 6
                        anchors.verticalCenter: parent.verticalCenter
                        QbzIcon {
                            name: "x"
                            width: 18
                            height: 18
                            anchors.centerIn: parent
                            tintName: tmCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: tmCloseArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.textModalOpen = false
                        }
                    }
                }
                Item {
                    width: parent.width
                    height: parent.height - 28 - 16
                    Flickable {
                        id: tmFlick
                        anchors.fill: parent
                        anchors.rightMargin: 22
                        clip: true
                        contentWidth: width
                        contentHeight: tmText.implicitHeight
                        boundsBehavior: Flickable.StopAtBounds
                        // Selectable, one size up from body text, paragraphs
                        // with room between them.
                        QbzSelectableText {
                            id: tmText
                            width: tmFlick.width
                            rich: true
                            text: root.textModalHtml(root.textModalBody)
                            color: theme.textPrimary
                            pixelSize: theme.fontBody + 1
                        }
                    }
                    QbzScrollBar {
                        target: tmFlick
                        alwaysShown: true
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.bottom: parent.bottom
                    }
                }
            }
        }
    }

    // --- Drag ghost (DragGhost.slint, phase 17) -----------------------------
    // The dark pill that follows the cursor while tracks are dragged onto
    // the sidebar: "N tracks" for a group, or title + "artist · album" for
    // one. Visual only (never blocks the pointer).
    Rectangle {
        visible: QbzShell.dragActive && !QbzShell.dragInlineVisual
        x: QbzShell.dragX + 10
        y: QbzShell.dragY + 14
        width: ghostCol.width + 28
        height: ghostCol.height + 16
        radius: 8
        color: "#e01e1e28"
        border.width: 1
        border.color: "#1fffffff"
        Column {
            id: ghostCol
            anchors.centerIn: parent
            spacing: 2
            Text {
                text: QbzShell.dragCount > 1
                    ? QbzShell.dragCount + " " + QbzSession.tr("tracks", QbzSession.trRev)
                    : QbzShell.dragTitle
                color: "#ffffff"
                font.pixelSize: 12
                font.weight: theme.weightMedium
            }
            Text {
                visible: QbzShell.dragCount === 1 && QbzShell.dragSubtitle !== ""
                text: QbzShell.dragSubtitle
                color: "#8cffffff"
                font.pixelSize: 10
            }
        }
    }

    // --- MyQBZ global overlays + the shared toast host ---------------------
    // Three window-level overlays, mounted ONCE here because each is reachable
    // from anywhere and must outlive the view that triggered it:
    //
    //   AddToMixtapeModal — the "Add to Mixtape/Collection" picker, opened from
    //     a TrackRow menu, an album/playlist page, a Local Library bulk bar or
    //     the now-playing bar. The view that opened it is often unmounted by
    //     the time the user picks a target.
    //   MyQbzModals       — Create / Edit (rename · description · delete) / DJ
    //     Mix, three sibling panels in one file (spec 01 §10/§11). Delete in
    //     particular navigates away from the page that raised it.
    //   QbzToast          — the ONE in-app toast host (toast_qt.rs publishes
    //     onto QbzShell.toastJson; the auto-hide timer lives in the QML).
    //
    // NO `z` on any of them, and that is the point. This file's convention is
    // DECLARATION ORDER (see the note at ArtPreviewOverlay below: "LAST child =
    // above every surface"), and the bezel-corner note at :200-264 explains why
    // `z` is the wrong tool out here — ADR-009 pins in-pane modals at z >= 3000
    // with no ceiling, so any number picked here loses to the next one added.
    // Position in the chain is byte-equivalent to Slint, where `Toast { }` sits
    // at AppShell.slint:950: after DragGhost, before TooltipOverlay and
    // ArtPreviewOverlay. Covering the content pane's rounded corners is CORRECT
    // for a window-level overlay — a window-wide scrim covers them too.
    //
    // All three self-gate on their own document, so an unopened overlay is an
    // invisible, non-interactive Item and costs nothing.
    AddToMixtapeModal {
        anchors.fill: parent
    }
    // PlaylistPickerModal — the "Add to playlist" picker, opened from the
    // queue footer (save the queue as a playlist) and the queue row menu.
    // Same reason it lives out here as its Mixtape sibling: the surface that
    // opened it is often gone by the time the user picks a target, and the
    // queue panel in particular is `visible`-gated.
    PlaylistPickerModal {
        anchors.fill: parent
    }
    // PlaylistImportModal — the public-playlist importer (Spotify / Apple
    // Music / Tidal / Deezer), opened from the sidebar `...` menu and from the
    // closed-sidebar flyout. It mounts HERE and not in the sidebar by explicit
    // contract (05 §5.8.5): closing it must not cancel an in-flight import, and
    // neither surface that opens it has a lifetime guarantee across a
    // navigation — the importer's own completion arm navigates to the imported
    // playlist while the modal is still up.
    PlaylistImportModal {
        anchors.fill: parent
    }
    // Open Music Link — app-wide because both the header row and Ctrl+L open
    // it, and a playlist result hands off to the importer beside it.
    LinkResolverModal {
        anchors.fill: parent
    }
    // Album Quick View — one navigation-free preview shared by every eligible
    // AlbumCard. It owns no page lifetime: a card can live in Home, Search,
    // Library or an artist rail, and Library's local/media-server ids are
    // source-routed by its controller rather than sent to Qobuz. The preview
    // always renders at shell level from its generation-guarded document.
    // Self-gated on `quickViewJson.open`, so the closed state is an invisible,
    // non-interactive Item.
    AlbumQuickView {
        anchors.fill: parent
    }
    // TrackReplacementModal — "Find available version" for a track Qobuz
    // pulled from the catalogue (2026-08-17 unavailable-tracks contract §6),
    // opened from the playlist row's context menu.
    //
    // Out here for the strongest form of its neighbours' reason: the apply
    // RELOADS the playlist underneath it (`refresh_after_membership_change`),
    // so a modal parented into the playlist view would be destroyed by its own
    // success — while a write is still settling. Self-gates on
    // QbzTrackReplace.replaceJson, so while closed it is an invisible,
    // non-interactive Item.
    TrackReplacementModal {
        anchors.fill: parent
    }
    // The two DISC modals. Out here for the same reason as their neighbours:
    // both of them RENAME the open session as their success case, so a modal
    // parented into the Local Library pane would be torn down by the very
    // publish that means it worked. Each self-gates on its own document, so a
    // closed one is an invisible, non-interactive Item.
    DiscMetaModal {
        anchors.fill: parent
    }
    RipWizardModal {
        anchors.fill: parent
    }
    // The rip's progress panel. Out here because the JOB outlives the pane —
    // the user can navigate away mid-rip and the drive keeps spinning — so it
    // must not be parented into a view that gets destroyed on a tab change.
    RipProgressModal {
        anchors.fill: parent
    }
    MyQbzModals {
        anchors.fill: parent
    }
    // MusicianModal — where a WEAK / NONE / resolve-error musician click lands.
    // It is the majority branch, not an edge case: most credited sidemen are
    // not Qobuz artists, and before this the click did nothing at all.
    //
    // Out here, and not in any of its six openers, for the strongest version of
    // the reason its neighbours give: FOUR of those six openers are themselves
    // modals or overlays (album credits, desktop track info, the immersive
    // track-info panel), and each CLOSES ITSELF as it dispatches — so a modal
    // parented into the opener would be destroyed by the very click that
    // summoned it. Self-gates on QbzMusician.modalJson, so while closed it is
    // an invisible, non-interactive Item.
    MusicianModal {
        anchors.fill: parent
    }
    // FolderModals — the "New folder" create panel, the full folder editor and
    // their delete confirm (contract D21). Out here for the same reason as its
    // neighbours: it is opened from the SIDEBAR's "..." menu and row menu as
    // well as from the Playlist Manager view, and the sidebar animates to
    // width 0 with clip: true, so a modal parented into it would disappear
    // with it. Self-gates on QbzFolderEdit.createJson / editJson, so while
    // both are closed it is an invisible, non-interactive Item.
    FolderModals {
        anchors.fill: parent
    }
    // PlaylistEditModal — the ONE playlist editor (rename · description ·
    // offline-only · delete), for both `local:` and Qobuz playlists (contract
    // D20/D21). Out here for the same reason as its neighbours: it is opened
    // from the manager's cards and rows, from the SIDEBAR's row context menu
    // and from the playlist detail header — and its delete arm navigates away
    // from the page that raised it. It replaces the inline Popup that used to
    // live in views/PlaylistView.qml, which could express neither a
    // description nor the offline-only flag. Self-gates on
    // QbzPlaylistEdit.editJson, so while closed it is an invisible,
    // non-interactive Item.
    PlaylistEditModal {
        anchors.fill: parent
    }
    // PlaylistDeleteConfirm — the "Delete playlist?" confirmation the sidebar
    // row menu and the playlist cards summon (QbzPlaylistEdit.askDelete).
    // Self-gates on QbzPlaylistEdit.deleteJson; invisible and inert while
    // closed. Out here for the same reason as FolderModals above.
    PlaylistDeleteConfirm {
        anchors.fill: parent
    }
    // PlaylistCreateModal — "New playlist" (name · description · folder ·
    // public · offline-only), raised by the sidebar's "+" and, like its
    // neighbour, self-gating on its own document (QbzPlaylistEdit.createJson).
    // It rides the editor's singleton because it is the same domain — a
    // playlist's own metadata — with a document that never interacts with the
    // editor's. It replaces the POC shortcut that created a playlist named
    // "New Playlist" with no way to set any of the five.
    PlaylistCreateModal {
        anchors.fill: parent
    }
    // Whole-album / whole-playlist offline preflight. Mounted once at shell
    // level because either detail view can open it and the retained track
    // snapshot lives in QbzOffline, not in the page that launched it.
    OfflineCacheChoiceModal {
        anchors.fill: parent
    }
    // The hotkeys pair (2026-08-03 hotkeys-port contract §4.4/§4.5, block
    // B3): the read-only cheatsheet (`?` / the HeaderBar menu row) and the
    // editable customize editor. Same declaration-order convention as their
    // neighbours — Slint mounts them with the global modals too
    // (AppShell.slint:908-917, after the other modals, BEFORE the immersive
    // overlay and the toast — so both keep painting above, byte-parity).
    // Both self-gate on QbzHotkeys.cheatsheetOpen / .customizeOpen, so while
    // closed each is an invisible, non-interactive Item.
    KeyboardShortcutsModal {
        anchors.fill: parent
    }
    CustomizeShortcutsModal {
        anchors.fill: parent
    }
    // The four overlays below are ALWAYS-ON-TOP: in the reference they are
    // mounted after every modal (AppShell.slint:941-953) and so paint above
    // them. This port relied on declaration order alone, which held only while
    // no modal carried an explicit `z` — and the modal band is 3000-3200
    // (ADR-009 as this port spells it), so any z-carrying modal buried them:
    // a toast raised by a background event behind an opaque panel is simply
    // never seen. They now carry an explicit band of their own, ABOVE the
    // modals, keeping the relative order they already had.
    QbzToast {
        anchors.fill: parent
        z: 3500
    }

    // Immersive mode root overlay (2026-08-02 immersive-port contract §5.1):
    // plain Item, LAST-child class like its Slint twin (a Popup would grab
    // the pointer and kill typing in the immersive search field — the
    // cortinilla doctrine, Cortinilla.qml:1-5). Self-gates on
    // QbzImmersive.open, so while closed it is an invisible, non-interactive
    // Item. Sits AFTER QbzToast and BEFORE ArtPreviewOverlay/QbzTooltip so
    // those two keep painting above it; covering the content pane's rounded
    // corners is correct for a window-level overlay (:555-563 above).
    ImmersiveView {
        anchors.fill: parent
        z: 3510
    }

    // Above every surface (see the always-on-top band above), exactly like
    // ArtPreviewOverlay.slint's mount in AppShell.slint. Non-interactive, so it
    // never steals the hover that is keeping it open (see the file header).
    ArtPreviewOverlay { z: 3520 }

    // The shell's ONE hover-tooltip overlay — the port of Slint's
    // TooltipOverlay/SidebarTooltip mechanism, topmost of the always-on-top
    // band for the reason TooltipOverlay.slint's header gives: "mounted last in
    // AppShell so no neighbour can cover it". Surfaces do not own a popup; they call
    // showRight()/showAbove()/hide() on this instance (see QbzTooltip.qml).
    //
    // WIRED SO FAR: the collapsed sidebar rail (Sidebar.qml). Everything else
    // Slint tooltips is still un-wired — the list is in the handoff notes; each
    // one is a two-line change in its own file, not a change here.
    QbzTooltip {
        id: tooltipOverlay
        anchors.fill: parent
        z: 3530
    }

    // The applied-filters summary rides the bridge (shell_bridge.rs
    // `filter_tip_json`) because the funnels that raise it live deep inside
    // view toolbars and cannot name this overlay — the same reason, and the
    // same solution, as the art preview above. Numbers in, numbers out: a
    // toolbar that is destroyed while its bubble is up leaves nothing dangling.
    Connections {
        target: QbzShell
        function onFilterTipJsonChanged() {
            var d = {}
            try {
                d = JSON.parse(QbzShell.filterTipJson)
            } catch (e) {
                d = {}
            }
            if (!d || !d.key) {
                tooltipOverlay.hideAll()
                return
            }
            tooltipOverlay.showSummaryAt(d.key, d.x || 0, d.y || 0,
                                         d.w || 0, d.h || 0, d.groups || [])
        }
    }

    // Qobuz Connect bootstrap conflict: mounted globally because it fences the
    // session loop until the user chooses which queue/renderer wins.
    QconnectPlaybackConflictModal { }

    // Qobuz Connect diagnostics modal (DeveloperSettings > QOBUZ CONNECT) —
    // mounted LAST, mirroring QconnectDevModal.slint's topmost mount in
    // AppShell.slint. Ordering against the tooltip above is a non-issue: this
    // is a Popup parented to Overlay.overlay (the CastPicker pattern), so it
    // renders in the overlay layer above EVERY plain Item in this tree —
    // the tooltip's "LAST child so no neighbour can cover it" invariant is
    // about Items, and no Item was added after it. Self-gates on
    // QbzQConnect.diagOpen, so while closed it is an invisible,
    // non-interactive Item.
    QconnectDevModal { }

    // The log viewer (Settings > "Share logs", and Developer). Mounted at the
    // AppShell root for the same reason as every other global modal: it is
    // opened FROM Settings but must not live inside its Flickable. Self-gates
    // on the document's `open`, so while closed it is an invisible,
    // non-interactive Item that parses one small JSON string.
    LogViewerModal { }

    // Report an issue (the header hamburger's row): explains the manual,
    // redacted log-sharing flow and offers "Go to logs" + the GitHub bug
    // template. Its `open` is LOCAL state — nothing in Rust needs to know it
    // is up — so the menu row flips it directly through this id.
    ReportIssueModal { id: reportIssueModal }

    // About QBZ + What's New (the header hamburger menu's last two rows),
    // mirroring AppShell.slint:925-936 where both are mounted with the global
    // modals. Each self-gates on its half of QbzAbout's two documents, so
    // while closed it is an invisible, non-interactive Item that parses one
    // small JSON string — and each carries an explicit `z: 3000` (ADR-009 as
    // this port spells it) rather than relying on declaration order alone.
    UpdatesModal { }
    AboutModal {
        anchors.fill: parent
    }
    WhatsNewModal {
        anchors.fill: parent
    }
    // A LOADER, not a static mount. `visible: false` still CONSTRUCTS the
    // whole subtree -- panel, Flickable, fourteen Repeater delegates, their
    // texts and bindings -- on Linux and macOS, where this modal can never be
    // shown. The Loader makes that cost real only where the modal exists.
    //
    // Last of the three so it lands on top at equal z: it is shown once per
    // version at startup and has to be answered.
    Loader {
        anchors.fill: parent
        active: QbzShell.isWindows && QbzShell.windowsDisclaimerOpen === true
        sourceComponent: WindowsDisclaimerModal { }
    }
}
