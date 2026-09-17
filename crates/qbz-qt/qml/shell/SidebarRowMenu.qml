// Sidebar row context menu — the port of the PopupWindow declared inside
// SidebarRow (shell/Sidebar.slint:400-545). Right-press any tree row and it
// opens, row-anchored, with one of two arms:
//
//   FOLDER    Edit folder · Hide from sidebar                (exactly two rows —
//             there is NO Delete here. Slint declares a delete-folder callback
//             and wires it, but no menu item ever fires it: it belonged to a
//             hover "..." button that was deleted. Folder deletion lives only
//             in the Playlist Manager, so it is not ported as a menu entry.)
//
//   PLAYLIST  Edit playlist · Hide from sidebar* · Add to Mixtape/Collection*
//             · divider* · "Move to folder" caption · folder search (>= 8) ·
//             the folder list · Move to root (when filed) · "No folders yet."
//             (*) Qobuz only — see below.
//
// Facts that look like bugs and are not:
//
//  - LOCALS CAN BE MOVED TO FOLDERS. The move-to-folder section is gated on
//    "not a folder row", never on "not a local": one folder holds both kinds,
//    and for a user with no Qobuz account that is the whole feature.
//  - LOCALS CANNOT BE HIDDEN from here, and cannot be added to a Mixtape.
//    `myqbz_add_qt::accepts_item` answers NONE for (Playlist, Local) and the
//    core rejects the enqueue outright, so the row is ABSENT rather than
//    dimmed — a permanently dead affordance is the defect this port removed
//    from the sidebar, not a state to render.
//  - The divider is Qobuz-only too, so a local's menu reads "Edit playlist"
//    straight into the "Move to folder" caption with no separator.
//  - "Move to root" (chevron-left) is this surface's wording; the manager's
//    sibling menu says "No folder". Two words for one concept, in two
//    surfaces, exactly as the reference has them. Not unified without the
//    owner.
//  - Folders in this list carry a GENERIC folder glyph. The sidebar never
//    receives a folder's icon preset / colour, and the manager's own move menu
//    shows no icon at all.
//
// THE CLOSE-POLICY INVERSION, recorded so nobody re-derives it: Slint's
// PopupWindow closes on ANY click, which is why its items barely need
// close(); this one had to opt OUT (close-on-click-outside) so clicking the
// folder-search input would not dismiss it. Qt is the mirror image —
// CloseOnPressOutside never closes on an inside click — so the search input
// works for free, and EVERY row that acts must call close() itself.
//
// Reuse: this inherits controls/QbzContextMenu.qml, which already delivers the
// exact ContextMenu.slint chrome (surface-main, Radius.sm, 1px border-muted,
// 5px vertical padding), the window-overlay parenting a clipping sidebar
// needs, and the 8px viewport clamp. Rejected controls/CardMenu.qml (a fixed
// play/queue/pin/fav set bound to a card document — nothing in common beyond
// "it is a popup") and writing a second popup surface. The rows are a local
// inline component rather than a control, because ContextMenuItem's shape is
// 33/11/16/10/15/13 full-bleed and no shipped control is that.
//
// The one chrome override: QbzContextMenu pads all four sides by 5, while
// ContextMenu.slint pads only top and bottom — its rows are full-bleed. Hence
// leftPadding/rightPadding 0 below.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

QbzContextMenu {
    id: root

    // Sidebar.slint:402 — `width: 210px`, not QbzContextMenu's 196 default.
    menuWidth: 210
    leftPadding: 0
    rightPadding: 0
    topPadding: 5
    bottomPadding: 5

    /// The clicked SidebarEntry: {kind, id, name, count, indent, folderId,
    /// covers, isLocal?}. Null before the first open.
    property var entry: null
    /// foldersJson MINUS the hidden folders, derived ONCE by the sidebar and
    /// passed in. Every folder-dependent decision in this file reads THIS, not
    /// the raw folder list: the sidebar tree drops hidden folders everywhere
    /// else, so offering one here — or counting one toward the >= 8 search
    /// threshold, or letting one suppress "No folders yet." — would show a
    /// search box over a three-row list and move a playlist somewhere it
    /// cannot be seen.
    property var visibleFolders: []
    /// Local substring filter over `visibleFolders`. The reference runs this
    /// in Rust (`search_menu_folders`) for ONE reason it states in its own
    /// source: Slint strings have no `contains`. JavaScript has `indexOf`, so
    /// the round trip per keystroke is dropped. Deliberate; do not re-add it
    /// "for parity".
    property string folderQuery: ""

    readonly property bool isFolder: root.entry !== null && root.entry.kind === "folder"
    readonly property bool isLocal: root.entry !== null && root.entry.isLocal === true
    /// The signed-in user owns the Qobuz playlist (sidebar_qt stamps it);
    /// a local playlist is always the user's own.
    readonly property bool isOwner: root.entry !== null
        && (root.entry.isLocal === true || root.entry.isOwner === true)
    readonly property string entryId: root.entry !== null ? String(root.entry.id) : ""
    readonly property string entryFolderId:
        (root.entry !== null && root.entry.folderId) ? String(root.entry.folderId) : ""

    readonly property var menuFolders: {
        var q = root.folderQuery.trim().toLowerCase()
        if (q === "")
            return root.visibleFolders
        return root.visibleFolders.filter(function (f) {
            return String(f.name).toLowerCase().indexOf(q) !== -1
        })
    }
    // Height source for the list. The CURRENT folder's row is excluded in
    // markup (where Slint has it), so sizing off `menuFolders.length` would
    // leave one row of dead space — the reference's own bug, not ported.
    readonly property int folderRowCount: root.menuFolders.filter(function (f) {
        return String(f.id) !== root.entryFolderId
    }).length

    /// Open on a PLAYLIST row. `anchorItem` is the row; `localX`/`localY` are
    /// row-local coordinates, so the sidebar reproduces Slint's row-anchored
    /// `x: root.width - 210px; y: 28px` rather than opening at the pointer
    /// (this menu is row-anchored in the reference, unlike the TrackRow menus).
    function openForPlaylist(e, anchorItem, localX, localY) {
        root._openWith(e, anchorItem, localX, localY)
    }

    /// Open on a FOLDER row. Same placement; the content arms switch on
    /// `entry.kind`.
    function openForFolder(e, anchorItem, localX, localY) {
        root._openWith(e, anchorItem, localX, localY)
    }

    function _openWith(e, anchorItem, localX, localY) {
        root.entry = e
        // Slint clears the folder query on EVERY right-press, folder rows
        // included, before showing the menu.
        root.folderQuery = ""
        folderSearch.clearSearch()
        root.openAtCursor(anchorItem, localX, localY)
    }

    // Declared BEFORE the inline component that reads it, matching
    // NavFlyout.qml's shipped shape. A QML file has no lexical scope across
    // files, so QbzContextMenu's own `theme` is not reachable from here.
    QbzTheme { id: theme }

    /// One 33px row — ContextMenuItem.slint, full-bleed, no radius.
    component MenuRow: Rectangle {
        id: mr
        property string icon: ""
        property string label: ""
        signal activated()

        width: parent ? parent.width : 0
        height: 33
        color: mrArea.containsMouse ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: 11
            anchors.rightMargin: 16
            spacing: 10
            QbzIcon {
                name: mr.icon
                width: 15
                height: 15
                anchors.verticalCenter: parent.verticalCenter
                tintName: mrArea.containsMouse ? "textPrimary" : "secondary"
            }
            Text {
                width: parent.width - 25
                height: parent.height
                text: mr.label
                color: mrArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: mrArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: mr.activated()
        }
    }

    // ---- Folder arm ------------------------------------------------------
    MenuRow {
        visible: root.isFolder
        icon: "pen-line"
        label: QbzSession.tr("Edit folder", QbzSession.trRev)
        onActivated: {
            root.close()
            QbzFolderEdit.openEditor(root.entryId)
        }
    }
    MenuRow {
        visible: root.isFolder
        icon: "eye-off"
        label: QbzSession.tr("Hide from sidebar", QbzSession.trRev)
        onActivated: {
            root.close()
            // The folder editor owns folder visibility, and its write
            // republishes foldersJson — otherwise a folder hidden from here
            // would stay in every other playlist's move-to-folder list.
            QbzFolderEdit.setHidden(root.entryId, true)
        }
    }

    // ---- Playlist arm ----------------------------------------------------
    MenuRow {
        visible: !root.isFolder
        icon: "pen-line"
        label: QbzSession.tr("Edit playlist", QbzSession.trRev)
        onActivated: {
            root.close()
            // A `local:` id survives untouched — the editor seeds itself from
            // the local DB for those and works offline.
            QbzPlaylistEdit.open(root.entryId)
        }
    }
    MenuRow {
        // Locals are hidden from the Playlist Manager, never from here: the
        // reference's own hide path parses a u64 and bails on a local id.
        visible: !root.isFolder && !root.isLocal
        icon: "eye-off"
        label: QbzSession.tr("Hide from sidebar", QbzSession.trRev)
        onActivated: {
            root.close()
            // One-way door FROM the sidebar; the manager's All/Visible/Hidden
            // filter is the only undo surface in the app, which is why this
            // row could not ship before that filter existed.
            QbzPlaylistManager.toggleHidden(root.entryId)
        }
    }
    MenuRow {
        visible: !root.isFolder && !root.isLocal
        icon: "cassette-tape"
        label: QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev)
        onActivated: {
            root.close()
            // Built HERE from the SidebarEntry the row already carries — the
            // mixtape payload is assembled at the call site everywhere in this
            // port, and the sidebar must not grow a Rust helper for another
            // domain. `source` is always "qobuz" because the row is absent on
            // a local entry.
            //
            // `trackCount` is deliberately OMITTED, which is the one place
            // this file departs from the contract's literal snippet. A
            // SidebarEntry's `count` is populated for FOLDER rows only —
            // `entry_for` / `local_entry_for` both hardcode `count: 0`
            // (sidebar_qt.rs:379, :390), and the field is the folder's member
            // count, not a track count. `myqbz_add_qt` PERSISTS this value
            // into the collection row (`add_item_with(..., it.track_count,
            // ...)`), so sending 0 would write a permanent wrong number;
            // absent means NULL, which is the truthful "unknown". It is
            // Option<i32> on the Rust side and every other optional key here
            // is already omitted the same way.
            var e = root.entry
            QbzMyQbzAdd.open(JSON.stringify([{
                "itemType": "playlist",
                "source": "qobuz",
                "sourceItemId": String(e.id),
                "title": String(e.name),
                "artworkUrl": (e.covers && e.covers.length > 0) ? String(e.covers[0]) : ""
            }]))
        }
    }
    // Divider before the move-to-folder section — Qobuz only.
    // Delete (2026-09-13): the fast path the editor's danger button already
    // offers, one click from the row, behind the shell-level confirmation
    // (PlaylistDeleteConfirm.qml). Owned Qobuz playlists and local ones only;
    // a followed playlist keeps its Unfollow elsewhere.
    MenuRow {
        visible: !root.isFolder && root.isOwner
        icon: "trash-2"
        label: QbzSession.tr("Delete playlist", QbzSession.trRev)
        onActivated: {
            root.close()
            QbzPlaylistEdit.askDelete(root.entryId, String(root.entry.name))
        }
    }
    Item {
        visible: !root.isFolder && !root.isLocal
        width: parent ? parent.width : 0
        height: 7
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            height: 1
            color: theme.borderSubtle
        }
    }

    // ---- Move to folder --------------------------------------------------
    // Caption block: padding L 11 / R 16 / bottom 3, 18px text row.
    Item {
        visible: !root.isFolder
        width: parent ? parent.width : 0
        height: 21
        Text {
            x: 11
            width: parent.width - 27
            height: 18
            text: QbzSession.tr("Move to folder", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: 11
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }
    // Search block: padding L 11 / R 16 / bottom 4, 26px field. Appears only
    // once the list is long enough to warrant it (the reference's threshold,
    // counted over the VISIBLE folders).
    Item {
        visible: !root.isFolder && root.visibleFolders.length >= 8
        width: parent ? parent.width : 0
        height: 30
        QbzLineEdit {
            id: folderSearch
            x: 11
            width: parent.width - 27
            height: 26
            searchMode: true
            placeholder: QbzSession.tr("Search folders", QbzSession.trRev)
            onEdited: function (v) { root.folderQuery = v }
        }
    }
    // The folder list. A ListView at a fixed clip height, not a Repeater in a
    // Flickable: only ~5 rows are VISIBLE, but the model is every folder the
    // user has.
    ListView {
        visible: !root.isFolder
        width: parent ? parent.width : 0
        height: Math.min(root.folderRowCount * 33, 165)
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        model: root.menuFolders
        delegate: MenuRow {
            required property var modelData
            // A ListView does NOT size its delegates: `parent.width` here is
            // the contentItem's, which is not the view's.
            width: ListView.view ? ListView.view.width : 0
            // The playlist's CURRENT folder is excluded in markup, exactly
            // where Slint has it.
            visible: String(modelData.id) !== root.entryFolderId
            height: visible ? 33 : 0
            icon: "folder"
            label: String(modelData.name)
            onActivated: {
                root.close()
                QbzPlaylistManager.moveToFolder(root.entryId, String(modelData.id))
            }
        }
    }
    // Move to root — only when the playlist is currently filed. It comes
    // AFTER the scrolling list, and "No folders yet." after that; both sit
    // outside the scroll area. Reference ordering, kept.
    MenuRow {
        visible: !root.isFolder && root.entryFolderId !== ""
        icon: "chevron-left"
        label: QbzSession.tr("Move to root", QbzSession.trRev)
        onActivated: {
            root.close()
            // "" is ROOT.
            QbzPlaylistManager.moveToFolder(root.entryId, "")
        }
    }
    // Empty state: padding L 11 / R 16 / bottom 4, 22px text row.
    Item {
        visible: !root.isFolder && root.visibleFolders.length === 0
        width: parent ? parent.width : 0
        height: 26
        Text {
            x: 11
            width: parent.width - 27
            height: 22
            text: QbzSession.tr("No folders yet.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: 12
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }
}
