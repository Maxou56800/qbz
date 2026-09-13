// "Add to playlist" picker — QML port of
// crates/qbz-ui/ui/primitives/PlaylistPickerModal.slint (+ its
// PlaylistDuplicateConfirmModal sibling, folded in here because the two
// share one document and one carried track set).
//
// A focus-opened dropdown lists the user's playlists; clicking a row only
// SELECTS it (the selection becomes the input's placeholder); the footer
// button fires the actual add. "+ Create new playlist" is always the first
// dropdown row and reveals an inline name field confirmed by the same
// footer button ("Create & Add").
//
// Mounted ONCE in AppShell next to AddToMixtapeModal: every host that opens
// it (the queue footer and row menu today) can be unmounted by the time the
// user picks a target, so the modal must outlive them.
//
// Data: QbzPlaylistPicker.pickerJson (playlist_picker_qt.rs PickerDoc). The
// name filter runs in RUST (the QbzMyQbzAdd.search convention), so the row
// list arrives already filtered; the 8-row window and the overflow line are
// applied here against `total`/`playlists.length`.
//
// LOCAL playlists are listed alongside the Qobuz ones (a hard-drive glyph
// leads the row, `isLocal` in the document) and are the ONLY rows while
// offline — a Qobuz playlist cannot be written to without a session, so
// offering one would be a dead target (D3/D11). The carried payload is
// likewise either Qobuz catalog ids or LocalLibrary refs; `localMode` only
// changes the counter's wording ("Adding {} local track(s)"), never the row
// list — a local selection can legitimately target a Qobuz playlist, which
// Rust routes through the library.db sidecar.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    // An unopened overlay is an invisible, non-interactive Item and costs
    // nothing (the AppShell mount comment states the contract).
    visible: doc.open === true || dup.open === true

    readonly property var doc: JSON.parse(QbzPlaylistPicker.pickerJson)
    readonly property var rows: doc.playlists || []
    readonly property var dup: doc.dup || ({})
    readonly property int trackCount: doc.trackCount || 0
    // Session-local pinned target (last successful add); null when unset,
    // deleted, unwritable, or filtered out by the current query.
    readonly property var lastUsed: doc.lastUsed || null
    // Containment strip: writable playlists already holding carried tracks,
    // honest per alreadyState ("loading"|"complete"|"updating"|"unavailable").
    readonly property var alreadyIn: doc.alreadyIn || []
    readonly property string alreadyState: doc.alreadyState || ""
    // The column renders only when it can back a claim: complete with
    // matches, or updating (where partial matches show under an explicit
    // caveat). It is a SIDE column, not an in-flow strip, so the dropdown
    // can never cover it.
    readonly property bool alreadyShowing:
        root.alreadyState === "updating"
        || (root.alreadyState === "complete" && root.alreadyIn.length > 0)
    // The carried payload is LocalLibrary refs (counter wording only).
    readonly property bool localMode: doc.localMode === true
    readonly property bool creatingOpen: doc.creatingOpen === true
    readonly property bool creating: doc.creating === true

    // Rows past the 8-row window collapse into the "{} more playlist(s)..."
    // line, exactly as the .slint's filter-rank window does.
    readonly property int windowSize: 8
    readonly property int shown: Math.min(rows.length, windowSize)
    readonly property int overflow: Math.max(0, rows.length - windowSize)

    // --- Local selection state (the .slint keeps these on the `if open:`
    // subtree so a remount resets them; here they are reset when `open`
    // flips, which is the same lifetime without a conditional Loader). ---
    property bool dropdownOpen: false
    property string selectedId: ""
    property string selectedLabel: ""
    property string filterText: ""
    property string createName: ""

    onVisibleChanged: if (!visible) resetLocals()
    function resetLocals() {
        dropdownOpen = false
        selectedId = ""
        selectedLabel = ""
        filterText = ""
        createName = ""
    }

    function selectRow(row) {
        selectedId = row.id
        selectedLabel = (row.tracksLine || "") === ""
            ? row.name : row.name + " (" + row.tracksLine + ")"
        // The reference clears the query on selection (Tauri §2.4).
        filterText = ""
        QbzPlaylistPicker.search("")
        QbzPlaylistPicker.showCreate(false)
        dropdownOpen = false
    }

    function confirm() {
        if (root.creating)
            return
        if (root.creatingOpen) {
            if (createName.trim() !== "")
                QbzPlaylistPicker.createAndAdd(createName)
        } else if (selectedId !== "") {
            QbzPlaylistPicker.pick(selectedId)
        }
    }

    QbzTheme { id: theme }

    // ======================= The picker ==================================
    Item {
        anchors.fill: parent
        visible: root.doc.open === true && root.dup.open !== true

        // Scrim — click outside dismisses. The wheel is eaten too: these
        // overlays are plain Items, not Popups, so no modal grab stops the
        // page underneath from scrolling (the DiscoverConfigModal contract).
        Rectangle {
            anchors.fill: parent
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzPlaylistPicker.close()
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        Rectangle {
            id: card
            anchors.centerIn: parent
            width: Math.min(parent.width - 80, 735)
            // Tauri: min-height 400 / max-height 90vh. The body stretches so
            // the footer pins to the bottom.
            height: Math.min(Math.max(400, panel.implicitHeight), parent.height * 0.9)
            radius: theme.radiusLg
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            // NO clip: the dropdown legitimately overflows the card bottom.

            // The "Already in" side column's slot. Zero when absent, so the
            // main content takes the whole card.
            readonly property real rightPanelW: root.alreadyShowing ? 221 : 0

            // Swallow clicks so they never reach the scrim; doubles as
            // "a click inside the card closes the list" (Tauri parity).
            // The wheel stops here too — same reason as the scrim.
            MouseArea {
                anchors.fill: parent
                onClicked: root.dropdownOpen = false
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: panel
                width: parent.width

                // --- Header -------------------------------------------
                Item {
                    width: parent.width
                    height: 68
                    Text {
                        anchors.left: parent.left
                        anchors.leftMargin: 24
                        anchors.right: closeX.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Add to Playlist", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Item {
                        id: closeX
                        anchors.right: parent.right
                        anchors.rightMargin: 24
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "x"
                            width: 17
                            height: 17
                            tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzPlaylistPicker.close()
                        }
                    }
                }
                Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

                // --- Body (the MAIN column; the "Already in" side column
                // takes the right slot when present) -------------------
                Column {
                    id: body
                    width: parent.width - card.rightPanelW
                    padding: 24
                    spacing: 16

                    // Loading — the playlists are still on the wire.
                    Item {
                        width: parent.width - 48
                        height: visible ? 30 : 0
                        visible: root.doc.loading === true
                        QbzSpinner {
                            anchors.horizontalCenter: parent.horizontalCenter
                            size: 30
                        }
                    }

                    Column {
                        width: parent.width - 48
                        spacing: 8

                        Item {
                            width: parent.width
                            height: 20
                            Text {
                                anchors.left: parent.left
                                anchors.verticalCenter: parent.verticalCenter
                                text: QbzSession.tr("Choose playlist", QbzSession.trRev)
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightMedium
                            }
                            // Two msgids, not one with an inserted word: the
                            // .slint counter switches the whole string on
                            // `local-mode` so every locale can order it its
                            // own way.
                            Text {
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                text: (root.localMode
                                        ? QbzSession.tr("Adding {} local track(s)", QbzSession.trRev)
                                        : QbzSession.tr("Adding {} track(s)", QbzSession.trRev))
                                      .replace("{}", root.trackCount)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLink
                            }
                        }

                        // The search / selection input (43px, the Tauri metric).
                        Rectangle {
                            id: inputBox
                            width: parent.width
                            height: 43
                            radius: theme.radiusSm
                            color: theme.surfaceCard
                            border.width: 1
                            border.color: searchInput.activeFocus ? theme.accent : theme.borderSubtle

                            TextInput {

                                QbzTextEditMenu { }
                                id: searchInput
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 36 // room for the clear chip
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLink
                                verticalAlignment: Text.AlignVCenter
                                clip: true
                                selectByMouse: true
                                enabled: root.doc.loading !== true
                                text: root.filterText
                                onTextEdited: {
                                    root.filterText = text
                                    root.dropdownOpen = true // typing re-opens
                                    QbzPlaylistPicker.search(text)
                                }
                                onAccepted: root.confirm()
                                // Tauri: the list opens on focus.
                                onActiveFocusChanged: if (activeFocus) root.dropdownOpen = true
                                // Re-seed while not being edited (the
                                // AddToMixtapeModal Binding shape).
                                Binding {
                                    target: searchInput
                                    property: "text"
                                    value: root.filterText
                                    when: !searchInput.activeFocus
                                }
                            }
                            // Placeholder, doubling as the SELECTION label —
                            // the filter is cleared on select, so the field is
                            // empty exactly when the label should show.
                            Text {
                                visible: searchInput.text === ""
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 36
                                verticalAlignment: Text.AlignVCenter
                                text: root.creatingOpen
                                    ? QbzSession.tr("+ Create new playlist", QbzSession.trRev)
                                    : (root.selectedLabel !== ""
                                        ? root.selectedLabel
                                        : QbzSession.tr("Search playlists...", QbzSession.trRev))
                                color: theme.textMuted
                                font.pixelSize: theme.fontLink
                                elide: Text.ElideRight
                            }
                            // Clear-selection chip — only with a selection (or
                            // the inline-create state) and the list closed.
                            Rectangle {
                                visible: (root.selectedId !== "" || root.creatingOpen)
                                    && !root.dropdownOpen
                                anchors.right: parent.right
                                anchors.rightMargin: 10
                                anchors.verticalCenter: parent.verticalCenter
                                width: 20
                                height: 20
                                radius: 10
                                color: chipArea.containsMouse ? theme.surfaceHover : theme.surfaceElevated
                                QbzIcon {
                                    anchors.centerIn: parent
                                    name: "x"
                                    width: 12
                                    height: 12
                                    tintName: chipArea.containsMouse ? "textPrimary" : "muted"
                                }
                                MouseArea {
                                    id: chipArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        root.selectedId = ""
                                        root.selectedLabel = ""
                                        root.createName = ""
                                        QbzPlaylistPicker.showCreate(false)
                                    }
                                }
                            }
                        }
                    }

                    // Inline create — a second form group BELOW the picker
                    // (Tauri §1.4.6); the footer button confirms it.
                    Column {
                        width: parent.width - 48
                        spacing: 8
                        visible: root.creatingOpen
                        Text {
                            text: QbzSession.tr("Playlist Name", QbzSession.trRev)
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            font.weight: theme.weightMedium
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
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLink
                                verticalAlignment: Text.AlignVCenter
                                clip: true
                                selectByMouse: true
                                enabled: !root.creating
                                text: root.createName
                                onTextEdited: root.createName = text
                                onAccepted: root.confirm()
                                Binding {
                                    target: nameInput
                                    property: "text"
                                    value: root.createName
                                    when: !nameInput.activeFocus
                                }
                            }
                            Text {
                                visible: nameInput.text === ""
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                verticalAlignment: Text.AlignVCenter
                                text: QbzSession.tr("My Playlist", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLink
                                elide: Text.ElideRight
                            }
                        }
                    }

                }
            }

            // --- "Already in" side column -----------------------------
            // The containment answer as a right-hand column instead of an
            // in-flow strip: the dropdown overlays the MAIN column only, so
            // this list stays readable while the playlist list is open.
            // Shown if and only if the index can back a claim (complete with
            // matches, or updating under the explicit caveat) — an empty
            // answer from an incomplete index renders nothing.
            Item {
                visible: root.alreadyShowing
                anchors.top: parent.top
                anchors.topMargin: 69
                anchors.bottom: footerDivider.top
                anchors.right: parent.right
                width: card.rightPanelW

                Rectangle {
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 1
                    color: theme.borderSubtle
                }

                Column {
                    anchors.fill: parent
                    anchors.leftMargin: 17
                    anchors.rightMargin: 16
                    anchors.topMargin: 16
                    spacing: 8

                    Text {
                        text: QbzSession.tr("Already in", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightMedium
                    }
                    Text {
                        visible: root.alreadyState === "updating"
                        width: parent.width
                        text: QbzSession.tr("Playlist index updating...", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.italic: true
                        wrapMode: Text.WordWrap
                    }

                    ListView {
                        width: parent.width
                        height: Math.max(0, parent.height - y - 12)
                        clip: true
                        spacing: 2
                        boundsBehavior: Flickable.StopAtBounds
                        model: root.alreadyIn
                        delegate: Rectangle {
                            required property var modelData
                            width: parent ? parent.width : 0
                            height: 34
                            radius: theme.radiusSm
                            color: alreadyArea.containsMouse
                                ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                id: alreadyGlyph
                                visible: modelData.isLocal === true
                                anchors.left: parent.left
                                anchors.leftMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                name: "hard-drive"
                                width: 12
                                height: 12
                                tintName: "muted"
                            }
                            Text {
                                anchors.left: alreadyGlyph.visible
                                    ? alreadyGlyph.right : parent.left
                                anchors.leftMargin: 6
                                anchors.right: alreadyCount.visible
                                    ? alreadyCount.left : parent.right
                                anchors.rightMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                text: modelData.name
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideRight
                            }
                            // The multi-track answer: "2 of 5". A full or
                            // single-track containment says it by presence.
                            Text {
                                id: alreadyCount
                                visible: root.trackCount > 1
                                    && modelData.contained < root.trackCount
                                anchors.right: parent.right
                                anchors.rightMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                text: QbzSession.tr("{} of {}", QbzSession.trRev)
                                      .replace("{}", modelData.contained)
                                      .replace("{}", root.trackCount)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                            }
                            MouseArea {
                                id: alreadyArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                // Selecting it is the useful action: "add the
                                // missing 3 of 5" is one click plus the
                                // dup-confirm's only-new answer.
                                onClicked: root.selectRow(modelData)
                            }
                        }
                    }
                }
            }

            // --- Footer, pinned to the card bottom --------------------
            // Keep a real footer band around the modal-standard 36px
            // controls. The old divider was anchored directly to the Row's
            // top edge, so both buttons physically touched the separator.
            Item {
                id: footerBar
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: 69

                Row {
                    id: footerRow
                    anchors.right: parent.right
                    anchors.rightMargin: 24
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 10

                    SettingsButton {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Cancel", QbzSession.trRev)
                        btnHeight: 36
                        minWidth: 0
                        onClicked: QbzPlaylistPicker.close()
                    }
                    // The single accent confirm (ADR-008). Disabled until
                    // valid; QbzPrimaryButton supplies the active theme's
                    // primary fill and matching on-accent label colour.
                    QbzPrimaryButton {
                        anchors.verticalCenter: parent.verticalCenter
                        btnHeight: 36
                        labelSize: 15
                        label: root.creating
                            ? QbzSession.tr("Saving...", QbzSession.trRev)
                            : (root.creatingOpen
                                ? QbzSession.tr("Create & Add", QbzSession.trRev)
                                : QbzSession.tr("Add", QbzSession.trRev))
                        btnEnabled: !root.creating
                            && (root.creatingOpen
                                ? root.createName.trim() !== ""
                                : root.selectedId !== "")
                        onClicked: root.confirm()
                    }
                }
            }
            Rectangle {
                id: footerDivider
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: footerBar.top
                height: 1
                color: theme.borderSubtle
            }

            // --- Dropdown overlay -------------------------------------
            // Declared LAST inside the card so it paints above the body and
            // the footer (declaration order IS z-order in QML), and it
            // legitimately overflows the unclipped card bottom.
            Rectangle {
                id: dropdown
                visible: root.dropdownOpen && root.doc.loading !== true
                x: 24
                // Anchored below the input by static arithmetic over fixed
                // metrics — never an absolute-position binding: header 68 +
                // 1 divider + 24 body pad + 20 label row + 8 spacing +
                // 43 input + 4 gap.
                y: 68 + 1 + 24 + 20 + 8 + 43 + 4
                // MAIN column only — the "Already in" side column stays
                // readable while the list is open (that is its whole point).
                width: parent.width - card.rightPanelW - 48
                // create row 41 + 1 divider, pinned last-used 41 + 1, 41 per
                // shown row, 41 no-results, 1 + 34 overflow. Tauri max-height
                // 320 + border.
                height: Math.min(2 + 42 + (root.lastUsed !== null ? 42 : 0)
                                 + root.shown * 41
                                 + ((root.rows.length === 0 && root.filterText !== "") ? 41 : 0)
                                 + (root.overflow > 0 ? 35 : 0), 322)
                radius: theme.radiusSm
                color: theme.surfaceCard
                border.width: 1
                border.color: theme.borderSubtle
                clip: true

                // Keep clicks on the list chrome from reaching the card
                // swallower, which would close the list under the pointer.
                MouseArea { anchors.fill: parent }

                Flickable {
                    anchors.fill: parent
                    anchors.margins: 1
                    contentWidth: width
                    contentHeight: dropInner.implicitHeight
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds

                    Column {
                        id: dropInner
                        width: parent.width

                        // "+ Create new playlist" — ALWAYS first, never
                        // filtered out (Tauri parity).
                        Rectangle {
                            width: parent.width
                            height: 41
                            color: createArea.containsMouse ? theme.surfaceHover : "transparent"
                            Text {
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 12
                                verticalAlignment: Text.AlignVCenter
                                text: QbzSession.tr("+ Create new playlist", QbzSession.trRev)
                                color: theme.accent
                                font.pixelSize: theme.fontLink
                                font.weight: theme.weightMedium
                                elide: Text.ElideRight
                            }
                            MouseArea {
                                id: createArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.selectedId = ""
                                    root.selectedLabel = ""
                                    root.filterText = ""
                                    QbzPlaylistPicker.search("")
                                    QbzPlaylistPicker.showCreate(true)
                                    root.dropdownOpen = false
                                }
                            }
                        }
                        Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

                        // Session's last successful target, pinned above the
                        // results. Rust already validated it (still listed,
                        // still writable) and de-duplicated it from `rows`;
                        // under a search it only survives a matching query.
                        Rectangle {
                            visible: root.lastUsed !== null
                            width: parent.width
                            height: visible ? 41 : 0
                            color: lastUsedArea.containsMouse ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                id: lastUsedGlyph
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.verticalCenter: parent.verticalCenter
                                name: "clock"
                                width: 14
                                height: 14
                                tintName: "muted"
                            }
                            Text {
                                anchors.left: lastUsedGlyph.right
                                anchors.leftMargin: 8
                                anchors.right: lastUsedCaption.left
                                anchors.rightMargin: 8
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.lastUsed ? root.lastUsed.name : ""
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLink
                                elide: Text.ElideRight
                            }
                            Text {
                                id: lastUsedCaption
                                anchors.right: parent.right
                                anchors.rightMargin: 12
                                anchors.verticalCenter: parent.verticalCenter
                                text: QbzSession.tr("Last playlist", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                font.italic: true
                            }
                            MouseArea {
                                id: lastUsedArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.selectRow(root.lastUsed)
                            }
                        }
                        Rectangle {
                            visible: root.lastUsed !== null
                            width: parent.width
                            height: visible ? 1 : 0
                            color: theme.borderSubtle
                        }

                        Repeater {
                            model: root.rows.slice(0, root.windowSize)
                            delegate: Rectangle {
                                required property var modelData
                                width: parent ? parent.width : 0
                                height: 41
                                color: rowArea.containsMouse ? theme.surfaceHover : "transparent"
                                // LOCAL playlists (library.db) keep a small
                                // hard-drive glyph LEADING the name, muted —
                                // PlaylistPickerModal.slint:62-69. It is what
                                // tells a local target apart from a Qobuz one
                                // in a list that mixes both.
                                QbzIcon {
                                    id: localGlyph
                                    visible: modelData.isLocal === true
                                    anchors.left: parent.left
                                    anchors.leftMargin: 12
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "hard-drive"
                                    width: 14
                                    height: 14
                                    tintName: "muted"
                                }
                                Text {
                                    anchors.left: localGlyph.visible ? localGlyph.right : parent.left
                                    anchors.leftMargin: localGlyph.visible ? 8 : 12
                                    anchors.right: countText.left
                                    anchors.rightMargin: 8
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.name
                                    color: theme.textPrimary
                                    font.pixelSize: theme.fontLink
                                    elide: Text.ElideRight
                                }
                                Text {
                                    id: countText
                                    anchors.right: parent.right
                                    anchors.rightMargin: 12
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.tracksLine || ""
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                }
                                MouseArea {
                                    id: rowArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.selectRow(modelData)
                                }
                            }
                        }

                        Text {
                            visible: root.rows.length === 0 && root.filterText !== ""
                            width: parent.width
                            height: visible ? 41 : 0
                            text: QbzSession.tr("No playlists found", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        // Overflow line — the rows past the 8-row window.
                        Rectangle {
                            visible: root.overflow > 0
                            width: parent.width
                            height: visible ? 1 : 0
                            color: theme.borderSubtle
                        }
                        Text {
                            visible: root.overflow > 0
                            width: parent.width
                            height: visible ? 34 : 0
                            text: QbzSession.tr("{} more playlist(s)...", QbzSession.trRev)
                                  .replace("{}", root.overflow)
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                            font.italic: true
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
                }
            }
        }
    }

    // ============= Duplicate-confirm sub-modal ===========================
    // Opens INSTEAD of adding when the chosen playlist already holds some of
    // the carried tracks. It replaces the picker rather than stacking on it
    // (the picker's own state is already parked in Rust).
    Item {
        anchors.fill: parent
        visible: root.dup.open === true

        Rectangle {
            anchors.fill: parent
            color: "#bf000000"
            MouseArea {
                anchors.fill: parent
                onClicked: QbzPlaylistPicker.cancelDuplicates()
                onWheel: function (wheel) { wheel.accepted = true }
            }
        }

        Rectangle {
            anchors.centerIn: parent
            width: Math.min(parent.width - 80, 460)
            height: dupPanel.implicitHeight
            radius: theme.radiusLg
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            MouseArea {
                anchors.fill: parent
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Column {
                id: dupPanel
                width: parent.width
                padding: 24
                spacing: 16

                Item {
                    width: parent.width - 48
                    height: 28
                    Text {
                        anchors.left: parent.left
                        anchors.right: dupClose.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Some of these tracks were already added", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Item {
                        id: dupClose
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "x"
                            width: 17
                            height: 17
                            tintName: dupCloseArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: dupCloseArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzPlaylistPicker.cancelDuplicates()
                        }
                    }
                }

                Text {
                    width: parent.width - 48
                    text: QbzSession.tr("This playlist already contains {} of the {} tracks you're adding. Add only the new ones, or add everything including duplicates.", QbzSession.trRev)
                          .replace("{}", root.dup.duplicateCount || 0)
                          .replace("{}", root.trackCount)
                    color: theme.textSecondary
                    font.pixelSize: theme.fontLink
                    wrapMode: Text.WordWrap
                }

                Row {
                    anchors.right: parent.right
                    anchors.rightMargin: 24
                    spacing: 12
                    SettingsButton {
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.dup.busy === true
                            ? QbzSession.tr("Adding...", QbzSession.trRev)
                            : QbzSession.tr("Add all tracks", QbzSession.trRev)
                        btnHeight: 34
                        minWidth: 0
                        enabled: root.dup.busy !== true
                        onClicked: QbzPlaylistPicker.confirmDuplicates(true)
                    }
                    QbzPrimaryButton {
                        anchors.verticalCenter: parent.verticalCenter
                        btnHeight: 34
                        label: root.dup.busy === true
                            ? QbzSession.tr("Adding...", QbzSession.trRev)
                            : QbzSession.tr("Add only new tracks", QbzSession.trRev)
                        btnEnabled: root.dup.busy !== true
                        onClicked: QbzPlaylistPicker.confirmDuplicates(false)
                    }
                }
            }
        }
    }
}
