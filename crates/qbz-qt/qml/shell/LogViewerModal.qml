// In-app log viewer — QML port of crates/qbz-ui/ui/shell/LogViewerModal.slint.
//
// THE SHAPE OF THIS MODAL IS THE POINT, and it is not "everything on screen":
//
//   default view   header (title · shown/total · [+] · X)
//                  column header (Time · Level · Target · Message)
//                  the log list
//                  the uploaded-url row, ONLY after an upload
//                  footer: Copy all · Upload (public)        <- two buttons
//
//   [+] reveals    the controls row (level · search · Refresh · Auto-tail)
//                  the advanced actions row (bundle · open file · Clear)
//
// `advancedOpen` starts FALSE. The reference's own comment says why: "the
// default view must serve FAST — the log list plus exactly TWO buttons, Copy
// all and Upload (the whole point of reaching this through Settings > Share
// logs)". The first Qt port drew every control at once and the footer's six
// buttons overflowed the panel; that is what this file exists to not do.
//
// The Rust side owns the ring and pushes an ALREADY filtered + capped slice
// into `QbzShell.logViewerJson` (`total` = ring size, `shown` = survivors), so
// nothing here filters and nothing here holds history.
//
// Mounted in AppShell by declaration order (ADR-009), z >= 3000.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    id: root

    readonly property var doc: {
        try {
            return JSON.parse(QbzShell.logViewerJson)
        } catch (e) {
            return ({})
        }
    }

    /// The [+] / [−] disclosure. Private to the view — the reference keeps it
    /// as a component-private property too, not in the shared state.
    property bool advancedOpen: false

    anchors.fill: parent
    z: 3100
    visible: root.doc.open === true
    enabled: root.visible

    QbzTheme { id: theme }

    readonly property var levelValues: ["all", "error", "warn", "info", "debug", "trace"]

    // Column geometry, shared by the header row and every delegate so they
    // cannot drift: 80 / 48 / 140 / stretch, 12px side padding, 10px gaps.
    readonly property int colTime: 80
    readonly property int colLevel: 48
    readonly property int colTarget: 140
    readonly property int colPad: 12
    readonly property int colGap: 10

    // Reset the disclosure on every open — the reference's `advanced-open` is
    // a fresh component property each time the modal is conditionally
    // mounted, so it can never reopen already expanded.
    onVisibleChanged: {
        if (visible) {
            root.advancedOpen = false
            // A modal's fields open clean (2026-09-14): the search box and the
            // filter it drives both outlived the close.
            if (searchInput.text !== "") {
                searchInput.text = ""
                QbzShell.logSetSearch("")
            }
            keyScope.forceActiveFocus()
        }
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent
        Keys.onEscapePressed: QbzShell.logClose()
    }

    // Dimmed backdrop; a click closes.
    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: QbzShell.logClose()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked 32px drop shadow (#00000080) — the LoginScreen/QbzConfirmModal
    // precedent; a real blurred shadow owes its own parity pass.
    Rectangle {
        anchors.centerIn: panel
        width: panel.width + 8
        height: panel.height + 8
        radius: theme.radiusMd
        color: "#80000000"
    }

    Rectangle {
        id: panel
        anchors.centerIn: parent
        // 760x640 capped, minus an 80px window margin — the reference's exact
        // numbers.
        width: Math.min(root.width - 80, 760)
        height: Math.min(root.height - 80, 640)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow clicks so they never reach the closing backdrop.
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        // ANCHORED, not a Column. The body is the only stretch row and its
        // height depends on which optional rows are mounted (the advanced
        // controls, the advanced actions, the uploaded-url row). Hand-counting
        // that in a Column meant a formula that had to be re-derived every time
        // a row was added — and it was already wrong by one gap in the
        // collapsed state. Anchoring the body BETWEEN its neighbours is exact
        // and stays exact.
        Item {
            id: stack
            anchors.fill: parent
            anchors.margins: 20
            // The single gap value the whole modal is spaced by.
            readonly property int gap: 14

            // ---- Header: title + shown/total + [+] + X --------------------
            Item {
                id: headerRow
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: 28

                Row {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 10
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Logs", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        // "shown / total" — the only way to tell "the filter
                        // matched nothing" from "the ring is empty".
                        text: (root.doc.shown || 0) + " / " + (root.doc.total || 0)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                }

                Row {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 10
                    // The discreet disclosure. A bare glyph, not a button.
                    Item {
                        width: 24
                        height: 24
                        anchors.verticalCenter: parent.verticalCenter
                        Text {
                            anchors.centerIn: parent
                            text: root.advancedOpen ? "−" : "+"
                            color: advArea.containsMouse ? theme.textPrimary : theme.textMuted
                            font.pixelSize: 18
                        }
                        MouseArea {
                            id: advArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.advancedOpen = !root.advancedOpen
                        }
                    }
                    Item {
                        width: 28
                        height: 28
                        anchors.verticalCenter: parent.verticalCenter
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "x"
                            width: 17
                            height: 17
                            tintName: closeArea.containsMouse ? "primary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzShell.logClose()
                        }
                    }
                }
            }

            // ---- Controls (ADVANCED): level / search / refresh / auto-tail -
            Item {
                id: controlsRow
                anchors.top: headerRow.bottom
                anchors.topMargin: stack.gap
                anchors.left: parent.left
                anchors.right: parent.right
                height: 34
                visible: root.advancedOpen

                QbzSelect {
                    id: levelSelect
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    menuWidth: 132
                    options: [
                        QbzSession.tr("All levels", QbzSession.trRev),
                        QbzSession.tr("Error", QbzSession.trRev),
                        QbzSession.tr("Warn", QbzSession.trRev),
                        QbzSession.tr("Info", QbzSession.trRev),
                        QbzSession.tr("Debug", QbzSession.trRev),
                        QbzSession.tr("Trace", QbzSession.trRev)
                    ]
                    currentIndex: Math.max(0, root.levelValues.indexOf(root.doc.filterLevel || "all"))
                    onSelected: function (i) {
                        QbzShell.logSetLevel(root.levelValues[i] || "all")
                    }
                }

                // Search box: a bordered shell with a leading glyph, a focus
                // ring, and its own placeholder (the reference builds it the
                // same way rather than using the shared line edit).
                Rectangle {
                    id: searchBox
                    anchors.left: levelSelect.right
                    anchors.leftMargin: 10
                    anchors.right: refreshBtn.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    height: 34
                    radius: theme.radiusSm
                    border.width: 1
                    border.color: searchInput.activeFocus ? theme.focusRing : theme.borderSubtle
                    color: theme.surfaceElevated

                    QbzIcon {
                        id: searchGlyph
                        anchors.left: parent.left
                        anchors.leftMargin: 11
                        anchors.verticalCenter: parent.verticalCenter
                        name: "search"
                        width: 14
                        height: 14
                        tintName: "muted"
                    }
                    TextInput {
                        QbzTextEditMenu { }
                        id: searchInput
                        anchors.left: searchGlyph.right
                        anchors.leftMargin: 8
                        anchors.right: parent.right
                        anchors.rightMargin: 11
                        anchors.verticalCenter: parent.verticalCenter
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        clip: true
                        selectByMouse: true
                        onTextEdited: QbzShell.logSetSearch(text)
                    }
                    Text {
                        visible: searchInput.text === ""
                        anchors.fill: searchInput
                        verticalAlignment: Text.AlignVCenter
                        text: QbzSession.tr("Search logs…", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontBody
                        elide: Text.ElideRight
                    }
                }

                IconTextButton {
                    id: refreshBtn
                    anchors.right: tailRow.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    label: QbzSession.tr("Refresh", QbzSession.trRev)
                    iconName: "refresh-cw"
                    onClicked: QbzShell.logRefresh()
                }

                Row {
                    id: tailRow
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 8
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Auto-tail", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 13
                    }
                    QbzToggle {
                        anchors.verticalCenter: parent.verticalCenter
                        checked: root.doc.autoTail === true
                        onToggled: function (v) { QbzShell.logSetAutoTail(v) }
                    }
                }
            }

            // ---- Column header (compact, muted) ---------------------------
            Item {
                id: colHeaderRow
                anchors.top: root.advancedOpen ? controlsRow.bottom : headerRow.bottom
                anchors.topMargin: stack.gap
                anchors.left: parent.left
                anchors.right: parent.right
                height: 14
                Row {
                    anchors.left: parent.left
                    anchors.leftMargin: root.colPad
                    anchors.right: parent.right
                    anchors.rightMargin: root.colPad
                    spacing: root.colGap
                    Text {
                        width: root.colTime
                        text: QbzSession.tr("Time", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                    Text {
                        width: root.colLevel
                        text: QbzSession.tr("Level", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                    Text {
                        width: root.colTarget
                        text: QbzSession.tr("Target", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                    Text {
                        text: QbzSession.tr("Message", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                }
            }

            // ---- Body -----------------------------------------------------
            // The stretch row: everything else in this Column is fixed-height,
            // so the list takes what is left. Computed rather than anchored
            // because a Column child cannot anchor to its siblings' bounds.
            Rectangle {
                id: bodyBox
                anchors.top: colHeaderRow.bottom
                anchors.topMargin: stack.gap
                anchors.left: parent.left
                anchors.right: parent.right
                // Stops at whichever optional row is actually mounted below it.
                anchors.bottom: uploadedRow.visible ? uploadedRow.top
                    : (root.advancedOpen ? advActionsRow.top : footerRow.top)
                anchors.bottomMargin: stack.gap
                radius: theme.radiusSm
                // surface-MAIN, a step darker than the card it sits on — the
                // list must read as a well, not as more card.
                color: theme.surfaceMain
                border.width: 1
                border.color: theme.borderSubtle
                clip: true

                ListView {
                    id: list
                    anchors.fill: parent
                    anchors.margins: 1
                    model: root.doc.rows || []
                    boundsBehavior: Flickable.StopAtBounds
                    onCountChanged: if (root.doc.autoTail === true) positionViewAtEnd()

                    delegate: Item {
                        width: list.width
                        // The delegate tracks the WRAPPED message so long lines
                        // are not clipped (the reference reads the child's
                        // preferred height — parent-reads-child is the
                        // non-looping direction).
                        height: Math.max(msg.implicitHeight, 14) + 6

                        Row {
                            anchors.left: parent.left
                            anchors.leftMargin: root.colPad
                            anchors.right: parent.right
                            anchors.rightMargin: root.colPad
                            anchors.top: parent.top
                            anchors.topMargin: 3
                            spacing: root.colGap

                            Text {
                                width: root.colTime
                                text: modelData.ts
                                color: theme.textMuted
                                font.pixelSize: 11
                                elide: Text.ElideRight
                            }
                            Text {
                                width: root.colLevel
                                text: modelData.level
                                font.pixelSize: 11
                                font.weight: theme.weightSemibold
                                // Fixed literals, 1:1 with the reference —
                                // NOT theme tokens there either.
                                color: modelData.level === "ERROR" ? "#f44336"
                                    : modelData.level === "WARN" ? "#e0a030"
                                    : (modelData.level === "DEBUG" || modelData.level === "TRACE")
                                        ? theme.textMuted
                                        : theme.textSecondary
                            }
                            Text {
                                width: root.colTarget
                                text: modelData.target
                                color: theme.textMuted
                                font.pixelSize: 11
                                elide: Text.ElideRight
                            }
                            Text {
                                id: msg
                                width: parent.width - root.colTime - root.colLevel
                                       - root.colTarget - root.colGap * 3
                                text: modelData.message
                                // The MESSAGE is always text-primary. Only the
                                // LEVEL column carries the level's colour —
                                // colouring the message too turns the body
                                // into confetti and is not what the reference
                                // does.
                                color: theme.textPrimary
                                font.pixelSize: 11
                                wrapMode: Text.WordWrap
                            }
                        }
                    }
                }

                // Empty state. ONE string — the reference does not distinguish
                // "ring empty" from "filter matched nothing"; the shown/total
                // counter in the header already says which it is.
                Text {
                    anchors.centerIn: parent
                    visible: (root.doc.rows || []).length === 0
                    text: QbzSession.tr("No log entries.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                }
            }

            // ---- Uploaded URL — after an upload, in BOTH views -------------
            // Explicitly NOT gated on `advancedOpen`: Upload lives in the
            // always-visible footer, so its result has to be reachable there.
            Rectangle {
                id: uploadedRow
                anchors.bottom: root.advancedOpen ? advActionsRow.top : footerRow.top
                anchors.bottomMargin: stack.gap
                anchors.left: parent.left
                anchors.right: parent.right
                height: 32
                visible: (root.doc.uploadedUrl || "") !== ""
                radius: theme.radiusSm
                border.width: 1
                border.color: theme.borderSubtle
                color: theme.surfaceElevated

                QbzIcon {
                    id: linkGlyph
                    anchors.left: parent.left
                    anchors.leftMargin: 11
                    anchors.verticalCenter: parent.verticalCenter
                    name: "link"
                    width: 14
                    height: 14
                    tintName: "accent"
                }
                TextInput {
                    QbzTextEditMenu { }
                    anchors.left: linkGlyph.right
                    anchors.leftMargin: 8
                    anchors.right: urlCopy.left
                    anchors.rightMargin: 4
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.doc.uploadedUrl || ""
                    // Read-only but selectable — the point is to grab the url.
                    readOnly: true
                    selectByMouse: true
                    color: theme.textSecondary
                    font.pixelSize: 12
                    clip: true
                }
                Item {
                    id: urlCopy
                    anchors.right: parent.right
                    anchors.rightMargin: 4
                    anchors.verticalCenter: parent.verticalCenter
                    width: 26
                    height: 26
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "copy"
                        width: 14
                        height: 14
                        tintName: urlCopyArea.containsMouse ? "primary" : "muted"
                    }
                    MouseArea {
                        id: urlCopyArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzShell.logCopyUrl()
                    }
                }
            }

            // ---- Advanced actions: bundle · (gap) · open file · Clear ------
            Item {
                id: advActionsRow
                anchors.bottom: footerRow.top
                anchors.bottomMargin: stack.gap
                anchors.left: parent.left
                anchors.right: parent.right
                height: 32
                visible: root.advancedOpen

                IconTextButton {
                    anchors.left: parent.left
                    label: QbzSession.tr("Copy diagnostics bundle", QbzSession.trRev)
                    iconName: "copy"
                    onClicked: QbzShell.logCopyBundle()
                }
                Row {
                    anchors.right: parent.right
                    spacing: 10
                    IconTextButton {
                        label: QbzSession.tr("Open log file", QbzSession.trRev)
                        iconName: "external-link"
                        onClicked: QbzShell.logOpenFile()
                    }
                    IconTextButton {
                        label: QbzSession.tr("Clear", QbzSession.trRev)
                        iconName: "trash-2"
                        danger: true
                        onClicked: QbzShell.logClear()
                    }
                }
            }

            // ---- Footer, ALWAYS visible: the two actions this modal is for -
            Item {
                id: footerRow
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 32
                Row {
                    anchors.left: parent.left
                    spacing: 10
                    IconTextButton {
                        // Flips to a check + "Copied" for a beat after the
                        // copy — the reference's `copied` flag, not a toast.
                        label: root.doc.copied === true
                            ? QbzSession.tr("Copied", QbzSession.trRev)
                            : QbzSession.tr("Copy all", QbzSession.trRev)
                        iconName: root.doc.copied === true ? "check" : "copy"
                        onClicked: QbzShell.logCopyAll()
                    }
                    IconTextButton {
                        label: root.doc.uploading === true
                            ? QbzSession.tr("Uploading…", QbzSession.trRev)
                            : QbzSession.tr("Upload (public)", QbzSession.trRev)
                        iconName: "cloud-upload"
                        btnEnabled: root.doc.uploading !== true
                        onClicked: QbzShell.logUpload()
                    }
                }
            }
        }
    }
}
