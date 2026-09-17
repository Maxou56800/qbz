// QbzSelectableText — a Text-shaped label the user can select and copy.
//
// Qt Quick's Text cannot be selected; a read-only TextEdit can, so the
// default arm is one: mouse and keyboard selection, the right-click Copy
// menu (QbzTextEditMenu) and a rich-text link for the fields that navigate
// (artist, label, musician). The `raised` arm keeps a plain Text with the
// Raised shadow style for the hosts that sit on the ambient field (the
// immersive TrackInfoPanel): TextEdit has no `style`, and legibility there
// beats selection. Same props on both arms, so a host switches with one flag.
//
// Sizing: hosts bind `width`; the height follows the laid-out text through
// implicitHeight, exactly like the Text it replaces.

import QtQuick
import "../theme"

Item {
    id: root

    property string text: ""
    property color color: theme.textPrimary
    property int pixelSize: theme.fontBody
    property int weight: theme.weightRegular
    property real letterSpacing: 0
    property int wrapMode: Text.WordWrap
    property int horizontalAlignment: Text.AlignLeft
    /// `text` is already HTML (paragraph and line-height markup). Not
    /// combined with `linkHref`.
    property bool rich: false
    /// Plain Text with the Raised shadow style, no selection (ambient hosts).
    property bool raised: false
    property color raisedShadow: "#b0000000"
    /// When set, the whole text is one link: a click raises linkActivated
    /// with it, hovering paints linkHoverColor and the pointing cursor.
    property string linkHref: ""
    property color linkHoverColor: theme.accent
    signal linkActivated(string href)

    QbzTheme { id: theme }

    implicitWidth: raised ? plain.implicitWidth : edit.implicitWidth
    implicitHeight: raised ? plain.implicitHeight : edit.implicitHeight

    function esc(s) {
        return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
            .replace(/>/g, "&gt;").replace(/"/g, "&quot;")
    }
    readonly property bool linkHovered: root.linkHref !== ""
        && (root.raised ? plainHover.hovered : editHover.hovered)
    readonly property color liveColor: linkHovered ? root.linkHoverColor : root.color

    TextEdit {
        id: edit
        visible: !root.raised
        width: root.width
        readOnly: true
        selectByMouse: true
        selectByKeyboard: true
        cursorVisible: false
        persistentSelection: false
        activeFocusOnTab: false
        textFormat: (root.linkHref !== "" || root.rich) ? TextEdit.RichText : TextEdit.PlainText
        // The link colour rides in the markup (TextEdit has no linkColor);
        // it is rebuilt only when the hover flips, not per mouse move.
        text: root.linkHref !== ""
            ? "<a href=\"" + root.esc(root.linkHref) + "\" style=\"text-decoration:none; color:"
              + root.liveColor + "\">" + root.esc(root.text) + "</a>"
            : root.text
        color: root.color
        font.pixelSize: root.pixelSize
        font.weight: root.weight
        font.letterSpacing: root.letterSpacing
        wrapMode: root.wrapMode
        horizontalAlignment: root.horizontalAlignment
        selectionColor: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.35)
        selectedTextColor: theme.textPrimary
        onLinkActivated: function (link) { root.linkActivated(link) }
        HoverHandler {
            id: editHover
            cursorShape: (root.linkHref !== "" && edit.hoveredLink !== "")
                ? Qt.PointingHandCursor : Qt.IBeamCursor
        }
        QbzTextEditMenu { }
    }

    Text {
        id: plain
        visible: root.raised
        width: root.width
        text: root.text
        textFormat: root.rich ? Text.RichText : Text.PlainText
        color: root.liveColor
        style: Text.Raised
        styleColor: root.raisedShadow
        font.pixelSize: root.pixelSize
        font.weight: root.weight
        font.letterSpacing: root.letterSpacing
        wrapMode: root.wrapMode
        horizontalAlignment: root.horizontalAlignment
        HoverHandler {
            id: plainHover
            enabled: root.linkHref !== ""
            cursorShape: Qt.PointingHandCursor
        }
        TapHandler {
            enabled: root.linkHref !== ""
            onTapped: root.linkActivated(root.linkHref)
        }
    }
}
