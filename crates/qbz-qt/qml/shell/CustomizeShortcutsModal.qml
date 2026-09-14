// CustomizeShortcutsModal — the EDITABLE keybindings editor, port of
// crates/qbz-ui/ui/shell/CustomizeShortcutsModal.slint (itself a 1:1 port of
// the Tauri KeybindingsSettings.svelte + ShortcutInput.svelte).
//
// Block B3 of the 2026-08-03 hotkeys-port contract (§4.5):
//   shell   same as the cheatsheet (scrim #bf000000, Radius.md surface-card,
//           1px border, faked 32px shadow), card width min(root - 80, 1080),
//           height min(max(420, content), root - 80)
//   header  tr("Customize Shortcuts") + "N modified" ACCENT BADGE
//           (modifiedCount > 0; text = count + " " + tr("modified")) + X.
//           NO keyboard icon — the editor header has none
//           (CustomizeShortcutsModal.slint:208-261, trap 6)
//   body    THREE round-robin columns from the SAME QbzHotkeys.groupsJson
//           doc the cheatsheet renders
//   keycap  the capture button: max(120, label + 24) x 32, Radius.sm;
//           border 2px while recording/modified else 1px; colour priority
//           danger(conflict) -> accent(recording/modified) -> subtle;
//           label = pendingDisplay live / tr("Press keys…") while recording /
//           the formatted shortcut; monospace; click -> startRecord(id)
//   reset   per-row 30x30 ELEVATED 1px-bordered chip (NOT a ghost, trap 6),
//           rotate-ccw icon, ONLY when the row is modified -> resetOne(id)
//   footer  tr("Reset All to Defaults") danger GHOST, opacity 0.3 + inert
//           when modifiedCount == 0 -> resetAll()
//
// The actual key capture happens in RUST (contract §3.3): this modal only
// flips state via startRecord / cancelRecord and DISPLAYS the live pending
// combo + any conflict. The modal's FocusScope SWALLOWS ALL KEYS while
// recording and routes them to QbzHotkeys.keyPressed(key, modifiers, text,
// false) — capture arm A, the "belt and suspenders" of contract §1.1(A) (the
// Rust arm is the semantic owner; the QML swallow keeps the key from also
// reaching the AppShell root dispatcher via the focus chain). Escape =
// cancelRecord if recording (via the routed capture arm) else close. NOT
// recording -> non-Escape keys are REJECTED so they propagate to the root
// dispatcher (Slint parity: the global hook still fires bindings under the
// open editor).
//
// Scrim-click and X both cancel an in-flight recording before closing
// (Slint :151-158, :247-251) so we never leave a dangling capture.
//
// FOCUS: same lifecycle as the cheatsheet — 30ms deferred grab on the open
// edge, shell-root restore on the close edge (§1.4.3; see
// KeyboardShortcutsModal.qml's header for the full note).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // Same guarded parse as the cheatsheet (see its header — trap 15 covers
    // the pre-publish frame, the try/catch covers a corrupt publish).
    readonly property var doc: {
        try {
            return JSON.parse(QbzHotkeys.groupsJson)
        } catch (e) {
            return ({ "col1": [], "col2": [], "col3": [] })
        }
    }

    readonly property bool recording: QbzHotkeys.recordingId !== ""

    visible: QbzHotkeys.customizeOpen
    enabled: root.visible

    function _close() {
        // Cancel an in-flight recording first — never leave a dangling
        // capture (Slint :154-155).
        if (root.recording)
            QbzHotkeys.cancelRecord()
        QbzHotkeys.customizeOpen = false
    }

    // §1.4.3 focus lifecycle (identical shape to the cheatsheet's — grab
    // deferred 30ms on open, shell-root restore on the close edge).
    Connections {
        target: QbzHotkeys
        function onCustomizeOpenChanged() {
            if (QbzHotkeys.customizeOpen) {
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

    Timer {
        id: focusGrab
        interval: 30
        repeat: false
        onTriggered: keyScope.forceActiveFocus()
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent

        // Capture arm A (contract §4.5 / §1.1(A)): while a row is recording,
        // swallow EVERY key here and route it to the Rust brain with
        // textInputFocused = false. Escape cancels the recording inside the
        // Rust capture arm; conflict keeps recording. NOT recording: Escape
        // closes, everything else propagates to the AppShell dispatcher.
        Keys.onPressed: function (event) {
            if (root.recording) {
                QbzHotkeys.keyPressed(event.key, event.modifiers, event.text, false)
                event.accepted = true
                return
            }
            if (event.key === Qt.Key_Escape) {
                root._close()
                event.accepted = true
            }
        }

        // Scrim — a click outside the panel closes the editor AND cancels
        // any in-progress recording (Slint :151-158).
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

        // Faked 32px drop shadow (#00000080) — the QbzConfirmModal precedent.
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
            width: Math.min(root.width - 80, 1080)
            // Slint :163-164: min(max(420, content), root - 80).
            height: Math.min(Math.max(420, headerRow.height + footerRow.height + 2
                                      + bodyRow.implicitHeight + 32),
                             root.height - 80)
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

            // --- Header: title + "N modified" badge + X (Slint :208-261) --
            // NO keyboard icon here — the editor header has none (trap 6).
            Item {
                id: headerRow
                anchors.top: parent.top
                width: parent.width
                height: 60   // Spacing.md (16) top/bottom around the 28px X
                Text {
                    id: titleLabel
                    x: 20   // Spacing.lg
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.t("Customize Shortcuts")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                }
                Rectangle {
                    id: modBadge
                    anchors.left: titleLabel.right
                    anchors.leftMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    visible: QbzHotkeys.modifiedCount > 0
                    width: badgeLabel.implicitWidth + 16
                    height: 20
                    radius: theme.radiusSm
                    color: theme.accent
                    Text {
                        id: badgeLabel
                        anchors.centerIn: parent
                        text: QbzHotkeys.modifiedCount + " " + root.t("modified")
                        color: theme.accentText
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightMedium
                    }
                }
                // Keymap preset. Switching swaps the BASE shortcut table
                // only — user overrides are kept and keep winning over it
                // (hotkeys_qt::active_bindings_with), so Default <-> Vim is
                // lossless. `keymapIndex` is republished with the groups, so
                // the selection and the keycaps cannot disagree.
                Row {
                    id: keymapPicker
                    anchors.right: closeX.left
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 4
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        rightPadding: 6
                        text: root.t("Keymap")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                    Repeater {
                        model: [{ "label": root.t("Default"), "index": 0 },
                                { "label": root.t("Vim"), "index": 1 }]
                        delegate: Rectangle {
                            id: seg
                            required property var modelData
                            readonly property bool current:
                                QbzHotkeys.keymapIndex === seg.modelData.index
                            width: segLabel.implicitWidth + 20
                            height: 26
                            radius: theme.radiusSm
                            color: seg.current
                                ? theme.accent
                                : (segArea.containsMouse ? theme.surfaceHover
                                                         : "transparent")
                            border.width: 1
                            border.color: seg.current ? theme.accent
                                                      : theme.borderSubtle
                            Text {
                                id: segLabel
                                anchors.centerIn: parent
                                text: seg.modelData.label
                                color: seg.current ? theme.accentText
                                                   : theme.textPrimary
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightMedium
                            }
                            MouseArea {
                                id: segArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzHotkeys.setKeymap(seg.modelData.index)
                            }
                        }
                    }
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

            // --- Body: scrollable three-column editor (Slint :270-301) ---
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
                    // Spacing.md (16) top/bottom padding.
                    height: bodyRow.implicitHeight + 32
                    Row {
                        id: bodyRow
                        x: 20   // Spacing.lg
                        y: 16
                        width: parent.width - 40
                        spacing: 20   // Spacing.lg
                        KbEditColumn { width: (bodyRow.width - 40) / 3; groups: root.doc.col1 }
                        KbEditColumn { width: (bodyRow.width - 40) / 3; groups: root.doc.col2 }
                        KbEditColumn { width: (bodyRow.width - 40) / 3; groups: root.doc.col3 }
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

            // --- Footer: "Reset All to Defaults" (Slint :310-360) --------
            Item {
                id: footerRow
                anchors.bottom: parent.bottom
                width: parent.width
                height: 70   // Spacing.md (16) top/bottom around the 38px button
                Rectangle {
                    id: resetAll
                    anchors.centerIn: parent
                    readonly property bool active: QbzHotkeys.modifiedCount > 0
                    width: resetAllRow.implicitWidth + 32   // padding 16 each side
                    height: 38
                    radius: theme.radiusSm
                    opacity: resetAll.active ? 1.0 : 0.3
                    // Danger GHOST: danger-bg fill on hover only, 1px danger
                    // border, danger label (theme.dangerBg IS Slint's
                    // Theme.danger-bg; QbzTheme.qml:71).
                    color: (resetAll.active && resetAllArea.containsMouse)
                           ? theme.dangerBg : "transparent"
                    border.width: 1
                    border.color: theme.danger
                    Row {
                        id: resetAllRow
                        anchors.centerIn: parent
                        spacing: 8
                        QbzIcon {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 14
                            height: 14
                            name: "rotate-ccw"
                            // "danger" is served by the runtime tint bake
                            // (icon_tint_qt.rs token_for; both glyphs are
                            // masters). The floor gap is the documented
                            // toast-class one: no qrc danger/ dir, so a dead
                            // bake renders nothing — same reachability as
                            // QbzToast's per-kind glyph.
                            tintName: "danger"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.t("Reset All to Defaults")
                            color: theme.danger
                            font.pixelSize: theme.fontBody
                        }
                    }
                    MouseArea {
                        id: resetAllArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: resetAll.active ? Qt.PointingHandCursor
                                                     : Qt.ArrowCursor
                        onClicked: if (resetAll.active) QbzHotkeys.resetAll()
                    }
                }
            }
        }
    }

    // One editable category block (Slint KbEditGroup :31-141): the verbatim
    // group header (NO divider — that is cheatsheet-only) + the capture rows.
    component KbEditColumn: Column {
        id: kbCol
        property var groups: []
        spacing: 20   // Spacing.lg between groups

        Repeater {
            model: kbCol.groups
            delegate: Column {
                id: kbGroup
                width: kbCol.width
                spacing: 8   // Spacing.sm (KbEditGroup's own spacing)
                readonly property var group: modelData

                Text {
                    // VERBATIM label — NOT uppercased (trap 6).
                    text: kbGroup.group.label
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    font.weight: theme.weightSemibold
                }

                Repeater {
                    model: kbGroup.group.rows
                    delegate: Item {
                        id: entryRow
                        width: kbGroup.width
                        readonly property var entry: modelData
                        readonly property bool recording:
                            QbzHotkeys.recordingId === entryRow.entry.id
                        readonly property bool conflicting:
                            entryRow.recording && QbzHotkeys.conflictLabel !== ""
                        // Slint :45: min-height 36; the conflict line grows it.
                        height: Math.max(36, rightCol.implicitHeight)

                        // LEFT: action label.
                        Text {
                            anchors.left: parent.left
                            anchors.right: rightCol.left
                            anchors.rightMargin: 10
                            anchors.verticalCenter: parent.verticalCenter
                            text: entryRow.entry.label
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            elide: Text.ElideRight
                        }

                        // RIGHT: keycap capture button + reset, the conflict
                        // message BELOW (Slint :56-139).
                        Column {
                            id: rightCol
                            anchors.right: parent.right
                            width: implicitWidth
                            spacing: 4

                            Row {
                                anchors.right: parent.right
                                width: implicitWidth
                                height: 32
                                spacing: 8

                                // Keycap capture button (Radius.sm rect, NOT
                                // a pill — ADR-008 / trap 6).
                                Rectangle {
                                    id: capture
                                    width: Math.max(120, capLabel.implicitWidth + 24)
                                    height: 32
                                    radius: theme.radiusSm
                                    color: entryRow.recording
                                        ? theme.bgHover
                                        : (capArea.containsMouse ? theme.surfaceElevatedHover
                                                                 : theme.surfaceElevated)
                                    border.width: (entryRow.recording || entryRow.entry.modified) ? 2 : 1
                                    // Colour priority (trap 6): danger on
                                    // conflict -> accent recording/modified
                                    // -> subtle.
                                    border.color: entryRow.conflicting
                                        ? theme.danger
                                        : (entryRow.recording
                                            ? theme.accent
                                            : (entryRow.entry.modified ? theme.accent
                                                                       : theme.borderSubtle))
                                    Text {
                                        id: capLabel
                                        anchors.centerIn: parent
                                        // Live pending combo / "Press keys…"
                                        // while recording / the formatted
                                        // shortcut (Slint :84-88).
                                        text: entryRow.recording
                                            ? (QbzHotkeys.pendingDisplay !== ""
                                                ? QbzHotkeys.pendingDisplay
                                                : root.t("Press keys…"))
                                            : entryRow.entry.shortcut
                                        color: theme.textPrimary
                                        font.pixelSize: theme.fontBody
                                        font.weight: theme.weightMedium
                                        font.family: "monospace"
                                    }
                                    MouseArea {
                                        id: capArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: QbzHotkeys.startRecord(entryRow.entry.id)
                                    }
                                }

                                // Per-row reset — ONLY when modified; a 30x30
                                // ELEVATED 1px-bordered chip, NOT a ghost
                                // (trap 6). Invisible children leave no gap
                                // in a Row.
                                Rectangle {
                                    visible: entryRow.entry.modified
                                    width: 30
                                    height: 30
                                    anchors.verticalCenter: parent.verticalCenter
                                    radius: theme.radiusSm
                                    color: resetArea.containsMouse ? theme.surfaceElevatedHover
                                                                   : theme.surfaceElevated
                                    border.width: 1
                                    border.color: theme.borderSubtle
                                    QbzIcon {
                                        anchors.centerIn: parent
                                        width: 14
                                        height: 14
                                        name: "rotate-ccw"
                                        tintName: resetArea.containsMouse ? "textPrimary" : "muted"
                                    }
                                    MouseArea {
                                        id: resetArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: QbzHotkeys.resetOne(entryRow.entry.id)
                                    }
                                }
                            }

                            // Conflict message BELOW the keycap, danger,
                            // right-aligned (Slint :132-138).
                            Text {
                                visible: entryRow.conflicting
                                width: rightCol.width
                                horizontalAlignment: Text.AlignRight
                                text: root.t("Already used by") + " " + QbzHotkeys.conflictLabel
                                color: theme.danger
                                font.pixelSize: theme.fontLegal
                            }
                        }
                    }
                }
            }
        }
    }
}
