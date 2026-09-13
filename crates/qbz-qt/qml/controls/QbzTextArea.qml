// QbzTextArea — the port's first MULTILINE input. Slint uses std-widgets
// `TextEdit` (MyQbzEditModal.slint:90-99, 96px tall, word-wrap); QML's
// QtQuick.Controls TextArea would bring its own styled background, so this is
// the same Rectangle-plus-input recipe controls/QbzLineEdit.qml uses for the
// single-line case: r8 surface-elevated box, 1px border that turns accent on
// focus, 12/8 padding, a Flickable so a description longer than the box
// scrolls instead of clipping silently.
//
// One call site today (the Edit modal's "Edit description" body). The API is
// deliberately the same shape as QbzLineEdit's: `text` in, `edited(value)`
// out, and the external re-seed goes through a `Binding { when: !activeFocus }`
// so a republish while the user is typing cannot fight the caret.
//
// No Enter-submit: a newline is legitimate content here (MyQbzEditModal has
// no `accepted` handler on this body either).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    property string text: ""
    property string placeholder: ""
    /// Exposed so a call site can pass TextEdit.WordWrap / .NoWrap explicitly;
    /// the .slint body is `wrap: word-wrap`.
    property int wrapMode: TextEdit.WordWrap
    signal edited(string value)

    QbzTheme { id: theme }

    width: 240
    height: 96
    radius: theme.radiusSm
    color: theme.surfaceElevated
    border.width: 1
    border.color: input.activeFocus ? theme.accent : theme.borderSubtle

    Flickable {
        id: flick
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        anchors.topMargin: 8
        anchors.bottomMargin: 8
        clip: true
        contentWidth: width
        contentHeight: input.implicitHeight
        boundsBehavior: Flickable.StopAtBounds

        TextEdit {

            QbzTextEditMenu { }
            id: input
            width: flick.width
            text: root.text
            color: theme.textPrimary
            font.pixelSize: theme.fontBody
            wrapMode: root.wrapMode
            selectByMouse: true
            onTextChanged: if (activeFocus) root.edited(text)

            // External republish re-seeds the field while it is not being
            // edited (the QbzLineEdit.qml:167-172 idiom).
            Binding {
                target: input
                property: "text"
                value: root.text
                when: !input.activeFocus
            }

            Text {
                visible: input.text === "" && root.placeholder !== ""
                width: parent.width
                text: root.placeholder
                color: theme.textMuted
                font.pixelSize: theme.fontBody
                wrapMode: Text.WordWrap
            }
        }
    }
}
