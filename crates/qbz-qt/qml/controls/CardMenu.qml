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

    // --- Submenus (2026-09-13) ------------------------------------------
    // An entry with `submenu: [ { label, icon, action }, … ]` opens a
    // second CardMenu beside its row ON HOVER (a click does the same and
    // never closes this one). The child forwards its pick here, so hosts
    // handle sub-actions in the same `onPicked`. Both menus stay open
    // while the pointer is over EITHER; once it has left both for a
    // moment they close together, as does a press outside or Escape.
    property var _sub: null
    property var _subRow: null
    // The child is the same type; QML refuses a type that contains itself
    // ("instantiated recursively"), so it is created from its file at run
    // time instead of through a Component.
    function _makeSubmenu() {
        var component = Qt.createComponent("CardMenu.qml")
        if (component.status !== Component.Ready) {
            console.warn("[CardMenu] submenu component: " + component.errorString())
            return null
        }
        var sub = component.createObject(cmRoot.contentItem, {
            "kioskHost": cmRoot.kioskHost,
            "menuWidth": cmRoot.menuWidth
        })
        if (sub)
            sub.picked.connect(function (a) {
                cmRoot.close()
                cmRoot.picked(a)
            })
        return sub
    }
    function hasSubmenu(entry) {
        return entry && entry.submenu !== undefined && entry.submenu !== null
            && entry.submenu.length !== undefined && entry.submenu.length > 0
    }
    function openSubmenu(row) {
        if (cmRoot._subRow === row && cmRoot._sub && cmRoot._sub.opened)
            return
        if (!cmRoot._sub)
            cmRoot._sub = cmRoot._makeSubmenu()
        if (!cmRoot._sub)
            return
        cmRoot._subRow = row
        cmRoot._sub.entries = row.modelData.submenu
        // Beside the row: to the right, or to the left when the window
        // has no room there. A 4px overlap leaves no gap to cross.
        var win = row.Window.window
        var g = row.mapToItem(null, 0, 0)
        var gx = cmRoot.x + cmRoot.width - 4
        if (win && gx + cmRoot._sub.width > win.width - 8)
            gx = cmRoot.x - cmRoot._sub.width + 4
        cmRoot._sub._place(gx, g.y - cmRoot._sub.topPadding, win)
        leaveTimer.stop()
    }
    function closeSubmenu() {
        cmRoot._subRow = null
        if (cmRoot._sub && cmRoot._sub.opened)
            cmRoot._sub.close()
        leaveTimer.stop()
    }
    readonly property bool subOpen: cmRoot._sub !== null && cmRoot._sub.opened
    // The pointer is over THIS menu: its panel or one of its rows. The rows'
    // MouseAreas take the hover events, so the panel's HoverHandler alone
    // sees only the 5px padding — the pair looked "left" the moment the
    // pointer crossed from that padding into a child row, and the leave
    // timer closed both (2026-09-14). One row is hot at a time, so an item
    // reference beats a counter that a destroyed row could leave high.
    property Item _hotRow: null
    readonly property bool hovering: cmRoot.menuHovered || cmRoot._hotRow !== null
    readonly property bool pairHovered: cmRoot.hovering
        || (cmRoot._sub !== null && cmRoot._sub.hovering)
    onPairHoveredChanged: {
        if (!cmRoot.subOpen)
            return
        if (cmRoot.pairHovered)
            leaveTimer.stop()
        else
            leaveTimer.restart()
    }
    Timer {
        id: leaveTimer
        interval: 350
        onTriggered: {
            if (cmRoot.subOpen && !cmRoot.pairHovered) {
                cmRoot.closeSubmenu()
                cmRoot.close()
            }
        }
    }
    onClosed: {
        cmRoot._hotRow = null
        closeSubmenu()
    }

    QbzTheme { id: theme }

    Repeater {
        model: cmRoot.entries
        delegate: Rectangle {
            id: row
            required property var modelData

            readonly property bool isSep: modelData.sep === true
            readonly property bool hasSub: cmRoot.hasSubmenu(modelData)
            readonly property bool rowEnabled: modelData.enabled !== false
            readonly property bool isDanger: modelData.danger === true
            readonly property bool hot: (cmiArea.containsMouse || cmRoot._subRow === row) && rowEnabled

            width: parent ? parent.width : 0
            height: isSep ? 7 : (cmRoot.kioskHost ? 44 : 33)
            radius: isSep ? 0 : 5
            color: hot ? theme.surfaceHover : "transparent"
            Component.onDestruction: {
                if (cmRoot._hotRow === row)
                    cmRoot._hotRow = null
            }
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
                    width: parent.width - 23 - (row.hasSub ? 22 : 0)
                    text: row.modelData.label || ""
                    color: row.isDanger ? theme.danger
                        : (row.hot ? theme.textPrimary : theme.textSecondary)
                    font.pixelSize: cmRoot.kioskHost ? 16 : 13
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                // Submenu marker: the row opens its child menu on hover.
                QbzIcon {
                    visible: row.hasSub
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
                    if (row.hasSub) {
                        cmRoot.openSubmenu(row)
                        return
                    }
                    cmRoot.close()
                    cmRoot.picked(row.modelData.action)
                }
                // Hovering a submenu row opens its child; hovering any other
                // row puts it away. Leaving the row itself does nothing —
                // the pointer is usually on its way into the child.
                onContainsMouseChanged: {
                    if (!containsMouse) {
                        if (cmRoot._hotRow === row)
                            cmRoot._hotRow = null
                        return
                    }
                    cmRoot._hotRow = row
                    if (row.hasSub)
                        cmRoot.openSubmenu(row)
                    else if (cmRoot.subOpen)
                        cmRoot.closeSubmenu()
                }
            }
        }
    }
}
