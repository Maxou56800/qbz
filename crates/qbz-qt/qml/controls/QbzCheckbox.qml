// QbzCheckbox (primitives/QbzCheckbox.slint) — 18px square, r4, accent fill
// with an accent-text check when on, muted border when off. Emits
// toggled(modifiers); like the Slint original it never self-flips (the owner of
// the state does).
//
// `modifiers` is the click's (or the key's), straight through: a multi-select
// host turns Shift into a range (controls/SelectionModel.qml). Hosts that only
// flip a flag keep a plain expression handler and never look at it.
//
// Used by Settings > Local Library (folder selection) and the Plex library
// picker, i.e. every place the Slint uses its own QbzCheckbox.

import QtQuick
import "../theme"

Rectangle {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    id: root

    property bool checked: false
    signal toggled(int modifiers)

    QbzTheme { id: theme }

    width: kioskHost ? 44 : 18
    height: kioskHost ? 44 : 18
    radius: 4
    color: root.checked ? theme.accent : "transparent"
    border.width: root.activeFocus ? 2 : (root.checked ? 0 : 2)
    border.color: root.activeFocus
        ? (root.checked ? theme.accentGlyphColor : theme.accent)
        : theme.textMuted
    activeFocusOnTab: root.enabled
    Accessible.role: Accessible.CheckBox
    Accessible.checked: root.checked
    Accessible.onToggleAction: if (root.enabled) root.toggled(Qt.NoModifier)

    Keys.onPressed: function (event) {
        if (root.enabled && !event.isAutoRepeat
                && (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter)) {
            root.toggled(event.modifiers)
            event.accepted = true
        }
    }

    QbzIcon {
        anchors.centerIn: parent
        visible: root.checked
        name: "check"
        // The glyph sits ON the accent fill. primitives/QbzCheckbox.slint:24
        // says `tint: Theme.accent-text` and this was 1:1 with it; it now
        // goes through the selector, which RETURNS accent-text on 34 of the
        // 35 palettes — so this is not a divergence from :24 so much as a
        // floor under it. The one row it changes is rose-pine-dawn, where
        // accent-text #575279 on accent #d7827e is 2.56:1, under the 3:1
        // non-text floor, and the selector hands back black at 7.38:1.
        // Routing every on-accent glyph through one property is also what
        // stops this control and views/local/SelectCheck.qml from drifting
        // apart again — upstream contradicts itself between the two
        // (SelectionCheckbox.slint:37 hardcodes #ffffff for the same job).
        tintName: theme.accentGlyphTint
        width: 12
        height: 12
    }

    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onPressed: root.forceActiveFocus()
        onClicked: function (mouse) { root.toggled(mouse.modifiers) }
    }
}
