import QtQuick
import QtTest
import "../../crates/qbz-qt/qml/controls"

Item {
    width: 640; height: 480
    QbzLineEdit { id: field; x: 20; y: 20; width: 320 }
    QbzTextArea { id: body; x: 20; y: 80; width: 320; onEdited: function(value) { text = value } }
    TextEdit { id: clipboard; visible: false }
    TextInput {
        id: readonlyField; x: 20; y: 200; width: 320; height: 30
        readOnly: true; text: "read-only text"
        QbzTextEditMenu { }
    }
    SignalSpy { id: edits; target: field; signalName: "edited" }
    SignalSpy { id: bodyEdits; target: body; signalName: "edited" }
    TestCase {
        name: "TextEditMenu"; when: windowShown
        function editor(item) {
            if (item instanceof TextInput || item instanceof TextEdit) return item
            for (const child of item.children) {
                const result = editor(child)
                if (result) return result
            }
            return null
        }
        function openMenu(input) {
            mouseClick(input, 12, 12, Qt.RightButton)
            const menu = findChild(input, "textEditContextMenu")
            verify(menu !== null)
            tryCompare(menu, "visible", true)
            return menu
        }
        function pick(menu, index) {
            verify(menu.entries[index].enabled, "Action enabled: " + index)
            waitForRendering(menu.contentItem)
            mouseClick(menu.contentItem, 50, 33 * index + 16)
            tryCompare(menu, "visible", false)
        }
        function init() {
            field.isPassword = false
            editor(field).text = "original"
            editor(field).deselect()
            editor(body).text = ""
            clipboard.text = "pasted text"
            clipboard.selectAll(); clipboard.copy()
            edits.clear(); bodyEdits.clear()
        }
        function test_paste_updates_live_input_once_and_copy_cut_work() {
            const input = editor(field)
            input.forceActiveFocus(); input.selectAll()
            pick(openMenu(input), 2)
            compare(input.text, "pasted text")
            compare(edits.count, 1)
            input.selectAll()
            pick(openMenu(input), 1)
            clipboard.text = ""; clipboard.paste()
            compare(clipboard.text, "pasted text")
            input.selectAll()
            pick(openMenu(input), 0)
            compare(input.text, "")
            compare(edits.count, 2)
        }
        function test_multiline_paste_notifies_form_and_survives_blur() {
            const input = editor(body)
            pick(openMenu(input), 2)
            compare(input.text, "pasted text")
            verify(bodyEdits.count > 0)
            compare(bodyEdits.signalArguments[bodyEdits.count - 1][0], "pasted text")
            editor(field).forceActiveFocus()
            compare(input.text, "pasted text")
        }
        function test_password_allows_paste_but_not_copy_or_cut() {
            field.isPassword = true
            const input = editor(field)
            input.forceActiveFocus(); input.selectAll()
            const menu = openMenu(input)
            verify(!menu.entries[0].enabled)
            verify(!menu.entries[1].enabled)
            verify(menu.entries[2].enabled)
            pick(menu, 2)
            compare(input.text, "pasted text")
        }
        function test_readonly_allows_copy_but_not_mutation() {
            readonlyField.forceActiveFocus(); readonlyField.selectAll()
            const menu = openMenu(readonlyField)
            verify(!menu.entries[0].enabled)
            verify(menu.entries[1].enabled)
            verify(!menu.entries[2].enabled)
            pick(menu, 1)
            clipboard.text = ""; clipboard.paste()
            compare(clipboard.text, "read-only text")
        }
    }
}
