// AddToMixtapeModal — the app-wide "Add to Mixtape/Collection" picker
// (myqbz/AddToMixtapeModal.slint, 553 lines). A GLOBAL singleton mounted ONCE
// in AppShell.qml (Slint mounts it the same way at AppShell.slint:824) and
// self-gated on its own document's `open` flag, so every caller only has to
// invoke `QbzMyQbzAdd.open(itemsJson)`.
//
// It carries its OWN backdrop and card — NOT the shared modal chrome (:5) —
// and TWO mutually exclusive body panels: the PICKER (search row + collection
// list + the two create chips) and the CREATE-NEW panel (name + kind +
// footer).
//
// Chrome (:209-231): scrim `#00000099` that closes on click, card
// `min(width - 80, 480) x min(height - 120, 560)` centred, r12 surface-card,
// 1px border-subtle, and a click-swallowing area over the card so a press
// inside it never reaches the scrim. The scrim carries `radius:
// theme.radiusMd` for the reason controls/DiscoverConfigModal.qml:59-75 spells
// out: QML `clip` is a rectangular scissor, so an opaque full-bleed child
// paints into the shell frame's four bezel corners and the panel reads square.
// The .slint's `drop-shadow-blur: 32px` is dropped — shadows render nothing on
// this port's software path; the 1px border carries the separation.
//
// The header's title/subtitle are PRECOMPUTED IN RUST (single item ->
// title+subtitle; bulk -> "{count} items" + "{first} + N more"), so this file
// never inspects the pending items.
//
// WHAT MAY GO WHERE (R1) IS ALSO RUST'S ANSWER, not a rule this file re-derives
// — see myqbz_add_qt.rs `payload_accepts`. The picker LIST arrives already
// filtered (a container that cannot take the payload is never a row), and the
// two creatable kinds arrive as `allowMixtape` / `allowCollection`, which gate
// the footer chips AND the create-panel radios. That second half matters: with
// the old single `restrictToMixtape` flag the chips narrowed to one but the
// create panel still showed BOTH radios, so a track payload could still be
// flipped to "Collections" and written into one. Nothing here ever renders an
// action the payload cannot perform, which is the whole point — the backend
// block is a safety net that must stay invisible.
//
// The name typed into the create panel is QML-LOCAL: the bridge takes it as an
// argument (`createAndAdd(kind, name)`) and has no name property. Same for the
// create KIND, which the two radios flip locally over the doc's value — there
// is no `setCreateKind` invokable, and re-calling `showCreate` for a radio tap
// would be a round trip for a value the submit call already carries.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // Guarded parse — a raw JSON.parse in a binding throws on the pre-publish
    // frame and takes the whole overlay down (PlaylistView.qml:55-61).
    readonly property var doc: {
        try {
            return JSON.parse(QbzMyQbzAdd.addJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property bool opened: root.doc.open === true
    readonly property bool loading: root.doc.loading === true
    readonly property bool creating: root.doc.creating === true
    readonly property bool createBusy: root.doc.createBusy === true
    // R1, as Rust computed it for the pending payload. Both may be true, one
    // may be true; both false cannot reach a visible modal (Rust refuses to
    // open the picker when nothing accepts the payload).
    readonly property bool allowMixtape: root.doc.allowMixtape === true
    readonly property bool allowCollection: root.doc.allowCollection === true
    readonly property string busyId: root.doc.busyId || ""
    readonly property string search: root.doc.search || ""
    readonly property var rows: root.doc.rows || []

    // Local create-panel state (see the header).
    property string createName: ""
    property string createKindOverride: ""
    readonly property string docCreateKind: root.doc.createKind || "mixtape"
    readonly property string createKind: root.createKindOverride !== ""
        ? root.createKindOverride : root.docCreateKind
    readonly property bool createMixtape: root.createKind === "mixtape"
    // R1 for the kind the panel is currently on. The forbidden radio is not
    // rendered, so this can only go false on a stale frame — but Create & Add
    // must not be pressable in that frame either.
    readonly property bool createKindAllowed: root.createKind === "collection"
        ? root.allowCollection : root.allowMixtape
    // :207 — the raw non-empty check, exactly as Slint gates it (Rust trims).
    readonly property bool canCreate: root.createName !== "" && !root.createBusy
        && root.createKindAllowed

    // A fresh panel starts empty and follows the kind Rust was asked for; a
    // closed modal must not keep a half-typed name for the next caller.
    onDocCreateKindChanged: root.createKindOverride = ""
    onCreatingChanged: if (root.creating) root.createName = ""
    onOpenedChanged: if (!root.opened) { root.createName = ""; root.createKindOverride = "" }

    visible: root.opened
    enabled: root.opened

    // --- Scrim. Declared FIRST so the card (later in source order, hence
    // above it) keeps its own presses.
    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#99000000"
        MouseArea {
            anchors.fill: parent
            onClicked: QbzMyQbzAdd.close()
        }
    }

    Rectangle {
        id: card
        width: Math.min(root.width - 80, 480)
        height: Math.min(root.height - 120, 560)
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        clip: true

        // Swallow clicks so they never reach the scrim.
        MouseArea { anchors.fill: parent }

        // ======================= HEADER (always) ======================
        Item {
            id: header
            width: parent.width
            // :235 — the row's own height plus 20 top + 16 bottom.
            height: Math.max(hdrText.implicitHeight, 28) + 36

            Column {
                id: hdrText
                x: 20
                y: 20
                width: Math.max(0, card.width - 40 - 28)
                spacing: 2

                Text {
                    text: root.t("ADD TO MIXTAPE/COLLECTION")
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.5
                }
                Text {
                    width: parent.width
                    text: root.doc.headerTitle || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightBold
                    elide: Text.ElideRight
                }
                Text {
                    visible: (root.doc.headerSubtitle || "") !== ""
                    width: parent.width
                    text: root.doc.headerSubtitle || ""
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }
            Item {
                x: card.width - 20 - 28
                y: 20
                width: 28
                height: 28
                QbzIcon {
                    anchors.centerIn: parent
                    name: "x"
                    width: 17
                    height: 17
                    tintName: hdrCloseArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: hdrCloseArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzMyQbzAdd.close()
                }
            }
            // Bottom border on the block's LAST pixel (:285-289).
            Rectangle {
                y: parent.height - 1
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }
        }

        // ======================== PICKER PANEL ========================
        Item {
            id: picker
            visible: !root.creating
            anchors.top: header.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom

            // --- Search row (:295-361). The leading glyph sits OUTSIDE the
            // box, which is unique to this modal — QbzLineEdit puts its
            // magnifier inside — so the box is inlined here rather than
            // forking the shared control.
            Item {
                id: searchRow
                width: parent.width
                height: 46

                QbzIcon {
                    x: 20
                    anchors.verticalCenter: searchBox.verticalCenter
                    name: "search"
                    width: 14
                    height: 14
                    tintName: "muted"
                }
                Rectangle {
                    id: searchBox
                    x: 20 + 14 + 8
                    width: Math.max(0, parent.width - x - 20)
                    height: 43
                    y: Math.round((parent.height - height) / 2)
                    radius: theme.radiusSm
                    color: theme.surfaceCard
                    border.width: 1
                    border.color: searchInput.activeFocus ? theme.accent : theme.borderSubtle

                    TextInput {

                        QbzTextEditMenu { }
                        id: searchInput
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        color: theme.textPrimary
                        font.pixelSize: theme.fontLink
                        verticalAlignment: Text.AlignVCenter
                        clip: true
                        selectByMouse: true
                        text: root.search
                        onTextEdited: QbzMyQbzAdd.search(text)
                        // A republish re-seeds the field while it is not being
                        // edited (QbzLineEdit.qml:167-172).
                        Binding {
                            target: searchInput
                            property: "text"
                            value: root.search
                            when: !searchInput.activeFocus
                        }
                    }
                    Text {
                        visible: searchInput.text === ""
                        anchors.fill: searchInput
                        text: root.t("Search…")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLink
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
                Rectangle {
                    y: parent.height - 1
                    width: parent.width
                    height: 1
                    color: theme.borderSubtle
                }
            }

            // --- Footer: the two create chips (:403-432). 60px, elevated, a
            // 1px rule at its TOP (:406-409 puts it at y=0), padding 12.
            Rectangle {
                id: pickerFooter
                anchors.bottom: parent.bottom
                width: parent.width
                height: 60
                color: theme.surfaceElevated

                Rectangle {
                    width: parent.width
                    height: 1
                    color: theme.borderSubtle
                }
                Row {
                    anchors.fill: parent
                    anchors.margins: 12
                    spacing: 8
                    // Both chips carry `horizontal-stretch: 1`, so one fills
                    // the row and two split it. R1 decides how many there are:
                    // one chip per CREATABLE container, never zero (a payload
                    // nothing accepts never opened this modal). The guard on
                    // `chips > 0` is only so a pre-publish frame cannot divide
                    // by zero.
                    readonly property int chips:
                        (root.allowMixtape ? 1 : 0) + (root.allowCollection ? 1 : 0)
                    readonly property real chipW:
                        chips > 0 ? (width - (chips - 1) * 8) / chips : 0

                    FooterIconButton {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.allowMixtape
                        width: visible ? parent.chipW : 0
                        label: root.t("Mixtapes")
                        onClicked: QbzMyQbzAdd.showCreate("mixtape")
                    }
                    FooterIconButton {
                        // Offered only when a Collection can take the payload:
                        // albums / singles / EPs, from any source. Tracks and
                        // playlists have no Collection slot (R1).
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.allowCollection
                        width: visible ? parent.chipW : 0
                        label: root.t("Collections")
                        onClicked: QbzMyQbzAdd.showCreate("collection")
                    }
                }
            }

            // --- Between the two: spinner / empty band / the list.
            Item {
                anchors.top: searchRow.bottom
                anchors.bottom: pickerFooter.top
                width: parent.width

                // Loading (:364-368): centred, 24px padding, 30px spinner.
                QbzSpinner {
                    visible: root.loading
                    anchors.horizontalCenter: parent.horizontalCenter
                    y: 24
                    size: 30
                }
                // Empty (:370-381).
                Text {
                    visible: !root.loading && root.rows.length === 0
                    width: parent.width
                    height: 80
                    text: root.search !== "" ? root.t("No matches.")
                        : root.t("No mixtapes or collections yet.")
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }
                // List (:384-400).
                ListView {
                    id: pickList
                    visible: !root.loading && root.rows.length > 0
                    // The .slint's `padding: 8` on the list layout, on all
                    // FOUR sides. The horizontal half is an ANCHOR margin on
                    // the view, never an `x` on the delegate: a vertical
                    // ListView writes its delegates' `x`, so a literal `x: 8`
                    // there is silently clobbered to 0 and the rows sat flush
                    // against the card's left border with a 16px gap opening
                    // on the right (the hover fill and the zebra band touched
                    // the border on one side only). The vertical half stays a
                    // Flickable content margin.
                    anchors.fill: parent
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    topMargin: 8
                    bottomMargin: 8
                    spacing: 2
                    cacheBuffer: 200
                    reuseItems: true
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    model: root.rows

                    delegate: AddPickRow {
                        required property var modelData
                        required property int index
                        width: pickList.width
                        rowData: modelData
                        rowIndex: index
                    }
                }
            }
        }

        // ====================== CREATE-NEW PANEL ======================
        Item {
            visible: root.creating
            anchors.top: header.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom

            Column {
                id: createTop
                x: 20
                y: 20
                width: Math.max(0, parent.width - 40)
                spacing: 16

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
                    Rectangle {
                        width: parent.width
                        height: 43
                        radius: theme.radiusSm
                        color: theme.surfaceCard
                        border.width: 1
                        border.color: nameInput.activeFocus ? theme.accent : theme.borderSubtle

                        TextInput {

                            QbzTextEditMenu { }
                            id: nameInput
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            enabled: !root.createBusy
                            color: theme.textPrimary
                            font.pixelSize: theme.fontLink
                            verticalAlignment: Text.AlignVCenter
                            clip: true
                            selectByMouse: true
                            // LOAD-BEARING, and the exact shape `searchInput`
                            // above uses (:242 + its Binding). With ONLY the
                            // focus-gated Binding below, a re-entered create
                            // panel kept the previously typed name on screen
                            // while `createName` had already been reset to ""
                            // — the field never re-seeds because it keeps
                            // activeFocus across the hide/show (the panel is
                            // only `visible: false` for a frame, focus comes
                            // straight back), so the `when:` guard never
                            // fires. `canCreate` was then false with a name
                            // visibly in the box: "Create & Add" sat at 50 %
                            // with a dead MouseArea and Enter did nothing.
                            // Repros: chip -> type -> Back -> chip; and
                            // type -> close -> reopen into the create panel.
                            // A declarative `text:` survives user editing (the
                            // edit does not break the binding), and
                            // `onTextEdited` writes the same value straight
                            // back, so typing is unaffected.
                            text: root.createName
                            onTextEdited: root.createName = text
                            onAccepted: {
                                if (root.canCreate)
                                    QbzMyQbzAdd.createAndAdd(root.createKind, root.createName)
                            }
                            Binding {
                                target: nameInput
                                property: "text"
                                value: root.createName
                                when: !nameInput.activeFocus
                            }
                        }
                        Text {
                            visible: nameInput.text === ""
                            anchors.fill: nameInput
                            // NOT translated — verbatim literals in the .slint
                            // (:482).
                            text: root.createMixtape ? "90s Cassettes" : "Jazz Library"
                            color: theme.textMuted
                            font.pixelSize: theme.fontLink
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                }

                // --- KIND (16px between the radios here; the Create modal
                // uses 12 — CreateMyQbzModal.slint:150).
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
                    // R1 again, and this is the half the old flag missed: a
                    // radio for a kind the payload cannot go into is NOT
                    // rendered. A Row skips invisible children, so the single
                    // remaining (and already selected) radio simply reads as
                    // the kind being created.
                    Row {
                        spacing: 16
                        QbzRadioOption {
                            visible: root.allowMixtape
                            label: root.t("Mixtapes")
                            selected: root.createMixtape
                            onClicked: if (!root.createBusy) root.createKindOverride = "mixtape"
                        }
                        QbzRadioOption {
                            visible: root.allowCollection
                            label: root.t("Collections")
                            selected: !root.createMixtape
                            onClicked: if (!root.createBusy) root.createKindOverride = "collection"
                        }
                    }
                }
            }

            // Footer, pushed to the bottom by the .slint's vertical stretch
            // spacer (:526).
            Row {
                anchors.right: parent.right
                anchors.rightMargin: 20
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 20
                spacing: 8

                SettingsButton {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.t("Back")
                    btnHeight: 38
                    minWidth: 0
                    enabled: !root.createBusy
                    onClicked: QbzMyQbzAdd.createBack()
                }
                QbzPrimaryButton {
                    anchors.verticalCenter: parent.verticalCenter
                    label: root.t("Create & Add")
                    btnHeight: 38
                    btnEnabled: root.canCreate
                    onClicked: QbzMyQbzAdd.createAndAdd(root.createKind, root.createName)
                }
            }
        }
    }

    // One collection row in the picker list (:63-158). 52px, r8, zebra on ODD
    // indices, dimmed and inert while ANOTHER row is being written to.
    component AddPickRow: Rectangle {
        id: pickRow
        // NOT `data`: Item already owns a `data` default property.
        property var rowData: ({})
        property int rowIndex: 0
        readonly property bool busy: root.busyId !== ""

        height: 52
        radius: 8
        opacity: pickRow.busy ? 0.5 : 1.0
        // TrackRow.qml:200-206's exact pair: the hover is the polarity-baked
        // alpha ramp (so it is visible on light themes too) with the
        // pre-publish fallback, and the zebra stays the dark literal.
        color: (pickHover.containsMouse && !pickRow.busy)
            ? (theme.alphaTiers.length > 0 ? theme.alphaTier(8)
                : (theme.isDark ? "#14ffffff" : "#14000000"))
            : (pickRow.rowIndex % 2 === 1 ? "#07ffffff" : "transparent")

        MouseArea {
            id: pickHover
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: pickRow.busy ? Qt.ArrowCursor : Qt.PointingHandCursor
            onClicked: if (!pickRow.busy) QbzMyQbzAdd.pick(pickRow.rowData.id)
        }

        Row {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 12
            spacing: 12

            Item {
                anchors.verticalCenter: parent.verticalCenter
                width: 32
                height: 32
                QbzIcon {
                    anchors.centerIn: parent
                    // Rust resolves the name (myqbz_add.rs:203-209).
                    name: pickRow.rowData.icon === "user" ? "user"
                        : pickRow.rowData.icon === "library-big" ? "library-big"
                        : "cassette-tape"
                    width: 18
                    height: 18
                    tintName: "muted"
                }
            }
            Column {
                anchors.verticalCenter: parent.verticalCenter
                width: Math.max(0, parent.width - 32 - 12 - kindTag.width - 12)
                Text {
                    width: parent.width
                    text: pickRow.rowData.name || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Row {
                    spacing: 6
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: pickRow.rowData.meta || ""
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        verticalAlignment: Text.AlignVCenter
                    }
                    // "Already added" — the collection already holds EVERY
                    // pending item (Rust's item_exists pass).
                    Row {
                        visible: pickRow.rowData.alreadyHas === true
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 3
                        QbzIcon {
                            anchors.verticalCenter: parent.verticalCenter
                            name: "circle-check-big"
                            width: 12
                            height: 12
                            tintName: "accent"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.t("Already added")
                            color: theme.accent
                            font.pixelSize: theme.fontLegal
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
                }
            }
            Text {
                id: kindTag
                anchors.verticalCenter: parent.verticalCenter
                text: pickRow.rowData.kindLabel || ""
                color: theme.textMuted
                font.pixelSize: 11
                font.weight: theme.weightSemibold
                font.letterSpacing: 1.2
            }
        }
    }

    // The "+ Mixtapes" / "+ Collections" footer chips (:164-201): a 38px
    // labelled icon button with a CENTRED [glyph · gap · label] group. Neither
    // SettingsButton (34px, 160 min width) nor QbzToolButton (30px) is this
    // shape, and both call sites are in this file, so it stays local.
    component FooterIconButton: Rectangle {
        id: chip
        property string label: ""
        signal clicked()

        height: 38
        radius: theme.radiusSm
        color: chipArea.containsMouse ? theme.surfaceHover : theme.surfaceElevated
        border.width: 1
        border.color: theme.borderSubtle

        Row {
            anchors.centerIn: parent
            spacing: 6
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: "plus"
                width: 14
                height: 14
                tintName: "textPrimary"
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                // The leading `+` is the GLYPH above, not part of the msgid.
                text: chip.label
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
                font.weight: theme.weightSemibold
                verticalAlignment: Text.AlignVCenter
            }
        }
        MouseArea {
            id: chipArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: chip.clicked()
        }
    }
}
