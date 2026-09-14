// Bulk action bar — the floating "N selected" strip with one icon button per
// bulk action (primitives/MultiSelectBar.slint).
//
// PROMOTED from views/local/LocalMultiSelectBar.qml, unchanged except for the
// import depth. The Slint component it ports was already shared — Local Library
// mounts it for Albums (LocalLibraryView.slint:1239) and Tracks (:1406), and
// the MyQBZ detail page mounts the same primitive (:1253) — so the "Local"
// prefix was a naming accident, not a scoping one. The old
// views/local/LocalMultiSelectBar.qml is GONE — this file is the only copy.
// Call sites: views/local/LocalAlbumsTab.qml, views/local/LocalTracksTab.qml,
// views/myqbz/MyQbzDetailView.qml.
//
// 44px tall, radius 8, surface-elevated: "N selected" on the left, then one
// 34x30 icon button per action — dimmed to 0.4 and disarmed while the action
// needs a selection and there is none, red-bordered when it is a danger
// action. ADR-008: bordered/hover buttons, not pills.
//
// `width` has NO default: every call site anchors or sizes it (the MyQBZ detail
// page uses the .slint's deliberately asymmetric `x: 18` / `width: parent.width
// - 40`). The label Text sizes itself from `parent.width - actionRow.width`, so
// a zero width silently collapses the label rather than the buttons.
//
// The Slint puts the action label in the shared tooltip bubble; the Qt port
// has no tooltip channel into this component, so the label rides Qt's own
// ToolTip. (controls/QbzTooltip.qml is the shell's single hover-tooltip
// overlay, but it is reached by calling showRight()/showAbove() on the
// AppShell instance, which a recycled delegate deep inside a view cannot name.)

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root
    property bool kioskHost: false

    /// [{ id, label, icon, danger, needsSelection }] — `id` is what `action`
    /// publishes; `label` is the tooltip text and must arrive ALREADY
    /// translated (this component calls no `tr`); `icon` must exist in the
    /// "warning", "textPrimary" and "secondary" tint dirs.
    property var actions: []
    property int selectedCount: 0
    signal action(string id)

    QbzTheme { id: theme }

    height: 44
    radius: 8
    color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)

    Row {
        anchors.fill: parent
        anchors.leftMargin: 14
        anchors.rightMargin: 10
        spacing: 6

        Text {
            width: parent.width - actionRow.width - 6
            height: parent.height
            text: root.selectedCount + " "
                + QbzSession.tr("selected", QbzSession.trRev)
            color: theme.textSecondary
            font.pixelSize: theme.fontBody
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        Row {
            id: actionRow
            height: parent.height
            spacing: 6
            Repeater {
                model: root.actions
                delegate: Rectangle {
                    id: btn
                    required property var modelData
                    readonly property bool armed: !modelData.needsSelection
                        || root.selectedCount > 0
                    width: root.kioskHost ? 44 : 34
                    height: root.kioskHost ? 44 : 30
                    anchors.verticalCenter: parent.verticalCenter
                    radius: theme.radiusSm
                    opacity: armed ? 1.0 : 0.4
                    color: modelData.danger
                        ? (btnArea.containsMouse ? theme.dangerHover : "transparent")
                        : (btnArea.containsMouse ? theme.surfaceHover : "transparent")
                    border.width: modelData.danger ? 1 : 0
                    border.color: theme.dangerBorder
                    QbzIcon {
                        name: btn.modelData.icon
                        width: 16
                        height: 16
                        anchors.centerIn: parent
                        tintName: btn.modelData.danger ? "warning"
                            : btnArea.containsMouse ? "textPrimary" : "secondary"
                    }
                    MouseArea {
                        id: btnArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: btn.armed ? Qt.PointingHandCursor : Qt.ArrowCursor
                        onClicked: if (btn.armed) root.action(btn.modelData.id)
                        ToolTip.visible: containsMouse
                        ToolTip.text: btn.modelData.label
                        ToolTip.delay: 400
                    }
                }
            }
        }
    }
}
