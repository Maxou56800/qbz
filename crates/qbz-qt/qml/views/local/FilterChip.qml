// One chip in the Albums quality/format/source filter popup
// (LocalLibraryView.slint:676 FilterChip). 30px tall, label width + 22px,
// radius 6 (ADR-008 — NOT a pill), accent fill + accent-text label when
// active, 1px border that follows the same state.

import QtQuick
import com.blitzfc.qbz
import "../../theme"

Rectangle {
    id: root

    property string label: ""
    property bool active: false
    signal toggled()

    QbzTheme { id: theme }

    height: 30
    width: lbl.implicitWidth + 22
    radius: 6
    border.width: 1
    border.color: active ? theme.accent : theme.borderSubtle
    color: active ? theme.accent
         : chipArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

    Text {
        id: lbl
        anchors.centerIn: parent
        text: root.label
        // On the accent fill. NOT a departure from
        // locallibrary/LocalLibraryView.slint:690 (`Theme.accent-text`) —
        // `accentGlyphColor` RETURNS accent-text on 34 of the 35 palettes.
        // It is a floor under it: rose-pine-dawn's accent-text #575279 on
        // accent #d7827e is 2.56:1, and the twin hands back black at 7.38:1.
        // Reading it here instead of accent-text is also what keeps this chip
        // agreeing with the genre pills, which had to diverge outright.
        color: root.active ? theme.accentGlyphColor : theme.textSecondary
        font.pixelSize: theme.fontLegal
        font.weight: root.active ? theme.weightSemibold : theme.weightRegular
    }
    MouseArea {
        id: chipArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.toggled()
    }
}
