// Settings SecondaryButton (IntegrationsSettings.slint:27 /
// AppearanceSettings.slint:83) — min 160px (compact variant 34px square for
// icon buttons), 34px tall, bordered, optional danger red (#e0564f ≈
// theme.danger), busy spinner text.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    id: root

    property string text: ""
    property string iconName: ""
    // Trailing chevron/glyph AFTER the label (the Slint settings rows that open
    // a sub-view: ContentFilteringSettings.slint's Manage affordance). "" = none.
    property string trailingIconName: ""
    property bool danger: false
    property bool busy: false
    // The two metrics the MyQBZ modal footers and the Disco builder need. The
    // Slint's SecondaryButton is 34px/min-160 in Settings and 38px/hug in a
    // modal footer, which is one control with two call-site numbers — not two
    // components (TRACK-RULES §5). minWidth: 0 = hug the content.
    property int btnHeight: 34
    property int minWidth: 160
    signal clicked()

    QbzTheme { id: theme }

    width: Math.max(iconName !== "" && text === "" ? 34 : minWidth, row.implicitWidth + 24)
    height: kioskHost ? Math.max(44, btnHeight) : btnHeight
    radius: theme.radiusSm
    border.width: root.activeFocus ? 2 : 1
    border.color: root.activeFocus ? theme.accent
        : (danger ? theme.danger : theme.borderSubtle)
    color: enabled && btnArea.containsMouse ? theme.elevatedHoverFill : theme.elevatedFill
    opacity: enabled ? 1.0 : 0.4
    activeFocusOnTab: enabled
    Accessible.role: Accessible.Button
    Accessible.name: text
    Accessible.onPressAction: if (root.enabled) root.clicked()

    Keys.onPressed: function (event) {
        if (root.enabled && !event.isAutoRepeat
                && (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter)) {
            root.clicked()
            event.accepted = true
        }
    }

    Row {
        id: row
        anchors.centerIn: parent
        spacing: 8
        QbzIcon {
            visible: iconName !== ""
            name: iconName
            width: 15
            height: 15
            anchors.verticalCenter: parent.verticalCenter
            tintName: danger ? "favorite" : "secondary"
        }
        Text {
            visible: text !== ""
            text: parent.parent.text
            color: parent.parent.danger ? theme.danger : theme.textPrimary
            font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
            font.weight: theme.weightMedium
            verticalAlignment: Text.AlignVCenter
        }
        QbzIcon {
            visible: parent.parent.trailingIconName !== ""
            name: parent.parent.trailingIconName
            width: 15
            height: 15
            anchors.verticalCenter: parent.verticalCenter
            tintName: parent.parent.danger ? "favorite" : "secondary"
        }
    }
    MouseArea {
        id: btnArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: parent.enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
        onPressed: if (root.enabled) root.forceActiveFocus()
        onClicked: if (root.enabled) root.clicked()
    }
}
