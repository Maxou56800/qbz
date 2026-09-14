// MyQbzModals — the THREE small MyQBZ modals in one file, as three sibling
// panels: Create (myqbz/CreateMyQbzModal.slint, 199 lines), Edit
// (MyQbzEditModal.slint, 147) and Mix (MyQbzMixModal.slint, 184). Slint keeps
// them apart only because each has its own global; they share the same
// `#bf000000` backdrop, centred surface-card panel and right-aligned action
// footer, and they are mutually exclusive in practice.
//
// A GLOBAL overlay: AppShell.qml mounts it once and each panel self-gates on
// its OWN document's `open` flag (`QbzMyQbz.createJson` / `editJson` /
// `mixJson`) — the same shape as Slint's four bare instantiations at
// AppShell.slint:814-824.
//
// Per-panel numbers that are NOT shared, and every one of them is deliberate:
//   * width 420 / 420 / 440 (Create / Edit / Mix);
//   * spacing 16 / 16 / 18;
//   * Edit follows the current modal standard (20px padding, heading + 28x28
//     close X, 10px footer spacing, 36px buttons); Create and Mix retain their
//     reference-specific metrics until their own parity pass.
//
// Drop shadows (`drop-shadow-blur: 32px` on all three) are dropped: shadows
// render nothing on this port's software path. The scrims carry `radius:
// theme.radiusMd` so an opaque full-bleed child cannot paint into the shell
// frame's bezel corners (controls/DiscoverConfigModal.qml:59-75).
//
// TEXT STATE IS QML-LOCAL. `MyQbzCreateState.name` and
// `MyQbzEditState.draft-name` / `draft-description` are NOT bridge members:
// every submit invokable takes the value as an argument (`createSubmit(name)`,
// `editSubmitRename(name)`, `editSubmitDescription(description)`). The drafts
// are seeded from the document when a panel opens and cleared when it closes.
//
// ENTER submits through `QbzLineEdit`'s `accepted` signal, NEVER `committed`:
// `committed` also fires on focus-out (QbzLineEdit.qml:155), and a blur must
// not create or rename anything.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // --- Documents (guarded parses; a raw JSON.parse in a binding throws on
    // the pre-publish frame — PlaylistView.qml:55-61).
    readonly property var createDoc: {
        try { return JSON.parse(QbzMyQbz.createJson) } catch (e) { return ({}) }
    }
    readonly property var editDoc: {
        try { return JSON.parse(QbzMyQbz.editJson) } catch (e) { return ({}) }
    }
    readonly property var mixDoc: {
        try { return JSON.parse(QbzMyQbz.mixJson) } catch (e) { return ({}) }
    }

    readonly property bool createOpen: root.createDoc.open === true
    readonly property bool editOpen: root.editDoc.open === true
    readonly property bool mixOpen: root.mixDoc.open === true

    visible: root.createOpen || root.editOpen || root.mixOpen
    enabled: root.visible

    // ================================================================
    // PANEL 1 — Create Mixtape / Collection (CreateMyQbzModal.slint)
    // ================================================================
    Item {
        id: createPanel
        anchors.fill: parent
        visible: root.createOpen

        readonly property string kind: root.createDoc.kind || "mixtape"
        readonly property bool isMixtape: createPanel.kind === "mixtape"
        readonly property bool creating: root.createDoc.creating === true
        property string name: ""
        readonly property bool canCreate: createPanel.name !== "" && !createPanel.creating

        // A fresh open starts with an empty field — reset on the OPEN edge,
        // never the close edge: at close the box still holds focus for a
        // frame, its focus-gated re-seed skips, and the next open found the
        // draft already "" with the old text still in the box (the
        // dirty-form report of 2026-09-14). `reset()` syncs the box itself.
        Connections {
            target: root
            function onCreateOpenChanged() {
                if (!root.createOpen)
                    return
                createPanel.name = ""
                createNameField.reset("")
            }
        }

        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzMyQbz.createClose()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }
        Rectangle {
            width: Math.min(root.width - 80, 420)
            height: createCol.implicitHeight + 48
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: createCol
                x: 24
                y: 24
                width: parent.width - 48
                spacing: 16

                // --- Title + close
                Item {
                    width: parent.width
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, parent.width - 28)
                        text: createPanel.isMixtape ? root.t("Create Mixtape")
                                                    : root.t("Create Collection")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightBold
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                    Item {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "x"
                            width: 17
                            height: 17
                            tintName: createCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: createCloseArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzMyQbz.createClose()
                        }
                    }
                }

                // --- NAME
                Column {
                    width: parent.width
                    spacing: 8
                    Text {
                        text: root.t("NAME")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.5
                    }
                    QbzLineEdit {
                        id: createNameField
                        width: parent.width
                        height: 34
                        text: createPanel.name
                        onEdited: function (v) { createPanel.name = v }
                        onAccepted: function (v) {
                            if (createPanel.canCreate) QbzMyQbz.createSubmit(createPanel.name)
                        }
                    }
                }

                // --- KIND (12px between the radios here; the Add modal uses
                // 16 — AddToMixtapeModal.slint:503).
                Column {
                    width: parent.width
                    spacing: 8
                    Text {
                        text: root.t("KIND")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.5
                    }
                    Row {
                        spacing: 12
                        QbzRadioOption {
                            label: root.t("Mixtapes")
                            selected: createPanel.isMixtape
                            onClicked: if (!createPanel.creating) QbzMyQbz.createSetKind("mixtape")
                        }
                        QbzRadioOption {
                            label: root.t("Collections")
                            selected: !createPanel.isMixtape
                            onClicked: if (!createPanel.creating) QbzMyQbz.createSetKind("collection")
                        }
                    }
                }

                // --- Footer
                Row {
                    anchors.right: parent.right
                    spacing: 8
                    SettingsButton {
                        text: root.t("Cancel")
                        btnHeight: 38
                        minWidth: 0
                        onClicked: QbzMyQbz.createClose()
                    }
                    QbzPrimaryButton {
                        label: createPanel.isMixtape ? root.t("+ New Mixtape")
                                                     : root.t("+ New Collection")
                        btnHeight: 38
                        btnEnabled: createPanel.canCreate
                        onClicked: QbzMyQbz.createSubmit(createPanel.name)
                    }
                }
            }
        }
    }

    // ================================================================
    // PANEL 2 — Rename / Edit description / Delete (MyQbzEditModal.slint)
    // ================================================================
    Item {
        id: editPanel
        anchors.fill: parent
        visible: root.editOpen

        readonly property string mode: root.editDoc.mode || ""
        readonly property bool isRename: editPanel.mode === "rename"
        readonly property bool isDescription: editPanel.mode === "description"
        readonly property bool isDelete: editPanel.mode === "delete"
        readonly property bool busy: root.editDoc.busy === true
        property string draftName: ""
        property string draftDescription: ""
        // :33-35 — busy blocks everything; rename additionally needs a
        // non-empty name (Rust trims again).
        readonly property bool canSubmit: editPanel.busy
            ? false : (editPanel.isRename ? editPanel.draftName !== "" : true)

        // Prefill from the document (Rust fills `name` for the rename + the
        // delete-confirm interpolation, `description` for the description
        // body — myqbz_edit sets the drafts when it opens, which is what this
        // reproduces).
        //
        // Keyed on mode+name, NOT on `open`: Rust may publish a second open
        // document for a DIFFERENT collection without an intervening closed
        // frame, and then `open` never changes and the prefill would be stale.
        // Keying it on the whole document instead would clobber what the user
        // is typing every time an unrelated field (`busy`) flips.
        // The open flag rides along so a reopen on the SAME collection
        // re-seeds too (a cancelled rename left its draft behind).
        readonly property string editKey: (root.editOpen ? "1" : "0") + "|"
            + editPanel.mode + "|" + (root.editDoc.name || "")
        onEditKeyChanged: {
            editPanel.draftName = root.editDoc.name || ""
            editPanel.draftDescription = root.editDoc.description || ""
            if (root.editOpen) {
                renameField.reset()
                descriptionField.reset()
            }
        }

        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzMyQbz.editClose()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }
        Rectangle {
            width: Math.min(root.width - 80, 420)
            height: editCol.implicitHeight + 40
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: editCol
                x: 20
                y: 20
                width: parent.width - 40
                spacing: 16

                // --- Standard modal title + close X.
                Item {
                    width: parent.width
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: Math.max(0, parent.width - 28)
                        text: editPanel.isRename ? root.t("Rename")
                            : editPanel.isDescription ? root.t("Edit description")
                            : root.t("Delete")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Item {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        opacity: editPanel.busy ? 0.5 : 1.0
                        QbzIcon {
                            anchors.centerIn: parent
                            width: 17
                            height: 17
                            name: "x"
                            tintName: editCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: editCloseArea
                            anchors.fill: parent
                            enabled: !editPanel.busy
                            hoverEnabled: !editPanel.busy
                            cursorShape: editPanel.busy ? Qt.ArrowCursor
                                                       : Qt.PointingHandCursor
                            onClicked: QbzMyQbz.editClose()
                        }
                    }
                }

                // --- Body: rename
                QbzLineEdit {
                    id: renameField
                    visible: editPanel.isRename
                    width: parent.width
                    height: visible ? 34 : 0
                    text: editPanel.draftName
                    onEdited: function (v) { editPanel.draftName = v }
                    onAccepted: function (v) {
                        if (editPanel.canSubmit) QbzMyQbz.editSubmitRename(editPanel.draftName)
                    }
                }

                // --- Body: description
                QbzTextArea {
                    id: descriptionField
                    visible: editPanel.isDescription
                    width: parent.width
                    // A FLAT 96, never `visible ? 96 : 0`: a Column does not
                    // position invisible children, so the hidden arm already
                    // costs no layout height, while a 0-height text area
                    // hands its inner Flickable height = 0 - 8 - 8 = -16 —
                    // the silent negative-size trap QbzLineEdit.qml:166-167
                    // documents. The sibling QbzLineEdit is single-line and
                    // lands at a clean 0, so it keeps its ternary.
                    height: 96
                    wrapMode: TextEdit.WordWrap
                    text: editPanel.draftDescription
                    onEdited: function (v) { editPanel.draftDescription = v }
                }

                // --- Body: delete confirm (:105, DOUBLE quotes around the
                // name, interpolated into the msgid's `{}`).
                Text {
                    visible: editPanel.isDelete
                    width: parent.width
                    text: root.t("Delete \"{}\"? This cannot be undone.")
                            .replace("{}", root.editDoc.name || "")
                    color: theme.textSecondary
                    font.pixelSize: theme.fontBody
                    wrapMode: Text.WordWrap
                }

                // --- Footer
                Row {
                    anchors.right: parent.right
                    spacing: 10
                    SettingsButton {
                        text: root.t("Cancel")
                        btnHeight: 36
                        minWidth: 0
                        enabled: !editPanel.busy
                        onClicked: QbzMyQbz.editClose()
                    }
                    QbzPrimaryButton {
                        label: editPanel.isDelete ? root.t("Delete") : root.t("Save")
                        btnHeight: 36
                        labelSize: theme.fontBody
                        destructive: editPanel.isDelete
                        btnEnabled: editPanel.canSubmit
                        onClicked: {
                            if (editPanel.isRename)
                                QbzMyQbz.editSubmitRename(editPanel.draftName)
                            else if (editPanel.isDescription)
                                QbzMyQbz.editSubmitDescription(editPanel.draftDescription)
                            else
                                QbzMyQbz.editConfirmDelete()
                        }
                    }
                }
            }
        }
    }

    // ================================================================
    // PANEL 3 — DJ-mix "Random queue" (MyQbzMixModal.slint)
    // ================================================================
    Item {
        id: mixPanel
        anchors.fill: parent
        visible: root.mixOpen

        readonly property bool loading: root.mixDoc.loading === true
        readonly property bool busy: root.mixDoc.busy === true
        readonly property var sizeOptions: root.mixDoc.sizeOptions || []
        readonly property int selectedIndex: root.mixDoc.selectedIndex || 0
        readonly property int selectedSize: root.mixDoc.selectedSize || 0
        readonly property bool selectedIsAll: root.mixDoc.selectedIsAll === true
        readonly property int uniqueCount: root.mixDoc.uniqueCount || 0
        // :35-39
        readonly property bool canShuffle: !mixPanel.loading && !mixPanel.busy
            && mixPanel.sizeOptions.length > 0 && mixPanel.selectedSize > 0
        // The slider travels over option INDICES, not song counts; a single
        // option (only "All (N)") renders a degenerate 0..0 track (:42).
        readonly property int sliderMax: Math.max(0, mixPanel.sizeOptions.length - 1)
        readonly property string sizeLabel: mixPanel.selectedIsAll
            ? root.t("All ({})").replace("{}", mixPanel.selectedSize)
            : String(mixPanel.selectedSize)

        Rectangle {
            anchors.fill: parent
            radius: theme.radiusMd
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzMyQbz.mixClose()
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }
        Rectangle {
            width: Math.min(root.width - 80, 440)
            height: mixCol.implicitHeight + 48
            x: Math.round((root.width - width) / 2)
            y: Math.round((root.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle

            MouseArea {
                anchors.fill: parent
                // Wheel-lock (the DiscoverConfigModal rule).
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: mixCol
                x: 24
                y: 24
                width: parent.width - 48
                spacing: 18

                Text {
                    width: parent.width
                    text: root.t("Random queue")
                    color: theme.textPrimary
                    font.pixelSize: 18
                    font.weight: theme.weightBold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: root.t("Create a random queue with songs from this collection. How many songs do you want?")
                    color: theme.textSecondary
                    font.pixelSize: 14
                    wrapMode: Text.WordWrap
                }

                // --- Loading (:93-106): left-aligned, 8px spacing.
                Row {
                    visible: mixPanel.loading
                    height: visible ? 18 : 0
                    spacing: 8
                    QbzSpinner {
                        anchors.verticalCenter: parent.verticalCenter
                        size: 18
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Loading…")
                        color: theme.textSecondary
                        font.pixelSize: 14
                        verticalAlignment: Text.AlignVCenter
                    }
                }

                // --- Size field (:109-154)
                Column {
                    visible: !mixPanel.loading && mixPanel.sizeOptions.length > 0
                    width: parent.width
                    spacing: 8

                    Text {
                        text: root.t("Number of songs")
                        color: theme.textPrimary
                        font.pixelSize: 13
                        font.weight: theme.weightMedium
                    }
                    Row {
                        width: parent.width
                        spacing: 12
                        Text {
                            id: sizeValue
                            anchors.verticalCenter: parent.verticalCenter
                            width: Math.max(64, implicitWidth)
                            text: mixPanel.sizeLabel
                            color: theme.accent
                            font.pixelSize: 15
                            font.weight: theme.weightSemibold
                            verticalAlignment: Text.AlignVCenter
                        }
                        QbzSlider {
                            anchors.verticalCenter: parent.verticalCenter
                            width: Math.max(0, parent.width - sizeValue.width - 12)
                            minimum: 0
                            maximum: mixPanel.sliderMax
                            value: mixPanel.selectedIndex
                            onChanged: function (v) { QbzMyQbz.mixSetIndex(v) }
                        }
                    }
                    Text {
                        width: parent.width
                        text: root.t("{} songs available").replace("{}", mixPanel.uniqueCount)
                        color: theme.textMuted
                        font.pixelSize: 12
                    }
                }

                // --- Footer: spacing 12 here, NOT 8 (:159).
                Row {
                    anchors.right: parent.right
                    spacing: 12
                    SettingsButton {
                        text: root.t("Cancel")
                        btnHeight: 38
                        minWidth: 0
                        enabled: !mixPanel.busy
                        onClicked: QbzMyQbz.mixClose()
                    }
                    QbzPrimaryButton {
                        label: root.t("Add to queue")
                        btnHeight: 38
                        btnEnabled: mixPanel.canShuffle
                        onClicked: QbzMyQbz.mixShuffle()
                    }
                }
            }
        }
    }
}
