// The app-wide selection checkbox (LocalLibraryView.slint:486-515): a 13px
// circle, accent-filled when on, 1.5px muted ring when off, 8px white check
// glyph — or a minus glyph for the folder tri-state "partial".
//
// Slint declares this inline inside TreeRow; the Qt port also needs it on
// the album cards and the track rows in multi-select, so it is its own file
// (three call sites, one shape).
//
// `toggled` carries the click's modifiers: this disc IS a select-mode click
// target, and one that dropped them turned every Shift-click on it into a
// plain toggle (controls/SelectionModel.qml).

import QtQuick
import com.blitzfc.qbz
import "../../theme"

Rectangle {
    id: root

    property bool on: false
    property bool partial: false
    property int diameter: 13
    signal toggled(int modifiers)

    QbzTheme { id: theme }

    width: diameter
    height: diameter
    radius: diameter / 2
    color: (on || partial) ? theme.accent : "transparent"
    border.width: (on || partial) ? 0 : 1.5
    border.color: checkArea.containsMouse ? theme.textPrimary : theme.textMuted

    QbzIcon {
        visible: root.on || root.partial
        name: root.partial ? "minus" : "check"
        width: Math.round(root.diameter * 0.62)
        height: Math.round(root.diameter * 0.62)
        anchors.centerIn: parent
        // DELIBERATE DIVERGENCE from primitives/SelectionCheckbox.slint:37,
        // which hardcodes `tint: #ffffff` on this same accent fill. That
        // white is under 2.6:1 on 16 of the 35 palettes, worst on
        // high-contrast at 1.70:1, then ikari 1.74 and wcag-dark 1.82 —
        // i.e. the two ACCESSIBILITY themes bracket the top of the list.
        // (This site used to claim ikari 1.74 was "the worst case in the
        // app"; it is second. The 8px glyph here does make it the smallest
        // thing painted at those ratios, which is a different argument and
        // the one worth keeping.) Slint's own QbzCheckbox.slint:24 uses
        // accent-text for the equivalent glyph, so upstream contradicts
        // ITSELF here; the selector settles it by measurement instead of by
        // picking a side. See theme/QbzTheme.qml, "ON AN ACCENT FILL".
        tintName: theme.accentGlyphTint
    }
    MouseArea {
        id: checkArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: function (mouse) { root.toggled(mouse.modifiers) }
    }
}
