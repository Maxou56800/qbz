// HiFi Wizard (DAC setup) — QML port of
// crates/qbz-ui/ui/primitives/DacWizardModal.slint.
//
// Six steps: Welcome · Check · DACs · Configure · Test · Done. The wizard is
// READ-ONLY — it auto-detects DACs, shows the exact commands and config to
// copy, and plays four test tracks so the user can watch their DAC switch
// rates. It NEVER writes a system file.
//
// MODAL RECIPE, from the reference (:8-9): an overlay Item, NOT a window grab,
// so playback keeps running underneath while the test step plays. The scrim,
// the header X and the footer Close all just hide it; Rust's `close()` stops
// nothing.
//
// MOUNTED AT THE SETTINGS VIEW ROOT, beside SettingsConfirmHost — the panels
// are Columns inside a Flickable, so a modal mounted in one would be sized by
// the scrolled content and ride the scroll. ADR-009: z >= 3000.
//
// ── WHAT THIS FILE OWNS AND WHAT RUST OWNS ────────────────────────────────
// Rust (dac_wizard_qt.rs) owns everything it COMPUTES — the probe verdict, the
// candidate list AND its checkboxes, the manual node name's validity, the
// generated configs and their accordion state, the read-back labels — plus
// `open`, because the Settings row's click must reset and probe atomically.
//
// This file owns the pure NAVIGATION state: `step`, the welcome checkbox, the
// three review progress checkboxes and the manual-entry disclosure. That is
// the reference's own split — DacWizardModal.slint mutates those inline and
// never calls Rust for them (:817 `step -= 1`, :853 `step += 1`). They reset
// on `openSeq`, which Rust bumps on every open.
//
// The one field that is deliberately BOTH: the manual node.name. Rust stores
// it (checked_dacs reads it) but this file keeps the TextInput's own text and
// never writes the document back into it — republishing into a focused input
// is how a cursor jumps to the end mid-word.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"
import "../views/local"

Item {
    property bool kioskHost: false

    id: root

    // An unopened overlay is an invisible, non-interactive Item and costs
    // nothing. `anchors.fill` + `enabled` live HERE, not at the mount site —
    // the sibling LibFolderEditModal.qml sets the same three, so a mount is
    // one line.
    anchors.fill: parent
    z: 3000
    visible: doc.open === true
    enabled: root.visible

    readonly property var doc: JSON.parse(QbzDacWizard.wizardJson)
    readonly property var candidates: doc.candidates || []
    readonly property var remediations: doc.remediations || []
    readonly property var dacConfigs: doc.dacConfigs || []
    readonly property var createdPaths: doc.createdPaths || []
    readonly property var testTrackLines: doc.testTracks || []

    // ---- QML-owned navigation state (see the header) ----------------------
    // 0 welcome · 1 check · 2 select-dacs · 3 review · 4 test · 5 done
    property int step: 0
    property bool welcomeConfirmed: false
    property bool reviewBackupDone: false
    property bool reviewConfigDone: false
    property bool reviewRestartDone: false
    // "Can't see your DAC?" is collapsed by default when enumeration found
    // devices; forced open when it found none.
    property bool showManual: false

    readonly property int openSeq: doc.openSeq || 0
    onVisibleChanged: if (visible) {
        Qt.callLater(function () { closeX.forceActiveFocus() })
    }
    onOpenSeqChanged: {
        root.step = 0
        root.welcomeConfirmed = false
        root.reviewBackupDone = false
        root.reviewConfigDone = false
        root.reviewRestartDone = false
        root.showManual = false
        manualInput.text = ""
        if (root.visible)
            Qt.callLater(function () { closeX.forceActiveFocus() })
    }

    Keys.onEscapePressed: function (event) {
        QbzDacWizard.close()
        event.accepted = true
    }

    // Step labels for the header rail (index = step).
    readonly property var stepLabels: [
        QbzSession.tr("Welcome", QbzSession.trRev),
        QbzSession.tr("Check", QbzSession.trRev),
        QbzSession.tr("DACs", QbzSession.trRev),
        QbzSession.tr("Configure", QbzSession.trRev),
        QbzSession.tr("Test", QbzSession.trRev),
        QbzSession.tr("Done", QbzSession.trRev)
    ]

    // The footer primary's gate, per step (reference :832-840):
    //   0 → the accept checkbox
    //   2 → at least one DAC checked, or a valid manual node.name
    //   3 → all three review checkboxes
    //   else → always enabled
    readonly property bool primaryEnabled: {
        if (root.step === 0)
            return root.welcomeConfirmed
        if (root.step === 2)
            return (root.doc.anyDacSelected === true) || (root.doc.manualValid === true)
        if (root.step === 3)
            return root.reviewBackupDone && root.reviewConfigDone && root.reviewRestartDone
        return true
    }

    QbzTheme { id: theme }

    // A whole-row-clickable checkbox (HTML <label> behaviour — clicking the
    // text toggles it, not just the box). Reference: the modal's own CheckRow.
    component CheckRow: Item {
        id: checkRow
        property bool checked: false
        property string label: ""
        signal toggled()

        width: parent ? parent.width : 0
        height: root.kioskHost ? 44 : 28
        activeFocusOnTab: visible && enabled
        Accessible.role: Accessible.CheckBox
        Accessible.name: label
        Accessible.checked: checked
        Accessible.onToggleAction: toggled()
        Keys.onPressed: function (event) {
            if (!event.isAutoRepeat
                    && (event.key === Qt.Key_Space
                        || event.key === Qt.Key_Return
                        || event.key === Qt.Key_Enter)) {
                checkRow.toggled()
                event.accepted = true
            }
        }
        Rectangle {
            anchors.fill: parent
            radius: theme.radiusSm
            color: "transparent"
            border.width: checkRow.activeFocus ? 2 : 0
            border.color: theme.accent
        }

        QbzCheckbox { kioskHost: root.kioskHost;
            id: box
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            checked: checkRow.checked
            enabled: false
            // DISPLAY-ONLY. The row MouseArea below is declared last, so it
            // sits on top and owns every click — exactly as the reference
            // shadows QbzCheckbox's internal toggle. No `onToggled` here on
            // purpose: if the box's own MouseArea ever did win an event, two
            // handlers would fire and the flip would cancel itself out,
            // leaving a checkbox that looks dead.
        }
        Text {
            anchors.left: box.right
            anchors.leftMargin: 10
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            text: checkRow.label
            color: theme.textSecondary
            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
            elide: Text.ElideRight
        }
        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onPressed: checkRow.forceActiveFocus()
            onClicked: checkRow.toggled()
        }
    }

    // A step body heading.
    component StepTitle: Text {
        width: parent ? parent.width : 0
        color: theme.textPrimary
        font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
        font.weight: theme.weightSemibold
        wrapMode: Text.WordWrap
    }

    // ------------------------------- scrim --------------------------------
    // Click outside closes (reference :84-89).
    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: QbzDacWizard.close()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 620)
        // 210 = header 68 + 2 hairlines + stepper 46 + footer 70/24 padding;
        // the body scrolls inside the Flickable past the 90 % clamp.
        height: Math.min(Math.max(500, bodyCol.implicitHeight + 210), parent.height * 0.9)
        radius: theme.radiusLg
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        // Swallow clicks so they never reach the scrim.
        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        readonly property int contentWidth: card.width - 48

        // ------------------------------ header ----------------------------
        Item {
            id: header
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            height: 68

            QbzIcon {
                id: wandIcon
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                width: 26
                height: 26
                name: "gandalf"
                // The reference tints this a literal #3fae6a. `success` has no
                // qrc bake directory, so a degraded tint set would render
                // NOTHING; `accent` is the port's established stand-in for the
                // success tone on GLYPHS (WarningBanner.qml:26-28). Every
                // success-toned TEXT in this file uses theme.success, which is
                // #3fae6a verbatim.
                tintName: "accent"
            }
            Text {
                anchors.left: wandIcon.right
                anchors.leftMargin: 10
                anchors.right: closeX.left
                anchors.rightMargin: 10
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("HiFi Wizard — DAC Setup", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? (theme.fontSection) * 1.2 : (theme.fontSection)
                font.weight: theme.weightSemibold
                elide: Text.ElideRight
            }
            Item {
                id: closeX
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                width: root.kioskHost ? 44 : 28
                height: root.kioskHost ? 44 : 28
                activeFocusOnTab: root.visible
                Accessible.role: Accessible.Button
                Accessible.name: QbzSession.tr("Close", QbzSession.trRev)
                Accessible.onPressAction: QbzDacWizard.close()
                Keys.onPressed: function (event) {
                    if (!event.isAutoRepeat
                            && (event.key === Qt.Key_Space
                                || event.key === Qt.Key_Return
                                || event.key === Qt.Key_Enter)) {
                        QbzDacWizard.close()
                        event.accepted = true
                    }
                }
                Rectangle {
                    anchors.fill: parent
                    radius: theme.radiusSm
                    color: "transparent"
                    border.width: closeX.activeFocus ? 2 : 0
                    border.color: theme.accent
                }
                QbzIcon {
                    anchors.centerIn: parent
                    width: 17
                    height: 17
                    name: "x"
                    tintName: closeTa.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: closeTa
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onPressed: closeX.forceActiveFocus()
                    onClicked: QbzDacWizard.close()
                }
            }
        }
        Rectangle {
            id: headerDiv
            anchors.top: header.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: theme.borderSubtle
        }

        // ---------------------------- stepper rail ------------------------
        // Dots + labels, coloured by progress (reference :153-182).
        Row {
            id: stepper
            anchors.top: headerDiv.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: 24
            anchors.rightMargin: 24
            anchors.topMargin: 14
            height: 18
            spacing: 6

            Repeater {
                model: root.stepLabels
                delegate: Row {
                    required property int index
                    required property string modelData
                    width: (stepper.width - stepper.spacing * (root.stepLabels.length - 1))
                        / root.stepLabels.length
                    spacing: 6

                    Rectangle {
                        anchors.verticalCenter: parent.verticalCenter
                        width: 10
                        height: 10
                        radius: 5
                        color: index <= root.step ? theme.success : theme.borderMuted
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        width: parent.width - 16
                        text: modelData
                        color: index === root.step ? theme.textPrimary
                            : (index < root.step ? theme.textSecondary : theme.textMuted)
                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                        font.weight: index === root.step ? theme.weightSemibold : theme.weightRegular
                        elide: Text.ElideRight
                    }
                }
            }
        }
        Rectangle {
            id: stepperDiv
            anchors.top: stepper.bottom
            anchors.topMargin: 14
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: theme.borderSubtle
        }

        // ------------------------------- body -----------------------------
        Flickable {
            id: bodyFlick
            anchors.top: stepperDiv.bottom
            anchors.bottom: footerDiv.top
            anchors.left: parent.left
            anchors.right: parent.right
            clip: true
            contentWidth: width
            contentHeight: bodyCol.implicitHeight
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: bodyCol
                width: card.width
                padding: 24
                spacing: 16

                // ═══════════════════ Step 0: welcome ═══════════════════════
                Column {
                    visible: root.step === 0
                    width: card.contentWidth
                    spacing: 14

                    StepTitle { text: QbzSession.tr("Guided DAC Setup", QbzSession.trRev) }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("This wizard helps you configure your DAC for bit-perfect playback on Linux. QBZ never changes your system automatically — it auto-detects your DACs, shows you the exact commands and config to copy, and lets you test playback right here.", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        wrapMode: Text.WordWrap
                    }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("You are responsible for your own backups. This is a helper, not a system recovery tool.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                        wrapMode: Text.WordWrap
                    }
                    WarningBanner {
                        variant: "info"
                        title: QbzSession.tr("What to test with", QbzSession.trRev)
                        body: QbzSession.tr("The built-in test plays four tracks spanning different bit-depths and sample rates (16/44.1, 24/44.1, 24/96, 24/192) so you can see your DAC switch. If you'd rather use your own queue, add one track of each kind first so every rate is exercised.", QbzSession.trRev)
                    }
                    CheckRow {
                        checked: root.welcomeConfirmed
                        label: QbzSession.tr("I understand and accept these terms", QbzSession.trRev)
                        onToggled: root.welcomeConfirmed = !root.welcomeConfirmed
                    }
                }

                // ═══════════════════ Step 1: check stack ═══════════════════
                Column {
                    visible: root.step === 1
                    width: card.contentWidth
                    spacing: 14

                    StepTitle { text: QbzSession.tr("Check Audio Stack", QbzSession.trRev) }

                    // Sandboxed (Flatpak/Snap): the host probes are blind, so
                    // there is no verdict — reference commands instead.
                    WarningBanner {
                        visible: root.doc.sandboxed === true
                        variant: "info"
                        title: QbzSession.tr("Sandboxed — host check unavailable", QbzSession.trRev)
                        body: QbzSession.tr("QBZ is packaged as Flatpak/Snap and can't inspect your host audio stack. If playback already works, you're set. Pick your distribution and init system below for the right setup commands.", QbzSession.trRev)
                    }
                    WarningBanner {
                        visible: root.doc.sandboxed !== true && root.doc.healthOk === true
                        variant: "success"
                        title: QbzSession.tr("Audio stack ready", QbzSession.trRev)
                        body: root.doc.healthSummary || ""
                    }
                    WarningBanner {
                        visible: root.doc.sandboxed !== true && root.doc.healthOk !== true
                        variant: "warning"
                        title: QbzSession.tr("Some packages are missing", QbzSession.trRev)
                        body: root.doc.healthSummary || ""
                    }

                    // Distro — auto-detected, always overridable.
                    Column {
                        width: parent.width
                        spacing: 8
                        Text {
                            text: QbzSession.tr("Your distribution", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            font.weight: theme.weightMedium
                        }
                        QbzSelect { kioskHost: root.kioskHost;
                            menuWidth: 360
                            options: root.doc.distroOptions || []
                            currentIndex: root.doc.distroIndex || 0
                            onSelected: function (i) { QbzDacWizard.setDistro(i) }
                        }
                    }
                    // Init system — auto-detected at runtime; override to
                    // generate service commands for another machine.
                    Column {
                        width: parent.width
                        spacing: 8
                        Text {
                            text: QbzSession.tr("Init system (service commands)", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            font.weight: theme.weightMedium
                        }
                        QbzSelect { kioskHost: root.kioskHost;
                            menuWidth: 240
                            options: root.doc.initOptions || []
                            currentIndex: root.doc.initIndex || 0
                            onSelected: function (i) { QbzDacWizard.setInit(i) }
                        }
                    }
                    // One copy-paste fix per missing piece (empty when ready).
                    Repeater {
                        model: root.remediations
                        delegate: CommandBlock {
                            required property var modelData
                            width: card.contentWidth
                            caption: modelData.caption || ""
                            command: modelData.command || ""
                        }
                    }
                }

                // ═══════════════════ Step 2: select DACs ═══════════════════
                Column {
                    visible: root.step === 2
                    width: card.contentWidth
                    spacing: 14

                    StepTitle {
                        text: QbzSession.tr("Which DACs do you want to configure?", QbzSession.trRev)
                    }
                    Text {
                        visible: root.doc.detecting === true
                        width: parent.width
                        text: QbzSession.tr("Detecting your DACs…", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    }

                    // Enumerated candidates — circular multi-select.
                    Repeater {
                        model: root.candidates
                        delegate: Rectangle {
                            id: candRow
                            required property int index
                            required property var modelData

                            width: card.contentWidth
                            height: candCol.implicitHeight + 16
                            radius: theme.radiusSm
                            color: modelData.checked ? theme.surfaceHover : "transparent"

                            SelectCheck {
                                id: candCheck
                                anchors.left: parent.left
                                anchors.leftMargin: 8
                                anchors.verticalCenter: parent.verticalCenter
                                diameter: 16
                                on: candRow.modelData.checked === true
                                onToggled: QbzDacWizard.toggleDac(candRow.index)
                            }
                            Column {
                                id: candCol
                                anchors.left: candCheck.right
                                anchors.leftMargin: 12
                                anchors.right: parent.right
                                anchors.rightMargin: 8
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 4

                                Text {
                                    width: parent.width
                                    text: candRow.modelData.description || ""
                                    color: theme.textPrimary
                                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                    font.weight: theme.weightMedium
                                    elide: Text.ElideRight
                                }
                                Row {
                                    width: parent.width
                                    spacing: 8
                                    // Bus label — a bordered box, NOT a pill (ADR-008).
                                    Rectangle {
                                        visible: (candRow.modelData.bus || "") !== ""
                                        width: visible ? busText.implicitWidth + 12 : 0
                                        height: 18
                                        radius: theme.radiusSm
                                        border.width: 1
                                        border.color: theme.borderSubtle
                                        color: theme.surfaceElevated
                                        Text {
                                            id: busText
                                            anchors.centerIn: parent
                                            text: candRow.modelData.bus || ""
                                            color: theme.textMuted
                                            font.pixelSize: root.kioskHost ? (10) * 1.2 : (10)
                                            font.weight: theme.weightSemibold
                                        }
                                    }
                                    Text {
                                        visible: candRow.modelData.isDefault === true
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: QbzSession.tr("default", QbzSession.trRev)
                                        color: theme.success
                                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                    }
                                    Text {
                                        visible: (candRow.modelData.ratesLabel || "") !== ""
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: candRow.modelData.ratesLabel || ""
                                        color: theme.textMuted
                                        font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                        elide: Text.ElideRight
                                    }
                                }
                            }
                        }
                    }

                    // Escape-hatch toggle (only when enumeration found devices;
                    // otherwise the manual block shows unconditionally).
                    Item {
                        id: manualEntryToggle
                        visible: root.doc.hasEnumeration === true
                        width: parent.width
                        height: visible ? 22 : 0
                        activeFocusOnTab: visible && enabled
                        Accessible.role: Accessible.Button
                        Accessible.name: manualToggle.text
                        Accessible.onPressAction: root.showManual = !root.showManual
                        Keys.onPressed: function (event) {
                            if (!event.isAutoRepeat
                                    && (event.key === Qt.Key_Space
                                        || event.key === Qt.Key_Return
                                        || event.key === Qt.Key_Enter)) {
                                root.showManual = !root.showManual
                                event.accepted = true
                            }
                        }
                        Rectangle {
                            anchors.left: parent.left
                            anchors.top: parent.top
                            width: manualToggle.implicitWidth
                            height: parent.height
                            radius: theme.radiusSm
                            color: "transparent"
                            border.width: manualEntryToggle.activeFocus ? 2 : 0
                            border.color: theme.accent
                        }
                        Text {
                            id: manualToggle
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.showManual
                                ? QbzSession.tr("Hide manual entry", QbzSession.trRev)
                                : QbzSession.tr("Can't see your DAC?", QbzSession.trRev)
                            color: theme.accent
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                        }
                        MouseArea {
                            anchors.left: parent.left
                            anchors.top: parent.top
                            anchors.bottom: parent.bottom
                            width: manualToggle.implicitWidth
                            cursorShape: Qt.PointingHandCursor
                            onPressed: manualEntryToggle.forceActiveFocus()
                            onClicked: root.showManual = !root.showManual
                        }
                    }

                    // Manual node.name entry (escape hatch).
                    Column {
                        visible: root.showManual || root.doc.hasEnumeration !== true
                        width: parent.width
                        spacing: 10

                        Text {
                            width: parent.width
                            text: QbzSession.tr("Find your DAC's PipeWire node name and paste it here:", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            wrapMode: Text.WordWrap
                        }
                        CommandBlock {
                            caption: QbzSession.tr("1. Find your DAC under 'Audio > Sinks' and note its number", QbzSession.trRev)
                            command: "wpctl status"
                        }
                        CommandBlock {
                            caption: QbzSession.tr("2. Run with that number to read the node.name", QbzSession.trRev)
                            command: "wpctl inspect <ID> | grep node.name"
                        }
                        Rectangle {
                            width: parent.width
                            height: 43
                            radius: theme.radiusSm
                            color: theme.surfaceCard
                            border.width: 1
                            border.color: manualInput.activeFocus ? theme.accent : theme.borderSubtle

                            TextInput {

                                QbzTextEditMenu { }
                                id: manualInput
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                verticalAlignment: TextInput.AlignVCenter
                                clip: true
                                color: theme.textPrimary
                                font.pixelSize: root.kioskHost ? (theme.fontLink) * 1.2 : (theme.fontLink)
                                selectByMouse: true
                                // No hotkey-guard call here on purpose: this
                                // port computes the guard at the DISPATCHER
                                // (AppShell.qml:94 tests `activeFocusItem
                                // instanceof TextInput`), so a plain TextInput
                                // is already guarded. The Slint needs its
                                // explicit `UiFocusState.text-input-focused`
                                // write because a std-widgets LineEdit's
                                // has-focus is an alias into an inner item and
                                // never fires at the use site (#619) — that is
                                // a Slint problem, not a contract.
                                onTextEdited: QbzDacWizard.validateManual(text)
                            }
                            Text {
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.verticalCenter: parent.verticalCenter
                                visible: manualInput.text === ""
                                text: "alsa_output.usb-..."
                                color: theme.textMuted
                                font.pixelSize: root.kioskHost ? (theme.fontLink) * 1.2 : (theme.fontLink)
                            }
                        }
                        Text {
                            visible: manualInput.text !== ""
                            width: parent.width
                            text: root.doc.manualValid === true
                                ? QbzSession.tr("Valid node name", QbzSession.trRev)
                                : QbzSession.tr("Invalid — should contain alsa_output or alsa_input", QbzSession.trRev)
                            color: root.doc.manualValid === true ? theme.success : theme.danger
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            wrapMode: Text.WordWrap
                        }
                    }
                }

                // ═══════════════════ Step 3: review & apply ════════════════
                Column {
                    visible: root.step === 3
                    width: card.contentWidth
                    spacing: 14

                    StepTitle { text: QbzSession.tr("Review & Apply", QbzSession.trRev) }
                    WarningBanner {
                        variant: "info"
                        title: QbzSession.tr("QBZ never writes these files", QbzSession.trRev)
                        body: QbzSession.tr("QBZ already pins rate, sink and exclusive mode at runtime. These files only change system-wide behavior — copy and run them yourself if you want them. Back up first.", QbzSession.trRev)
                    }

                    // 1. Backup.
                    CommandBlock {
                        caption: QbzSession.tr("1. Back up your current PipeWire/WirePlumber config", QbzSession.trRev)
                        command: root.doc.backupCmd || ""
                    }
                    CheckRow {
                        checked: root.reviewBackupDone
                        label: QbzSession.tr("I created a backup", QbzSession.trRev)
                        onToggled: root.reviewBackupDone = !root.reviewBackupDone
                    }

                    // 2. Per-DAC config — collapsible accordions.
                    Repeater {
                        model: root.dacConfigs
                        delegate: Column {
                            id: cfgCol
                            required property int index
                            required property var modelData

                            width: card.contentWidth
                            spacing: 8

                            Rectangle {
                                id: configHeader
                                width: parent.width
                                height: 40
                                radius: theme.radiusSm
                                color: cfgHead.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                                border.width: 1
                                border.color: activeFocus ? theme.accent : theme.borderSubtle
                                activeFocusOnTab: visible && enabled
                                Accessible.role: Accessible.Button
                                Accessible.name: cfgCol.modelData.name || ""
                                Accessible.onPressAction: QbzDacWizard.toggleConfig(cfgCol.index)
                                Keys.onPressed: function (event) {
                                    if (!event.isAutoRepeat
                                            && (event.key === Qt.Key_Space
                                                || event.key === Qt.Key_Return
                                                || event.key === Qt.Key_Enter)) {
                                        QbzDacWizard.toggleConfig(cfgCol.index)
                                        event.accepted = true
                                    }
                                }

                                QbzIcon {
                                    id: cfgChevron
                                    anchors.left: parent.left
                                    anchors.leftMargin: 12
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 16
                                    height: 16
                                    name: cfgCol.modelData.expanded ? "chevron-down" : "chevron-right"
                                    tintName: "muted"
                                }
                                Text {
                                    anchors.left: cfgChevron.right
                                    anchors.leftMargin: 8
                                    anchors.right: parent.right
                                    anchors.rightMargin: 12
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: cfgCol.modelData.name || ""
                                    color: theme.textPrimary
                                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                    font.weight: theme.weightMedium
                                    elide: Text.ElideRight
                                }
                                MouseArea {
                                    id: cfgHead
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onPressed: configHeader.forceActiveFocus()
                                    onClicked: QbzDacWizard.toggleConfig(cfgCol.index)
                                }
                            }
                            Column {
                                visible: cfgCol.modelData.expanded === true
                                width: parent.width
                                spacing: 8
                                CommandBlock {
                                    caption: QbzSession.tr("Sample-rate switching (PipeWire)", QbzSession.trRev)
                                    command: cfgCol.modelData.pipewireConf || ""
                                }
                                CommandBlock {
                                    caption: QbzSession.tr("Per-app bit-perfect (PipeWire client)", QbzSession.trRev)
                                    command: cfgCol.modelData.pulseConf || ""
                                }
                                CommandBlock {
                                    caption: QbzSession.tr("Pin the DAC + rates (WirePlumber)", QbzSession.trRev)
                                    command: cfgCol.modelData.wireplumberConf || ""
                                }
                            }
                        }
                    }
                    CheckRow {
                        checked: root.reviewConfigDone
                        label: QbzSession.tr("I created the configuration files", QbzSession.trRev)
                        onToggled: root.reviewConfigDone = !root.reviewConfigDone
                    }

                    // 3. Restart (init-aware).
                    CommandBlock {
                        caption: QbzSession.tr("3. Restart the audio services to load the config", QbzSession.trRev)
                        command: root.doc.restartCmd || ""
                    }
                    CheckRow {
                        checked: root.reviewRestartDone
                        label: QbzSession.tr("I restarted the audio services", QbzSession.trRev)
                        onToggled: root.reviewRestartDone = !root.reviewRestartDone
                    }
                }

                // ═══════════════════ Step 4: test playback ═════════════════
                Column {
                    id: testStep
                    visible: root.step === 4
                    width: card.contentWidth
                    spacing: 14

                    readonly property bool playing: root.doc.testPlaying === true

                    StepTitle { text: QbzSession.tr("Test Playback", QbzSession.trRev) }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Plays four tracks at different bit-depths and sample rates through the selected DAC. Watch your DAC's display switch rates — and confirm it below.", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        wrapMode: Text.WordWrap
                    }

                    // The four tracks — click one to jump straight to it while
                    // testing (no need to wait for each to finish).
                    Column {
                        width: parent.width
                        spacing: 4
                        Repeater {
                            model: root.testTrackLines
                            delegate: Rectangle {
                                id: trackRow
                                required property int index
                                required property string modelData

                                width: card.contentWidth
                                height: 26
                                radius: theme.radiusSm
                                color: (trackTa.containsMouse && testStep.playing)
                                    ? theme.surfaceHover : "transparent"
                                activeFocusOnTab: testStep.playing
                                border.width: activeFocus ? 2 : 0
                                border.color: theme.accent
                                Accessible.role: Accessible.Button
                                Accessible.name: trackRow.modelData
                                Accessible.onPressAction: {
                                    if (testStep.playing)
                                        QbzDacWizard.testPlayIndex(trackRow.index)
                                }
                                Keys.onPressed: function (event) {
                                    if (testStep.playing && !event.isAutoRepeat
                                            && (event.key === Qt.Key_Space
                                                || event.key === Qt.Key_Return
                                                || event.key === Qt.Key_Enter)) {
                                        QbzDacWizard.testPlayIndex(trackRow.index)
                                        event.accepted = true
                                    }
                                }

                                // Fixed glyph slot (width AND height) so the
                                // ♪→▶ hover swap can never reflow the row.
                                Text {
                                    id: trackGlyph
                                    anchors.left: parent.left
                                    anchors.leftMargin: 6
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 14
                                    height: 26
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                    text: (trackTa.containsMouse && testStep.playing) ? "▶" : "♪"
                                    color: (trackTa.containsMouse && testStep.playing)
                                        ? theme.accent : theme.textMuted
                                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                }
                                Text {
                                    anchors.left: trackGlyph.right
                                    anchors.leftMargin: 8
                                    anchors.right: parent.right
                                    anchors.rightMargin: 6
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: trackRow.modelData
                                    color: theme.textSecondary
                                    font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                    elide: Text.ElideRight
                                }
                                MouseArea {
                                    id: trackTa
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: testStep.playing ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onPressed: if (testStep.playing) trackRow.forceActiveFocus()
                                    onClicked: {
                                        if (testStep.playing)
                                            QbzDacWizard.testPlayIndex(trackRow.index)
                                    }
                                }
                            }
                        }
                    }

                    // Play / Stop · Prev / Next · use-my-queue.
                    Row {
                        width: parent.width
                        height: 34
                        spacing: 10

                        QbzPrimaryButton {
                            btnHeight: 34
                            label: testStep.playing
                                ? QbzSession.tr("Stop", QbzSession.trRev)
                                : QbzSession.tr("Play test", QbzSession.trRev)
                            onClicked: {
                                if (testStep.playing)
                                    QbzDacWizard.stopTest()
                                else
                                    QbzDacWizard.startTest()
                            }
                        }

                        // Explicit Prev / Next — they work on whatever queue is
                        // playing (the 4 test tracks or the user's own), which
                        // is why they call the PLAYER, not the wizard.
                        Rectangle {
                            id: prevButton
                            visible: testStep.playing
                            width: visible ? 34 : 0
                            height: 34
                            radius: theme.radiusSm
                            border.width: 1
                            border.color: activeFocus ? theme.accent : theme.borderSubtle
                            activeFocusOnTab: visible && enabled
                            Accessible.role: Accessible.Button
                            Accessible.name: QbzSession.tr("Previous", QbzSession.trRev)
                            Accessible.onPressAction: QbzPlayer.previous()
                            Keys.onPressed: function (event) {
                                if (!event.isAutoRepeat
                                        && (event.key === Qt.Key_Space
                                            || event.key === Qt.Key_Return
                                            || event.key === Qt.Key_Enter)) {
                                    QbzPlayer.previous()
                                    event.accepted = true
                                }
                            }
                            color: prevTa.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                            QbzIcon {
                                anchors.centerIn: parent
                                width: 16
                                height: 16
                                name: "skip-back"
                                tintName: prevTa.containsMouse ? "textPrimary" : "secondary"
                            }
                            MouseArea {
                                id: prevTa
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onPressed: prevButton.forceActiveFocus()
                                onClicked: QbzPlayer.previous()
                            }
                        }
                        Rectangle {
                            id: nextButton
                            visible: testStep.playing
                            width: visible ? 34 : 0
                            height: 34
                            radius: theme.radiusSm
                            border.width: 1
                            border.color: activeFocus ? theme.accent : theme.borderSubtle
                            activeFocusOnTab: visible && enabled
                            Accessible.role: Accessible.Button
                            Accessible.name: QbzSession.tr("Next", QbzSession.trRev)
                            Accessible.onPressAction: QbzPlayer.next()
                            Keys.onPressed: function (event) {
                                if (!event.isAutoRepeat
                                        && (event.key === Qt.Key_Space
                                            || event.key === Qt.Key_Return
                                            || event.key === Qt.Key_Enter)) {
                                    QbzPlayer.next()
                                    event.accepted = true
                                }
                            }
                            color: nextTa.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                            QbzIcon {
                                anchors.centerIn: parent
                                width: 16
                                height: 16
                                name: "skip-forward"
                                tintName: nextTa.containsMouse ? "textPrimary" : "secondary"
                            }
                            MouseArea {
                                id: nextTa
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onPressed: nextButton.forceActiveFocus()
                                onClicked: QbzPlayer.next()
                            }
                        }

                        // Verify with the user's current queue instead of the
                        // curated tracks. minWidth 0 — this is a modal footer
                        // button, not a Settings row's, and the 160px floor
                        // would push the transport buttons off the row.
                        SettingsButton { kioskHost: root.kioskHost;
                            visible: !testStep.playing
                            minWidth: 0
                            text: QbzSession.tr("Use my current queue", QbzSession.trRev)
                            onClicked: QbzDacWizard.verifyOwn()
                        }
                    }

                    // Live read-back: requested (honest) + DAC negotiated (N6).
                    Rectangle {
                        visible: testStep.playing || (root.doc.testRequestedLabel || "") !== ""
                        width: parent.width
                        height: visible ? readout.implicitHeight + 24 : 0
                        radius: theme.radiusSm
                        color: theme.surfaceElevated
                        border.width: 1
                        border.color: theme.borderSubtle

                        Column {
                            id: readout
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.top: parent.top
                            anchors.margins: 12
                            spacing: 6

                            Text {
                                width: parent.width
                                text: root.doc.testRequestedLabel || ""
                                color: theme.textPrimary
                                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                font.weight: theme.weightMedium
                                wrapMode: Text.WordWrap
                            }
                            Row {
                                width: parent.width
                                spacing: 8
                                Text {
                                    visible: root.doc.testRateMatched === true
                                    text: "✓"
                                    color: theme.success
                                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                    font.weight: theme.weightBold
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: root.doc.testNegotiatedLabel || ""
                                    color: root.doc.testRateMatched === true
                                        ? theme.success : theme.textSecondary
                                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                }
                            }
                        }
                    }

                    // Drives the read-back while the test plays. `running` is
                    // bound to the document, so leaving the step or stopping
                    // the test parks the timer — it never polls in the dark.
                    Timer {
                        interval: 1500
                        repeat: true
                        running: testStep.visible && testStep.playing
                        onTriggered: QbzDacWizard.pollTest()
                    }
                }

                // ═══════════════════════ Step 5: done ══════════════════════
                Column {
                    visible: root.step === 5
                    width: card.contentWidth
                    spacing: 14

                    Row {
                        width: parent.width
                        height: 26
                        spacing: 10
                        QbzIcon {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 24
                            height: 24
                            name: "circle-check-big"
                            // Same reasoning as the header wand.
                            tintName: "accent"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("All set", QbzSession.trRev)
                            color: theme.textPrimary
                            font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
                            font.weight: theme.weightSemibold
                        }
                    }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Your DAC is ready for bit-perfect playback. If you applied the config files, restart the audio services (or log out and back in) for them to take effect.", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        wrapMode: Text.WordWrap
                    }
                    Column {
                        visible: root.createdPaths.length > 0
                        width: parent.width
                        spacing: 6
                        Text {
                            text: QbzSession.tr("Config files you can create:", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            font.weight: theme.weightMedium
                        }
                        Repeater {
                            model: root.createdPaths
                            delegate: Text {
                                required property string modelData
                                width: card.contentWidth
                                text: modelData
                                color: theme.textMuted
                                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                font.family: "monospace"
                                elide: Text.ElideRight
                            }
                        }
                    }
                }
            }
        }

        // The 14px gutter scrollbar, inset 4px from the right — the port's
        // shared control, attached as a SIBLING of the Flickable.
        QbzScrollBar {
            target: bodyFlick
            anchors.right: parent.right
            anchors.rightMargin: 4
            anchors.top: bodyFlick.top
            anchors.bottom: bodyFlick.bottom
        }

        // ------------------------------ footer ----------------------------
        Rectangle {
            id: footerDiv
            anchors.bottom: footer.top
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: theme.borderSubtle
        }
        Item {
            id: footer
            anchors.bottom: parent.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: 70

            // Back — hidden on the first step. minWidth 0 on both: these are
            // modal footer buttons (the MyQBZ modals' convention), and the
            // Settings row's 160px floor would make a three-button footer
            // wider than the card.
            SettingsButton { kioskHost: root.kioskHost;
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                visible: root.step > 0
                minWidth: 0
                text: QbzSession.tr("Back", QbzSession.trRev)
                onClicked: root.step -= 1
            }
            SettingsButton { kioskHost: root.kioskHost;
                id: footerClose
                anchors.right: primaryBtn.left
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                minWidth: 0
                text: QbzSession.tr("Close", QbzSession.trRev)
                onClicked: QbzDacWizard.close()
            }
            QbzPrimaryButton {
                id: primaryBtn
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                btnHeight: 34
                btnEnabled: root.primaryEnabled
                label: root.step === 0 ? QbzSession.tr("Start", QbzSession.trRev)
                    : (root.step === 5 ? QbzSession.tr("Finish", QbzSession.trRev)
                                       : QbzSession.tr("Next", QbzSession.trRev))
                onClicked: {
                    if (root.step === 5) {
                        QbzDacWizard.close()
                        return
                    }
                    // Entering the DACs step kicks off enumeration; entering
                    // review generates the per-DAC config. Both are Rust-side
                    // and asynchronous — the step advances immediately and the
                    // body renders its "detecting…" / empty state until the
                    // document lands (reference :845-853).
                    if (root.step === 1)
                        QbzDacWizard.runDetect()
                    if (root.step === 2)
                        QbzDacWizard.genConfigs()
                    root.step += 1
                }
            }
        }
    }
}
