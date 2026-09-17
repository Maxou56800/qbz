// TBtn — the miniplayer footer's transport/volume button (2026-08-03
// miniplayer/tray contract A-29, §4.3.3), port of the inner
// `component TBtn inherits Rectangle` at
// `crates/qbz-ui/ui/miniplayer/MiniFooter.slint:16-49`.
//
// A file of its own rather than a QML inline component: the rule is A-29's and
// it is a SIZE rule, not a language rule — a 396-line MiniFooter.slint whose
// port folded five inner types in would land well past TRACK-RULES' ~500-line
// line. (QML does have `component X: Rectangle { … }`, and this tree uses it at
// qml/immersive/QueueTabsPanel.qml:135; nothing in this diff qualifies.)
//
// TWO details that look like bugs and are not:
//
//   - THE ICON TINT DOES NOT CHECK `enabled` (§12-P7). A disabled button still
//     lightens its glyph under the cursor; only the root's 0.3 opacity conveys
//     disabled. That is the reference's behaviour (:36-38 has no enabled arm
//     where :28 does), and it is reproduced rather than "fixed".
//   - The hover BACKGROUND does check it (:28).
//
// The idle glyph is `Theme.alpha-70`, which is not a name in QbzIcon's tint
// vocabulary — the vocabulary carries theme TOKENS, not the alpha ramp. It is
// spelled here as textPrimary at 0.70 opacity, which is what alpha-70 IS: the
// ramp is white-based on dark themes and black-based on light ones
// (QbzTheme.qml's alphaTier), and textPrimary follows the same polarity. Using
// "secondary" instead — the QbzIconButton idle tint — would be a different
// colour (#cccccc, 80 %) and would not track the ramp on light themes.
//
// `btnEnabled`, not `enabled`: `enabled` is a QQuickItem property and
// shadowing it disarms the whole subtree. Same spelling as QbzIconButton.

import QtQuick
import "../theme"

Rectangle {
    id: root
    property bool lightForeground: false

    /// Icon file stem, e.g. "shuffle" (QbzIcon appends ".svg").
    property string name: ""
    property int iconSize: 16
    /// The square edge; the radius is half of it, so the hover fill is a disc.
    property int btn: 30
    property bool active: false
    property bool btnEnabled: true
    signal clicked()

    QbzTheme { id: theme }

    width: root.btn
    height: root.btn
    radius: root.btn / 2
    antialiasing: true
    opacity: root.btnEnabled ? 1.0 : 0.3
    color: (ta.containsMouse && root.btnEnabled) ? theme.alphaTier(8) : "transparent"

    QbzIcon {
        name: root.name
        width: root.iconSize
        height: root.iconSize
        // Pixel-snapped centring, 1:1 with the reference's
        // `Math.round((parent.width - self.width) / 2 / 1px) * 1px` (:34-35):
        // a half-pixel offset on a 13 px glyph is visible as a blur.
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        tintName: root.active ? "accent" : (root.lightForeground ? "white" : "textPrimary")
        opacity: (root.active || ta.containsMouse) ? 1.0 : 0.70
    }

    MouseArea {
        id: ta
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: root.btnEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: if (root.btnEnabled) root.clicked()
    }
}
