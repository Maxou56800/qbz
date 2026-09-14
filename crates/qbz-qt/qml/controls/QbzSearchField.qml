// QbzSearchField — the inline search box the views share (2026-09-13): the
// rounded elevated field with a leading search glyph, a placeholder, a clear
// cross floating at the right while there is text, and one key contract —
// Escape clears the text AND hands the keyboard back to the shell root (the
// walk the header search does), so no search box keeps a stale query or a
// blinking cursor after Escape.
//
// FOCUS RETENTION. A search box that filters a list on every keystroke sits
// above a surface that rebuilds on every keystroke, and the field must never
// lose the keyboard mid-word to that rebuild. After each edit, once the event
// loop has settled, the field checks it still has focus and reclaims it
// unless another text input took it: a click elsewhere is a decision, a
// rebuilt list is not.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    property alias text: input.text
    property string placeholder: ""
    property int glyphSize: 14
    property int fontPx: 13
    property int sidePadding: 10
    /// Fired on every keystroke and on clear, with the live text.
    signal edited(string text)
    readonly property bool fieldActive: input.activeFocus

    implicitWidth: 168
    implicitHeight: 34
    radius: 6
    // The header search's ambient-aware fill, so every search box reads alike.
    color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
    border.width: 1
    border.color: input.activeFocus ? theme.accent : theme.borderSubtle

    QbzTheme { id: theme }

    function clear() {
        if (input.text === "")
            return
        input.text = ""
        root.edited("")
    }
    /// Escape's contract: empty AND hand the keyboard to the shell root.
    function clearAndBlur() {
        root.clear()
        root.blur()
    }
    function blur() {
        var p = root
        while (p.parent) {
            if (p.parent.isQbzShellRoot === true) {
                p.parent.forceActiveFocus()
                return
            }
            p = p.parent
        }
        input.focus = false
    }
    function focusField() { input.forceActiveFocus() }

    property bool _reclaim: false
    function _reclaimFocus() {
        if (!root._reclaim)
            return
        root._reclaim = false
        if (input.activeFocus)
            return
        var win = root.Window.window
        var current = win ? win.activeFocusItem : null
        if (current instanceof TextInput || current instanceof TextEdit)
            return
        console.log("[QbzSearchField] focus left the field after an edit; reclaimed (holder was "
                    + (current ? current.toString() : "none") + ")")
        input.forceActiveFocus()
    }

    QbzIcon {
        name: "search"
        width: root.glyphSize
        height: root.glyphSize
        x: root.sidePadding
        anchors.verticalCenter: parent.verticalCenter
        tintName: "muted"
    }
    TextInput {
        id: input
        QbzTextEditMenu { }
        anchors.left: parent.left
        anchors.right: clearSlot.left
        anchors.leftMargin: root.sidePadding + root.glyphSize + 7
        anchors.rightMargin: 4
        height: parent.height
        color: theme.textPrimary
        font.pixelSize: root.fontPx
        verticalAlignment: Text.AlignVCenter
        clip: true
        onTextEdited: {
            root.edited(text)
            root._reclaim = true
            Qt.callLater(root._reclaimFocus)
        }
        Keys.onEscapePressed: function (event) {
            root.clearAndBlur()
            event.accepted = true
        }
        Text {
            visible: parent.text === ""
            anchors.fill: parent
            text: root.placeholder
            color: theme.textMuted
            font.pixelSize: root.fontPx
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
    }
    // The clear cross: floats at the right while there is text; a click
    // empties the field and keeps the keyboard in it.
    Item {
        id: clearSlot
        anchors.right: parent.right
        anchors.rightMargin: 5
        width: input.text !== "" ? 22 : 0
        height: parent.height
        visible: input.text !== ""
        Rectangle {
            anchors.centerIn: parent
            width: 22
            height: 22
            radius: 11
            color: clearArea.containsMouse ? theme.surfaceHover : "transparent"
            QbzIcon {
                name: "x"
                width: 12
                height: 12
                anchors.centerIn: parent
                tintName: clearArea.containsMouse ? "textPrimary" : "muted"
            }
            MouseArea {
                id: clearArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    root.clear()
                    input.forceActiveFocus()
                }
            }
        }
    }
}
