// MiniSeek — the footer's seek bar (2026-08-03 miniplayer/tray contract A-29,
// §4.3.3), port of `component MiniSeek inherits Rectangle` at
// `crates/qbz-ui/ui/miniplayer/MiniFooter.slint:168-209`.
//
// A THIN TRACK INSIDE A TALL HIT AREA: the visible bar is `trackHeight`
// (3 px full, 2 px compact, 1 px micro) and the pointer target is the whole
// root height (14 / 10 / 4). No thumb, no hover growth, no buffered overlay —
// the reference has none of the three (:168-209 read whole), and the deferred
// polish its file header records (a pending-seek spinner thumb, a micro marquee)
// is deferred THERE too.
//
// This component owns NO state. It reports `preview` while the pointer is down
// and `commit` on release, and MiniFooter owns the pending-seek machine — which
// is why `progress` is an input rather than a read of QbzPlayer: the footer
// feeds it the EFFECTIVE progress, so the fill does not snap back to the
// player's stale value between the click and the engine catching up.
//
// DELIBERATELY NOT clamped to `QbzPlayer.npSeekableMax`, unlike the main bars:
// the reference's mini has no seekable-max notion at all, and §12-P16 keeps the
// mini's "ignores is-remote / volume-locked / cast-active" behaviour as a
// REPRODUCE-1:1 row rather than half-fixing it here.

import QtQuick
import "../theme"

Item {
    id: root
    property bool lightForeground: false

    /// Effective 0..1, fed by MiniFooter's pending-seek machine.
    property real progress: 0.0
    property int trackHeight: 3
    /// Micro's bar is square-ended (:390).
    property bool rounded: true

    signal preview(real fraction)
    signal commit(real fraction)

    QbzTheme { id: theme }

    function fractionAt(px) {
        return root.width > 0 ? Math.max(0, Math.min(1, px / root.width)) : 0
    }

    Rectangle {
        id: track
        width: root.width
        height: root.trackHeight
        y: Math.round((root.height - height) / 2)
        radius: root.rounded ? height / 2 : 0
        color: root.lightForeground ? "#40ffffff" : theme.alphaTier(12)
        clip: true

        Rectangle {
            x: 0
            y: 0
            width: parent.width * Math.max(0, Math.min(1, root.progress))
            height: parent.height
            radius: parent.radius
            color: theme.accent
        }
    }

    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        // down -> preview, drag -> preview, up -> commit (:195-207).
        onPressed: function (mouse) { root.preview(root.fractionAt(mouse.x)) }
        onPositionChanged: function (mouse) {
            if (pressed)
                root.preview(root.fractionAt(mouse.x))
        }
        onReleased: function (mouse) { root.commit(root.fractionAt(mouse.x)) }
    }
}
