// Folders tree-mode LEFT RAIL (LocalLibraryView.slint:1641).
//
// Header toolbar: select-mode, collapse-all, open-ephemeral-folder, then the
// search box (stretches). Below it the compact bulk bar (only in select mode
// with a selection — icon buttons, because the rail is narrow), then the
// tree itself.
//
// The tree is a FLAT array windowed by a ListView — the recursive component
// is deliberately not reproduced, and levels are fetched lazily on expand.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Item {
    id: root

    property var view: null

    QbzTheme { id: theme }

    // ---- Shift / Ctrl on the rail's checkboxes ----
    // The selection lives in RUST (QbzLocal.tree*Select owns the set of
    // paths); what the rail holds is the one piece the rule needs on top of
    // it, the ANCHOR: the path of the last plain or Ctrl click. A Shift-click
    // looks it up among the rows drawn right now and SELECTS every row from
    // there to the clicked one (QbzLocal.treeSelectRange — additive, like
    // controls/SelectionModel.qml); an anchor that is not drawn any more
    // (collapsed, filtered away) makes it a plain toggle again.
    property string selectAnchor: ""
    Connections {
        target: root.view
        function onTreeSelectModeChanged() {
            if (!root.view.treeSelectMode)
                root.selectAnchor = ""
        }
    }
    /// The rows from `anchorPath` to `path`, inclusive, in rail order, as the
    /// `{path, isFolder}` pairs treeSelectRange takes. [] when either is not
    /// drawn.
    function rangeNodes(rows, anchorPath, path) {
        var from = -1
        var to = -1
        for (var i = 0; i < rows.length; i++) {
            if (rows[i].path === anchorPath) from = i
            if (rows[i].path === path) to = i
        }
        if (from < 0 || to < 0)
            return []
        var out = []
        for (var j = Math.min(from, to); j <= Math.max(from, to); j++)
            out.push({ "path": rows[j].path, "isFolder": rows[j].isFolder === true })
        return out
    }
    function toggleNodeSelect(node, mods) {
        if (((mods || 0) & Qt.ShiftModifier) !== 0 && root.selectAnchor !== "") {
            var nodes = root.rangeNodes(root.view.tree || [], root.selectAnchor, node.path)
            if (nodes.length > 0) {
                QbzLocal.treeSelectRange(JSON.stringify(nodes))
                return
            }
        }
        if (node.isFolder) QbzLocal.treeToggleFolderSelect(node.path)
        else QbzLocal.treeToggleTrackSelect(node.path)
        root.selectAnchor = node.path
    }

    Column {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 8
        anchors.topMargin: 12
        anchors.bottomMargin: 8
        spacing: 8

        // ---- Header toolbar ----
        Row {
            width: parent.width
            height: 30
            spacing: 4
            QbzIconButton {
                name: "square-check-big"
                btnSize: 30
                iconSize: 15
                active: root.view.treeSelectMode
                activeBackground: true
                onClicked: root.view.toggleTreeSelectMode()
            }
            QbzIconButton {
                name: "chevron-up"
                btnSize: 30
                iconSize: 15
                onClicked: QbzLocal.treeCollapseAll()
            }
            // Filler: the search collapses to a 30px magnifier, so the slot
            // stays right-aligned and the field opens LEFT over this gap.
            Item {
                // TWO buttons now, not three: "open folder" moved to the
                // header's `Open` menu, which serves every medium.
                width: Math.max(0, parent.width - 2 * 30 - 30 - 3 * 4)
                height: 1
            }
            LocalSearchBox {
                boxWidth: parent.width - 2 * 34
                placeholder: QbzSession.tr("Search folders", QbzSession.trRev)
                onEdited: function (v) {
                    root.view.treeSearch = v
                    treeSearchDebounce.restart()
                }
                Timer {
                    id: treeSearchDebounce
                    interval: 180
                    onTriggered: QbzLocal.treeSearch(root.view.treeSearch)
                }
            }
        }

        // ---- Compact bulk bar (select mode + a selection) ----
        Rectangle {
            visible: root.view.treeSelectMode && QbzLocal.localTreeSelectedCount > 0
            width: parent.width
            height: visible ? 36 : 0
            radius: 8
            color: theme.surfaceElevated
            Row {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 6
                spacing: 2
                Text {
                    width: parent.width - 7 * 30
                    height: parent.height
                    text: QbzLocal.localTreeSelectedCount + " "
                        + QbzSession.tr("sel", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: theme.fontLegal
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                Repeater {
                    // ASSET GAP: Slint's "Check all" uses folder-check, which
                    // is not baked in the Qt icon set (see GLUE).
                    model: [
                        { "icon": "square-check-big", "action": "select-all", "tip": QbzSession.tr("Check all", QbzSession.trRev) },
                        { "icon": "list-start", "action": "play-next", "tip": QbzSession.tr("Play next", QbzSession.trRev) },
                        { "icon": "list-plus", "action": "play-later", "tip": QbzSession.tr("Play later", QbzSession.trRev) },
                        { "icon": "list-end", "action": "queue", "tip": QbzSession.tr("Add to queue", QbzSession.trRev) },
                        { "icon": "list-music", "action": "add-to-playlist", "tip": QbzSession.tr("Add to playlist", QbzSession.trRev) },
                        { "icon": "cassette-tape", "action": "add-to-mixtape", "tip": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev) },
                        { "icon": "x", "action": "clear", "tip": QbzSession.tr("Clear", QbzSession.trRev) },
                    ]
                    delegate: QbzIconButton {
                        required property var modelData
                        anchors.verticalCenter: parent.verticalCenter
                        name: modelData.icon
                        btnSize: 28
                        iconSize: 15
                        onClicked: QbzLocal.foldersBulkAction(modelData.action)
                    }
                }
            }
        }

        // ---- The tree ----
        Item {
            width: parent.width
            height: parent.height - 38
                - (root.view.treeSelectMode && QbzLocal.localTreeSelectedCount > 0 ? 44 : 0)

            // Lazy level fetch: the rail shows the shape of the tree rows
            // (26px, small leading glyph) instead of a 28px spinner.
            QbzSkeleton {
                variant: "rowList"
                anchors.fill: parent
                visible: QbzLocal.localTreeLoading
                rowH: 26
                rowGap: 0
                rowArtSize: 14
                phase: root.view ? root.view.skelPhase : false
            }
            ListView {
                id: treeList
                anchors.fill: parent
                visible: !QbzLocal.localTreeLoading
                clip: true
                cacheBuffer: 26 * 20
                boundsBehavior: Flickable.StopAtBounds
                model: root.view.tree
                delegate: TreeRow {
                    required property var modelData
                    width: treeList.width
                    node: modelData
                    selected: modelData.path === root.view.selectedFolder
                    selectMode: root.view.treeSelectMode
                    onToggled: QbzLocal.treeToggle(modelData.path, !modelData.expanded)
                    onActivated: root.view.selectFolder(modelData.path)
                    // Shift ranges from the rail's anchor (see
                    // `toggleNodeSelect` at the top of this file).
                    onToggleSelect: function (mods) { root.toggleNodeSelect(modelData, mods) }
                }
            }
            QbzScrollBar {
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                target: treeList
                visible: treeList.contentHeight > treeList.height
            }
        }
    }
}
