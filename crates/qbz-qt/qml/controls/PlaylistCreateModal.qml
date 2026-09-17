// PlaylistCreateModal — "New playlist" (primitives/CreatePlaylistModal.slint).
//
// WHAT THIS REPLACES. The sidebar "+" used to call `QbzShell.createPlaylist()`,
// which created a playlist literally named "New Playlist" and navigated to it.
// Four of this panel's five fields had no other door — and one of them, the
// offline-only toggle, had no door AT ALL for an online user, so someone with a
// Qobuz session could not create a LOCAL playlist without pulling the network
// cable. `playlist_create_qt`'s header has the table.
//
// Mounted ONCE in qml/shell/AppShell.qml beside PlaylistEditModal, with no `z`
// — that file's convention is DECLARATION ORDER, and ADR-009's ">= 3000" is
// satisfied structurally.
//
// ── SHAPE, from the reference ─────────────────────────────────────────────
//   Name (LineEdit, Enter submits) · Description (multiline, optional) ·
//   Folder (only when the user HAS folders) · "Make playlist public" ·
//   "Offline-only playlist" (+ hint while locked) · Cancel / Create.
//
// ── THE DRAFTS LIVE HERE ──────────────────────────────────────────────────
// Name, description, folder index, public and offline-only are QML-LOCAL and
// reach Rust only as arguments to `createSubmit(...)` — MyQbzModals.qml's
// standing rule, the same one PlaylistEditModal.qml follows. Rust owns only
// `busy`, the offline lock and the folder list.
//
// ── THE FOLDER IS SENT AS AN ID, NOT AN INDEX ─────────────────────────────
// The reference keeps two parallel arrays (`folder-options` / `folder-ids`) and
// resolves the id from the index at submit time. Here the document is a list of
// `{id, name}` objects and the id travels: parallel arrays that must stay in
// step across a process boundary are a class of bug this port does not need,
// and index 0's EMPTY id is what "no folder" means — never the index itself.
//
// ── THE FOLDER PICKER'S VISIBILITY IS THE REFERENCE'S ─────────────────────
// `folder-options.length > 1` (CreatePlaylistModal.slint:118): the row is
// absent when the only option is "No folder". Plus one thing the reference does
// not need: it also hides on the OFFLINE-ONLY arm, because `folders_qt
// ::move_playlist` keys on a Qobuz `u64` and a `local:<uuid>` has none. Showing
// a picker whose choice is silently dropped is the dead-affordance class the
// port refuses; the reference never hit it because its own local arm ignores
// the field just as quietly.
//
// ── PUBLIC DIMS UNDER OFFLINE-ONLY ────────────────────────────────────────
// A local playlist has nothing to be public ON (reference :138-146). It dims to
// 0.4 and stops taking clicks — it is not merely ignored at submit.
//
// ── `busy` HOLDS THE MODAL OPEN ───────────────────────────────────────────
// Rust closes it, and only after the write landed. `closeCreate()` is REFUSED
// while busy, so the close X and the scrim go inert for that window rather than
// rendering and no-opping.
//
// ── ESCAPE IS A WRAPPING FocusScope, NOT A SIBLING ONE ────────────────────
// The name field takes active focus and QML propagates an unhandled key up the
// FOCUSED item's parent chain, so a sibling scope would be dead the moment the
// field is focused (the FolderModals.qml note).
//
// ── ENTER ─────────────────────────────────────────────────────────────────
// Through QbzLineEdit's `accepted`, NEVER `committed`: committed also fires on
// focus-out, and closing the modal blurs the field, which would create twice.

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
        try { return JSON.parse(QbzPlaylistEdit.createJson) } catch (e) { return ({}) }
    }

    readonly property bool createOpen: root.doc.open === true
    readonly property bool busy: root.doc.busy === true
    readonly property bool offlineLocked: root.doc.offlineLocked === true
    readonly property var folders: root.doc.folders || []

    // --- Drafts -----------------------------------------------------------
    property string draftName: ""
    property string draftDescription: ""
    property int draftFolderIndex: 0
    property bool draftIsPublic: false
    property bool draftOfflineOnly: false

    readonly property bool canCreate: root.draftName.trim() !== "" && !root.busy
    // The reference's `folder-options.length > 1`, plus the local-arm gate.
    readonly property bool folderPickerVisible:
        root.folders.length > 1 && !root.draftOfflineOnly

    visible: root.createOpen
    enabled: root.visible

    // Seeds are consumed ONCE per open. A plain republish (the `busy` flip on
    // submit) must NOT reseed — it would throw away what the user typed.
    onCreateOpenChanged: {
        if (root.createOpen) {
            root.draftName = ""
            root.draftDescription = ""
            root.draftFolderIndex = 0
            root.draftIsPublic = false
            // Offline forces it ON and locks it (reference :184-216).
            root.draftOfflineOnly = root.offlineLocked
            // The boxes too (QbzLineEdit.reset()): "" is also what the
            // drafts held after the last cancel, so nothing above re-seeds.
            nameField.reset("")
            descriptionField.reset("")
            scope.forceActiveFocus()
            nameField.focusField()
        } else {
            root._restoreShellFocus()
        }
    }

    // §1.4.3 (2026-08-03 hotkeys-port contract): the FocusScope grabs focus on
    // open and Qt strands it on the now-invisible scope at close, which kills
    // the AppShell key dispatcher until the next click. Restore the shell root.
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
        if (!root.canCreate)
            return
        // "" is ROOT. The id comes from the row, never from the index — and on
        // the local arm the picker is not even rendered, so it is "" there by
        // construction rather than by a second rule.
        var fid = ""
        if (root.folderPickerVisible
                && root.draftFolderIndex > 0
                && root.draftFolderIndex < root.folders.length)
            fid = String(root.folders[root.draftFolderIndex].id || "")
        QbzPlaylistEdit.createSubmit(root.draftName, root.draftDescription,
                                     fid, root.draftIsPublic,
                                     root.draftOfflineOnly)
    }

    FocusScope {
        id: scope
        anchors.fill: parent
        visible: root.createOpen
        enabled: root.createOpen

        // Taken HERE rather than only in the open handler: an invisible item
        // cannot hold active focus, and a handler on the document property is
        // not ordered against the `visible` binding it shares a frame with.
        onVisibleChanged: if (scope.visible) scope.forceActiveFocus()

        Keys.onEscapePressed: function (event) {
            QbzPlaylistEdit.closeCreate()
            event.accepted = true
        }

        // `radius` is load-bearing: this Item fills AppShell's ROUNDED content
        // frame and Qt Quick's clip is a rectangular scissor, so an opaque
        // full-bleed child paints into the frame's four bezel corners.
        // #bf000000 is Slint's #000000bf CONVERTED — Slint is #RRGGBBAA, Qt is
        // #AARRGGBB.
        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzPlaylistEdit.closeCreate()
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
                        text: root.t("New playlist")
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
                            // `closeCreate()` is refused while the write is in
                            // flight, so the affordance goes with it rather
                            // than lying.
                            enabled: !root.busy
                            hoverEnabled: !root.busy
                            cursorShape: root.busy ? Qt.ArrowCursor
                                                   : Qt.PointingHandCursor
                            onClicked: QbzPlaylistEdit.closeCreate()
                        }
                    }
                }

                // ================= Name =================================
                Column {
                    width: parent.width
                    spacing: 8
                    Text {
                        text: root.t("Name")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightMedium
                    }
                    QbzLineEdit {
                        id: nameField
                        width: parent.width
                        text: root.draftName
                        placeholder: root.t("My Playlist")
                        enabled: !root.busy
                        onEdited: function (value) { root.draftName = value }
                        onAccepted: function (value) {
                            root.draftName = value
                            root.submit()
                        }
                    }
                }

                // ================= Description ==========================
                Column {
                    width: parent.width
                    spacing: 8
                    Text {
                        text: root.t("Description (optional)")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightMedium
                    }
                    QbzTextArea {
                        id: descriptionField
                        width: parent.width
                        height: 80
                        text: root.draftDescription
                        enabled: !root.busy
                        onEdited: function (value) { root.draftDescription = value }
                    }
                }

                // ================= Folder ===============================
                // A Column skips invisible children, so this leaves no gap
                // when the user has no folders (or is on the local arm).
                Column {
                    width: parent.width
                    spacing: 8
                    visible: root.folderPickerVisible
                    Text {
                        text: root.t("Folder")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightMedium
                    }
                    QbzSelect {
                        width: parent.width
                        popupWidth: parent.width
                        enabled: !root.busy
                        // QbzSelect takes plain labels; the ids stay on
                        // root.folders and are resolved by INDEX at submit.
                        options: root.folders.map(function (f) { return String(f.name) })
                        currentIndex: root.draftFolderIndex
                        onSelected: function (i) { root.draftFolderIndex = i }
                    }
                }

                // ================= Public ===============================
                // Meaningless for a local playlist, so it dims AND locks while
                // offline-only is on (reference :138-146).
                //
                // An Item with a FIXED height, not a Row: a Row's height comes
                // from its tallest child, so anchoring a child to
                // `parent.verticalCenter` inside one is a binding loop.
                Item {
                    width: parent.width
                    height: 20
                    opacity: root.draftOfflineOnly ? 0.4 : 1.0
                    Item {
                        id: publicHit
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: 20
                        height: 20
                        QbzCheckbox {
                            anchors.centerIn: parent
                            checked: root.draftIsPublic
                            enabled: !root.busy && !root.draftOfflineOnly
                            // QbzCheckbox never self-flips; the owner of the
                            // state does.
                            onToggled: root.draftIsPublic = !root.draftIsPublic
                        }
                    }
                    Text {
                        anchors.left: publicHit.right
                        anchors.leftMargin: 10
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Make playlist public")
                        color: theme.textSecondary
                        font.pixelSize: 15
                        elide: Text.ElideRight
                    }
                }

                // ================= Offline-only =========================
                // D8: creates a LOCAL playlist (library.db, id
                // `local:<uuid>`) that never reaches Qobuz. Forced ON and
                // LOCKED while the app is offline, with the hint under it.
                Column {
                    width: parent.width
                    spacing: 6
                    Item {
                        width: parent.width
                        height: 20
                        opacity: root.offlineLocked ? 0.7 : 1.0
                        Item {
                            id: offlineHit
                            anchors.left: parent.left
                            anchors.verticalCenter: parent.verticalCenter
                            width: 20
                            height: 20
                            QbzCheckbox {
                                anchors.centerIn: parent
                                checked: root.draftOfflineOnly
                                enabled: !root.busy && !root.offlineLocked
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
                    // Hint indented to the label's left edge (20px checkbox +
                    // 10px spacing), so it reads as the row's description.
                    Text {
                        visible: root.offlineLocked
                        x: 30
                        width: parent.width - 30
                        text: root.t("Created locally — you're offline")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        elide: Text.ElideRight
                    }
                }

                // ================= Footer ===============================
                Item {
                    width: parent.width
                    height: 36

                    Row {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 10

                        // Cancel — the reference's neutral button: an
                        // elevated fill that lifts to surface-hover.
                        Rectangle {
                            width: cancelLabel.implicitWidth + 28
                            height: 36
                            radius: theme.radiusSm
                            color: (cancelArea.containsMouse && !root.busy)
                                ? theme.surfaceElevatedHover : theme.surfaceElevated
                            opacity: root.busy ? 0.5 : 1.0
                            Text {
                                id: cancelLabel
                                anchors.centerIn: parent
                                text: root.t("Cancel")
                                color: theme.textPrimary
                                font.pixelSize: 15
                            }
                            MouseArea {
                                id: cancelArea
                                anchors.fill: parent
                                hoverEnabled: true
                                enabled: !root.busy
                                cursorShape: root.busy ? Qt.ArrowCursor
                                                       : Qt.PointingHandCursor
                                onClicked: QbzPlaylistEdit.closeCreate()
                            }
                        }

                        // Create. The reference's `#ffffffd6` hover on an
                        // accent fill and its hardcoded `#ffffff` label are
                        // NOT copied — both wash out on the light palettes,
                        // which is the same call PlaylistEditModal made.
                        QbzPrimaryButton {
                            label: root.busy ? root.t("Creating...") : root.t("Create")
                            btnHeight: 36
                            labelSize: 15
                            btnEnabled: root.canCreate
                            onClicked: root.submit()
                        }
                    }
                }
            }
        }
    }
}
