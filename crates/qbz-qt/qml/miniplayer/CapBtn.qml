// CapBtn — the hover capsule's button (2026-08-03 miniplayer/tray contract
// A-29, §4.3.4), port of `component CapBtn inherits Rectangle` at
// `crates/qbz-ui/ui/miniplayer/MiniWindowControls.slint:14-53`.
//
// 22 x 20, radius 6 — NOT a disc like TBtn, and the trigger overrides the
// height to 22 so the collapsed capsule is a circle.
//
// TWO SIGNALS, and the difference is load-bearing (§15 trap 8): `clicked` is
// the ordinary press-release, `pressed` fires on pointer DOWN. `a-move` and the
// micro header's drag handle use the second one, because `startSystemMove()`
// hands the pointer grab to the compositor and a press-release would never
// arrive — a clicked handler would simply never drag.
//
// `danger` paints the close button's red hover fill, and the glyph on it stays
// a LITERAL #ffffff (tint "white"). QbzTheme's owner-approved
// glyph-on-accent selector (`accentGlyphTint`) does NOT apply: that decides the
// glyph on an ACCENT fill, and #ef4444 is not the accent (§4.11).
//
// The idle tint is `Theme.alpha-65`, spelled as textPrimary at 0.65 opacity for
// the reason TBtn.qml's header gives.
//
// The drop shadow the reference draws on the capsule chrome is dropped by the
// port's standing convention (QbzToast.qml:30, QbzTooltip.qml:51-61,
// MyQbzModals.qml:23) — it lives on MiniWindowControls, not here.

import QtQuick
import "../theme"

Rectangle {
    id: root

    property string name: ""
    property int iconSize: 13
    property bool active: false
    property real idleOpacity: 0.65
    /// The close button: red hover fill, white glyph on it.
    property bool danger: false
    /// `a-move` only — the grab cursor that says "this drags the window".
    property bool dragCursor: false
    /// Read by MiniWindowControls' collapse machine when it aggregates hover
    /// per button; the shipped machine uses one HoverHandler on the chrome
    /// instead, so this stays available without being on a hot path.
    readonly property bool hovered: ta.containsMouse

    signal clicked()
    /// Pointer DOWN. See the header.
    signal pressed()

    QbzTheme { id: theme }

    width: 22
    height: 20
    radius: 6
    antialiasing: true
    color: ta.containsMouse
           ? (root.danger ? "#ccef4444" : theme.alphaTier(12))  // #ef4444cc in Slint; Qt is #AARRGGBB
           : "transparent"

    QbzIcon {
        name: root.name
        width: root.iconSize
        height: root.iconSize
        // Pixel-snapped, exactly as TBtn does (:35-36).
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        tintName: root.active
                  ? "accent"
                  : ((ta.containsMouse && root.danger) ? "white" : "textPrimary")
        opacity: (root.active || ta.containsMouse) ? 1.0 : root.idleOpacity
    }

    MouseArea {
        id: ta
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: root.dragCursor ? Qt.OpenHandCursor : Qt.PointingHandCursor
        onClicked: root.clicked()
        onPressed: root.pressed()
    }
}
