// The "Delete playlist?" confirmation summoned from context menus — the
// sidebar row menu and the playlist card (2026-09-13). Self-gates on
// QbzPlaylistEdit.deleteJson ({open, id, name, isLocal}) the way FolderModals
// gates on editJson, and lives at the AppShell level for the same reason: the
// sidebar clips to width 0 when collapsed, so a modal parented into it would
// vanish with it. Delete / Cancel go back through the bridge; Rust runs the
// same delete the editor's own button runs and refreshes the surfaces.
import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    readonly property var doc: {
        try { return JSON.parse(QbzPlaylistEdit.deleteJson) } catch (e) { return ({}) }
    }
    readonly property bool askOpen: root.doc.open === true

    onAskOpenChanged: {
        if (root.askOpen)
            confirm.open()
        else if (confirm.opened)
            confirm.close()
    }

    QbzConfirmModal {
        id: confirm
        anchors.fill: parent
        danger: true
        title: root.t("Delete playlist?")
        body: root.t("\u201c{}\u201d will be deleted. This cannot be undone.")
                  .replace("{}", root.doc.name || "")
        confirmLabel: root.t("Delete")
        onConfirmed: QbzPlaylistEdit.confirmDelete()
        onCancelled: QbzPlaylistEdit.cancelDelete()
    }
}
