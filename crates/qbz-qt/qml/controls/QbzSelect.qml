// QbzSelect (primitives/QbzSelect.slint, standard size) — extracted from
// SettingsView.qml in phase 19 so every settings panel shares the one
// replica (the ui-control-alignment-standard searchable dropdown: the
// filter box shows past ~8 options via `searchable: true`).
// 34px bordered control (elevated, hover -> surface-hover) + a Popup
// list (surface-main r8 border-muted, 32px rows, capped at 360px with
// scroll, optional 42px filter box, 24px group-header rows, "BP" badge
// / speaker glyph on device options). `options` entries are either
// plain strings or {label, bp, group} objects (the device list).

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"

Rectangle {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    property var options: []
    property int currentIndex: 0
    property int menuWidth: 240
    property int popupWidth: 0
    // The default keeps the standard dropdown below the field. Flyouts near a
    // window edge may opt into a lateral list without changing every other
    // QbzSelect instance. Supported values: "below" and "left".
    property string popupPlacement: "below"
    property bool searchable: false
    // Bootstrap-style small variant (QbzSelect.slint:74-76). The default is the
    // standard Settings size; TOOLBARS opt in with `sm: true` — 30px tall, r6,
    // NO outline (it matches the favorites toolbar buttons), 12px label and a
    // 14px chevron, and 40px narrower because toolbar selects were needlessly
    // wide (QbzSelect.slint:86-88,102-105,118-119,126,147-148,299).
    // Optional sort selector: reselecting an option still emits selected.
    property string sortDirection: ""
    property bool sm: false
    // Optional leading glyph before the label (e.g. a sort field's
    // arrow-down-up). Empty = none, and the positioner ignores the hidden
    // slot so every existing select is byte-identical.
    property string leadingIcon: ""
    // When set, the COLLAPSED control always shows this fixed label instead of
    // the current option — a placeholder select whose active choice lives in
    // the outline + tooltip, not the field (the Sort control). The open list
    // still highlights the current option.
    property string placeholderText: ""
    // "This select holds a non-default choice": draw an accent outline at rest.
    // Only the border changes — the fill stays the ambient-aware token, so the
    // control still goes translucent under the dynamic background.
    property bool outlineActive: false
    // Optional shared-tooltip host (a QbzTooltip) + text, shown on hover. Used
    // to name the active sort when it differs from the tab default.
    property var tooltipHost: null
    property string tooltipText: ""
    property string tooltipKey: ""
    signal selected(int index)

    QbzTheme { id: theme }

    id: selectRoot
    width: sm ? Math.max(0, menuWidth - 40) : menuWidth
    height: kioskHost ? 44 : (sm ? 30 : 34)
    radius: sm ? 6 : theme.radiusSm
    // sm (toolbar) selects show NO focus ring: it lingered after a pick and
    // read as "still modified", so the outline could never fully clear on a
    // return to default. There, only outlineActive draws a border, and it is
    // gone the instant the choice is the default. Settings-size selects keep
    // the keyboard focus ring.
    border.width: (selectRoot.activeFocus && !sm) ? 2
        : (selectRoot.outlineActive ? 1 : (sm ? 0 : 1))
    border.color: (selectRoot.activeFocus && !sm) || selectRoot.outlineActive
        ? theme.accent : theme.borderSubtle
    // Resting fill goes translucent under the dynamic background so the field
    // shows through the control (QbzSelect.slint:110-116). Hover keeps its own
    // token, which is already translucent.
    color: selArea.containsMouse && enabled
        ? theme.elevatedHoverFill
        : theme.elevatedFill
    opacity: enabled ? 1.0 : 0.4
    activeFocusOnTab: enabled
    Accessible.role: Accessible.ComboBox
    Accessible.name: currentIndex >= 0 && currentIndex < options.length
        ? optLabel(currentIndex) : ""
    Accessible.onPressAction: selectRoot.openPopup()

    Keys.onPressed: function (event) {
        if (!selectRoot.enabled || event.isAutoRepeat)
            return
        if (event.key === Qt.Key_Space
                || event.key === Qt.Key_Return
                || event.key === Qt.Key_Enter) {
            if (popup.opened)
                popup.close()
            else
                selectRoot.openPopup()
        } else if (event.key === Qt.Key_Up) {
            var previous = selectRoot.adjacentEnabled(selectRoot.currentIndex, -1)
            if (previous >= 0) selectRoot.selected(previous)
        } else if (event.key === Qt.Key_Down) {
            var next = selectRoot.adjacentEnabled(selectRoot.currentIndex, 1)
            if (next >= 0) selectRoot.selected(next)
        } else {
            return
        }
        // Do not let Space reach the global play/pause binding.
        event.accepted = true
    }

    // `width`, NOT `menuWidth`: QbzSelect.slint:89-91 resolves the open list as
    // `max(popup-width, EFF-WIDTH)`, and eff-width is `menu-width - 40px` at
    // `sm` (:88) — the same value it uses for the collapsed control. Reading the
    // RAW menuWidth here made every toolbar dropdown's PANEL 40px wider than the
    // reference's (MyQBZ sort 180 vs 140, kind filter 200 vs 160, detail sort
    // 170 vs 130, and the Local Library toolbar / LabelReleases selects with it).
    // The three `popupWidth` callers (AudioSettings 480, CastPicker 260,
    // LyricsControlsFlyout 178) are all non-`sm`, where width === menuWidth and
    // popupWidth wins anyway, so none of them moves.
    readonly property int listWidth: Math.max(popupWidth, selectRoot.width)
    readonly property int rowHeight: kioskHost ? 44 : 32
    readonly property int headerHeight: 24
    readonly property int searchHeight: searchable ? (kioskHost ? 44 : 42) : 0
    readonly property int maxListHeight: kioskHost ? Math.max(44, Math.min(360, (selectRoot.Window.window ? selectRoot.Window.window.height : 480) - searchHeight - 100)) : 360
    property string filter: ""

    function optLabel(i) {
        const o = options[i]
        return (typeof o === "string") ? o : (o && o.label !== undefined ? o.label : "")
    }
    function optHasBadges() {
        return options.length > 0 && typeof options[0] !== "string"
            && options[0] && options[0].bp !== undefined
    }
    function optBp(i) {
        const o = options[i]
        return o && o.bp === true
    }
    function optGroup(i) {
        const o = options[i]
        return (o && o.group !== undefined) ? o.group : ""
    }
    function optDetail(i) {
        const o = options[i]
        return (o && o.detail !== undefined) ? o.detail : ""
    }
    function optEnabled(i) {
        const o = options[i]
        return !(o && o.enabled !== undefined) || o.enabled === true
    }
    function adjacentEnabled(start, delta) {
        for (var i = start + delta; i >= 0 && i < options.length; i += delta) {
            if (optEnabled(i)) return i
        }
        return -1
    }
    function openPopup() {
        if (!selectRoot.enabled || selectRoot.options.length === 0)
            return
        selectRoot.filter = ""
        listContent.currentIndex = selectRoot.currentIndex
        // A field tooltip must not sit over its own open list.
        if (selectRoot.tooltipHost)
            selectRoot.tooltipHost.hide(selectRoot.tooltipKey !== "" ? selectRoot.tooltipKey : "qbz-select")
        popup.open()
        if (selectRoot.searchable)
            searchInput.forceActiveFocus()
    }
    function optionMatches(i) {
        return i >= 0 && i < options.length
            && optEnabled(i)
            && (filter === ""
                || optLabel(i).toLowerCase().indexOf(filter.toLowerCase()) >= 0)
    }
    function moveSearchHighlight(delta) {
        let i = listContent.currentIndex
        for (let n = 0; n < options.length; n++) {
            i += delta
            if (i < 0 || i >= options.length)
                break
            if (optionMatches(i)) {
                listContent.currentIndex = i
                listContent.positionViewAtIndex(i, ListView.Contain)
                return
            }
        }
    }
    function acceptSearchHighlight() {
        const i = listContent.currentIndex
        if (!optionMatches(i))
            return
        popup.close()
        selectRoot.forceActiveFocus()
        selectRoot.selected(i)
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: selectRoot.sm ? 10 : 12
        anchors.rightMargin: selectRoot.sm ? 8 : 10
        spacing: 8
        // Leading glyph (opt-in). A Row ignores a hidden child entirely — no
        // slot, no spacing — so selects without a leadingIcon are unchanged.
        QbzIcon {
            id: leadingGlyph
            visible: selectRoot.leadingIcon !== ""
            name: selectRoot.leadingIcon
            width: visible ? (selectRoot.sm ? 14 : 16) : 0
            height: selectRoot.sm ? 14 : 16
            anchors.verticalCenter: parent.verticalCenter
            tintName: selectRoot.outlineActive ? "accent" : "secondary"
        }
        Text {
            width: parent.width - (leadingGlyph.visible ? leadingGlyph.width + 8 : 0)
                - 16 - 8 - badgeSlot.width - (badgeSlot.visible ? 8 : 0)
            height: parent.height
            // A placeholder select shows its fixed label; otherwise the current
            // option. The active choice of a placeholder select is surfaced by
            // the outline + tooltip, not here.
            text: selectRoot.placeholderText !== ""
                ? selectRoot.placeholderText
                : (selectRoot.currentIndex >= 0 && selectRoot.currentIndex < selectRoot.options.length
                    ? selectRoot.optLabel(selectRoot.currentIndex) : "")
            color: theme.textPrimary
            font.pixelSize: selectRoot.kioskHost ? (selectRoot.sm ? 12 : theme.fontBody) * 1.2 : (selectRoot.sm ? 12 : theme.fontBody)
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }
        // Trailing BP badge / speaker glyph for the CURRENT device option.
        Item {
            id: badgeSlot
            visible: selectRoot.optHasBadges()
            width: visible ? 20 : 0
            height: parent.height
            Text {
                visible: selectRoot.optHasBadges() && selectRoot.currentIndex < selectRoot.options.length
                    && selectRoot.optBp(selectRoot.currentIndex)
                anchors.centerIn: parent
                text: "BP"
                color: theme.accent
                font.pixelSize: selectRoot.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                font.weight: theme.weightSemibold
                font.letterSpacing: 0.5
            }
            QbzIcon {
                visible: selectRoot.optHasBadges() && selectRoot.currentIndex < selectRoot.options.length
                    && !selectRoot.optBp(selectRoot.currentIndex)
                name: "volume-2"
                width: 14
                height: 14
                anchors.centerIn: parent
                tintName: "muted"
            }
        }
        QbzIcon {
            // A placeholder select keeps a NEUTRAL chevron collapsed — its
            // direction is not surfaced in the field (it lives in the tooltip
            // and the open list), so the field says nothing about the active
            // sort. Non-placeholder selects keep the direction chevron.
            name: (selectRoot.sortDirection !== "" && selectRoot.placeholderText === "")
                ? (selectRoot.sortDirection === "asc" ? "chevron-up" : "chevron-down")
                : (selectRoot.popupPlacement === "left" ? "chevron-left" : "chevron-down")
            width: selectRoot.sm ? 14 : 16
            height: selectRoot.sm ? 14 : 16
            anchors.verticalCenter: parent.verticalCenter
            tintName: "muted"
        }
    }
    MouseArea {
        id: selArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: selectRoot.enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: {
            if (selectRoot.enabled) {
                selectRoot.forceActiveFocus()
                selectRoot.openPopup()
            }
        }
        onContainsMouseChanged: {
            if (selectRoot.tooltipHost && selectRoot.tooltipText !== "")
                selectRoot.tooltipHost.hover(containsMouse && !popup.opened, selectRoot,
                    selectRoot.tooltipKey !== "" ? selectRoot.tooltipKey : "qbz-select",
                    selectRoot.tooltipText)
        }
    }

    Popup {
        id: popup
        parent: selectRoot
        // Standard selects grow down and left-align their right edge. A
        // right-edge flyout may place the list beside the field instead; its
        // bottom edge stays aligned so it cannot fall into the player bar.
        x: selectRoot.popupPlacement === "left"
            ? -selectRoot.listWidth - 4
            : selectRoot.width - selectRoot.listWidth
        y: selectRoot.popupPlacement === "left"
            ? selectRoot.height - popup.height
            : selectRoot.height + 4
        width: selectRoot.listWidth
        height: selectRoot.searchHeight + Math.min(listContent.contentHeight, selectRoot.maxListHeight) + 10
        padding: 0
        closePolicy: Popup.CloseOnPressOutside | Popup.CloseOnEscape

        background: Rectangle {
            color: theme.surfaceMain
            radius: theme.radiusSm
            border.width: 1
            border.color: theme.borderMuted
        }
        contentItem: Item {
            implicitWidth: selectRoot.listWidth
            implicitHeight: popup.height

            // Filter box (searchable lists only).
            Rectangle {
                id: searchBox
                visible: selectRoot.searchable
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: selectRoot.searchHeight
                color: "transparent"
                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 12
                    anchors.rightMargin: 12
                    anchors.bottomMargin: 6
                    spacing: 8
                    QbzIcon {
                        name: "search"
                        width: 14
                        height: 14
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: "muted"
                    }
                    Item {
                        width: parent.width - 14 - 8
                        height: parent.height
                        TextInput {
                            QbzTextEditMenu { }
                            id: searchInput
                            anchors.fill: parent
                            color: theme.textPrimary
                            font.pixelSize: selectRoot.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            verticalAlignment: Text.AlignVCenter
                            clip: true
                            text: selectRoot.filter
                            onTextChanged: {
                                selectRoot.filter = text
                                if (!listContent)
                                    return
                                for (let i = 0; i < selectRoot.options.length; i++) {
                                    if (selectRoot.optionMatches(i)) {
                                        listContent.currentIndex = i
                                        return
                                    }
                                }
                                listContent.currentIndex = -1
                            }
                            Keys.onPressed: function (event) {
                                if (event.key === Qt.Key_Down)
                                    selectRoot.moveSearchHighlight(1)
                                else if (event.key === Qt.Key_Up)
                                    selectRoot.moveSearchHighlight(-1)
                                else if (event.key === Qt.Key_Return
                                         || event.key === Qt.Key_Enter)
                                    selectRoot.acceptSearchHighlight()
                                else
                                    return
                                event.accepted = true
                            }
                        }
                        Text {
                            visible: searchInput.text === ""
                            anchors.fill: parent
                            text: QbzSession.tr("Search…", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: selectRoot.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
                }
            }

            ListView {
                id: listContent
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: searchBox.bottom
                anchors.bottom: parent.bottom
                anchors.topMargin: 5
                anchors.bottomMargin: 5
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: selectRoot.options

                delegate: Column {
                    id: optRow
                    required property int index
                    required property var modelData
                    width: listContent.width

                    readonly property string label: selectRoot.optLabel(index)
                    readonly property bool shown: selectRoot.filter === ""
                        || label.toLowerCase().indexOf(selectRoot.filter.toLowerCase()) >= 0

                    // Group-header row (ALSA device sections).
                    Rectangle {
                        visible: optRow.shown && selectRoot.optGroup(optRow.index) !== ""
                        width: parent.width
                        height: visible ? selectRoot.headerHeight : 0
                        color: "transparent"
                        Text {
                            anchors.left: parent.left
                            anchors.right: parent.right
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            anchors.top: parent.top
                            anchors.topMargin: 4
                            height: parent.height - 4
                            text: selectRoot.optGroup(optRow.index)
                            color: theme.textMuted
                            font.pixelSize: selectRoot.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                            font.weight: theme.weightSemibold
                            font.letterSpacing: 0.5
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                    Rectangle {
                        visible: optRow.shown
                        width: parent.width
                        height: visible
                            ? (selectRoot.optDetail(optRow.index) !== "" ? 50 : selectRoot.rowHeight)
                            : 0
                        color: optArea.containsMouse ? theme.surfaceHover : "transparent"
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 8
                            Column {
                                width: parent.width - rowBadge.width - (rowBadge.visible ? 8 : 0)
                                height: parent.height
                                anchors.verticalCenter: parent.verticalCenter
                                Text {
                                    width: parent.width
                                    height: selectRoot.optDetail(optRow.index) !== "" ? 26 : parent.height
                                    text: optRow.label
                                    color: !selectRoot.optEnabled(optRow.index)
                                        ? theme.textMuted
                                        : (optRow.index === selectRoot.currentIndex
                                            ? theme.accent : theme.textSecondary)
                                    font.pixelSize: selectRoot.kioskHost ? (selectRoot.sm ? 12 : theme.fontBody) * 1.2 : (selectRoot.sm ? 12 : theme.fontBody)
                                    verticalAlignment: Text.AlignVCenter
                                    elide: Text.ElideRight
                                }
                                Text {
                                    visible: selectRoot.optDetail(optRow.index) !== ""
                                    width: parent.width
                                    height: visible ? 18 : 0
                                    text: selectRoot.optDetail(optRow.index)
                                    color: theme.textMuted
                                    font.pixelSize: selectRoot.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                    verticalAlignment: Text.AlignVCenter
                                    elide: Text.ElideRight
                                }
                            }
                            Item {
                                id: rowBadge
                                visible: selectRoot.optHasBadges()
                                    || (selectRoot.sortDirection !== "" && optRow.index === selectRoot.currentIndex)
                                width: visible ? 20 : 0
                                height: parent.height
                                Text {
                                    visible: selectRoot.optBp(optRow.index)
                                    anchors.centerIn: parent
                                    text: "BP"
                                    color: theme.accent
                                    font.pixelSize: selectRoot.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                    font.weight: theme.weightSemibold
                                    font.letterSpacing: 0.5
                                }
                                QbzIcon {
                                    visible: !selectRoot.optBp(optRow.index)
                                    name: selectRoot.sortDirection !== ""
                                        ? (selectRoot.sortDirection === "asc" ? "chevron-up" : "chevron-down")
                                        : "volume-2"
                                    width: 14
                                    height: 14
                                    anchors.centerIn: parent
                                    tintName: "muted"
                                }
                            }
                        }
                        MouseArea {
                            id: optArea
                            anchors.fill: parent
                            hoverEnabled: true
                            enabled: selectRoot.optEnabled(optRow.index)
                            cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                            onClicked: {
                                popup.close()
                                selectRoot.selected(optRow.index)
                            }
                        }
                    }
                }
            }
        }
    }
}
