// NowPlayingBar SHELL — the mode seam (NowPlayingBar.slint, phase 18):
// mounts NowPlayingBarSmall for mode 2 (Small) and the full PlayerBar for
// modes 0 (New) / 1 (Classic) / 3 (Large), and sizes itself mode-aware
// (42px Small / 112px otherwise, plus the gutter below).
//
// THE BAND (2026-09-13). This root is the bottom chrome band, the exact
// counterpart of HeaderBar at the top: one full-width piece of chrome, flush
// on the window's bottom edge, painted surface-card @ 0.5 over the field
// while the ambient background is active and opaque surface-card otherwise.
// The bar LAYOUT mounted below is inset by `gutter` on its left, right and
// bottom, so the bar's ends line up with the content pane's and nothing in
// it touches the window edge — while the band stays continuous around it.
// Both alternatives were tried and rejected the same day: insetting the
// whole bar left the raw field showing on three sides (the only zone of the
// shell without chrome), and a frame slab behind an inset plate read as a
// bar glued onto a slab. A layout knows nothing about the band or the
// gutter: it paints no background of its own and fills the rect it is given,
// which is what keeps a future layout from needing either.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root
    color: ambientOn ? theme.surfaceCardA50 : theme.surfaceCard
    readonly property bool ambientOn: theme.ambientOn

    QbzTheme { id: theme }

    /// The gutter the bar layout keeps to the window's left, right and
    /// bottom edges: the pane's 8px, wider on macOS (QbzTheme.npbGutterMac).
    readonly property int gutter: Qt.platform.os === "osx" ? theme.npbGutterMac : theme.npbGutter
    /// The layout's own height (PlayerBar.slint / PlayerBarSmall.slint).
    readonly property int layoutHeight: QbzShell.npbMode === 2 ? theme.npbSmallHeight : theme.npbLargeHeight
    implicitHeight: layoutHeight + gutter

    // The shell's shared hover-tooltip overlay (controls/QbzTooltip.qml),
    // fed in by AppShell (same pattern as Sidebar.tooltip). Forwarded into
    // whichever mode is loaded below: all four modes use it for dynamic
    // Shuffle/Repeat state, and the full bar also uses it for Qobuz Connect.
    property Item tooltip: null

    Loader {
        id: barLoader
        anchors.fill: parent
        // The inset. Top stays flush: the band's top edge IS the layout's.
        anchors.leftMargin: root.gutter
        anchors.rightMargin: root.gutter
        anchors.bottomMargin: root.gutter
        source: QbzShell.npbMode === 2 ? "NowPlayingBarSmall.qml" : "PlayerBar.qml"
    }

    // Same shape as AppShell's viewLoader "kind" Binding: applies the moment
    // the item exists, re-applies on a mode switch, RestoreNone because the
    // target is DESTROYED on unload. Both bar implementations declare the
    // property, so a mode switch keeps the same overlay host.
    Binding {
        target: barLoader.item
        property: "tooltip"
        value: root.tooltip
        when: barLoader.item !== null
        restoreMode: Binding.RestoreNone
    }
}
