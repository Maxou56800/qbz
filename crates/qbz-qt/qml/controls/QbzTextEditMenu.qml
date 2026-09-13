// Pointer editing for both TextInput and TextEdit, including native TextFields.
// Keep focus on the editor so form bindings and focus-loss commits stay intact.
import QtQuick
import com.blitzfc.qbz

Item {
    id: root
    anchors.fill: parent
    readonly property var editor: parent
    readonly property bool secret: editor.echoMode !== undefined && editor.echoMode !== TextInput.Normal
    readonly property bool writable: editor.enabled && !editor.readOnly
    Component.onCompleted: editor.selectByMouse = true
    property int editRevision: 0
    Connections {
        target: root.editor
        ignoreUnknownSignals: true
        function onTextEdited() { root.editRevision += 1 }
    }

    function perform(action) {
        editor.forceActiveFocus()
        const before = editor.text
        const revision = editRevision
        if (action === "copy" && !secret) editor.copy()
        else if (action === "cut" && writable && !secret) editor.cut()
        else if (action === "paste" && writable) editor.paste()
        else if (action === "select-all") editor.selectAll()
        // Drive the same callback as typing if this Qt version did not emit
        // textEdited for cut/paste. Avoid double requests on versions that do.
        // TextEdit already notifies its consumers via textChanged.
        if (editor.text !== before && editor instanceof TextInput && editRevision === revision)
            editor.textEdited()
    }

    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.RightButton
        onClicked: function(mouse) {
            root.editor.forceActiveFocus()
            menuLoader.active = true
            menuLoader.item.openAtCursor(root.editor, mouse.x, mouse.y)
        }
    }
    // Allocate menu rows/icons only when this field is first right-clicked.
    Loader {
        id: menuLoader
        active: false
        sourceComponent: CardMenu {
            id: menu
            objectName: "textEditContextMenu"
            menuWidth: 180
            focus: false
            entries: [
                {label: QbzSession.tr("Cut", QbzSession.trRev), icon: "scissors", action: "cut",
                    enabled: root.writable && !root.secret && root.editor.selectedText !== ""},
                {label: QbzSession.tr("Copy", QbzSession.trRev), icon: "copy", action: "copy",
                    enabled: !root.secret && root.editor.selectedText !== ""},
                {label: QbzSession.tr("Paste", QbzSession.trRev), icon: "clipboard", action: "paste",
                    enabled: root.writable && root.editor.canPaste},
                {label: QbzSession.tr("Select all", QbzSession.trRev), icon: "square-check-big", action: "select-all",
                    enabled: root.editor.text !== ""}
            ]
            onPicked: function(action) { root.perform(action) }
        }
    }
}
