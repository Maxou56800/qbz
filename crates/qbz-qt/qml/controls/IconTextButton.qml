// Small bordered icon+label button — the QML port of the private
// `IconTextButton` in crates/qbz-ui/ui/shell/LogViewerModal.slint:29-75.
//
// It exists because it AUTO-SIZES TO ITS LABEL. `controls/SettingsButton.qml`
// carries `minWidth: 160`, which is right for a settings row's right rail and
// wrong for a footer of five buttons — using it there overflowed the panel and
// the labels collided (observed 2026-08-05).
//
// Geometry is 1:1 with the reference: height 32, Radius.sm, 1px border-subtle,
// surface-elevated (surface-hover on hover), opacity 0.4 when disabled,
// 12px side padding, 7px gap, 14px icon, 13px medium label. `danger` recolors
// BOTH the icon and the label — it does not fill.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    property string label: ""
    property string iconName: ""
    /// The reference's `has-icon`: an icon-less variant keeps the same
    /// padding and height.
    property bool hasIcon: true
    property bool danger: false
    property bool btnEnabled: true

    signal clicked()

    QbzTheme { id: theme }

    height: 32
    // Auto-size: padding + optional icon + gap + label.
    width: 12 + (root.hasIcon ? 14 + 7 : 0) + labelText.implicitWidth + 12
    radius: theme.radiusSm
    border.width: 1
    border.color: theme.borderSubtle
    color: hoverArea.containsMouse && root.btnEnabled ? theme.elevatedHoverFill : theme.elevatedFill
    opacity: root.btnEnabled ? 1.0 : 0.4

    Row {
        anchors.centerIn: parent
        spacing: 7
        QbzIcon {
            visible: root.hasIcon
            anchors.verticalCenter: parent.verticalCenter
            name: root.iconName
            width: 14
            height: 14
            // `favorite` is the port's documented stand-in for the danger red
            // (icon_tint_qt.rs:63); there is no `danger` bake.
            tintName: root.danger ? "favorite" : "secondary"
        }
        Text {
            id: labelText
            anchors.verticalCenter: parent.verticalCenter
            text: root.label
            color: root.danger ? theme.danger : theme.textSecondary
            font.pixelSize: 13
            font.weight: theme.weightMedium
        }
    }

    MouseArea {
        id: hoverArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: root.btnEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: if (root.btnEnabled) root.clicked()
    }
}
