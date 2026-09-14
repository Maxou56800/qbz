// PlaylistEditModal — THE playlist editor (primitives/EditPlaylistModal.slint):
// rename, description, the offline-only flag and delete, for BOTH kinds of
// playlist (`local:<uuid>` and a Qobuz u64) and for every surface that offers
// "Edit playlist":
//
//   * the manager's PmGridCard / PmListRow / PmTreePlaylistRow pencils,
//   * qml/shell/SidebarRowMenu.qml's "Edit playlist" row,
//   * qml/views/PlaylistView.qml's header pencil (contract block 7) — whose
//     inline rename+delete Popup this file REPLACES. That popup could express
//     neither a description nor the offline-only flag, and it carried a delete
//     arm, so the replacement has to cover both verbs or deleting it removes
//     working functionality (D20).
//
// Mounted ONCE in qml/shell/AppShell.qml as a direct child of the root, after
// FolderModals and before QbzToast, with no `z` — that file's convention is
// DECLARATION ORDER, and ADR-009's ">= 3000" is satisfied structurally.
//
// ── NO INPUTS ──────────────────────────────────────────────────────────────
// It reads QbzPlaylistEdit.editJson (§4.6) and owns its own drafts. QML has no
// lexical scope across files, so everything it reads is declared on its own
// root or comes from a singleton.
//
// ── THE DRAFTS LIVE HERE, THE ID DOES NOT ──────────────────────────────────
// Name, description and the offline-only checkbox are QML-LOCAL and reach Rust
// only as arguments to `save(name, description, offlineOnly)` (MyQbzModals.qml's
// standing rule). The id being edited is RUST state and is never echoed back,
// so a republish can never make this modal save under the wrong playlist.
//
// ── `descLoaded` IS NOT COSMETIC ───────────────────────────────────────────
// When Rust could not resolve the real description, the field is NOT RENDERED
// and `save` sends `None`, i.e. "leave the stored description alone". A field
// the user never saw can never overwrite stored data. The reference seeds ""
// and always writes it back, which deletes the description on every rename
// (§5.2). Do not "simplify" this into an always-visible empty field.
//
// ── `busy` HOLDS THE MODAL OPEN ────────────────────────────────────────────
// The modal never closes itself. Rust closes it, and only after the write
// landed (D22) — a failed rename keeps what the user typed on screen and
// toasts. `close()` is REFUSED while busy, so the close X and the scrim go
// inert for that window rather than rendering and no-opping.
//
// ── NO DELETE CONFIRM, ON PURPOSE ──────────────────────────────────────────
// The reference's danger button calls `EditPlaylistActions.delete()` straight
// (EditPlaylistModal.slint:150-155), unlike the FOLDER editor, whose reference
// raises a native rfd message box that this port replaced with
// QbzConfirmModal. Adding one here would be a redesign, not a port.
//
// ── ESCAPE IS A WRAPPING FocusScope, NOT A SIBLING ONE ─────────────────────
// The name field takes active focus and QML propagates an unhandled key up the
// focused item's PARENT chain, so a sibling scope would be dead the moment the
// field is focused (the FolderModals.qml note). QbzLineEdit's TextInput leaves
// Escape unaccepted on the plain arm, so it reaches us.
//
// ── ENTER ──────────────────────────────────────────────────────────────────
// Through QbzLineEdit's `accepted`, NEVER `committed`: committed also fires on
// focus-out, and closing the modal blurs the field, which would rename twice
// (MyQbzModals.qml:34-36).
//
// ── REUSED, NOT REDRAWN ────────────────────────────────────────────────────
//   QbzLineEdit      the name field. Its width MUST be set — the plain arm is
//                    a fixed `width: 240` (QbzLineEdit.qml:62) where the Slint
//                    LineEdit stretches to the panel.
//   QbzTextArea      the description body, the port's multiline input (its one
//                    prior call site is MyQBZ's edit modal, the same job).
//   QbzCheckbox      the offline-only row. Already 18x18 r4, accent when
//                    checked, 2px muted ring when not, and it never self-flips
//                    — exactly what a QML-local draft needs (§5.21).
//   QbzPrimaryButton Save. The reference's `#ffffffd6` hover on an accent fill
//                    and its hardcoded `#ffffff` label are NOT copied; both
//                    wash out on the light palettes.
//   Delete button    hand-drawn, byte-for-byte the ghost danger button
//                    FolderEditPanel.qml uses, for the same reason: the port's
//                    filled danger control is a different affordance.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // Guarded parse: a raw JSON.parse in a binding throws on the pre-publish
    // frame (PlaylistView.qml:55-61).
    readonly property var doc: {
        try { return JSON.parse(QbzPlaylistEdit.editJson) } catch (e) { return ({}) }
    }

    readonly property bool editOpen: root.doc.open === true
    readonly property bool busy: root.doc.busy === true
    readonly property bool isLocal: root.doc.isLocal === true
    readonly property bool descLoaded: root.doc.descLoaded === true

    // --- Drafts -----------------------------------------------------------
    property string draftName: ""
    property string draftDescription: ""
    property bool draftOfflineOnly: false
    /// The folder the playlist will be filed in on Save ("" = root). Seeded
    /// from `doc.folderId`; the move rides QbzPlaylistManager.moveToFolder,
    /// the same verb the sidebar row menu uses (2026-09-13).
    property string draftFolderId: ""
    readonly property var folders: {
        try {
            var f = JSON.parse(QbzPlaylistManager.foldersJson || "[]")
            return Array.isArray(f) ? f : []
        } catch (e) {
            return []
        }
    }
    readonly property var folderOptions: [root.t("No folder")].concat(
        root.folders.map(function (f) { return String(f.name) }))
    readonly property int folderIndex: {
        for (var i = 0; i < root.folders.length; i++)
            if (String(root.folders[i].id) === root.draftFolderId) return i + 1
        return 0
    }

    readonly property bool canSave: root.draftName.trim() !== "" && !root.busy

    visible: root.editOpen
    enabled: root.visible

    // Seeds are consumed ONCE per open, and again when the editor is pointed
    // at a different playlist without closing. A plain republish (the `busy`
    // flip on save) must NOT reseed — it would throw away what the user typed.
    readonly property string seedKey:
        (root.editOpen ? "1" : "0") + "|" + (root.doc.id || "")

    onSeedKeyChanged: {
        if (root.editOpen) {
            root.draftName = root.doc.name || ""
            root.draftDescription = root.doc.description || ""
            root.draftOfflineOnly = root.doc.offlineOnly === true
            root.draftFolderId = root.doc.folderId || ""
            scope.forceActiveFocus()
            nameField.focusField()
        }
    }

    // §1.4.3 (2026-08-03 hotkeys-port contract): the FocusScope grabs focus
    // on open and Qt strands it on the now-invisible scope at close, which
    // kills the AppShell key dispatcher until the next click. Restore the
    // shell root — the dispatcher's fallback focus item — on the close edge.
    onEditOpenChanged: if (!root.editOpen) root._restoreShellFocus()

    function _restoreShellFocus() {
        var p = root
        while (p.parent) {
            if (p.parent.isQbzShellRoot === true) {
                p.parent.forceActiveFocus()
                return
            }
            p = p.parent
        }
    }

    function submit() {
        if (!root.canSave)
            return
        // The folder is a separate write (the sidebar's own move verb, with
        // its optimistic patch); only when it changed.
        if (root.draftFolderId !== (root.doc.folderId || ""))
            QbzPlaylistManager.moveToFolder(String(root.doc.id), root.draftFolderId)
        QbzPlaylistEdit.save(root.draftName, root.draftDescription,
                             root.draftOfflineOnly)
    }

    FocusScope {
        id: scope
        anchors.fill: parent
        visible: root.editOpen
        enabled: root.editOpen

        // Taken HERE rather than only in the open handler: an invisible item
        // cannot hold active focus, and a handler on the document property is
        // not ordered against the `visible` binding it shares a frame with.
        onVisibleChanged: if (scope.visible) scope.forceActiveFocus()

        Keys.onEscapePressed: function (event) {
            QbzPlaylistEdit.close()
            event.accepted = true
        }

        // `radius` is load-bearing: this Item fills AppShell's ROUNDED content
        // frame and Qt Quick's clip is a rectangular scissor, so an opaque
        // full-bleed child paints into the frame's four bezel corners
        // (AppShell.qml:246-310). #bf000000 is Slint's #000000bf CONVERTED —
        // Slint is #RRGGBBAA, Qt is #AARRGGBB.
        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzPlaylistEdit.close()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        Rectangle {
            width: Math.min(root.width - 80, 420)
            height: col.implicitHeight + 40
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            // FIRST child: swallows clicks so they never reach the scrim.
            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: col
                x: 20
                y: 20
                width: parent.width - 40
                spacing: 16

                // ================= Title + close X ======================
                Item {
                    width: parent.width
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, parent.width - 28)
                        text: root.t("Edit playlist")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Item {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        opacity: root.busy ? 0.5 : 1.0
                        QbzIcon {
                            anchors.centerIn: parent
                            width: 17
                            height: 17
                            name: "x"
                            tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            // `close()` is refused while a write is in flight,
                            // so the affordance goes with it rather than lying.
                            enabled: !root.busy
                            hoverEnabled: !root.busy
                            cursorShape: root.busy ? Qt.ArrowCursor
                                                   : Qt.PointingHandCursor
                            onClicked: QbzPlaylistEdit.close()
                        }
                    }
                }

                // ================= Name =================================
                // No label above it — the reference has none here, only the
                // placeholder (EditPlaylistModal.slint:66).
                QbzLineEdit {
                    id: nameField
                    width: parent.width
                    text: root.draftName
                    placeholder: root.t("Playlist name")
                    enabled: !root.busy
                    onEdited: function (value) { root.draftName = value }
                    onAccepted: function (value) {
                        root.draftName = value
                        root.submit()
                    }
                }

                // ================= Cover ================================
                // The shared editor is reached from the manager, sidebar and
                // detail header, so cover editing belongs here too. Actions
                // stay on the trailing modal grid; the native picker owns its
                // own preview and the playlist surfaces repaint after write.
                Item {
                    width: parent.width
                    height: 34
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Cover")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                    Row {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 8
                        SettingsButton {
                            minWidth: 0
                            text: root.t("Change cover")
                            iconName: "image-plus"
                            enabled: !root.busy
                            onClicked: QbzPlaylistEdit.chooseCover()
                        }
                        SettingsButton {
                            minWidth: 0
                            text: root.t("Remove cover")
                            iconName: "trash-2"
                            danger: true
                            enabled: !root.busy
                            onClicked: QbzPlaylistEdit.removeCover()
                        }
                    }
                }

                // ================= Description ==========================
                // Rendered ONLY when Rust resolved the real one. A Column
                // skips invisible children, so this leaves no gap.
                Column {
                    width: parent.width
                    spacing: 6
                    visible: root.descLoaded
                    Text {
                        text: root.t("Description")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                    QbzTextArea {
                        width: parent.width
                        height: 90
                        text: root.draftDescription
                        enabled: !root.busy
                        onEdited: function (value) { root.draftDescription = value }
                    }
                }

                // ================= Offline-only =========================
                // LOCAL playlists only — the flag is a `local_playlists`
                // column with no Qobuz analogue. Unmark it to make "Upload to
                // Qobuz" available (D8).
                // An Item with a FIXED height, not a Row: a Row's height comes
                // from its tallest child, so anchoring a child to
                // `parent.verticalCenter` inside one is a binding loop. Same
                // shape FolderEditPanel.qml's hidden row uses.
                // Folder (2026-09-13): the same folders the sidebar and the
                // manager file playlists into; "No folder" = the root.
                Column {
                    width: parent.width
                    spacing: 6
                    Text {
                        text: root.t("Folder")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                    QbzSelect {
                        menuWidth: 260
                        // A folder tree grows past what a plain list can
                        // scan: the filter box, like the ui-control standard's
                        // searchable dropdown.
                        searchable: true
                        options: root.folderOptions
                        currentIndex: root.folderIndex
                        enabled: !root.busy
                        onSelected: function (i) {
                            root.draftFolderId = i <= 0 ? "" : String(root.folders[i - 1].id)
                        }
                    }
                }

                Item {
                    width: parent.width
                    height: 20
                    visible: root.isLocal
                    // 20x20 wrapper: the larger hit target the reference's
                    // TouchArea gives the 18px box (§5.21).
                    Item {
                        id: offlineHit
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: 20
                        height: 20
                        QbzCheckbox {
                            anchors.centerIn: parent
                            checked: root.draftOfflineOnly
                            enabled: !root.busy
                            // QbzCheckbox never self-flips; the owner of the
                            // state does.
                            onToggled: root.draftOfflineOnly = !root.draftOfflineOnly
                        }
                    }
                    Text {
                        anchors.left: offlineHit.right
                        anchors.leftMargin: 10
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Offline-only playlist")
                        color: theme.textSecondary
                        font.pixelSize: 15
                        elide: Text.ElideRight
                    }
                }

                // ================= Footer ===============================
                Item {
                    width: parent.width
                    height: 36

                    // Delete — a ghost danger button: fill on hover only, 1px
                    // danger border, danger label. theme.danger /
                    // theme.dangerHover ARE the reference's #ef4444 /
                    // #ef444433 under the default theme (the engine defaults
                    // danger to rgb(ef,44,44) and derives danger_hover as a
                    // 0.2 tint, i.e. alpha 0x33) and the correct colours under
                    // the other 35.
                    Rectangle {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: delLabel.implicitWidth + 28
                        height: 36
                        radius: 8
                        color: (delArea.containsMouse && !root.busy) ? theme.dangerHover
                                                                     : "transparent"
                        border.width: 1
                        border.color: theme.danger
                        opacity: root.busy ? 0.5 : 1.0
                        Text {
                            id: delLabel
                            anchors.centerIn: parent
                            text: root.t("Delete")
                            color: theme.danger
                            font.pixelSize: 15
                            font.weight: theme.weightMedium
                        }
                        MouseArea {
                            id: delArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: root.busy ? Qt.ArrowCursor
                                                   : Qt.PointingHandCursor
                            onClicked: if (!root.busy) QbzPlaylistEdit.deletePlaylist()
                        }
                    }

                    QbzPrimaryButton {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        label: root.t("Save")
                        btnHeight: 36
                        labelSize: 15
                        btnEnabled: root.canSave
                        onClicked: root.submit()
                    }
                }
            }
        }
    }
}
