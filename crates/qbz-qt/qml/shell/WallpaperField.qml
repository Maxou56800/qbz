// WallpaperField — the desktop wallpaper behind the window (2026-09-14): the
// part of it that lies UNDER this window, wherever the window is, so the
// window reads as translucent to the desktop — an X-ray straight to the
// wallpaper, never to the windows in between (a really translucent surface
// would show those). Lightly blurred beneath the half-alpha chrome: enough
// that nothing in the picture competes with the text, little enough that the
// picture is still recognisable. Nothing animates: it costs a frame only when
// the window moves or resizes, and a screen-sized texture at most
// (`sourceSize` caps the decode — a 3x-upscaled wallpaper never reaches the
// GPU whole).
//
// WHERE THE WINDOW IS. X11, macOS and Windows tell Qt (`hostWindow.x/y`).
// Wayland hides it from clients by design; on KDE Plasma the compositor
// reports every window's geometry through org_kde_plasma_window_management
// (wallpaper_wayland_qt.rs), published as QbzShell.wallpaperWindowsJson —
// this process's windows with absolute coordinates — and the one whose size
// is this window's is the one the picture sits under (the miniplayer and the
// dialogs are this process's windows too). Where neither can say, the crop
// is centred on the screen. `hostWindow` is the ApplicationWindow.

import QtQuick
import QtQuick.Window
import QtQuick.Effects
import com.blitzfc.qbz

Item {
    id: root

    /// file:// URL of the wallpaper (QbzShell.wallpaperUrl).
    property string source: ""
    property var hostWindow: null
    /// MultiEffect blur amount, 0 = crisp.
    property real blur: 0.0
    /// Darkening on top (the dark-theme legibility veil).
    property real dim: 0.0

    clip: true

    readonly property bool onWayland: Qt.platform.pluginName === "wayland"

    // Ask the compositor as soon as this field is actually painting on a
    // Wayland session (idempotent on the Rust side; a no-op elsewhere).
    function _track() {
        if (root.visible && root.onWayland && root.source !== "")
            QbzShell.trackWindowPosition()
    }
    onVisibleChanged: _track()
    onSourceChanged: _track()
    Component.onCompleted: _track()

    // Read only while this field is the background: the tracker keeps
    // reporting moves after the mode is switched away, and an invisible item
    // whose geometry changes still dirties the window (the repaint rule).
    readonly property var compositorRects: {
        if (!root.visible || !root.onWayland)
            return []
        try {
            var a = JSON.parse(QbzShell.wallpaperWindowsJson || "[]")
            return Array.isArray(a) ? a : []
        } catch (e) {
            return []
        }
    }
    /// The compositor's rectangle for THIS window, or null when it has said
    /// nothing (no protocol, or nothing our size).
    readonly property var compositorRect: root.pickRect(root.compositorRects,
        root.hostWindow ? root.hostWindow.width : root.width,
        root.hostWindow ? root.hostWindow.height : root.height)

    /// The rectangle whose size is closest to `w` x `h`, when it is close
    /// enough to be this window at all: a geometry that is not our size
    /// belongs to another of our windows, and the centred crop beats that
    /// window's place.
    function pickRect(rects, w, h) {
        var best = null
        var bestScore = 0
        for (var i = 0; i < rects.length; i++) {
            var r = rects[i]
            if (!r || typeof r.w !== "number" || typeof r.h !== "number"
                    || typeof r.x !== "number" || typeof r.y !== "number")
                continue
            var score = Math.abs(r.w - w) + Math.abs(r.h - h)
            if (best === null || score < bestScore) {
                best = r
                bestScore = score
            }
        }
        return (best !== null && bestScore <= 64) ? best : null
    }

    readonly property bool positionKnown: root.visible && root.hostWindow !== null
        && (!root.onWayland || root.compositorRect !== null)
    readonly property real screenW: Math.max(1, Screen.width)
    readonly property real screenH: Math.max(1, Screen.height)
    // The window's origin inside its screen; centred when unknown.
    readonly property real offX: !root.positionKnown
        ? Math.max(0, (root.screenW - root.width) / 2)
        : (root.onWayland ? root.compositorRect.x : root.hostWindow.x) - Screen.virtualX
    readonly property real offY: !root.positionKnown
        ? Math.max(0, (root.screenH - root.height) / 2)
        : (root.onWayland ? root.compositorRect.y : root.hostWindow.y) - Screen.virtualY

    Image {
        id: picture
        source: root.source
        x: -root.offX
        y: -root.offY
        width: root.screenW
        height: root.screenH
        fillMode: Image.PreserveAspectCrop
        asynchronous: true
        cache: false
        sourceSize.width: Math.round(root.screenW * Screen.devicePixelRatio)
        sourceSize.height: Math.round(root.screenH * Screen.devicePixelRatio)
        smooth: true
    }
    MultiEffect {
        visible: root.blur > 0 && picture.status === Image.Ready
        source: picture
        x: picture.x
        y: picture.y
        width: picture.width
        height: picture.height
        autoPaddingEnabled: false
        blurEnabled: true
        blur: root.blur
        blurMax: 64
    }
    Rectangle {
        anchors.fill: parent
        color: "#000000"
        opacity: root.dim
    }
}
