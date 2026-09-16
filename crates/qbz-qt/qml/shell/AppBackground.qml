// Shared app/miniplayer background. The host owns visibility; all animation
// stays on the existing shell pulse and each field observes its own window.
import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../immersive"
import "../theme"

Item {
    id: root
    property var hostWindow: null
    property bool activeBackground: theme.ambientOn
    readonly property bool painting: root.activeBackground && root.visible
        && (!root.Window.window || (root.Window.window.visibility !== Window.Hidden
                                    && root.Window.window.visibility !== Window.Minimized))
    QbzTheme { id: theme }

    // --- The background layers (AppShell.slint:206-242) --------------------
    // The bottom-most visual layer, declared before every chrome surface so
    // they all paint above it. Mode 1 and mode 2 are DIFFERENT looks and are
    // mutually exclusive; neither is mounted while a mode is off or nothing is
    // playing, which is the D4 "opaque theme restored" case.
    readonly property bool ambientModeOn: root.painting && QbzShell.ambientMode === 1
    readonly property bool blurredModeOn: root.painting && QbzShell.ambientMode === 2

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
    readonly property bool wallpaperModeOn: root.painting && QbzShell.ambientMode === 3
    readonly property bool wallpaperBlurOn: root.painting && QbzShell.ambientMode === 4
    WallpaperField {
        anchors.fill: parent
        visible: root.wallpaperModeOn
        source: root.wallpaperModeOn ? QbzShell.wallpaperUrl : ""
        hostWindow: root.hostWindow
        // The user's choice (Settings > Appearance, 0-100 % of Qt's 0.0-1.0),
        // live while that slider drags; 0.75 by default.
        blur: QbzShell.wallpaperBlur
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

}
