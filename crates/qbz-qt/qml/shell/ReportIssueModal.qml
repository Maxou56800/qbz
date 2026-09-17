// Report an issue — the QML port of crates/qbz-ui/ui/shell/ReportIssueModal.slint,
// opened from the header hamburger menu.
//
// Structure, scrim, faked shadow and the self-gated mount follow
// LogViewerModal.qml; `z: 3000` explicitly, ADR-009 as this port spells it.
// Colour literals are `Qt.rgba(...)` floats: Slint writes #RRGGBBAA and Qt
// reads #AARRGGBB, and five modals in this tree shipped an invisible scrim or
// shadow from copying one verbatim (fixed 2026-08-14).
//
// The open state is LOCAL, not a bridge document: nothing in Rust needs to know
// this modal is up, and the two buttons call verbs that already exist
// (QbzShell.logOpen, QbzShell.reportIssueOpen). The header menu sets `open`.
//
// No pills (ADR-008): both buttons are bordered radiusSm rectangles.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    property bool open: false

    anchors.fill: parent
    z: 3000
    visible: root.open
    enabled: root.visible

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    FocusScope {
        anchors.fill: parent
        focus: root.visible
        Keys.onEscapePressed: root.open = false
    }

    // Dimmed backdrop; a click closes.
    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.75)
        MouseArea {
            anchors.fill: parent
            onClicked: root.open = false
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked 32px drop shadow — the LogViewerModal/QbzConfirmModal precedent; a
    // real blurred shadow owes its own parity pass.
    Rectangle {
        anchors.centerIn: panel
        width: panel.width + 8
        height: panel.height + 8
        radius: theme.radiusMd
        color: Qt.rgba(0, 0, 0, 0.5)
    }

    Rectangle {
        id: panel
        anchors.centerIn: parent
        width: Math.min(root.width - 80, 520)
        height: Math.min(root.height - 80, 300)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow clicks so the scrim below does not close the modal.
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            anchors.fill: parent
            anchors.margins: 20
            spacing: 14

            // ---- Header: title + close X.
            Item {
                width: parent.width
                height: 28
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.t("Report an issue")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                }
                Rectangle {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    radius: theme.radiusSm
                    color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        anchors.centerIn: parent
                        width: 17
                        height: 17
                        name: "x"
                        tintName: closeArea.containsMouse ? "primary" : "muted"
                    }
                    MouseArea {
                        id: closeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.open = false
                    }
                }
            }

            // ---- Intro: the manual, redacted log-sharing explanation.
            Text {
                width: parent.width
                height: parent.height - 28 - 36 - 28
                text: root.t("To make bug and issue resolution easier, it's recommended to share the application logs. We have been careful to mask the logs we send so no sensitive data is included. The process is manual, never automatic, so you can verify what is being shared.")
                color: theme.textSecondary
                font.pixelSize: theme.fontBody
                wrapMode: Text.WordWrap
            }

            // ---- Actions: Go to logs (secondary) / Create issue (primary).
            Row {
                anchors.right: parent.right
                spacing: 10

                Rectangle {
                    width: goLabel.implicitWidth + 28
                    height: 36
                    radius: theme.radiusSm
                    border.width: 1
                    border.color: theme.borderSubtle
                    color: goArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                    Text {
                        id: goLabel
                        anchors.centerIn: parent
                        text: root.t("Go to logs")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLink
                        font.weight: theme.weightMedium
                    }
                    MouseArea {
                        id: goArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.open = false
                            QbzShell.logOpen()
                        }
                    }
                }

                Rectangle {
                    width: createLabel.implicitWidth + 28
                    height: 36
                    radius: theme.radiusSm
                    color: createArea.containsMouse ? theme.accentHover : theme.accent
                    Text {
                        id: createLabel
                        anchors.centerIn: parent
                        text: root.t("Create issue report")
                        color: theme.accentText
                        font.pixelSize: theme.fontLink
                        font.weight: theme.weightMedium
                    }
                    MouseArea {
                        id: createArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.open = false
                            QbzShell.reportIssueOpen()
                        }
                    }
                }
            }
        }
    }
}
