// QbzConfirmModal — THE port's confirm dialog. Before this file the tree had
// no confirm surface at all (grep -rl confirm qml/ found only LyricsPanel and
// ArtistView, neither a dialog), so every destructive action either mutated
// straight away or was not shipped. It is written generic on purpose: the
// Blacklist manager's Clear-All is the first caller, and MyQBZ delete,
// playlist delete and the offline-cache purge all need the same surface.
//
// Geometry is 1:1 with the Slint Clear-All modal
// (settings/BlacklistManagerView.slint:992-1113), which itself mirrors
// PlaylistDuplicateConfirmModal:
//   scrim   full-bleed #000000bf, click dismisses
//   panel   width min(host - 80, 400), height = content, centered, Radius.md,
//           surface-main, 1px border-subtle, drop-shadow blur 32 #00000080
//   padding 24, spacing 16; title heading/semibold, body 15 text-secondary
//   buttons right-aligned, spacing 10, both 36px tall, Radius.sm,
//           width = label + 32
//   danger  fill Theme.danger, hover Theme.danger.darker(0.12) — NOT
//           `danger-hover`, which is a translucent tint for GHOST danger
//           surfaces and would wash a filled button out (the Slint comment at
//           :1080-1089 spells this out at length); label hard #ffffff
//
// MOUNT IT LAST inside the view root with `anchors.fill: parent`. ADR-009's
// ">= 3000" is satisfied both structurally (declaration order IS z-order) and
// by the explicit z: 3100 below — the same layer the port's only other in-pane
// modal scrim sits on (controls/DiscoverConfigModal.qml:56), so the two never
// fight.
//
// The shadow is FAKED (a translucent rounded rect behind the panel) — the
// LoginScreen.qml:30-49 precedent, including its 2026-07-29 note: the old
// "effects render nothing here" doctrine is RETIRED (both effect modules are
// installed and this port runs on the GPU; RoundedImage.qml detects the
// software path with GraphicsInfo.api instead of assuming it). A real blurred
// drop shadow is a visual change owing its own parity pass, so the fake stays
// for now, exactly as LoginScreen's does. The port's only other in-pane modal
// (controls/DiscoverConfigModal.qml) draws no shadow at all.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    property bool kioskHost: false
    id: root

    property string title: ""
    property string body: ""
    /// "" is not a valid label — the default is the shared msgid, as a BINDING
    /// so a live language switch re-translates it (QbzSession.trRev).
    property string cancelLabel: QbzSession.tr("Cancel", QbzSession.trRev)
    property string confirmLabel: ""
    /// Filled-red confirm button (the Clear-All / delete voice). false gives
    /// the accent fill instead, for a non-destructive confirmation.
    ///
    /// NAMED `danger`, not `destructive`: spec 03 §6.10/§10 specifies `danger`,
    /// and `controls/SettingsButton.qml:15` already carries `property bool
    /// danger` for exactly this concept (red border + red label + a `favorite`
    /// glyph tint). Two names for one idea across two neighbouring controls is
    /// how a call site ends up setting a property that does not exist — which in
    /// QML is not an error, just a silently-ignored line.
    property bool danger: true
    property string checkboxLabel: ""
    property bool checkboxChecked: false

    readonly property bool opened: root._open
    property bool _open: false

    signal confirmed()
    signal cancelled()

    QbzTheme { id: theme }

    function open() {
        root._open = true
        // AFTER _open, so `visible` is already true — an invisible item cannot
        // take active focus and the Esc handler would be dead.
        Qt.callLater(function () { cancelButton.forceActiveFocus() })
    }
    function close() {
        root._open = false
        root._restoreShellFocus()
    }
    // §1.4.3 (2026-08-03 hotkeys-port contract): a modal control GRABS focus
    // on open and Qt strands activeFocus on that now-invisible item at close,
    // which kills the AppShell key dispatcher until the next click. Restore
    // the shell root — the dispatcher's fallback focus item — on every close
    // path. Duck-walk, because this modal mounts under several parents (the
    // Blacklist manager, FolderModals, MyQBZ delete).
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
    function _cancel() {
        root.close()
        root.cancelled()
    }
    function _confirm() {
        root.close()
        root.confirmed()
    }

    visible: root._open
    enabled: root._open
    z: 3100

    Keys.onEscapePressed: function (event) {
        root._cancel()
        event.accepted = true
    }

    // Scrim. `radius` is load-bearing, not decoration: this Item fills the
    // view root, which fills AppShell's rounded content frame, and Qt Quick's
    // `clip` is a rectangular scissor that does not follow `radius` — so an
    // opaque full-bleed child paints into the frame's four bezel corners
    // (AppShell.qml:252-259, which names DiscoverConfigModal's scrim as the
    // one that had to be fixed exactly this way).
    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root._cancel()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked drop shadow (blur 32 / #00000080) — see the header note.
    Rectangle {
        x: panel.x
        y: panel.y + 8
        width: panel.width
        height: panel.height
        radius: theme.radiusMd
        color: "#80000000"
        opacity: 0.5
    }

    Rectangle {
        id: panel
        width: Math.min(root.width - (root.kioskHost ? 24 : 80), root.kioskHost ? 640 : 400)
        // The Slint panel is `height: panel.preferred-height` over a
        // VerticalLayout with padding 24 -> content + 48.
        height: panelCol.implicitHeight + 48
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        radius: theme.radiusMd
        color: theme.surfaceMain
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow clicks so they never reach the scrim (the Slint's empty
        // panel TouchArea, :1030).
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            id: panelCol
            x: 24
            y: 24
            width: parent.width - 48
            spacing: 16

            Text {
                width: parent.width
                text: root.title
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
                font.weight: theme.weightSemibold
                wrapMode: Text.WordWrap
            }
            Text {
                visible: root.body !== ""
                width: parent.width
                text: root.body
                color: theme.textSecondary
                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                wrapMode: Text.WordWrap
            }
            Row {
                visible: root.checkboxLabel !== ""
                width: parent.width
                spacing: 10
                QbzCheckbox {
                    kioskHost: root.kioskHost
                    checked: root.checkboxChecked
                    Accessible.name: root.checkboxLabel
                    onToggled: root.checkboxChecked = !root.checkboxChecked
                }
                Text {
                    width: parent.width - 54
                    text: root.checkboxLabel
                    color: theme.textSecondary
                    font.pixelSize: theme.fontBody
                    wrapMode: Text.WordWrap
                    MouseArea {
                        anchors.fill: parent
                        onClicked: root.checkboxChecked = !root.checkboxChecked
                    }
                }
            }
            Item {
                width: parent.width
                height: root.kioskHost ? 44 : 36
                Row {
                    anchors.right: parent.right
                    spacing: 10

                    Rectangle {
                        id: cancelButton
                        width: cancelLbl.implicitWidth + 32
                        height: root.kioskHost ? 44 : 36
                        radius: theme.radiusSm
                        color: cancelArea.containsMouse ? theme.surfaceElevatedHover
                                                        : theme.surfaceElevated
                        activeFocusOnTab: root.opened
                        border.width: activeFocus ? 2 : 0
                        border.color: theme.accent
                        Accessible.role: Accessible.Button
                        Accessible.name: root.cancelLabel
                        Accessible.onPressAction: root._cancel()
                        KeyNavigation.tab: confirmButton
                        KeyNavigation.backtab: confirmButton
                        Keys.onPressed: function (event) {
                            if (!event.isAutoRepeat
                                    && (event.key === Qt.Key_Space
                                        || event.key === Qt.Key_Return
                                        || event.key === Qt.Key_Enter)) {
                                root._cancel()
                                event.accepted = true
                            }
                        }
                        Text {
                            id: cancelLbl
                            anchors.centerIn: parent
                            text: root.cancelLabel
                            color: theme.textPrimary
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        }
                        MouseArea {
                            id: cancelArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onPressed: cancelButton.forceActiveFocus()
                            onClicked: root._cancel()
                        }
                    }
                    Rectangle {
                        id: confirmButton
                        width: confirmLbl.implicitWidth + 32
                        height: root.kioskHost ? 44 : 36
                        radius: theme.radiusSm
                        color: root.danger
                            ? (confirmArea.containsMouse ? Qt.darker(theme.danger, 1.12)
                                                         : theme.danger)
                            : (confirmArea.containsMouse ? theme.accentHover : theme.accent)
                        activeFocusOnTab: root.opened
                        border.width: activeFocus ? 2 : 0
                        border.color: root.danger ? "#ffffff" : theme.accentGlyphColor
                        Accessible.role: Accessible.Button
                        Accessible.name: root.confirmLabel
                        Accessible.onPressAction: root._confirm()
                        KeyNavigation.tab: cancelButton
                        KeyNavigation.backtab: cancelButton
                        Keys.onPressed: function (event) {
                            if (!event.isAutoRepeat
                                    && (event.key === Qt.Key_Space
                                        || event.key === Qt.Key_Return
                                        || event.key === Qt.Key_Enter)) {
                                root._confirm()
                                event.accepted = true
                            }
                        }
                        Text {
                            id: confirmLbl
                            anchors.centerIn: parent
                            text: root.confirmLabel
                            // The Slint hard-codes #ffffff on the filled danger
                            // button and that is correct here — `danger` is a
                            // fixed red under every palette. The accent arm goes
                            // through the theme's on-accent selector instead
                            // (QbzTheme.qml:314-317), which is the port's
                            // owner-approved WCAG deviation.
                            color: root.danger ? "#ffffff" : theme.accentGlyphColor
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            font.weight: theme.weightMedium
                        }
                        MouseArea {
                            id: confirmArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onPressed: confirmButton.forceActiveFocus()
                            onClicked: root._confirm()
                        }
                    }
                }
            }
        }
    }
}
