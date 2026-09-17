// QbzRadioOption — one labelled radio row (the `KindRadio` component that
// AddToMixtapeModal.slint:28-60 and CreateMyQbzModal.slint:22-55 declare
// BYTE-IDENTICALLY, once each). The port has no radio control at all, so this
// is the first one; both MyQBZ modals use it and nothing else does yet.
//
// Geometry off the .slint: an 18x18 circle (r9) whose ring is 1.5px
// `text-muted` when unselected and vanishes when selected (the fill takes
// over), holding a 7x7 r3.5 dot; 6px to a `Typography.body` label. The whole
// row is the hit target and shows the pointer cursor (the .slint's root IS a
// TouchArea).
//
// The dot is `theme.accentGlyphColor`, not the .slint's literal `#ffffff`
// (:49 / :44) — the measured on-accent selector (theme/QbzTheme.qml, "ON AN
// ACCENT FILL"), the same divergence views/local/SelectCheck.qml already
// takes for its check glyph.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    property string label: ""
    property string description: ""
    property bool kioskHost: false
    property bool selected: false
    signal clicked()

    QbzTheme { id: theme }

    implicitWidth: lbl.implicitWidth + 24
    implicitHeight: Math.max(root.kioskHost ? 44 : 18, row.implicitHeight)
    activeFocusOnTab: enabled
    Accessible.role: Accessible.RadioButton
    Accessible.name: label
    Accessible.description: description
    Accessible.checked: selected
    Accessible.onPressAction: if (enabled) clicked()
    Keys.onSpacePressed: clicked()
    Keys.onReturnPressed: clicked()
    Keys.onEnterPressed: clicked()

    Row {
        id: row
        width: parent.width
        anchors.verticalCenter: parent.verticalCenter
        spacing: 6

        Rectangle {
            y: Math.max(0, (lbl.height - height) / 2)
            width: 18
            height: 18
            radius: 9
            border.width: root.selected && !root.activeFocus ? 0 : 1.5
            border.color: root.activeFocus ? theme.accent : theme.textMuted
            color: root.selected ? theme.accent : "transparent"
            Rectangle {
                visible: root.selected
                anchors.centerIn: parent
                width: 7
                height: 7
                radius: 3.5
                color: theme.accentGlyphColor
            }
        }
        Column {
            width: parent.width - 24
            spacing: 4
            Text {
                id: lbl
                width: parent.width
                text: root.label
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
                wrapMode: Text.WordWrap
                verticalAlignment: Text.AlignVCenter
            }
            Text {
                visible: root.description !== ""
                width: parent.width
                text: root.description
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                wrapMode: Text.WordWrap
            }
        }
    }

    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onClicked: { root.forceActiveFocus(); root.clicked() }
    }
}
