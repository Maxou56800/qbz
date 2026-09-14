// FolderModals — the TWO folder panels and their delete confirm, in one file,
// as sibling self-gating overlays (the MyQbzModals.qml shape):
//
//   CREATE   primitives/CreateFolderModal.slint (109 lines) — a name field and
//            a Create button, opened from the sidebar's "..." -> New folder.
//            Panel width min(host - 80, 400).
//   EDITOR   primitives/FolderEditModal.slint (277) — the full editor, opened
//            from every folder card / chip / tree row (D7) and from the
//            sidebar row menu, and by the manager toolbar's own "New folder"
//            in CREATE mode (`id == ""`). Panel width min(host - 80, 480). Its
//            body is FolderEditPanel.qml; this file owns the chrome.
//   CONFIRM  QbzConfirmModal, declared LAST so it stacks over the editor, with
//            its own z: 3100.
//
// Mounted ONCE in qml/shell/AppShell.qml as a direct child of the root, after
// MyQbzModals and before QbzToast, with no `z` — that file's convention is
// declaration order, and ADR-009's ">= 3000" is satisfied structurally
// (AppShell.qml:533-563). 05 §5.4's "mount the create modal inside Sidebar.qml"
// is rejected: the sidebar animates to width 0 with clip: true and already
// force-closes its own flyout on a state change, so a modal parented into it
// disappears with the sidebar.
//
// ── TWO DRAFTS, NOT ONE ────────────────────────────────────────────────────
// `draftCreateName` belongs to the CREATE panel and is reset on its OWN open
// transition; the editor's four drafts live inside FolderEditPanel. Sharing
// one draft set between them (04 §5.6) makes "New folder" reopen holding
// whatever was last typed in the editor.
//
// ── `busy` HOLDS THE PANEL OPEN ────────────────────────────────────────────
// Neither panel closes itself. Rust closes them, and only after the write
// landed (contract D22) — a failed create keeps the name on screen and toasts,
// where the reference closes first and loses both the name and the error.
//
// ── ESCAPE IS A WRAPPING FocusScope, NOT A SIBLING ONE ─────────────────────
// QbzConfirmModal gets away with a sibling FocusScope because it has no text
// field. Here the name input takes active focus, and QML propagates an
// unhandled key up the focused item's PARENT chain — a sibling scope is never
// on that chain, so Escape would be dead the moment the field is focused.
// Each panel is therefore wrapped in its scope. QbzLineEdit's TextInput leaves
// Escape unaccepted on the plain arm (QbzLineEdit.qml:177-184), so it reaches
// us. Escape closes the innermost open thing: while the confirm is up it holds
// focus and handles its own.
//
// ── ENTER ──────────────────────────────────────────────────────────────────
// Through QbzLineEdit's `accepted`, NEVER `committed`: committed also fires on
// focus-out, and closing the modal blurs the field, which would submit the
// same value twice (MyQbzModals.qml:34-36).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // --- Documents. Guarded parses: a raw JSON.parse in a binding throws on
    // the pre-publish frame (PlaylistView.qml:55-61).
    readonly property var createDoc: {
        try { return JSON.parse(QbzFolderEdit.createJson) } catch (e) { return ({}) }
    }
    readonly property var editDoc: {
        try { return JSON.parse(QbzFolderEdit.editJson) } catch (e) { return ({}) }
    }

    readonly property bool createOpen: root.createDoc.open === true
    readonly property bool creating: root.createDoc.creating === true
    readonly property bool editOpen: root.editDoc.open === true
    /// Rust REFUSES `createClose()` / `editClose()` while a write is in flight
    /// (D22 — `busy` holds the panel open). The close X must therefore SHOW
    /// that it is inert for that window, or it is a control that renders and
    /// no-ops. Same treatment the panel's own Delete button already carries.
    readonly property bool editBusy: root.editDoc.busy === true

    visible: root.createOpen || root.editOpen
    enabled: root.visible

    // §1.4.3 (2026-08-03 hotkeys-port contract): both FocusScopes grab focus
    // on open and Qt strands it on the now-invisible scope at close, which
    // kills the AppShell key dispatcher until the next click. Restore the
    // shell root — the dispatcher's fallback focus item — on the close edge.
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

    // ==================================================================
    // PANEL 1 — Create folder (CreateFolderModal.slint)
    // ==================================================================
    property string draftCreateName: ""
    readonly property bool canCreate: root.draftCreateName.trim() !== "" && !root.creating

    onCreateOpenChanged: {
        if (root.createOpen) {
            // Its OWN draft, reset on its OWN open transition.
            root.draftCreateName = ""
            createField.reset("")
            createScope.forceActiveFocus()
            createField.focusField()
        } else {
            root._restoreShellFocus()
        }
    }

    FocusScope {
        id: createScope
        anchors.fill: parent
        visible: root.createOpen
        enabled: root.createOpen

        // Taken HERE rather than in the open handler: an invisible item cannot
        // hold active focus, and a handler on the document property is not
        // ordered against the `visible` binding it shares a frame with.
        onVisibleChanged: if (createScope.visible) createScope.forceActiveFocus()

        Keys.onEscapePressed: function (event) {
            QbzFolderEdit.createClose()
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
                onClicked: QbzFolderEdit.createClose()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        Rectangle {
            width: Math.min(root.width - 80, 400)
            height: createCol.implicitHeight + 40
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
                id: createCol
                x: 20
                y: 20
                width: parent.width - 40
                spacing: 16

                // --- Title + close X.
                Item {
                    width: parent.width
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, parent.width - 28)
                        text: root.t("New folder")
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
                        opacity: root.creating ? 0.5 : 1.0
                        QbzIcon {
                            anchors.centerIn: parent
                            width: 17
                            height: 17
                            name: "x"
                            tintName: createCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: createCloseArea
                            anchors.fill: parent
                            // `createClose()` is refused mid-create, so the
                            // affordance goes with it rather than lying.
                            enabled: !root.creating
                            hoverEnabled: !root.creating
                            cursorShape: root.creating ? Qt.ArrowCursor
                                                       : Qt.PointingHandCursor
                            onClicked: QbzFolderEdit.createClose()
                        }
                    }
                }

                // --- Name. No label above it — the reference has none here.
                QbzLineEdit {
                    id: createField
                    width: parent.width
                    text: root.draftCreateName
                    placeholder: root.t("Folder name")
                    onEdited: function (value) { root.draftCreateName = value }
                    onAccepted: function (value) {
                        root.draftCreateName = value
                        if (root.canCreate)
                            QbzFolderEdit.createSubmit(root.draftCreateName)
                    }
                }

                // --- Footer.
                Item {
                    width: parent.width
                    height: 36
                    QbzPrimaryButton {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        label: root.creating ? root.t("Creating...") : root.t("Create")
                        btnHeight: 36
                        labelSize: 15
                        btnEnabled: root.canCreate
                        onClicked: QbzFolderEdit.createSubmit(root.draftCreateName)
                    }
                }
            }
        }
    }

    // ==================================================================
    // PANEL 2 — The folder editor (FolderEditModal.slint)
    // ==================================================================
    // Seeds are consumed ONCE per open, and again when the editor is pointed
    // at a different folder without closing. A plain republish (a busy flip, a
    // picked image) must NOT reseed — it would throw away what the user typed.
    readonly property string editSeedKey:
        (root.editOpen ? "1" : "0") + "|" + (root.editDoc.id || "")

    onEditSeedKeyChanged: {
        if (root.editOpen) {
            editPanel.seed()
            editScope.forceActiveFocus()
            editPanel.focusName()
        }
    }
    onEditOpenChanged: {
        // A sub-modal must never outlive its host panel.
        if (!root.editOpen)
            deleteConfirm.close()
        if (!root.editOpen)
            root._restoreShellFocus()
    }

    FocusScope {
        id: editScope
        anchors.fill: parent
        visible: root.editOpen
        enabled: root.editOpen

        onVisibleChanged: if (editScope.visible) editScope.forceActiveFocus()

        Keys.onEscapePressed: function (event) {
            // The confirm holds focus while it is up and handles its own Esc,
            // so reaching here means the panel is the innermost open thing.
            QbzFolderEdit.editClose()
            event.accepted = true
        }

        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzFolderEdit.editClose()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        Rectangle {
            width: Math.min(root.width - 80, 480)
            height: editCol.implicitHeight + 40
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: editCol
                x: 20
                y: 20
                width: parent.width - 40
                spacing: 16

                // --- Title + close X.
                Item {
                    width: parent.width
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, parent.width - 28)
                        // "" is CREATE mode — the manager toolbar's New folder
                        // opens this same editor with no id.
                        text: (root.editDoc.id || "") === "" ? root.t("New folder")
                                                             : root.t("Edit folder")
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
                        opacity: root.editBusy ? 0.5 : 1.0
                        QbzIcon {
                            anchors.centerIn: parent
                            width: 17
                            height: 17
                            name: "x"
                            tintName: editCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: editCloseArea
                            anchors.fill: parent
                            // `editClose()` is refused while a save or delete
                            // is in flight — see `editBusy`.
                            enabled: !root.editBusy
                            hoverEnabled: !root.editBusy
                            cursorShape: root.editBusy ? Qt.ArrowCursor
                                                       : Qt.PointingHandCursor
                            onClicked: QbzFolderEdit.editClose()
                        }
                    }
                }

                FolderEditPanel {
                    id: editPanel
                    width: parent.width
                    doc: root.editDoc
                    onDeleteRequested: deleteConfirm.open()
                }
            }
        }
    }

    // ==================================================================
    // The delete confirm — LAST, so it stacks over the editor (D21).
    // ==================================================================
    // The reference raises a NATIVE rfd message box with untranslated &str
    // (main.rs:5724); this is the in-app surface the port already has, and its
    // two strings are new msgids. The body interpolates the LIVE draft name,
    // matching the reference's `get_name()` — rename the folder, then delete
    // it without saving, and the prompt still names what is on screen.
    QbzConfirmModal {
        id: deleteConfirm
        anchors.fill: parent
        danger: true
        title: root.t("Delete folder")
        body: root.t("Delete the folder “{}”? Its playlists move back to the root.")
                  .replace("{}", editPanel.draftName)
        confirmLabel: root.t("Delete")
        onConfirmed: {
            QbzFolderEdit.editDeleteConfirmed()
            // Hand focus back so Esc keeps working on the panel that is still
            // up while the delete is in flight.
            if (root.editOpen)
                editScope.forceActiveFocus()
        }
        onCancelled: {
            if (root.editOpen)
                editScope.forceActiveFocus()
        }
    }
}
