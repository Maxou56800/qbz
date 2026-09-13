// GenreFilterPopup — QML port of crates/qbz-ui/ui/discover/GenreFilterPopup.slint
// (+ its GenreCard / GenreTreeRowItem sub-components).
//
// ONE shared overlay used by every surface that draws a "Filter by genre"
// button. Mount it as the LAST child of a view root (declaration order IS
// z-order in QML) with `anchors.fill: parent`, give it the surface's
// `context`, and drive it from the button:
//
//     GenreFilterPopup { id: genrePopup; anchors.fill: parent
//                        context: "discover"; anchorTop: 62 }
//     ... MouseArea { onClicked: genrePopup.toggle() }   // click handler: safe
//
// The BUTTON's badge must NOT bind to `genrePopup.selectedCount`: the popup is
// declared last, and a creation-time binding that dereferences a
// not-yet-created id registers no dependency and never re-evaluates. Views
// parse QbzBridge.genreFilterJson themselves for that (see HomeView's
// `genreCount`); `selectedCount` here is for post-creation reads.
//
// Data: QbzBridge.genreFilterJson (src/genre_filter_qt.rs) — chips, the
// 3-level advanced tree, remember/advanced flags, and the PER-CONTEXT
// selection counts + names. Actions go straight back through the QbzBridge
// invokables; the controller persists and refreshes the right consumer
// (Discover re-fetches the index, Library > All re-derives in JS).
//
// The counts are PER CONTEXT, so Discover's button and Library's button each
// show their own badge no matter which one was opened last (the Slint pair
// both read the shared current-context count — one of them showed the
// other's number).

import QtQuick
import com.blitzfc.qbz
// Self-referential directory import: QbzToggle lives next to this file and
// the port's proven way to reach a control is the explicit directory import
// (a missing one resolves LAZILY and dies at runtime, not at build).
import "../controls"
import "../theme"

Item {
    id: root

    // "discover" | "library-all" — which selection this instance edits.
    property string context: "discover"
    // Card position: below the toolbar, pinned to the right edge (the Slint
    // overlay sits under the Filter-by-genre button).
    property int anchorTop: 62
    property int anchorRight: 32

    readonly property bool opened: root._open
    property bool _open: false

    QbzTheme { id: theme }

    readonly property var doc: {
        try {
            return JSON.parse(QbzBridge.genreFilterJson)
        } catch (e) {
            return {}
        }
    }
    // This surface's own selection size (button badge).
    readonly property int selectedCount: (doc.counts || {})[root.context] || 0
    // The popup body only ever shows the context it was opened for.
    readonly property bool mine: (doc.context || "") === root.context
    readonly property var chips: root.mine ? (doc.genres || []) : []
    readonly property var treeRows: root.mine ? (doc.tree || []) : []
    readonly property bool advanced: root.mine && doc.advanced === true
    readonly property bool remember: doc.remember !== false
    readonly property bool loading: root.mine && doc.loading === true
    readonly property int liveCount: root.mine ? (doc.selectedCount || 0) : 0

    // `genreOpen` resets the controller's query, and `setAdvanced(false)`
    // clears it too — mirror that in the box so a reopened advanced view
    // never shows a stale needle over an unfiltered tree.
    onAdvancedChanged: if (!root.advanced) searchInput.text = ""

    function open() {
        searchInput.text = ""
        QbzBridge.genreOpen(root.context)
        root._open = true
    }
    function close() {
        root._open = false
    }
    function toggle() {
        if (root._open)
            root.close()
        else
            root.open()
    }

    visible: root._open
    enabled: root._open
    z: 3000

    // Click-out backdrop — declared FIRST so every later sibling (the card)
    // sits above it and keeps its own presses.
    MouseArea {
        anchors.fill: parent
        onClicked: root.close()
        // Same wheel rule as the configurator modal: a MouseArea eats the
        // press and not the wheel, so without this the page kept scrolling
        // under an open popup.
        onWheel: function (wheel) { wheel.accepted = true }
    }

    Rectangle {
        id: card
        x: parent.width - width - root.anchorRight
        y: root.anchorTop
        width: 470
        height: col.implicitHeight + 32
        radius: theme.radiusMd
        color: theme.surfaceMain
        border.width: 1
        border.color: theme.borderMuted

        // Swallow clicks that land on the card's own chrome so the backdrop
        // does not close it. Declared FIRST: the content below stays on top.
        MouseArea {
            anchors.fill: parent
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            id: col
            x: 16
            y: 16
            width: parent.width - 32
            spacing: 14

            // --- Header ---------------------------------------------------
            Item {
                width: parent.width
                height: 26
                Row {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 8
                    QbzIcon {
                        name: "list-filter"
                        width: 16
                        height: 16
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "secondary"
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Filter by genre", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightSemibold
                    }
                }
                Rectangle {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 26
                    height: 26
                    radius: 6
                    color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 15
                        height: 15
                        tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: closeArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.close()
                    }
                }
            }

            // --- Options: Remember selection + Advanced view ---------------
            Item {
                width: parent.width
                height: 22
                Row {
                    anchors.left: parent.left
                    width: (parent.width - 16) / 2
                    spacing: 10
                    Text {
                        width: parent.width - 50
                        height: 22
                        text: QbzSession.tr("Remember selection", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                    QbzToggle {
                        checked: root.remember
                        onToggled: function (v) { QbzBridge.genreSetRemember(v) }
                    }
                }
                Row {
                    anchors.right: parent.right
                    width: (parent.width - 16) / 2
                    spacing: 10
                    Text {
                        width: parent.width - 50
                        height: 22
                        text: QbzSession.tr("Advanced view", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 13
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                    QbzToggle {
                        checked: root.advanced
                        onToggled: function (v) { QbzBridge.genreSetAdvanced(v) }
                    }
                }
            }

            // --- Advanced view: search + 3-level tree ----------------------
            Column {
                visible: root.advanced
                width: parent.width
                spacing: 10

                Rectangle {
                    width: parent.width
                    height: 34
                    radius: 6
                    color: theme.surfaceElevated
                    border.width: 1
                    border.color: searchInput.activeFocus ? theme.borderSubtle : theme.surfaceElevated
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8
                        QbzIcon {
                            name: "search"
                            width: 14
                            height: 14
                            anchors.verticalCenter: parent.verticalCenter
                            tintName: "muted"
                        }
                        TextInput {
                            QbzTextEditMenu { }
                            id: searchInput
                            width: parent.width - 22
                            height: parent.height
                            color: theme.textPrimary
                            font.pixelSize: 13
                            verticalAlignment: Text.AlignVCenter
                            clip: true
                            onTextEdited: QbzBridge.genreSearch(text)
                            Text {
                                visible: searchInput.text === ""
                                anchors.fill: parent
                                text: QbzSession.tr("Search genres…", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: 13
                                verticalAlignment: Text.AlignVCenter
                            }
                        }
                    }
                }

                Item {
                    width: parent.width
                    height: 320
                    Flickable {
                        id: treeFlick
                        anchors.fill: parent
                        clip: true
                        contentWidth: width
                        contentHeight: treeCol.height
                        boundsBehavior: Flickable.StopAtBounds
                        Column {
                            id: treeCol
                            width: treeFlick.width
                            spacing: 1
                            Repeater {
                                model: root.treeRows
                                delegate: Rectangle {
                                    id: treeRow
                                    required property var modelData
                                    width: treeCol.width
                                    height: 34
                                    radius: 6
                                    color: rowArea.containsMouse || arrowArea.containsMouse
                                        ? theme.surfaceHover : "transparent"

                                    // Row click toggles selection. Declared
                                    // FIRST so the chevron's MouseArea below
                                    // sits ON TOP of it — otherwise the row
                                    // swallows the chevron press and the
                                    // sub-genres never expand (Slint #545,
                                    // same z-order rule).
                                    MouseArea {
                                        id: rowArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: QbzBridge.genreToggle(treeRow.modelData.id)
                                    }

                                    Row {
                                        anchors.fill: parent
                                        anchors.leftMargin: 6 + treeRow.modelData.level * 18
                                        anchors.rightMargin: 10
                                        spacing: 6

                                        Item {
                                            width: 20
                                            height: parent.height
                                            QbzIcon {
                                                visible: treeRow.modelData.hasChildren
                                                anchors.centerIn: parent
                                                name: treeRow.modelData.expanded ? "chevron-down" : "chevron-right"
                                                width: 13
                                                height: 13
                                                tintName: "muted"
                                            }
                                            MouseArea {
                                                id: arrowArea
                                                anchors.fill: parent
                                                // Disabled on leaf rows so the
                                                // press falls through to the row
                                                // (an enabled MouseArea eats it
                                                // even with a no-op handler —
                                                // 20px dead zone otherwise).
                                                enabled: treeRow.modelData.hasChildren
                                                hoverEnabled: treeRow.modelData.hasChildren
                                                cursorShape: Qt.PointingHandCursor
                                                onClicked: QbzBridge.genreToggleExpand(treeRow.modelData.id)
                                            }
                                        }

                                        Rectangle {
                                            anchors.verticalCenter: parent.verticalCenter
                                            width: 16
                                            height: 16
                                            radius: 4
                                            color: treeRow.modelData.selected ? theme.accent : "transparent"
                                            border.width: treeRow.modelData.selected ? 0 : 2
                                            border.color: theme.borderMuted
                                            QbzIcon {
                                                visible: treeRow.modelData.selected
                                                anchors.centerIn: parent
                                                name: "check"
                                                width: 11
                                                height: 11
                                                // DELIBERATE DIVERGENCE from
                                                // discover/GenreFilterPopup.slint:85,
                                                // which hardcodes `tint: #ffffff`
                                                // on this accent box — 1.70:1 on
                                                // high-contrast, 1.74 on ikari,
                                                // 16 of the 35 palettes under
                                                // 2.6:1. theme/QbzTheme.qml, "ON
                                                // AN ACCENT FILL".
                                                tintName: theme.accentGlyphTint
                                            }
                                        }

                                        Text {
                                            // Row: [20 arrow][16 box][name][count],
                                            // spacing 6 — the count and ITS
                                            // spacing only exist when shown
                                            // (an invisible Text still has a
                                            // width, so it cannot be
                                            // subtracted unconditionally).
                                            width: parent.width - 26 - 22
                                                   - (countLabel.visible ? countLabel.width + 6 : 0)
                                            height: parent.height
                                            text: treeRow.modelData.name
                                            color: treeRow.modelData.selected ? theme.textPrimary : theme.textSecondary
                                            font.pixelSize: 13
                                            font.weight: treeRow.modelData.level === 0
                                                ? theme.weightMedium : theme.weightRegular
                                            verticalAlignment: Text.AlignVCenter
                                            elide: Text.ElideRight
                                        }
                                        Text {
                                            id: countLabel
                                            visible: (treeRow.modelData.count || 0) > 0
                                            height: parent.height
                                            text: treeRow.modelData.count
                                            color: theme.textMuted
                                            font.pixelSize: theme.fontLegal
                                            verticalAlignment: Text.AlignVCenter
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // --- Simple grid: parent genres as chips -----------------------
            Grid {
                id: chipGrid
                visible: !root.advanced && root.chips.length > 0
                width: parent.width
                columns: 3
                columnSpacing: 8
                rowSpacing: 8
                readonly property int cellWidth: Math.floor((width - 2 * columnSpacing) / columns)

                Repeater {
                    model: root.chips
                    delegate: Rectangle {
                        id: chip
                        required property var modelData
                        width: chipGrid.cellWidth
                        height: 38
                        radius: 6
                        color: chip.modelData.selected ? theme.accent
                             : chipArea.containsMouse ? theme.surfaceHover : theme.surfaceElevated

                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 8
                            Text {
                                width: parent.width - 24
                                height: parent.height
                                text: chip.modelData.name
                                // On the accent fill -> the measured on-accent
                                // colour. GenreFilterPopup.slint:125 hardcodes
                                // #ffffff (same defect as :85 above); the port
                                // had already moved to accent-text, and
                                // `accentGlyphColor` IS accent-text on 34 of
                                // the 35 palettes — it only differs where
                                // accent-text itself drops under 3:1
                                // (rose-pine-dawn, 2.56:1).
                                color: chip.modelData.selected
                                    ? theme.accentGlyphColor : theme.textSecondary
                                font.pixelSize: 13
                                font.weight: chip.modelData.selected ? theme.weightMedium : theme.weightRegular
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                            Rectangle {
                                anchors.verticalCenter: parent.verticalCenter
                                width: 16
                                height: 16
                                radius: 8
                                // INVERTED host: this 16px disc is the chip's
                                // FOREGROUND punched out of the accent fill,
                                // and the check inside it is tinted `accent`.
                                // So the pair that has to clear the floor is
                                // (accent glyph, this fill) — the same two
                                // colours as everywhere else on this pill,
                                // just swapped. Painting it with
                                // `accentGlyphColor` rather than raw
                                // accent-text is what keeps that true:
                                // identical on 34 palettes, and on
                                // rose-pine-dawn it turns a 2.56:1 check into
                                // 7.38:1. GenreFilterPopup.slint:141 paints
                                // this disc #ffffff.
                                color: chip.modelData.selected
                                    ? theme.accentGlyphColor : "transparent"
                                border.width: chip.modelData.selected ? 0 : 2
                                border.color: theme.borderMuted
                                QbzIcon {
                                    visible: chip.modelData.selected
                                    anchors.centerIn: parent
                                    name: "check"
                                    width: 11
                                    height: 11
                                    tintName: "accent"
                                }
                            }
                        }
                        MouseArea {
                            id: chipArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzBridge.genreToggle(chip.modelData.id)
                        }
                    }
                }
            }

            // Empty/loading state for the simple grid — the parent list loads
            // on first open, so the card would otherwise flash a bare header.
            Text {
                visible: !root.advanced && root.chips.length === 0
                width: parent.width
                height: 38
                // `!mine` = the context switch has been sent but Rust's
                // republish has not landed yet (one event-loop hop), which
                // reads as loading, not as "nothing here".
                text: (root.loading || !root.mine)
                    ? QbzSession.tr("Loading…", QbzSession.trRev)
                    : QbzSession.tr("No genres available.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: 13
                verticalAlignment: Text.AlignVCenter
            }

            // --- Footer: Clear filter -------------------------------------
            Rectangle {
                width: parent.width
                height: 34
                radius: 6
                opacity: root.liveCount > 0 ? 1.0 : 0.5
                color: clearArea.containsMouse && root.liveCount > 0
                    ? theme.surfaceHover : theme.surfaceElevated
                Text {
                    anchors.centerIn: parent
                    text: QbzSession.tr("Clear filter", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: 13
                }
                MouseArea {
                    id: clearArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: root.liveCount > 0 ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: if (root.liveCount > 0) QbzBridge.genreClear()
                }
            }
        }
    }
}
