// SidebarNowPlayingDock — the Large-NPB (mode 3) cover + spectrum dock: the
// L's vertical arm. QML port of crates/qbz-ui/ui/shell/SidebarNowPlayingDock.slint.
//
// Mounted as a ROOT overlay by AppShell, pinned flush to the window
// bottom-left (NOT inside the sidebar — that floated it above the bar).
// Layout, bottom-up: a square album cover (= the fed width) at the very
// bottom; ABOVE it either the compact spectrum band or a square scope the
// user can hide with the top-right eye button on the cover.
//
// HEIGHT IS LOAD-BEARING. `QbzShell.largeDockHeight` (shell_bridge.rs) is the
// single source of truth and the layout below is derived from the SAME
// constants and the persisted render mode: legacy modes are 42px high;
// scope modes use the same square size as the artwork and grow upward.
// Sidebar.qml reserves `largeDockHeight - npb height` so the playlist list
// stops ABOVE the cover, and AppShell pins this overlay at
// `height - largeDockHeight`. Change the arithmetic in one place and the
// cover slides off the window edge.
//
// The spectrum strip itself lives in SpectrumBand.qml (render modes + motion);
// its perf contract is documented there. This file owns the gating and the
// layout arithmetic only.
//
// GATING RULE (owner, 2026-07-28): freeze on NOT VISIBLE, never on lost
// focus — a tiling desktop keeps windows visible and unfocused.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    // Fed by AppShell (sidebar content width). The cover is square.
    // height comes from the bridge — never recompute it here.
    height: QbzShell.largeDockHeight

    // Fed by AppShell so the ambient-vs-solid surface rule lives in ONE place.
    property bool ambientOn: false

    readonly property bool bandOn: QbzShell.largeVisualizerOn
    readonly property int bandHeight: QbzShell.gpuTier
        && QbzShell.largeSpectrumMode >= 3
        ? Math.round(root.width) : 42
    readonly property int padTop: 9
    readonly property int bandGap: 10
    // The cover's top edge: below the band when it is shown.
    readonly property int artY: padTop + (bandOn ? bandHeight + bandGap : 0)

    // True unless the window is minimized or hidden — see the GATING RULE.
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true

    // Capture gate — the Slint AppShell's `viz-should-run`. OFF stops the FFT
    // producer outright (viz_qt.rs parks it), so leaving Large or hiding the
    // band costs nothing. The transport is mirrored separately (a paused
    // player parks the producer via now_playing.rs).
    readonly property bool vizShouldRun: root.visible && root.bandOn && root.windowShowing
    onVizShouldRunChanged: QbzViz.setEnabled(root.vizShouldRun)
    Component.onCompleted: QbzViz.setEnabled(root.vizShouldRun)
    Component.onDestruction: QbzViz.setEnabled(false)

    // Top hairline between the playlists and the dock.
    Rectangle {
        x: 0
        y: 0
        width: root.width
        height: 1
        color: theme.borderSubtle
    }

    // ---- Spectrum band -----------------------------------------------------
    SpectrumBand {
        x: 0
        y: root.padTop
        width: root.width
        height: root.bandHeight
        shown: root.bandOn
        capturing: root.vizShouldRun
    }

    // ---- Album cover -------------------------------------------------------
    // Drop-shadow approximation of the Slint dock's 24px blur (two offset
    // rects).
    // SUPERSEDED (2026-07-29): this used to say "QtQuick.Effects renders
    // nothing on the software path". Effects need shaders, and this port runs
    // on the GPU (OpenGL RHI, measured); that note came from an offscreen
    // session, which forces the software renderer by definition — see
    // theme/RoundedImage.qml, which now DETECTS the software path with
    // `GraphicsInfo.api`. A real blurred shadow is possible; it is a visual
    // change needing its own parity pass, so the approximation stays for now.
    Rectangle {
        x: 0
        y: root.artY + 4
        width: root.width
        height: root.width
        radius: theme.radiusMd
        color: "#66000000"
    }

    Rectangle {
        id: art
        x: 0
        y: root.artY
        width: root.width
        height: root.width
        radius: theme.radiusMd
        // The art WELL takes surface-main @ 0.5, the chrome tier — not the
        // content pane's 0.22 (SidebarNowPlayingDock.slint:193). It shows
        // only in the frame around a cover that has not loaded, so at 0.22
        // it read as a hole in the sidebar rather than a recess in it.
        color: root.ambientOn ? theme.surfaceMainA50 : theme.surfaceMain
        // No clip: RoundedImage confines itself on both arms; a clip is an
        // unconditional batch root. The 28x28 eye chip and the centred icon
        // are strictly inside the square.

        RoundedImage {
            visible: QbzPlayer.npHasTrack
            anchors.fill: parent
            source: root.visible && QbzPlayer.npHasTrack ? QbzPlayer.npArtworkPath : ""
            radius: theme.radiusMd
        }
        QbzIcon {
            visible: !QbzPlayer.npHasTrack
            name: "music"
            width: root.width * 0.32
            height: root.width * 0.32
            anchors.centerIn: parent
            tintName: "muted"
        }

        // Hover tracker over the whole cover — reveals the toggle. The toggle
        // ALSO stays visible while the band is hidden, so it can be brought back.
        MouseArea {
            id: coverHover
            anchors.fill: parent
            hoverEnabled: true
        }

        // Single top-right toggle: show/hide the spectrum band.
        Rectangle {
            width: 28
            height: 28
            x: parent.width - width - 8
            y: 8
            radius: 8
            color: eyeHover.containsMouse ? "#2effffff" : "#80000000"
            border.width: 1
            border.color: "#33ffffff"
            opacity: (coverHover.containsMouse || eyeHover.containsMouse || !root.bandOn) ? 1.0 : 0.0
            Behavior on opacity { NumberAnimation { duration: 140 } }

            QbzIcon {
                anchors.centerIn: parent
                name: root.bandOn ? "eye" : "eye-off"
                width: 15
                height: 15
                // On the #80000000 / #2effffff chip over the artwork —
                // theme-independent, so a fixed white glyph is correct.
                tintName: "white"
            }
            MouseArea {
                id: eyeHover
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: QbzShell.largeToggleVisualizer()
            }
        }
    }
}
