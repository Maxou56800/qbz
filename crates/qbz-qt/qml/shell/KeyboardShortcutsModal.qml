// KeyboardShortcutsModal — the READ-ONLY hotkeys cheatsheet, port of
// crates/qbz-ui/ui/shell/KeyboardShortcutsModal.slint (itself a 1:1 port of
// the Tauri KeyboardShortcutsModal.svelte). Every keybinding grouped by
// category (Playback / Navigation / Interface / ...), each row showing the
// localized action label (left) and its formatted shortcut keycap (right).
// The footer "Customize Shortcuts" button hands off to the editable editor
// (CustomizeShortcutsModal.qml).
//
// Block B3 of the 2026-08-03 hotkeys-port contract (§4.4 — layout is
// CONTRACTUAL):
//   scrim   full-window #bf000000 (Slint #000000bf CONVERTED — Slint is
//           #RRGGBBAA, Qt is #AARRGGBB), click closes
//   card    width min(root - 80, 980), height min(content, root - 80),
//           Radius.md, surface-card, 1px border-subtle, 32px shadow (faked —
//           the QbzConfirmModal/LoginScreen precedent: a translucent rounded
//           rect behind the panel, because a real blurred drop shadow is a
//           visual change owing its own parity pass)
//   header  keyboard icon 20px + tr("Keyboard Shortcuts") + X close
//   body    THREE round-robin columns from QbzHotkeys.groupsJson (the Rust
//           brain splits the 5 groups cols[i%3], §3.4); group header = muted
//           legal-size semibold label VERBATIM (NOT uppercased, trap 6) +
//           1px divider (the divider is cheatsheet-only — the editor has
//           none); rows = elevated rects, label left + keycap chip right
//   keycap  Radius.sm rect, NOT a pill (ADR-008, trap 6), surface-card bg,
//           1px border-muted, monospace
//   footer  divider + centered 40px tr("Customize Shortcuts") button ->
//           close + QbzHotkeys.openCustomize()
//
// Self-gated mount (the ADR-010 shape every AppShell global overlay uses):
// an invisible, non-interactive Item while QbzHotkeys.cheatsheetOpen is
// false. Opened by the `?` hotkey (ui.showShortcuts) and by the HeaderBar
// app-menu row (B2). Escape / scrim-click / X all close it.
//
// The data doc arrives as ONE JSON string ({col1,col2,col3} of
// {label, rows[{id,label,shortcut,modified,contextual}]}) because cxx-qt
// qproperties cannot express nested models — the same document shape
// QbzPlaylistEdit.editJson / QbzFolderEdit.editJson use. Labels arrive
// LOCALIZED (Rust-side qbz_i18n::t, B1); the parse is guarded because a raw
// JSON.parse in a binding throws on the pre-publish frame
// (PlaylistView.qml:55-61) — the bridge's construction-seeded default is the
// full shape (trap 15), but the guard also covers a corrupt publish.
//
// FOCUS (contract §4.4 + §1.4.3): the modal owns a FocusScope whose Escape
// closes (Item/FocusScope modal Escapes fire BEFORE the central §1.2 stack —
// trap 4, they accept the event). The grab is DEFERRED 30ms past the open
// edge (the Slint focus-timer :119-126, and this port's ImmersiveView.qml
// :105-112 idiom): focusing synchronously races the visibility transition.
// On the close edge Qt strands activeFocus on the now-invisible scope, which
// kills the AppShell key dispatcher until the next click — so every close
// path hands focus back to the shell root (the dispatcher's fallback focus
// item, §1.4.3; the duck-walk is QbzConfirmModal.qml's _restoreShellFocus).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // Guarded parse (see the header). Full-shape fallback so the three
    // columns render empty instead of throwing on a bad frame.
    readonly property var doc: {
        try {
            return JSON.parse(QbzHotkeys.groupsJson)
        } catch (e) {
            return ({ "col1": [], "col2": [], "col3": [] })
        }
    }

    visible: QbzHotkeys.cheatsheetOpen
    enabled: root.visible

    function _close() {
        QbzHotkeys.cheatsheetOpen = false
    }

    // §1.4.3: the FocusScope GRABS focus while open and Qt strands it on the
    // now-invisible scope at close. Restore the shell root on the close edge
    // (the edge, not the close functions, so the ui.escape stack arm 3 and
    // closeAll() restore too, whichever path closed us).
    Connections {
        target: QbzHotkeys
        function onCheatsheetOpenChanged() {
            if (QbzHotkeys.cheatsheetOpen) {
                focusGrab.restart()
            } else {
                var p = root
                while (p.parent) {
                    if (p.parent.isQbzShellRoot === true) {
                        p.parent.forceActiveFocus()
                        return
                    }
                    p = p.parent
                }
            }
        }
    }

    // 30ms deferred focus grab (the Slint focus-timer, :119-126;
    // ImmersiveView.qml:105-112 in this port).
    Timer {
        id: focusGrab
        interval: 30
        repeat: false
        onTriggered: keyScope.forceActiveFocus()
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent

        // The modal's OWN Escape (contract §4.4) — accepts, so the central
        // §1.2 stack arm 3 never double-fires on the same press.
        Keys.onEscapePressed: function (event) {
            root._close()
            event.accepted = true
        }

        // Scrim — click outside the card closes (Slint :87-92).
        Rectangle {
            anchors.fill: parent
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: root._close()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        // Faked 32px drop shadow (#00000080) — see the header note.
        Rectangle {
            x: card.x
            y: card.y + 8
            width: card.width
            height: card.height
            radius: theme.radiusMd
            color: "#80000000"
            opacity: 0.5
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 80, 980)
            // Slint :100-105: chrome (header + footer + the two 1px dividers)
            // + the body's real content height, capped at root - 80. The
            // Flickable stretches to whatever room is left and scrolls.
            height: Math.min(headerRow.height + footerRow.height + 2
                             + bodyRow.implicitHeight + 40,
                             root.height - 80)
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            // FIRST child: swallows clicks so they never reach the scrim
            // (the Slint's empty panel TouchArea, :137).
            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            // --- Header: icon + title + X (Slint :143-184) ---------------
            Item {
                id: headerRow
                anchors.top: parent.top
                width: parent.width
                height: 68   // padding 20 top/bottom around the 28px close-x
                QbzIcon {
                    id: titleIcon
                    x: 20
                    anchors.verticalCenter: parent.verticalCenter
                    width: 20
                    height: 20
                    name: "keyboard"
                    tintName: "textPrimary"
                }
                Text {
                    anchors.left: titleIcon.right
                    anchors.leftMargin: 10
                    anchors.right: closeX.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.t("Keyboard Shortcuts")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Item {
                    id: closeX
                    anchors.right: parent.right
                    anchors.rightMargin: 20
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
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
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root._close()
                    }
                }
            }
            Rectangle {
                id: headerDivider
                anchors.top: headerRow.bottom
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            // --- Body: scrollable grouped list (Slint :193-220) ----------
            Flickable {
                anchors.top: headerDivider.bottom
                anchors.bottom: footerDivider.top
                width: parent.width
                clip: true
                contentWidth: width
                contentHeight: bodyContent.height
                Item {
                    id: bodyContent
                    width: parent.width
                    height: bodyRow.implicitHeight + 40
                    // Groups laid out across THREE columns (round-robin by
                    // index — the Rust brain already split them, §3.4) so the
                    // cheatsheet fits without a tall scroll.
                    Row {
                        id: bodyRow
                        x: 20
                        y: 20
                        width: parent.width - 40
                        spacing: 24
                        KbCheatsheetColumn { width: (bodyRow.width - 48) / 3; groups: root.doc.col1 }
                        KbCheatsheetColumn { width: (bodyRow.width - 48) / 3; groups: root.doc.col2 }
                        KbCheatsheetColumn { width: (bodyRow.width - 48) / 3; groups: root.doc.col3 }
                    }
                }
            }

            Rectangle {
                id: footerDivider
                anchors.bottom: footerRow.top
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            // --- Footer: Customize Shortcuts (Slint :229-255) ------------
            Item {
                id: footerRow
                anchors.bottom: parent.bottom
                width: parent.width
                height: 72   // padding 16 top/bottom around the 40px button
                Rectangle {
                    anchors.centerIn: parent
                    width: customizeLabel.implicitWidth + 40
                    height: 40
                    radius: theme.radiusSm
                    color: customizeArea.containsMouse ? theme.surfaceElevatedHover
                                                       : theme.surfaceElevated
                    border.width: 1
                    border.color: customizeArea.containsMouse ? theme.accent
                                                              : theme.borderSubtle
                    Text {
                        id: customizeLabel
                        anchors.centerIn: parent
                        text: root.t("Customize Shortcuts")
                        color: customizeArea.containsMouse ? theme.textPrimary
                                                           : theme.textSecondary
                        font.pixelSize: theme.fontBody
                    }
                    MouseArea {
                        id: customizeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        // Slint :249-252: close the cheatsheet, open the
                        // editor (openCustomize recomputes the groups, §3.4).
                        onClicked: {
                            QbzHotkeys.cheatsheetOpen = false
                            QbzHotkeys.openCustomize()
                        }
                    }
                }
            }
        }
    }

    // One category block (Slint KbGroupBlock :19-78): the verbatim group
    // header + 1px divider (cheatsheet-only) + its binding rows. Inline
    // component — used three times above, nowhere else.
    component KbCheatsheetColumn: Column {
        id: kbCol
        // The column model: an array of {label, rows} from groupsJson.
        property var groups: []
        spacing: 16

        Repeater {
            model: kbCol.groups
            delegate: Column {
                id: kbGroup
                width: kbCol.width
                spacing: 6
                // `group` captured once: inside the nested rows Repeater,
                // `modelData` rebinds to the ROW.
                readonly property var group: modelData

                Column {
                    width: parent.width
                    spacing: 4
                    Text {
                        // VERBATIM label — NOT uppercased (trap 6).
                        text: kbGroup.group.label
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                    Rectangle {
                        width: parent.width
                        height: 1
                        color: theme.borderSubtle
                    }
                }

                Repeater {
                    model: kbGroup.group.rows
                    delegate: Rectangle {
                        width: kbGroup.width
                        // Compact rows: the keycap chip plus a tight
                        // vertical padding (was +40; owner asked for shorter,
                        // denser rows).
                        height: keycapLabel.implicitHeight + 20
                        radius: theme.radiusSm
                        color: theme.surfaceElevated

                        Text {
                            anchors.left: parent.left
                            anchors.leftMargin: 12
                            anchors.right: keycap.left
                            anchors.rightMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            text: modelData.label
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            elide: Text.ElideRight
                        }

                        // Keycap — a subtle key-like chip (Radius.sm, NOT a
                        // pill, ADR-008 / trap 6).
                        Rectangle {
                            id: keycap
                            anchors.right: parent.right
                            anchors.rightMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            width: keycapLabel.implicitWidth + 16
                            height: keycapLabel.implicitHeight + 6
                            radius: theme.radiusSm
                            color: theme.surfaceCard
                            border.width: 1
                            border.color: theme.borderMuted
                            Text {
                                id: keycapLabel
                                anchors.centerIn: parent
                                text: modelData.shortcut
                                color: theme.textSecondary
                                // fontBody so the arrow-key glyphs (← → ↑ ↓)
                                // read clearly (Slint :68-70); monospace per
                                // the contract's keycap rule.
                                font.pixelSize: theme.fontBody
                                font.family: "monospace"
                            }
                        }
                    }
                }
            }
        }
    }
}
