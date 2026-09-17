// KioskShell — the small-panel touch shell (QML port of
// crates/qbz-ui/ui/shell/KioskShell.slint, 748 lines).
//
// A SIBLING of AppShell, mounted by Main.qml when QbzSession.screen is
// "kiosk". It reads the SAME bridges as the desktop shell — nothing is
// mirrored — so the two swap live with nothing torn down. The only
// differences are chrome: a thin back bar + a bottom NavRail replace the
// HeaderBar + Sidebar, the transport is forced to NowPlayingBarSmall, and
// there is no queue/lyrics side panel.
//
// Structure, top to bottom (KioskShell.slint:229-747). The last three are
// overlays drawn on top of the layout, and the ordering is load-bearing:
//   1. Column: back bar (42) / content frame / transport (42, conditional) /
//      NavRail (80)                                       KioskShell.slint:229-659
//   2. the player-zone focus ring overlay                              :666-677
//   3. the root key handler + the deferred initial focus grab      :684-735
//   4. the kiosk's OWN Immersive mount                                 :741-747
//
// The view mounts live in the SHARED shell/ContentRouter.qml (contract §4.2,
// divergence D3) — the Slint's verbatim copy of AppShell's block declares
// that extraction as its own follow-up (KioskShell.slint:10-17).

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../immersive"
import "../kiosk"
import "../theme"

Rectangle {
    id: root

    // KioskShell.slint:58 sets NO background — every visible surface is
    // painted by a child, and what shows through is the window's own fill
    // (Main.qml:93, the same surface-card the back bar uses).
    color: "transparent"

    // The host ApplicationWindow. Main.qml:463 assigns this UNCONDITIONALLY
    // on load, so the property must exist or the shell fails to mount
    // (the desktop declaration is shell/AppShell.qml:59).
    property var hostWindow: null

    // The shell-root marker six components walk the parent chain for
    // (immersive/ImmersiveView.qml:93-99 hands focus BACK to whatever carries
    // it). Without it every hotkey after an immersive close is swallowed.
    // `focus` + the explicit grab are AppShell.qml:69,78's hard-won pair:
    // declarative focus alone did not survive the splash -> shell Loader swap
    // in a WM-less session.
    readonly property bool isQbzShellRoot: true
    focus: true
    Component.onCompleted: {
        root.forceActiveFocus()
        Qt.callLater(function () { QbzAbout.updatesLaunch() })
    }

    QbzTheme { id: theme }
    UpdatesModal { kioskHost: true }
    WhatsNewModal { }

    /// The back bar's one button form: 44x36, radius-sm hover fill, a 20px
    /// glyph. `available` dims the glyph and disarms the area (Back/Forward
    /// with no history); `active` lights the glyph accent (Cast connected).
    /// Connect keeps its own golden arm below because its active state is a
    /// gold tint + border the shared form cannot express.
    component ChromeButton: Rectangle {
        id: cb
        property string name: ""
        property bool available: true
        property bool active: false
        property string label: ""
        signal clicked()
        width: 44
        height: 36
        radius: theme.radiusSm
        color: (cbArea.containsMouse && cb.available) ? root.chromeHoverBg : "transparent"
        Accessible.role: Accessible.Button
        Accessible.name: cb.label
        QbzIcon {
            opacity: cb.available ? 1.0 : 0.32
            name: cb.name
            width: 20
            height: 20
            anchors.centerIn: cb
            tintName: cb.active ? "accent" : "secondary"
        }
        MouseArea {
            id: cbArea
            anchors.fill: cb
            enabled: cb.available
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: cb.clicked()
        }
    }

    // KioskShell.slint:204 — the NavRail is a fat touch target.
    readonly property int navRailHeight: 80
    readonly property int transportHeight: 64

    // The chrome hover fill (Theme.alpha-8, KioskShell.slint:255,273). The
    // ramp is empty on the pre-publish frame, where alphaTier() silently
    // returns "transparent" — the tree's required guard idiom, whose reason
    // is written out at rows/TrackRow.qml:264-269.
    readonly property color chromeHoverBg: theme.alphaTiers.length > 0
        ? theme.alphaTier(8)
        : (theme.isDark ? "#14ffffff" : "#14000000")

    // ---------------------------------------------------------------------
    // The offline gate (KioskShell.slint:209-227)
    // ---------------------------------------------------------------------
    // Qobuz-only routes mount only online; the local-DB routes (Local
    // Library, local album, My QBZ, Settings, Now Playing, the manager
    // views) keep mounting offline and are deliberately absent below.
    //
    // A live Desktop -> Kiosk switch retains the current route. Offline
    // gating therefore belongs here as well as at navigation entry points.
    // Local playlists continue to mount their available sidecar tracks.
    readonly property var playlistDoc: root.parsePlaylistDoc()
    function parsePlaylistDoc() {
        try {
            return JSON.parse(QbzBridge.playlistJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property bool qobuzViewBlocked: QbzSession.offline && (
           QbzShell.currentView === "home"
        || QbzShell.currentView === "discoverbrowse"
        || QbzShell.currentView === "playlistbrowse"
        || QbzShell.currentView === "search"
        || QbzShell.currentView === "library"
        || QbzShell.currentView === "album"
        || QbzShell.currentView === "artist"
        || QbzShell.currentView === "artistreleases"
        || QbzShell.currentView === "label"
        || QbzShell.currentView === "labelreleases"
        || QbzShell.currentView === "mix"
        || (QbzShell.currentView === "playlist"
            && root.playlistDoc.isLocalPlaylist !== true))

    // ---------------------------------------------------------------------
    // NavRail routing (KioskShell.slint:637-657)
    // ---------------------------------------------------------------------
    // The Slint tiles fire `header-menu-navigate("discover-home")` etc., and
    // the Rust handler behind that route both switches the view AND selects a
    // landing tab (crates/qbz/src/main.rs:18017-18040). Qt has no such
    // combined seam — QbzShell.navigateTo carries only the route — so the
    // landing tab uses the port's existing pending-tab handshake
    // (shell/NavFlyout.qml:263-291): remember the tab, wait for the route to
    // land, stamp it on the mounted view. The view is reached through
    // ContentRouter's public `currentItem` rather than NavFlyout's depth-
    // capped tree walk, which the kiosk's deeper mount chain would outrun.
    function navigateWithTab(view, tab) {
        QbzShell.navigateToTab(view, tab)
    }

    Connections {
        target: QbzShell
        function onCurrentViewChanged() { QbzKioskNav.resetForView() }
    }

    // =====================================================================
    // 1. The layout column (KioskShell.slint:229-659)
    // =====================================================================
    Column {
        id: shellColumn
        width: root.width
        spacing: 0

        // --- Back bar (KioskShell.slint:237-319) -------------------------
        // Persistent global back/forward (ADR-004) + the kiosk search entry.
        // The desktop search box lives in the HeaderBar, which the kiosk
        // drops, so this field is what makes Search reachable at all.
        Rectangle {
            id: backBar
            width: shellColumn.width
            // 44px — owner feedback 2026-09-07 asked for ~60% of the previous
            // 72px bar. The four chrome buttons (Back, Forward, Connect,
            // Cast) share ONE 44x36 / 20px-glyph form (ChromeButton below).
            height: 44
            color: theme.surfaceCard

            // Custom-chrome drag surface, the HeaderBar.qml:132-152 pattern
            // verbatim: declared FIRST so every control above wins
            // hit-testing; the system move starts only after a real
            // movement so taps still land; double-click toggles maximize.
            // Inert under the system title bar (native chrome owns it). A
            // kiosk on a NUC with a desktop monitor is a windowed app like
            // any other, and a bar that could not be dragged was reported
            // as a defect on 2026-09-07.
            MouseArea {
                anchors.fill: parent
                enabled: !QbzShell.systemTitleBar
                property bool dragStarted: false
                onPressed: dragStarted = false
                onPositionChanged: {
                    if (pressed && !dragStarted && root.hostWindow) {
                        dragStarted = true
                        root.hostWindow.startSystemMove()
                    }
                }
                onDoubleClicked: {
                    if (root.hostWindow) {
                        root.hostWindow.visibility =
                            root.hostWindow.visibility === Window.Maximized
                            ? Window.Windowed : Window.Maximized
                    }
                }
            }

            // The kiosk carries no HeaderBar, so it owns the window chrome
            // too (KioskShell.slint:240-245): macOS overlay leaves the native
            // traffic-light inset, Linux frameless draws the controls.
            //
            // DIVERGENCE, reported: the reference reads
            // AppearanceState.is-macos and .show-window-controls. Neither has
            // a Qt bridge member (QbzShell exposes only systemTitleBar,
            // shell_bridge.rs:111), and this port must not add Rust here. The
            // platform half uses Qt's own Qt.platform.os; the pref half is
            // dropped, which puts the kiosk on exactly the predicate the
            // desktop chrome already uses (HeaderBar.qml:924).
            readonly property real macInset:
                (Qt.platform.os === "osx" && !QbzShell.systemTitleBar) ? 78 : 0
            readonly property bool linChrome:
                Qt.platform.os !== "osx" && !QbzShell.systemTitleBar

            // padding-left 6 + macInset, padding-right 118 (controls) or 8,
            // padding-top/bottom 4, spacing 6 -> a 36px content row.
            // The 118 is not a round number: WindowControls is 34*3 + 2*2 =
            // 106 wide, pinned 8px from the right edge, leaving 4px of
            // clearance to the search field.
            Row {
                id: backBarRow
                x: 6 + backBar.macInset
                y: 4
                width: backBar.width - (6 + backBar.macInset)
                       - (backBar.linChrome ? 118 : 8)
                       - sessionCluster.width - 6
                height: backBar.height - 8
                spacing: 6

                // KioskShell.slint:252-269 / 270-287. Disabled = dimmed,
                // un-hoverable and un-clickable, all three (an opacity-only
                // port leaves a clickable ghost).
                ChromeButton {
                    id: backBtn
                    name: "chevron-left"
                    label: QbzSession.tr("Back", QbzSession.trRev)
                    available: QbzShell.canBack
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzShell.navigateBack()
                }
                ChromeButton {
                    id: fwdBtn
                    name: "chevron-right"
                    label: QbzSession.tr("Forward", QbzSession.trRev)
                    available: QbzShell.canForward
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzShell.navigateForward()
                }

                // KioskShell.slint:288-312. Submit-only: there is NO live /
                // debounced search in the back bar and no cortinilla — do not
                // add one.
                QbzLineEdit {
                    id: searchField
                    width: backBarRow.width - backBtn.width - fwdBtn.width
                           - 2 * backBarRow.spacing
                    height: backBarRow.height
                    placeholder: QbzSession.tr("Search", QbzSession.trRev)
                    // The full accept sequence, KioskShell.slint:301-311.
                    // Rust's search_submit navigates to the results view
                    // itself (search_qt.rs:1971), which is the reference's
                    // explicit `NavState.view = ContentView.search` (:303).
                    // Slint then needs TWO more steps — clear-focus() so the
                    // OS on-screen keyboard closes (:307), then keys.focus()
                    // to hand the arrows back (:310). In Qt keyboard focus is
                    // exclusive, so grabbing it at the root does both at once:
                    // the inner TextInput loses activeFocus (text-input
                    // DISABLE -> squeekboard hides) and the nav scope has it.
                    onAccepted: function (value) {
                        QbzSearch.searchSubmit(value)
                        root.forceActiveFocus()
                    }
                }
            }

            // Session cluster — Qobuz Connect + Cast, always visible, right
            // of the search field and before the window controls. Owner
            // feedback 2026-09-07: both are fundamental on a kiosk (Connect
            // especially — the panel is a renderer other devices drive), so
            // they left the hidden "more" menu of the transport bar for a
            // permanent home in the chrome. A 1px pipe separates the
            // cluster from its neighbours on either side. Connect is the
            // PlayerBar.qml:537-575 golden button at touch size; Cast is the
            // shared icon button lit while a renderer is connected.
            Row {
                id: sessionCluster
                x: backBar.width - (backBar.linChrome ? 118 : 8) - width
                y: 4
                height: backBar.height - 8
                spacing: 6

                Rectangle {
                    width: 1
                    height: 20
                    anchors.verticalCenter: parent.verticalCenter
                    color: theme.borderSubtle
                }

                Rectangle {
                    id: kioskQconnectBtn
                    readonly property bool qcActive: QbzQConnect.qconnectConnected
                    readonly property color gold: "#e0b341"
                    width: 44
                    height: 36
                    anchors.verticalCenter: parent.verticalCenter
                    radius: theme.radiusSm
                    color: qcActive ? Qt.rgba(gold.r, gold.g, gold.b, 0.16)
                        : (kioskQcArea.containsMouse ? root.chromeHoverBg : "transparent")
                    border.width: qcActive ? 1 : 0
                    border.color: Qt.rgba(gold.r, gold.g, gold.b, 0.45)
                    Accessible.role: Accessible.Button
                    Accessible.name: QbzSession.tr("Qobuz Connect", QbzSession.trRev)
                    QbzIcon {
                        name: "monitor-speaker"
                        width: 20
                        height: 20
                        anchors.centerIn: parent
                        tintName: kioskQconnectBtn.qcActive ? "amber"
                            : kioskQcArea.containsMouse ? "textPrimary" : "secondary"
                    }
                    MouseArea {
                        id: kioskQcArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: connectFlyout.openBelowRight(kioskQconnectBtn)
                    }
                }

                ChromeButton {
                    name: "cast"
                    label: QbzSession.tr("Cast", QbzSession.trRev)
                    active: QbzPlayer.npCastActive
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzCast.openPicker()
                }

                Rectangle {
                    visible: backBar.linChrome
                    width: 1
                    height: 20
                    anchors.verticalCenter: parent.verticalCenter
                    color: theme.borderSubtle
                }
            }

            // Linux custom window controls at the right edge
            // (KioskShell.slint:315-318). Inlined rather than extracted from
            // HeaderBar.qml:923-999: a shared component would need a new file
            // in build.rs, and touching HeaderBar risks the desktop
            // non-regression guard (contract D3). Same three 34x26 buttons,
            // same 2px spacing, same fixed #e81123 close hover.
            Row {
                id: windowControls
                visible: backBar.linChrome
                x: backBar.width - width - 8
                y: (backBar.height - height) / 2
                height: 26
                spacing: 2

                Rectangle {
                    id: wcMin
                    width: 34
                    height: 26
                    radius: theme.radiusSm
                    color: wcMinArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        name: "minus"
                        width: 14
                        height: 14
                        anchors.centerIn: wcMin
                        tintName: "secondary"
                    }
                    MouseArea {
                        id: wcMinArea
                        anchors.fill: wcMin
                        hoverEnabled: true
                        onClicked: if (root.hostWindow) root.hostWindow.showMinimized()
                    }
                }
                Rectangle {
                    id: wcMax
                    width: 34
                    height: 26
                    radius: theme.radiusSm
                    color: wcMaxArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        // The glyph swaps while maximized
                        // (WindowControls.slint:52-54).
                        name: root.hostWindow
                              && root.hostWindow.visibility === Window.Maximized
                              ? "minimize-2" : "maximize-2"
                        width: 14
                        height: 14
                        anchors.centerIn: wcMax
                        tintName: "secondary"
                    }
                    MouseArea {
                        id: wcMaxArea
                        anchors.fill: wcMax
                        hoverEnabled: true
                        onClicked: {
                            if (root.hostWindow) {
                                root.hostWindow.visibility =
                                    root.hostWindow.visibility === Window.Maximized
                                    ? Window.Windowed : Window.Maximized
                            }
                        }
                    }
                }
                Rectangle {
                    id: wcClose
                    width: 34
                    height: 26
                    radius: theme.radiusSm
                    color: wcCloseArea.containsMouse ? "#e81123" : "transparent"
                    QbzIcon {
                        name: "x"
                        width: 14
                        height: 14
                        anchors.centerIn: wcClose
                        // The hover fill is a FIXED #e81123 under every
                        // theme, so the hovered glyph is literal white.
                        tintName: wcCloseArea.containsMouse ? "white" : "secondary"
                    }
                    MouseArea {
                        id: wcCloseArea
                        anchors.fill: wcClose
                        hoverEnabled: true
                        // The FIFTH exit. §5.7 of the 2026-08-03 miniplayer/
                        // tray contract enumerates four; this shell grew its
                        // own drawn X after that contract was written, and an
                        // exit still calling Qt.quit() directly is precisely
                        // the half-port the enumeration exists to prevent. Same
                        // choreography as the desktop chrome: Main.qml's
                        // closeOrHide hides to the tray while QbzTray.trayLive
                        // && QbzTray.closeToTray and quits otherwise.
                        onClicked: if (root.hostWindow) root.hostWindow.closeOrHide(null)
                    }
                }
            }
        }

        // --- Content frame (KioskShell.slint:325-611) --------------------
        // The outer surface-card frame takes the leftover height; the inner
        // surface-main panel is inset 8px left/right/bottom (0 top — it butts
        // the back bar) and takes an EXPLICIT height, because a
        // conditionally-mounted view does not stretch to fill a layout cell.
        //
        // THE NOW-PLAYING EXCEPTION (:348-355): the transport row is hidden
        // on that view, so its 42px must NOT be subtracted — otherwise the
        // content stays short and the NavRail rides up, leaving dead space
        // below the footer.
        OrbitBanner {
            id: orbitBanner
            width: shellColumn.width
            remoteActive: QbzOrbit.enabled && QbzOrbit.controllingRemote
            connectionLost: QbzOrbit.connectionLost
            hostName: QbzOrbit.hostName
        }

        Rectangle {
            id: contentFrame
            width: shellColumn.width
            height: root.height - backBar.height - orbitBanner.height
                    - (QbzShell.currentView === "nowplaying" ? 0 : root.transportHeight)
                    - root.navRailHeight
            color: theme.surfaceCard

            // Tap anywhere in the content area to dismiss the OS on-screen
            // keyboard (KioskShell.slint:328-339): releasing the field's
            // focus makes Qt send text-input DISABLE and squeekboard closes.
            // Declared FIRST so it sits BEHIND the views — later siblings get
            // the event first, so this only catches empty-area taps.
            MouseArea {
                anchors.fill: contentFrame
                onClicked: root.forceActiveFocus()
            }

            Rectangle {
                id: contentPanel
                x: 8
                y: 0
                width: contentFrame.width - 16
                height: contentFrame.height - 8
                color: theme.surfaceMain
                radius: theme.radiusMd
                clip: true

                // The offline placeholder (KioskShell.slint:362-365). The
                // reference's `favorites` variant nests a FavoritesOfflineRail
                // inside it; that rail is ABSENT-BY-RULING (contract R1), so
                // the plain placeholder is the correct kiosk rendering for
                // every blocked route, favourites included.
                QbzOfflinePlaceholder {
                    visible: root.qobuzViewBlocked
                    anchors.centerIn: contentPanel
                    showSettingsAction: true
                    onSettingsClicked: QbzShell.navigateTo("settings")
                }

                // The shared router, in its kiosk arm. `active` rather than
                // `visible`: the reference UNMOUNTS the view while the gate is
                // up (every Qobuz route carries its own `&& !offline` guard,
                // :367-608), and an invisible view would keep running.
                Loader {
                    id: contentLoader
                    anchors.fill: contentPanel
                    active: !root.qobuzViewBlocked
                    sourceComponent: kioskRouter
                }
            }
        }

        // --- The transport (KioskShell.slint:617-631) --------------------
        // Forced to the small bar, exactly one header row tall. Hidden on Now
        // Playing — that view carries the full transport, so the bar would be
        // redundant there.
        Loader {
            id: transport
            width: shellColumn.width
            height: transport.active ? root.transportHeight : 0
            active: QbzShell.currentView !== "nowplaying"
            visible: transport.active
            sourceComponent: transportBar
        }

        // --- Bottom NavRail (KioskShell.slint:635-658) -------------------
        NavRail {
            id: navRail
            width: shellColumn.width
            height: root.navRailHeight

            onDiscover: root.navigateWithTab("home", "home")
            onLibrary: root.navigateWithTab("library", "albums")
            onLocalLibrary: root.navigateWithTab("local", "albums")
            onMyqbz: QbzShell.navigateTo("mixtapes")
            // The reference assigns NavState.view directly here (:650), so it
            // records NO history entry. Qt's only view-switch path is
            // navigateTo, which always records (shell_bridge.rs:645 ->
            // main.rs:1682); the extra history entry is the accepted
            // divergence rather than a second Rust entry point.
            onPlaying: QbzShell.navigateTo("nowplaying")
            // The kiosk owns its Immersive mount below, so the visualizer tile
            // follows the same open funnel as the desktop view-mode row.
            onVisualizer: QbzImmersive.openFromMenu()
            onMenu: QbzShell.navigateTo("settings")
        }
    }

    // =====================================================================
    // 2. The player-zone focus ring (KioskShell.slint:666-677)
    // =====================================================================
    // An overlay in ROOT coordinates, outside the column, so it never
    // reflows anything. The transport is ONE focusable unit; on Now Playing
    // the bar is hidden and that view draws its own in-page ring instead.
    Rectangle {
        id: playerRing
        visible: QbzShell.currentView !== "nowplaying"
                 && QbzKioskNav.navActive
                 && QbzKioskNav.zone === "player"
        x: 0
        y: root.height - root.navRailHeight - root.transportHeight
        width: root.width
        height: root.transportHeight
        border.width: 3
        border.color: theme.accent
        radius: theme.radiusSm
        color: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.08)
    }

    // =====================================================================
    // 3. Root key handling (KioskShell.slint:684-723)
    // =====================================================================
    // One scope drives the whole kiosk: arrows move the ring through the
    // three zones (rail -> content -> player -> rail on Down, reversed on
    // Up), Enter activates, Esc goes back. MouseAreas never take keyboard
    // focus, so this survives every card/tile tap and only yields to the
    // search field.
    //
    // Evaluation order is literal and must be preserved. The reference's two
    // guards RETURN REJECT — the key keeps its normal path — and in Qt that
    // path is the hotkeys dispatcher at the end of this handler, whose own
    // central text-input gate (hotkeys_bridge.rs:295-299) is the counterpart
    // of the first guard. The textInputFocused predicate is computed exactly
    // as the desktop shell computes it (AppShell.qml:91).
    InputFocusDismiss { id: inputFocusDismiss }
    Connections {
        target: QbzShell
        function onCurrentViewChanged() { inputFocusDismiss.dismiss() }
    }

    Keys.onPressed: function (event) {
        var w = root.Window.window
        var afi = w !== null ? w.activeFocusItem : null
        var textInputFocused = (afi instanceof TextInput) || (afi instanceof TextEdit)

        if (!textInputFocused && !QbzImmersive.open) {
            if (event.key === Qt.Key_Left) {
                QbzKioskNav.navLeft()
                event.accepted = true
                return
            }
            if (event.key === Qt.Key_Right) {
                QbzKioskNav.navRight()
                event.accepted = true
                return
            }
            if (event.key === Qt.Key_Up) {
                QbzKioskNav.navUp()
                event.accepted = true
                return
            }
            if (event.key === Qt.Key_Down) {
                QbzKioskNav.navDown()
                event.accepted = true
                return
            }
            if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                // The three-way dispatch, KioskShell.slint:175-190. The rail
                // is always mounted, so it is called directly; a content view
                // is conditionally mounted and cannot be addressed from here,
                // so its request goes out as the activate-seq pulse. Both the
                // rail and the player arm still have to latch the ring (:176)
                // — bumpActivate() does it for the content arm itself
                // (kiosk_nav_qt.rs:222-225).
                if (QbzKioskNav.zone === "player") {
                    QbzKioskNav.markActive()
                    if (QbzPlayer.npHasTrack || QbzQueue.hasPlayTarget)
                        QbzPlayer.togglePlay()
                } else if (QbzKioskNav.zone === "rail") {
                    QbzKioskNav.markActive()
                    navRail.activateIndex(QbzKioskNav.index)
                } else {
                    QbzKioskNav.bumpActivate()
                }
                event.accepted = true
                return
            }
            // Escape only CONSUMES when there is history; otherwise it keeps
            // its normal path (:714-720), which here is the dispatcher's own
            // Escape stack.
            if (event.key === Qt.Key_Escape && QbzShell.canBack) {
                QbzShell.navigateBack()
                event.accepted = true
                return
            }
        }

        event.accepted = QbzHotkeys.keyPressed(event.key, event.modifiers,
                                               event.text, textInputFocused)
    }

    // Deferred mount focus (KioskShell.slint:728-735): synchronous focus
    // during the initial layout races it, so a one-shot timer grabs it a tick
    // later. The port already uses this exact 30ms idiom at
    // controls/QbzLineEdit.qml:123-128.
    Timer {
        interval: 30
        running: true
        repeat: false
        onTriggered: root.forceActiveFocus()
    }

    // QConnect bootstrap conflicts are global and can also originate from the
    // compact player bar used by kiosk mode.
    QconnectPlaybackConflictModal { }

    // The Connect flyout and the Cast picker are shell-level: their triggers
    // live in the back bar above, which is mounted on every route (the
    // transport bar that used to host them is unmounted on Now Playing).
    QconnectFlyout { id: connectFlyout }
    CastPicker { }

    // =====================================================================
    // 4. The kiosk's OWN Immersive mount (KioskShell.slint:737-747)
    // =====================================================================
    // The desktop hangs its copy inside AppShell, so the kiosk shell must
    // mount its own; declared LAST = on top of everything.
    //
    // The same surface as desktop, hosted locally because AppShell and
    // KioskShell are sibling roots. `preKiosk` prevents an Immersive close
    // from changing a fullscreen appliance window back to Windowed.
    ImmersiveView {
        anchors.fill: root
        preKiosk: true
    }

    // The two conditionally-mounted children, hoisted out of the column so
    // the positioner never sees a non-visual child.
    Component {
        id: kioskRouter
        ContentRouter { kiosk: true }
    }
    Component {
        id: transportBar
        KioskNowPlayingBar { }
    }
}
