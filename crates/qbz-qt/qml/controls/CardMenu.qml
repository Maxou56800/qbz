// CardMenu — THE shared ⋯ / right-click menu surface (promoted from
// LibraryView in phase 21, brought up to primitives/ContextMenuItem.slint
// in the menu-parity round).
//
// Driven by an `entries` model; emits picked(action). Entry shape:
//
//   { label, icon, action }              a normal row (33px)
//   { ..., enabled: false }              muted row, ignores clicks
//                                        (ContextMenuItem.slint `enabled`)
//   { ..., danger: true }                destructive row, red label
//                                        (ContextMenuItem.slint `danger`)
//   { sep: true }                        1px group separator + 3px air
//                                        (AlbumContextMenu.slint's
//                                        `Rectangle { height: 1px; }`)
//
// Row metrics are ContextMenuItem.slint's: 33px, icon 15px, label 13px,
// and BOTH go text-primary on hover (the .slint tints icon + label
// together; the old POC row kept them permanently secondary).
//
// DEVIATION, deliberate: a `danger` row reddens the LABEL only. Slint
// tints the icon with Theme.danger at render time; QML tinting here is
// pre-baked per tint directory (assets/icons/<tint>/) and there is no
// `danger` bake — inventing one needs a new asset set + build.rs glue, and
// a missing pre-baked file renders NOTHING at runtime. The red label
// carries the signal on its own.

import QtQuick
import com.blitzfc.qbz
import "../theme"

QbzContextMenu {
    id: cmRoot

    property var entries: []
    signal picked(string action)

    QbzTheme { id: theme }

    Repeater {
        model: cmRoot.entries
        delegate: Rectangle {
            id: row
            required property var modelData

            readonly property bool isSep: modelData.sep === true
            readonly property bool rowEnabled: modelData.enabled !== false
            readonly property bool isDanger: modelData.danger === true
            readonly property bool hot: cmiArea.containsMouse && rowEnabled

            width: parent ? parent.width : 0
            height: isSep ? 7 : (cmRoot.kioskHost ? 44 : 33)
            radius: isSep ? 0 : 5
            color: hot ? theme.surfaceHover : "transparent"
            // ContextMenuItem.slint: `opacity: enabled ? 1.0 : 0.4`.
            opacity: (isSep || rowEnabled) ? 1.0 : 0.4

            // Separator: a 1px rule centred in the 7px band.
            Rectangle {
                visible: row.isSep
                y: 3
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            Row {
                visible: !row.isSep
                anchors.fill: parent
                anchors.leftMargin: 8
                spacing: 8
                QbzIcon {
                    name: row.modelData.icon || ""
                    width: 15
                    height: 15
                    anchors.verticalCenter: parent.verticalCenter
                    // Host is `surfaceHover`/transparent over a theme surface,
                    // so the glyph tracks the sibling label (textPrimary/
                    // textSecondary below) rather than a fixed white.
                    tintName: row.hot ? "textPrimary" : "secondary"
                }
                Text {
                    height: parent.height
                    width: parent.width - 23 - (row.modelData.submenu === true ? 22 : 0)
                    text: row.modelData.label || ""
                    color: row.isDanger ? theme.danger
                        : (row.hot ? theme.textPrimary : theme.textSecondary)
                    font.pixelSize: cmRoot.kioskHost ? 16 : 13
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                // Submenu marker (`submenu: true`): the host answers the pick
                // by opening a second menu on the same anchor.
                QbzIcon {
                    visible: row.modelData.submenu === true
                    name: "chevron-right"
                    width: 14
                    height: 14
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: row.hot ? "textPrimary" : "muted"
                }
            }

            MouseArea {
                id: cmiArea
                anchors.fill: parent
                // A separator and a muted row take no input at all — a
                // disabled MouseArea also reports no hover, so neither
                // highlights.
                enabled: !row.isSep && row.rowEnabled
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    cmRoot.close()
                    cmRoot.picked(row.modelData.action)
                }
            }
        }
    }
}
