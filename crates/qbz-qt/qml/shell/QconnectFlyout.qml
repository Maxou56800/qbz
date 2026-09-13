// Qobuz Connect device flyout — the QML port of the PopupWindow both Slint
// bars mount (`shell/PlayerBar.slint:412-642` + `shell/PlayerBarSmall.slint:
// 374-596`). ONE shared component is the contract §8 sanctioned collapse of
// the two Slint flyouts (their only delta was 1px of popup y); the bar-side
// asymmetry that survives is the BUTTON's tooltip, which lives in the bars,
// not here.
//
// STRUCTURE + BEHAVIOR are 1:1 with the Slint; geometry follows this port's
// own popup conventions (controls/QbzContextMenu.qml): a Popup parented to
// the Overlay, opened above the bottom-bar trigger with right edges aligned
// and clamped into the window with an 8px margin. The original 280x244 panel
// grows vertically only
// for the persisted playback-conflict policy added to this live Qt surface;
// its 14px padding and 12px block spacing are kept.
//
// STATE: all session data is push-driven off the QbzQConnect singleton
// (src/qconnect_bridge.rs) — `devicesJson` is parsed here exactly like
// CastPicker.qml parses QbzCast.devicesJson, guarded so a malformed/absent
// document degrades to the empty state instead of throwing. The 8s
// "Looking for devices…" discovery window is a QML-side flag + Timer
// (`qcConnecting`), 1:1 with the Slint `qc-connecting` UI timer — there is
// deliberately NO Rust busy flag (contract §8 state-mapping table).
//
// List states, mutually exclusive (Slint :504-601):
//   discovering  qcConnecting && devices empty -> spinner + caption
//   empty       !qcConnecting && devices empty -> "No devices found" + hint
//   devices      devices non-empty             -> 34px rows, tap to set active

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Popup {
    id: root

    parent: Overlay.overlay
    width: 280
    height: 310
    padding: 14
    closePolicy: Popup.CloseOnPressOutside | Popup.CloseOnEscape

    QbzTheme { id: theme }

    // True while a Connect click is discovering renderers (PlayerBar.slint:
    // 164). The spinner is gated on this + the 8s timer below; UI-only.
    property bool qcConnecting: false

    // The parsed device document (QbzQConnect.devicesJson). Rows:
    // { renderer_id, name, is_local, is_active, icon } — the exact shape the
    // bridge header documents — plus, for a renderer seen on this network
    // that the session does not list yet, `lan: true` and `lan_uuid`.
    readonly property var devices: {
        try {
            var d = JSON.parse(QbzQConnect.devicesJson || "[]")
            return Array.isArray(d) ? d : []
        } catch (e) {
            return []
        }
    }
    readonly property var sessionDevices: root.devices.filter(function (d) { return d.lan !== true })
    readonly property var lanDevices: root.devices.filter(function (d) { return d.lan === true })

    readonly property var conflictPolicyOptions: [
        QbzSession.tr("Ask every time", QbzSession.trRev),
        QbzSession.tr("Continue on the active renderer", QbzSession.trRev),
        QbzSession.tr("Continue here with the Qobuz Connect queue", QbzSession.trRev),
        QbzSession.tr("Continue local playback and replace the Qobuz Connect queue", QbzSession.trRev)
    ]

    // Discovery timer — bounds the spinner so the "no devices" empty state
    // shows if none appear within ~8s (Slint :486-493). Started on a Connect
    // click; stopped on disconnect. Single-shot, so triggering self-stops.
    Timer {
        id: discoverTimer
        interval: 8000
        onTriggered: root.qcConnecting = false
    }

    // Bottom-bar placement: explicitly above the trigger, right edges
    // aligned, then clamped 8px into the window. Computing the upward anchor
    // before clamping prevents the panel from covering its own button.
    function _place(gx, gy, win) {
        if (win) {
            x = Math.max(8, Math.min(gx, win.width - width - 8))
            y = Math.max(8, Math.min(gy, win.height - height - 8))
        } else {
            x = gx
            y = gy
        }
        open()
    }

    function openAboveRight(sourceItem) {
        var g = sourceItem.mapToItem(null, sourceItem.width - width, -height - 4)
        _place(g.x, g.y, sourceItem.Window.window)
    }

    // Top-bar placement (the kiosk back bar): below the trigger, right edges
    // aligned, then the same 8px window clamp.
    function openBelowRight(sourceItem) {
        var g = sourceItem.mapToItem(null, sourceItem.width - width, sourceItem.height + 4)
        _place(g.x, g.y, sourceItem.Window.window)
    }

    // The Slint chrome: surface-main, Radius.sm, 1px border-muted.
    background: Rectangle {
        color: theme.surfaceMain
        radius: theme.radiusSm
        border.width: 1
        border.color: theme.borderMuted
    }

    contentItem: Item {
        Column {
            id: topCol
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            spacing: 12

            // Header.
            Row {
                width: parent.width
                height: 16
                spacing: 8
                QbzIcon {
                    name: "monitor-speaker"
                    width: 16
                    height: 16
                    tintName: "textPrimary"
                }
                Text {
                    text: QbzSession.tr("Qobuz Connect", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: 14
                    font.weight: theme.weightSemibold
                    anchors.verticalCenter: parent.verticalCenter
                }
            }

            // Status divider.
            Rectangle {
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            // Status row.
            Row {
                width: parent.width
                height: 12
                spacing: 8
                Rectangle {
                    width: 8
                    height: 8
                    radius: 4
                    anchors.verticalCenter: parent.verticalCenter
                    color: QbzQConnect.qconnectConnected ? "#22c55e" : theme.textMuted
                }
                Text {
                    text: QbzQConnect.qconnectConnected
                        ? QbzSession.tr("Connected", QbzSession.trRev)
                        : QbzSession.tr("Disconnected", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: 12
                    anchors.verticalCenter: parent.verticalCenter
                }
            }

            // "Playing on" line — only while a peer renderer owns playback
            // (controller mode). Invisible children are skipped by the Column.
            Text {
                visible: QbzPlayer.npIsRemote && QbzPlayer.npCastTarget !== ""
                width: parent.width
                text: QbzSession.tr("Playing on: {}", QbzSession.trRev)
                    .replace("{}", QbzPlayer.npCastTarget)
                color: theme.accent
                font.pixelSize: 11
                elide: Text.ElideRight
            }
        }

        // Connect / Disconnect toggle (declared before the list box so the
        // box can anchor against it; the list takes the stretch between).
        Rectangle {
            id: connectBtn
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: 34
            radius: theme.radiusSm
            color: connectArea.pressed ? theme.accentPressed
                : connectArea.containsMouse ? theme.accentHover : theme.accent
            Text {
                anchors.centerIn: parent
                text: QbzQConnect.qconnectConnected
                    ? QbzSession.tr("Disconnect", QbzSession.trRev)
                    : QbzSession.tr("Connect", QbzSession.trRev)
                // On an accent fill: the theme's on-accent selector, the same
                // correction CastPicker's tabs document (Slint's raw
                // accent-text fails contrast on light themes).
                color: theme.accentGlyphColor
                font.pixelSize: 13
                font.weight: theme.weightSemibold
            }
            MouseArea {
                id: connectArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    if (QbzQConnect.qconnectConnected) {
                        // Connected -> this click DISCONNECTS -> close the
                        // flyout (Slint :612-618).
                        QbzQConnect.connectToggle()
                        root.qcConnecting = false
                        discoverTimer.stop()
                        root.close()
                    } else {
                        // Disconnected -> CONNECT -> keep the flyout open and
                        // spin while renderers are discovered (Slint :619-626).
                        QbzQConnect.connectToggle()
                        root.qcConnecting = true
                        discoverTimer.restart()
                    }
                }
            }
        }

        Column {
            id: conflictPolicy
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: connectBtn.top
            anchors.bottomMargin: 12
            spacing: 5

            Text {
                width: parent.width
                text: QbzSession.tr("When playback conflicts", QbzSession.trRev)
                color: theme.textSecondary
                font.pixelSize: 11
                font.weight: theme.weightMedium
                elide: Text.ElideRight
            }

            QbzSelect {
                width: parent.width
                menuWidth: parent.width
                popupWidth: 420
                sm: true
                popupPlacement: "left"
                options: root.conflictPolicyOptions
                currentIndex: QbzQConnect.playbackConflictPolicyIndex
                onSelected: function (i) {
                    QbzBridge.settingsSelect("qconnect-conflict-policy", i)
                }
            }
        }

        // Device list / empty state — the Slint's vertical-stretch block
        // (:495-602): surface-elevated, Radius.sm, clipped.
        Rectangle {
            id: listBox
            anchors.top: topCol.bottom
            anchors.topMargin: 12
            anchors.bottom: conflictPolicy.top
            anchors.bottomMargin: 12
            anchors.left: parent.left
            anchors.right: parent.right
            radius: theme.radiusSm
            color: theme.surfaceElevated
            clip: true

            // Discovering — Connect was clicked and no renderers have arrived
            // yet. Hides once devices appear or the timer expires.
            Column {
                anchors.centerIn: parent
                spacing: 6
                visible: root.qcConnecting && root.devices.length === 0
                QbzSpinner {
                    size: 22
                    anchors.horizontalCenter: parent.horizontalCenter
                }
                Text {
                    text: QbzSession.tr("Looking for devices…", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: 12
                }
            }

            // Empty state (no session renderers yet).
            Column {
                anchors.centerIn: parent
                width: parent.width - 24
                spacing: 4
                visible: !root.qcConnecting && root.devices.length === 0
                Text {
                    width: parent.width
                    horizontalAlignment: Text.AlignHCenter
                    text: QbzSession.tr("No devices found", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: 12
                }
                Text {
                    width: parent.width
                    horizontalAlignment: Text.AlignHCenter
                    text: QbzSession.tr("Make sure your renderer is on the same network", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 10
                    wrapMode: Text.WordWrap
                }
            }

            // Device picker — plain rows (ADR-008: no pills). Tap a row to
            // make it the active renderer; the local row is "Play here".
            Flickable {
                id: listFlick
                anchors.fill: parent
                anchors.margins: 4
                visible: root.devices.length > 0
                contentWidth: width
                contentHeight: devRows.height
                boundsBehavior: Flickable.StopAtBounds
                clip: true

                Column {
                    id: devRows
                    width: listFlick.width
                    spacing: 2

                    Repeater {
                        model: root.sessionDevices

                        delegate: Rectangle {
                            id: deviceRow
                            required property var modelData
                            // The Slint lights `d.is-active` from the row
                            // (PlayerBar.slint:576,581,583,595); the bridge's
                            // activeRendererId is the fallback when a row
                            // lacks the flag.
                            readonly property bool rowActive: modelData.is_active === true
                                || (modelData.is_active === undefined
                                    && modelData.renderer_id === QbzQConnect.activeRendererId)

                            width: devRows.width
                            height: 34
                            radius: theme.radiusSm
                            color: rowArea.containsMouse ? theme.surfaceHover : "transparent"

                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 8
                                QbzIcon {
                                    // Device-type glyph, mirroring Qobuz
                                    // Connect. The sink's keys are mobile /
                                    // web / computer / speaker (its own
                                    // default arm is "speaker"), so the
                                    // fallback here is speaker too.
                                    name: deviceRow.modelData.icon === "mobile" ? "smartphone"
                                        : deviceRow.modelData.icon === "web" ? "globe"
                                        : deviceRow.modelData.icon === "computer" ? "monitor"
                                        : "speaker"
                                    width: 14
                                    height: 14
                                    anchors.verticalCenter: parent.verticalCenter
                                    tintName: deviceRow.rowActive ? "accent" : "secondary"
                                }
                                Text {
                                    width: parent.width - 14 - 8 - 16
                                    text: deviceRow.modelData.is_local === true
                                        ? QbzSession.tr("Play here", QbzSession.trRev)
                                        : (deviceRow.modelData.name || "")
                                    color: deviceRow.rowActive ? theme.textPrimary : theme.textSecondary
                                    font.pixelSize: 12
                                    font.weight: deviceRow.rowActive ? theme.weightSemibold : Font.Normal
                                    elide: Text.ElideRight
                                    anchors.verticalCenter: parent.verticalCenter
                                }
                                // Active indicator dot (no pill).
                                Rectangle {
                                    width: 8
                                    height: 8
                                    radius: 4
                                    anchors.verticalCenter: parent.verticalCenter
                                    color: deviceRow.rowActive ? theme.accent : "transparent"
                                }
                            }
                            MouseArea {
                                id: rowArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    QbzQConnect.setActive(deviceRow.modelData.renderer_id)
                                    root.close()
                                }
                            }
                        }
                    }

                    // Renderers on this network that the session does not
                    // list yet (the controller's LAN half, 2026-09-13). The
                    // official apps show these the same way; picking one
                    // pairs it — delegated credentials over the LAN — and the
                    // cloud then lists it above like any other renderer.
                    Text {
                        visible: root.lanDevices.length > 0
                        width: devRows.width
                        leftPadding: 8
                        topPadding: 8
                        bottomPadding: 2
                        text: QbzSession.tr("On this network", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 11
                        font.weight: theme.weightSemibold
                    }
                    Repeater {
                        model: root.lanDevices

                        delegate: Rectangle {
                            id: lanRow
                            required property var modelData

                            width: devRows.width
                            height: 34
                            radius: theme.radiusSm
                            color: lanArea.containsMouse ? theme.surfaceHover : "transparent"

                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 8
                                QbzIcon {
                                    name: lanRow.modelData.icon === "mobile" ? "smartphone"
                                        : lanRow.modelData.icon === "computer" ? "monitor"
                                        : "speaker"
                                    width: 14
                                    height: 14
                                    anchors.verticalCenter: parent.verticalCenter
                                    tintName: "secondary"
                                }
                                Text {
                                    width: parent.width - 14 - 8 - pairLabel.implicitWidth - 8
                                    text: lanRow.modelData.name || ""
                                    color: theme.textSecondary
                                    font.pixelSize: 12
                                    elide: Text.ElideRight
                                    anchors.verticalCenter: parent.verticalCenter
                                }
                                Text {
                                    id: pairLabel
                                    text: QbzSession.tr("Pair", QbzSession.trRev)
                                    color: theme.accent
                                    font.pixelSize: 11
                                    font.weight: theme.weightSemibold
                                    anchors.verticalCenter: parent.verticalCenter
                                }
                            }
                            MouseArea {
                                id: lanArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    QbzQConnect.pairLan(lanRow.modelData.lan_uuid)
                                    root.close()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
