// PmFolderMenu — the manager's move-to-folder dropdown
// (PlaylistManagerView.slint:803-866). ONE instance, mounted at the VIEW level
// (contract D17): with `reuseItems: true` a per-row menu would give every
// recycled list delegate its own Popup, so the row emits
// `folderMenuRequested(anchorItem)` and the view calls `openFor()`.
//
// It is the ONLY menu on a playlist row in this surface: no Play, no Play next,
// no Add to queue, no Delete, no Copy link, no Follow.
//
// Reuse: `controls/QbzContextMenu.qml`, exactly the way controls/CardMenu.qml
// does — the popup surface, the `Overlay.overlay` parenting and the 8 px window
// clamp all come for free. Accepted, recorded surface deltas (contract D15):
// `surfaceMain` vs the reference's `surface-card`, `borderMuted` vs
// `border-subtle`, padding 5 vs 6, and NO drop shadow (port-wide). Escape
// closes, which the Slint `PopupWindow` cannot.
//
// The name filter runs CLIENT-SIDE (contract D4). The reference's Rust round
// trip exists for one stated reason — Slint strings have no `contains`
// (playlist_manager.rs:652-654) — and it costs one invokable, one published
// model and a round trip per keystroke. Do NOT "restore consistency" with
// PlaylistPickerModal, which made the opposite call for its own reasons.
//
// The playlist's CURRENT folder is excluded in the MARKUP (`shown:`), where the
// reference has it (`:857`). Its height math counts that excluded row and
// leaves one row of dead space (`:850` vs `:857`); this port counts the rows it
// will actually show instead — recorded as an improvement.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

QbzContextMenu {
    id: menuRoot

    /// `QbzPlaylistManager.foldersJson`, UNFILTERED — hidden folders are legal
    /// move targets here (contract D11; the reference's `menu_folders` is
    /// unfiltered too).
    property var folders: []

    /// Set by `openFor()`; never read from a delegate.
    property string playlistId: ""
    property string currentFolderId: ""
    property string folderQuery: ""

    QbzTheme { id: theme }

    menuWidth: 220
    // The reference's 1 px row gap. `QbzContextMenu`'s Column defaults to 0 and
    // its `col` id is not reachable from a call site — this alias is what makes
    // the gap settable at all (contract D15).
    contentSpacing: 1

    readonly property int rowW: menuRoot.menuWidth - 10   // minus padding 5+5

    readonly property var filteredFolders: {
        const q = menuRoot.folderQuery.trim().toLowerCase()
        const src = menuRoot.folders || []
        if (q === "")
            return src
        var out = []
        for (var i = 0; i < src.length; i++) {
            if (String(src[i].name || "").toLowerCase().indexOf(q) >= 0)
                out.push(src[i])
        }
        return out
    }

    /// Rows that will actually render — the current folder collapses to 0.
    readonly property int shownCount: {
        const src = menuRoot.filteredFolders
        var n = 0
        for (var i = 0; i < src.length; i++) {
            if (String(src[i].id || "") !== menuRoot.currentFolderId)
                n++
        }
        return n
    }

    /// Opened by the VIEW, never by a delegate. The folder query resets on
    /// EVERY open (§5.16) even though the filter is client-side now.
    function openFor(pid, fid, anchorItem) {
        menuRoot.playlistId = String(pid || "")
        menuRoot.currentFolderId = String(fid || "")
        menuRoot.folderQuery = ""
        // The box too: QbzLineEdit re-seeds only while unfocused.
        folderSearchField.reset("")
        menuRoot.openBelowRight(anchorItem)
    }

    // Caption — not a row: no hover, no click (`:817-822`).
    Text {
        width: menuRoot.rowW
        height: 18
        text: QbzSession.tr("Move to folder", QbzSession.trRev)
        color: theme.textMuted
        font.pixelSize: 11
        verticalAlignment: Text.AlignVCenter
    }

    // Folder search — the sidebar's >= 8 threshold (`:826`, verified).
    Item {
        visible: (menuRoot.folders || []).length >= 8
        width: menuRoot.rowW
        height: visible ? (menuRoot.kioskHost ? 44 : 32) : 0

        QbzLineEdit {
            id: folderSearchField
            kioskHost: menuRoot.kioskHost
            anchors.verticalCenter: parent.verticalCenter
            // The plain arm is a FIXED width: 240 — a modal/menu must set it.
            width: menuRoot.rowW
            // And its height defaults to `collapsedSize` (34), which would
            // OVERFLOW this 32 px slot by 1 px top and bottom and touch the
            // caption above and the `No folder` row below (the panel's row gap
            // is 1). The reference field is 28 inside a 32 px Rectangle
            // (`:828-830`); the sidebar row menu sets its own height the same
            // way (shell/SidebarRowMenu.qml:305).
            height: menuRoot.kioskHost ? 44 : 28
            searchMode: true
            elevated: false
            placeholder: QbzSession.tr("Search folders", QbzSession.trRev)
            text: menuRoot.folderQuery
            onEdited: function (v) { menuRoot.folderQuery = v }
        }
    }

    // Root target. The ONLY row that can ever be `selected` in this menu — the
    // current folder's row is excluded, which makes the folder arm's selected
    // state dead by construction. Do not "fix" it.
    PmMenuRow {
            kioskHost: menuRoot.kioskHost
        width: menuRoot.rowW
        label: QbzSession.tr("No folder", QbzSession.trRev)
        selected: menuRoot.currentFolderId === ""
        onActivated: {
            QbzPlaylistManager.moveToFolder(menuRoot.playlistId, "")
            menuRoot.close()
        }
    }

    // At most 4 rows visible (30 px each), then it scrolls. A ListView, not a
    // Repeater in a Flickable: 4 rows are VISIBLE but the model is every folder
    // the user has (contract D16).
    ListView {
        width: menuRoot.rowW
        height: Math.min(menuRoot.shownCount * 30, 120)
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        // ZERO, not the panel's 1. The reference's 1 px gap is the OUTER
        // VerticalLayout's (`:815`, = `contentSpacing` here); the folder rows
        // live in the ScrollView's own inner VerticalLayout, which declares no
        // spacing (`:854`). With 1 here the `min(n * 30, 120)` cap — the
        // reference's own formula, kept — stops being a whole number of rows
        // and the fourth one is sliced 3 px short.
        spacing: 0
        model: menuRoot.filteredFolders

        delegate: PmMenuRow {
            kioskHost: menuRoot.kioskHost
            required property var modelData
            width: menuRoot.rowW
            // The current-folder exclusion, in the markup (D4).
            shown: String(modelData.id || "") !== menuRoot.currentFolderId
            label: modelData.name || ""
            selected: menuRoot.currentFolderId === String(modelData.id || "")
            onActivated: {
                QbzPlaylistManager.moveToFolder(menuRoot.playlistId,
                                               String(modelData.id || ""))
                menuRoot.close()
            }
        }
    }
}
