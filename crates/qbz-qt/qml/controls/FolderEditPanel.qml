// FolderEditPanel — the BODY of the folder editor
// (primitives/FolderEditModal.slint:66-266): name field, the seven icon-preset
// tiles plus the custom-image tile, the eleven colour swatches, the "Hide from
// sidebar" row and the Delete / Save footer.
//
// The chrome around it — scrim, card, title, close X, Escape scope and the
// delete confirm — belongs to FolderModals.qml, which mounts this. The split
// is declared up front (rule 2): 04 §5.2's single ~380-line FolderModals is
// rejected because the two panels plus the confirm plus the drafts land nearer
// 600, and the port's precedents for "one file, several panels" are
// MyQbzModals.qml at 523 lines for three SIMPLER panels and
// PlaylistPickerModal.qml at 685.
//
// ── THE DRAFTS LIVE HERE ───────────────────────────────────────────────────
// Name, preset, colour and the hidden switch are QML-LOCAL and reach Rust only
// as arguments to `editSave(name, preset, color, hidden)`. That is the port's
// standing rule (MyQbzModals.qml's header: "TEXT STATE IS QML-LOCAL ... every
// submit invokable takes the value as an argument") and it is why the bridge
// has no `selectPreset` / `selectColor` / `toggleHidden` invokables: a colour
// click has no business making a round trip.
//
// Preset selection is the ONE draft write with a Rust side effect — it also
// calls `QbzFolderEdit.clearImage()`, because picking a preset means dropping
// the custom image, and the image is Rust-owned (the file dialog is native).
// That call is also what makes the reference's declared-but-never-called
// `clear-image` callback (main.rs:5611-5619) live.
//
// ── SEEDING ────────────────────────────────────────────────────────────────
// `doc.name` / `iconPreset` / `iconColor` / `isHidden` are SEEDS, consumed
// once on the open transition. They are NOT bound: Rust republishes editJson
// on every `busy` flip and on every image pick, and a bound field would throw
// away whatever the user had typed each time. `hasCustomImage` /
// `customImagePath` / `busy` ARE read live, because Rust owns them.
//
// ── REUSED, NOT REDRAWN ────────────────────────────────────────────────────
//   QbzLineEdit      the name field (its width MUST be set — the plain arm is
//                    a fixed 240, where the Slint LineEdit stretches).
//   QbzToggle        the hidden row. The reference uses QbzToggle too
//                    (FolderEditModal.slint:214). A second switch is exactly
//                    the fork rule 5 forbids.
//   QbzPrimaryButton the Save button. The reference's `#ffffffd6` hover on an
//                    accent fill and its hardcoded `#ffffff` label are NOT
//                    copied — that white-wash is the white-button-on-artwork
//                    treatment and it washes out on an accent fill (illegible
//                    on the 11 light themes). The sibling
//                    LibFolderEditModal.slint:376 already uses accent-hover.
//                    Contract §10 Q5 records the exact literals if parity is
//                    ever wanted.
//   PmFolderIcon     NOT reused for the preset tiles: a tile is a
//                    surface-elevated square with a theme-tinted 17px glyph,
//                    where PmFolderIcon is a saturated colour tile with a
//                    fixed-white glyph at half its own size. Only the id ->
//                    glyph chain is common, and it is seven ternary arms; a
//                    shared .js would need its own build.rs entry for that.
//                    Keep the two in step — PmFolderIcon.qml carries the
//                    same chain.
//   Delete button    hand-drawn, because it is a GHOST danger button and the
//                    port's filled danger control (QbzPrimaryButton
//                    `destructive`) is a different affordance. SettingsButton
//                    has `danger`, but it is a settings-row control with its
//                    own leading-glyph geometry.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    /// The parsed QbzFolderEdit.editJson (§4.5). The HOST parses it once; a
    /// second `JSON.parse` in here would double the work on every republish.
    property var doc: ({})
    /// The panel never raises the confirm itself — FolderModals owns it, so
    /// the confirm can stack over this panel with its own z (D21).
    signal deleteRequested()

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // --- Drafts -----------------------------------------------------------
    property string draftName: ""
    property string draftPreset: "folder"
    property string draftColor: ""
    property bool draftHidden: false

    readonly property bool busy: root.doc.busy === true
    readonly property bool isCreate: (root.doc.id || "") === ""
    readonly property bool canSave: root.draftName.trim() !== "" && !root.busy

    /// Consume the seeds. Called by the host on the open transition — never on
    /// a plain republish.
    function seed() {
        root.draftName = root.doc.name || ""
        root.draftPreset = root.doc.iconPreset || "folder"
        root.draftColor = root.doc.iconColor || ""
        root.draftHidden = root.doc.isHidden === true
    }

    /// Focus the name field. Split out so the host can call it AFTER the panel
    /// is actually visible — an invisible item cannot take active focus.
    function focusName() { nameField.focusField() }

    /// The id -> glyph chain, shared with PmFolderIcon.qml. "folder" is not a
    /// case in the reference's chain at all and falls through to the default
    /// arm, which is also the unknown-id arm; "headphones" draws audio-lines.
    /// Both are upstream intent (§4.5) — do not "fix" either.
    function presetGlyph(id) {
        return id === "heart" ? "heart"
             : id === "star" ? "star"
             : id === "music" ? "music"
             : id === "disc" ? "disc"
             : id === "library" ? "library"
             : id === "headphones" ? "audio-lines"
             : "folder"
    }

    implicitHeight: col.implicitHeight

    Column {
        id: col
        width: parent.width
        spacing: 16

        // ================= Name =========================================
        Column {
            width: parent.width
            spacing: 6
            Text {
                text: root.t("Folder name")
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }
            QbzLineEdit {
                id: nameField
                // MUST be explicit: the plain arm is a fixed `width: 240`
                // (QbzLineEdit.qml:62), where the Slint LineEdit stretches to
                // the panel.
                width: parent.width
                text: root.draftName
                placeholder: root.t("Enter folder name")
                onEdited: function (value) { root.draftName = value }
                // `accepted`, NEVER `committed`: committed also fires on blur,
                // and closing the modal blurs the field, which would submit a
                // second time (MyQbzModals.qml:34-36 documents the incident).
                onAccepted: function (value) {
                    root.draftName = value
                    if (root.draftName.trim() !== "" && !root.busy)
                        QbzFolderEdit.editSave(root.draftName, root.draftPreset,
                                               root.draftColor, root.draftHidden)
                }
            }
        }

        // ================= Icon =========================================
        Column {
            width: parent.width
            spacing: 6
            Text {
                text: root.t("Icon")
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }
            Row {
                spacing: 8

                Repeater {
                    model: root.doc.presets || []
                    delegate: Rectangle {
                        id: presetTile
                        required property var modelData
                        // A preset is only "the" selection while there is no
                        // custom image — the image wins, exactly as in the
                        // reference (`custom-image-path == "" && ...`).
                        readonly property bool active:
                            root.doc.hasCustomImage !== true
                            && root.draftPreset === presetTile.modelData
                        width: 40
                        height: 40
                        radius: 8
                        color: presetTile.active
                            ? theme.accent
                            : (presetArea.containsMouse ? theme.surfaceElevatedHover
                                                        : theme.surfaceElevated)
                        QbzIcon {
                            anchors.centerIn: parent
                            width: 17
                            height: 17
                            name: root.presetGlyph(presetTile.modelData)
                            // On an accent fill the port goes through the
                            // measured on-accent selector, not a hardcoded
                            // #ffffff (QbzTheme.qml "ON AN ACCENT FILL").
                            tintName: presetTile.active ? theme.accentGlyphTint : "secondary"
                        }
                        MouseArea {
                            id: presetArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                root.draftPreset = presetTile.modelData
                                // The image is RUST-owned (native dialog), so
                                // dropping it is the one draft write that has
                                // to make a round trip.
                                QbzFolderEdit.clearImage()
                            }
                        }
                    }
                }

                // --- The custom-image tile.
                Rectangle {
                    id: imageTile
                    readonly property bool active: root.doc.hasCustomImage === true
                    width: 40
                    height: 40
                    radius: 8
                    color: imageTile.active
                        ? theme.accent
                        : (imageArea.containsMouse ? theme.surfaceElevatedHover
                                                   : theme.surfaceElevated)
                    // No `clip: true` — it is a rectangular scissor and does
                    // not follow `radius`. RoundedImage masks its own corners.
                    RoundedImage {
                        anchors.fill: parent
                        visible: imageTile.active
                        source: imageTile.active ? (root.doc.customImagePath || "") : ""
                        radius: 8
                        fit: "crop"
                    }
                    QbzIcon {
                        anchors.centerIn: parent
                        visible: !imageTile.active
                        width: 17
                        height: 17
                        name: "image-plus"
                        tintName: "secondary"
                    }
                    MouseArea {
                        id: imageArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzFolderEdit.pickImage()
                    }
                }
            }
        }

        // ================= Colour =======================================
        Column {
            width: parent.width
            spacing: 6
            Text {
                text: root.t("Color")
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }
            // ELEVEN swatches, ONE row — 11 * 28 + 10 * 8 = 388 px. The
            // .slint comment at :167 saying "Two rows" is wrong about its own
            // grid. The wrap arithmetic is kept anyway, so a longer constant
            // list wraps instead of overflowing the card.
            Item {
                id: swatchGrid
                readonly property int columns: 11
                readonly property int sw: 28
                readonly property int gap: 8
                readonly property int count: (root.doc.swatches || []).length
                readonly property int rows: Math.ceil(swatchGrid.count / swatchGrid.columns)
                width: parent.width
                height: swatchGrid.rows * swatchGrid.sw
                        + Math.max(0, swatchGrid.rows - 1) * swatchGrid.gap

                Repeater {
                    model: root.doc.swatches || []
                    delegate: Rectangle {
                        id: swatch
                        required property int index
                        required property var modelData
                        readonly property string value: swatch.modelData.value || ""
                        readonly property bool active: root.draftColor === swatch.value
                        x: (swatch.index % swatchGrid.columns) * (swatchGrid.sw + swatchGrid.gap)
                        y: Math.floor(swatch.index / swatchGrid.columns)
                           * (swatchGrid.sw + swatchGrid.gap)
                        width: swatchGrid.sw
                        height: swatchGrid.sw
                        radius: swatchGrid.sw / 2
                        color: swatch.modelData.isAccent === true ? theme.accent : swatch.value
                        // A fixed white ring: the host is a saturated swatch
                        // under every theme, so a theme-following border would
                        // vanish on half of them.
                        border.width: swatch.active ? 2 : 0
                        border.color: "#ffffff"
                        QbzIcon {
                            anchors.centerIn: parent
                            visible: swatch.active
                            width: 14
                            height: 14
                            name: "check"
                            tintName: "white"
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            // "" IS a value ("use the theme accent") and it
                            // reaches the DB as "" — see contract D24.
                            onClicked: root.draftColor = swatch.value
                        }
                    }
                }
            }
        }

        // ================= Hidden =======================================
        Item {
            width: parent.width
            height: 22
            Text {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                width: Math.max(0, parent.width - 50)
                text: root.t("Hide from sidebar")
                color: theme.textSecondary
                font.pixelSize: theme.fontBody
                elide: Text.ElideRight
            }
            QbzToggle {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                checked: root.draftHidden
                enabled: !root.busy
                // QbzToggle never self-flips; the owner of the state does.
                onToggled: function (value) { root.draftHidden = value }
            }
        }

        // ================= Footer =======================================
        Item {
            width: parent.width
            height: 36

            // Delete — edit mode only. A ghost danger button: fill on hover
            // only, 1px danger border, danger label. theme.danger /
            // theme.dangerHover ARE the reference's #ef4444 / #ef444433 under
            // the default theme (the engine defaults danger to rgb(ef,44,44)
            // and derives danger_hover as a 0.2 tint, i.e. alpha 0x33) and the
            // correct colours under the other 35.
            Rectangle {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.isCreate
                width: delLabel.implicitWidth + 28
                height: 36
                radius: 8
                color: (delArea.containsMouse && !root.busy) ? theme.dangerHover : "transparent"
                border.width: 1
                border.color: theme.danger
                opacity: root.busy ? 0.5 : 1.0
                Text {
                    id: delLabel
                    anchors.centerIn: parent
                    text: root.t("Delete")
                    color: theme.danger
                    font.pixelSize: 15
                    font.weight: theme.weightMedium
                }
                MouseArea {
                    id: delArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: root.busy ? Qt.ArrowCursor : Qt.PointingHandCursor
                    onClicked: if (!root.busy) root.deleteRequested()
                }
            }

            // Save. The label stays "Save" in create mode too — that is the
            // reference (FolderEditModal.slint:246); the small sidebar create
            // panel is the one that says "Create".
            QbzPrimaryButton {
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                label: root.t("Save")
                btnHeight: 36
                labelSize: 15
                btnEnabled: root.canSave
                onClicked: QbzFolderEdit.editSave(root.draftName, root.draftPreset,
                                                  root.draftColor, root.draftHidden)
            }
        }
    }
}
