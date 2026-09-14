// WallpaperField — the desktop wallpaper behind the window (2026-09-14): the
// part of it that lies UNDER this window, so moving the window slides the
// picture the way a translucent window would, lightly blurred beneath the
// half-alpha chrome. Nothing animates: it costs a frame only when the window
// moves or resizes, and a screen-sized texture at most (`sourceSize` caps the
// decode — a 3x-upscaled wallpaper never reaches the GPU whole).
//
// WHERE THE WINDOW IS. X11, macOS and Windows report the window's position;
// Wayland hides it from clients by design, so there the crop is centred on
// the screen until the Plasma window-management protocol is wired
// (wallpaper_qt.rs header). `hostWindow` is the ApplicationWindow.

import QtQuick
import QtQuick.Window
import QtQuick.Effects

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

    readonly property bool positionKnown: root.hostWindow !== null
        && Qt.platform.pluginName !== "wayland"
    readonly property real screenW: Math.max(1, Screen.width)
    readonly property real screenH: Math.max(1, Screen.height)
    // The window's origin inside its screen; centred when unknown.
    readonly property real offX: root.positionKnown
        ? root.hostWindow.x - Screen.virtualX
        : Math.max(0, (root.screenW - root.width) / 2)
    readonly property real offY: root.positionKnown
        ? root.hostWindow.y - Screen.virtualY
        : Math.max(0, (root.screenH - root.height) / 2)

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
        blurMax: 48
    }
    Rectangle {
        anchors.fill: parent
        color: "#000000"
        opacity: root.dim
    }
}
