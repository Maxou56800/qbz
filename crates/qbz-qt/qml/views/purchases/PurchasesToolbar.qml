// PurchasesToolbar — the per-tab control row of the Purchases list. 1:1 port of
// purchases/PurchasesView.slint's `AlbumsToolbar` (:915-1050) and
// `TracksToolbar` (:1055-1120), folded into one file because they are the same
// bar with a different set of controls.
//
// THE TWO BARS ARE NOT THE SAME, and that is the whole point of the file:
//
//   Albums   [stretch] group · sort · filter · grid/list
//   Tracks   [stretch] group · filter
//
// No sort and no grid/list on Tracks (§5, `PurchasesView.svelte:574` vs `:727`).
// Building the Albums bar on both tabs invents two controls the reference never
// had, so the arms are separate `visible` blocks rather than one parameterised
// row that quietly grows a control.
//
// Everything is right-aligned (the .slint opens each bar with a stretch
// spacer). Controls are 30px on a 8px gap — the toolbar scale of
// ui-control-alignment-standard, the same one LibraryToolbar's chips use.
//
// The dropdowns are `QbzContextMenu`, not bespoke anchored Rectangles: it is
// this port's shared menu surface, it clamps itself into the window, it closes
// on an outside press, and it carries the ADR-009 stacking the .slint has to
// spell out by hand (`z: 3000` on its filter panel).

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Item {
    id: root
    property bool kioskHost: false

    /// "albums" | "tracks".
    property string tab: "albums"
    /// `list_json.toolbar` (§G.2).
    property var toolbar: ({})

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    readonly property string group: root.toolbar.group || "off"
    readonly property string sortKey: root.toolbar.sort || "purchased"
    readonly property bool ascending: root.toolbar.ascending === true
    readonly property string viewMode: root.toolbar.viewMode || "grid"
    readonly property bool hideUnavailable: root.toolbar.hideUnavailable === true
    readonly property bool hideDownloaded: root.toolbar.hideDownloaded === true
    readonly property string qualityFilter: root.toolbar.qualityFilter || "all"
    readonly property var genres: root.toolbar.genres || []
    readonly property string genreFilter: root.toolbar.genreFilter || ""

    /// The funnel's badge. Counted GLOBALLY — the Tracks bar hides the
    /// Availability section but the preference itself is still in force, which
    /// is exactly what the .slint's single `active-filter-count` does.
    readonly property int activeFilterCount:
          (root.hideUnavailable ? 1 : 0)
        + (root.hideDownloaded ? 1 : 0)
        + (root.qualityFilter !== "all" ? 1 : 0)
        + (root.genreFilter !== "" && root.tab !== "tracks" ? 1 : 0)

    /// The same three settings as the badge, but SAID — the applied-filters
    /// tooltip. Counted the same way and in the menu's own order, so the number
    /// on the disc and the lines in the bubble can never disagree.
    readonly property var filterSummaryGroups: {
        var avail = []
        if (root.hideUnavailable)
            avail.push(root.t("Hide unavailable"))
        if (root.hideDownloaded)
            avail.push(root.t("Hide downloaded"))
        var q = []
        if (root.qualityFilter === "hires")
            q.push(root.t("Hi-Res"))
        else if (root.qualityFilter === "cd")
            q.push(root.t("CD"))
        else if (root.qualityFilter === "lossy")
            q.push(root.t("Lossy"))
        var g = root.genreFilter !== "" && root.tab !== "tracks" ? [root.genreFilter] : []
        return [
            { group: root.t("Availability"), values: avail },
            { group: root.t("Quality"), values: q },
            { group: root.t("Genre"), values: g }
        ]
    }

    width: parent ? parent.width : 0
    height: root.kioskHost ? 64 : 30

    // ── One toolbar dropdown trigger (.slint `ToolbarButton`, :37-70) ──────
    component ToolbarButton: Rectangle {
        id: tb
        property bool kioskHost: false
        property string label: ""
        property bool active: false
        signal clicked()
        width: tb.kioskHost ? Math.max(64, tbRow.width) : tbRow.width
        height: tb.kioskHost ? 64 : 30
        radius: 6
        color: tbArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
        Row {
            id: tbRow
            height: parent.height
            leftPadding: 10
            rightPadding: 8
            spacing: 6
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: tb.label
                color: tb.active ? theme.accent : theme.textPrimary
                font.pixelSize: 12
            }
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: "chevron-down"
                width: 14
                height: 14
                tintName: "muted"
            }
        }
        MouseArea {
            id: tbArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: tb.clicked()
        }
    }

    // ── One row inside an open dropdown (.slint `MenuRow`, :73-104) ────────
    component MenuRow: Rectangle {
        id: mr
        property string label: ""
        property bool selected: false
        /// "↑" / "↓" on the active sort row, "" everywhere else. A bare glyph,
        /// not a translatable string — the .slint uses the same two literals.
        property string arrow: ""
        signal picked()
        width: parent ? parent.width : 0
        height: 32
        radius: 4
        color: mrArea.containsMouse ? theme.surfaceHover : "transparent"
        Row {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 12
            spacing: 8
            Text {
                width: Math.max(0, parent.width - (mr.arrow !== "" ? arrowText.width + 8 : 0))
                height: parent.height
                text: mr.label
                color: mr.selected ? theme.accent : theme.textSecondary
                font.pixelSize: 12
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
            Text {
                id: arrowText
                visible: mr.arrow !== ""
                height: parent.height
                text: mr.arrow
                color: theme.accent
                font.pixelSize: 12
                verticalAlignment: Text.AlignVCenter
            }
        }
        MouseArea {
            id: mrArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: mr.picked()
        }
    }

    // ── Section label inside the filter panel ─────────────────────────────
    component FilterSectionLabel: Text {
        color: theme.textMuted
        font.pixelSize: theme.fontLegal
        font.weight: theme.weightSemibold
    }

    // ── One checkbox + label line inside the filter panel ─────────────────
    component FilterCheckRow: Item {
        id: fcr
        property string label: ""
        property bool checked: false
        signal toggled()
        width: parent ? parent.width : 0
        height: 24
        Row {
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            QbzCheckbox {
                anchors.verticalCenter: parent.verticalCenter
                checked: fcr.checked
                onToggled: fcr.toggled()
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: fcr.label
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
            }
        }
        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: fcr.toggled()
        }
    }

    // ── One Quality radio (.slint `QualityRadio`, :168-222) ───────────────
    component QualityRadioRow: Item {
        id: qr
        property string label: ""
        property string hint: ""
        property bool selected: false
        signal picked()
        width: parent ? parent.width : 0
        height: 26
        Row {
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: 16
                height: 16
                radius: 8
                border.width: qr.selected ? 0 : 1.5
                border.color: theme.textMuted
                color: qr.selected ? theme.accent : "transparent"
                Rectangle {
                    visible: qr.selected
                    anchors.centerIn: parent
                    width: 6
                    height: 6
                    radius: 3
                    // The .slint says `Theme.accent-text`; this port routes
                    // every on-accent mark through the measured selector
                    // (theme/QbzTheme.qml, "ON AN ACCENT FILL"), exactly like
                    // controls/QbzRadioOption.qml.
                    color: theme.accentGlyphColor
                }
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: qr.label
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
            }
            Text {
                visible: qr.hint !== ""
                anchors.verticalCenter: parent.verticalCenter
                text: qr.hint
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }
        }
        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: qr.picked()
        }
    }

    // ── The bar ───────────────────────────────────────────────────────────
    Row {
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        spacing: 8

        // --- Group ---------------------------------------------------------
        ToolbarButton {
            id: groupBtn
            kioskHost: root.kioskHost
            anchors.verticalCenter: parent.verticalCenter
            label: root.tab === "tracks"
                ? (root.group === "album" ? root.t("Group: Album")
                  : root.group === "artist" ? root.t("Group: Artist")
                  : root.group === "name" ? root.t("Group: Name")
                  : root.t("Group: Off"))
                : (root.group === "artist" ? root.t("Group: Artist")
                  : root.group === "alpha" ? root.t("Group: A–Z")
                  : root.t("Group: Off"))
            onClicked: groupMenu.openBelowLeft(groupBtn)

            QbzContextMenu {
                id: groupMenu
                menuWidth: 180
                MenuRow {
                    label: root.t("Off")
                    selected: root.group === "off"
                    onPicked: { QbzPurchases.setGroup("off"); groupMenu.close() }
                }
                // Albums arm: A–Z / Artist. `visible: false` drops the row AND
                // its spacing — the panel's Column skips it entirely, so the
                // two arms never leave a gap behind.
                MenuRow {
                    visible: root.tab !== "tracks"
                    label: root.t("A–Z")
                    selected: root.group === "alpha"
                    onPicked: { QbzPurchases.setGroup("alpha"); groupMenu.close() }
                }
                // Tracks arm: Album / Artist / Name.
                MenuRow {
                    visible: root.tab === "tracks"
                    label: root.t("Album")
                    selected: root.group === "album"
                    onPicked: { QbzPurchases.setGroup("album"); groupMenu.close() }
                }
                MenuRow {
                    label: root.t("Artist")
                    selected: root.group === "artist"
                    onPicked: { QbzPurchases.setGroup("artist"); groupMenu.close() }
                }
                MenuRow {
                    visible: root.tab === "tracks"
                    label: root.t("Name")
                    selected: root.group === "name"
                    onPicked: { QbzPurchases.setGroup("name"); groupMenu.close() }
                }
            }
        }

        // --- Sort (ALBUMS ONLY) --------------------------------------------
        ToolbarButton {
            id: sortBtn
            kioskHost: root.kioskHost
            // A Row skips an invisible child AND its spacing, so the Tracks bar
            // closes up on its own — no width dance is needed here.
            visible: root.tab !== "tracks"
            anchors.verticalCenter: parent.verticalCenter
            label: root.sortKey === "artist" ? root.t("Sort: Artist")
                 : root.sortKey === "album" ? root.t("Sort: Album")
                 : root.sortKey === "quality" ? root.t("Sort: Quality")
                 : root.sortKey === "released" ? root.t("Sort: Release date")
                 : root.t("Sort: Date")
            onClicked: sortMenu.openBelowLeft(sortBtn)

            // Re-picking the ACTIVE key flips the direction, which is the
            // .slint's `select-album-sort` (:790-800) and Tauri's behaviour:
            // there is no separate direction control on this screen.
            function pick(key) {
                if (root.sortKey === key)
                    QbzPurchases.setSort(key, !root.ascending)
                else
                    QbzPurchases.setSort(key, false)
                sortMenu.close()
            }
            function arrowFor(key) {
                return root.sortKey === key ? (root.ascending ? "↑" : "↓") : ""
            }

            QbzContextMenu {
                id: sortMenu
                menuWidth: 180
                // "purchased" is §G.2's key for the purchase date; the label is
                // the existing "Sort: Date" / "Date" msgid pair.
                MenuRow {
                    label: root.t("Date")
                    selected: root.sortKey === "purchased"
                    arrow: sortBtn.arrowFor("purchased")
                    onPicked: sortBtn.pick("purchased")
                }
                MenuRow {
                    label: root.t("Release date")
                    selected: root.sortKey === "released"
                    arrow: sortBtn.arrowFor("released")
                    onPicked: sortBtn.pick("released")
                }
                MenuRow {
                    label: root.t("Artist")
                    selected: root.sortKey === "artist"
                    arrow: sortBtn.arrowFor("artist")
                    onPicked: sortBtn.pick("artist")
                }
                MenuRow {
                    label: root.t("Album")
                    selected: root.sortKey === "album"
                    arrow: sortBtn.arrowFor("album")
                    onPicked: sortBtn.pick("album")
                }
                MenuRow {
                    label: root.t("Quality")
                    selected: root.sortKey === "quality"
                    arrow: sortBtn.arrowFor("quality")
                    onPicked: sortBtn.pick("quality")
                }
            }
        }

        // --- Filter funnel + panel ------------------------------------------
        Rectangle {
            id: filterBtn
            anchors.verticalCenter: parent.verticalCenter
            width: root.kioskHost ? 64 : 30
            height: root.kioskHost ? 64 : 30
            radius: 6
            color: filterArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
            QbzIcon {
                name: "list-filter"
                width: 16
                height: 16
                anchors.centerIn: parent
                tintName: root.activeFilterCount > 0 ? "accent" : "muted"
            }
            // Active-count badge — a 14px accent disc in the corner
            // (.slint:124-140). Declared after the glyph so it paints over it.
            Rectangle {
                visible: root.activeFilterCount > 0
                x: parent.width - width - 1
                y: 1
                width: 14
                height: 14
                radius: 7
                color: theme.accent
                Text {
                    anchors.centerIn: parent
                    text: root.activeFilterCount
                    color: theme.accentGlyphColor
                    font.pixelSize: 9
                    font.weight: theme.weightSemibold
                }
            }
            MouseArea {
                id: filterArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    purchasesFilterTip.exit()
                    filterMenu.openBelowRight(filterBtn)
                }
                onEntered: purchasesFilterTip.enter()
                onExited: purchasesFilterTip.exit()
            }

            QbzFilterTip {
                id: purchasesFilterTip
                ownerKey: "purchases-filter"
                anchor: filterBtn
                groups: root.filterSummaryGroups
            }

            QbzContextMenu {
                id: filterMenu
                menuWidth: 280
                contentSpacing: 10

                // Header + "Clear all".
                Item {
                    width: parent ? parent.width : 0
                    height: 22
                    Text {
                        anchors.left: parent.left
                        anchors.leftMargin: 4
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Filters")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }
                    // §G.1 has no `clear_all_filters` invokable, so "Clear all"
                    // is the three writes it stands for. Same end state, no new
                    // seam.
                    Text {
                        id: clearAll
                        visible: root.activeFilterCount > 0
                        anchors.right: parent.right
                        anchors.rightMargin: 4
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.t("Clear all")
                        color: theme.accent
                        font.pixelSize: theme.fontLegal
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                QbzPurchases.setHideUnavailable(false)
                                QbzPurchases.setHideDownloaded(false)
                                QbzPurchases.setQualityFilter("all")
                                QbzPurchases.setGenreFilter("")
                            }
                        }
                    }
                }

                // Availability — ALBUMS ONLY (.slint:1090 `show-availability`).
                Column {
                    visible: root.tab !== "tracks"
                    width: parent ? parent.width : 0
                    spacing: 6
                    FilterSectionLabel { text: root.t("Availability") }
                    FilterCheckRow {
                        label: root.t("Hide unavailable")
                        checked: root.hideUnavailable
                        onToggled: QbzPurchases.setHideUnavailable(!root.hideUnavailable)
                    }
                }

                // Quality.
                Column {
                    width: parent ? parent.width : 0
                    spacing: 4
                    FilterSectionLabel { text: root.t("Quality") }
                    QualityRadioRow {
                        label: root.t("All")
                        selected: root.qualityFilter === "all"
                        onPicked: QbzPurchases.setQualityFilter("all")
                    }
                    QualityRadioRow {
                        label: root.t("Hi-Res")
                        // Bare units, not a sentence — untranslated in the
                        // .slint too (:1082).
                        hint: "24bit+"
                        selected: root.qualityFilter === "hires"
                        onPicked: QbzPurchases.setQualityFilter("hires")
                    }
                    QualityRadioRow {
                        label: root.t("CD Quality")
                        hint: "16/44.1"
                        selected: root.qualityFilter === "cd"
                        onPicked: QbzPurchases.setQualityFilter("cd")
                    }
                    QualityRadioRow {
                        label: root.t("MP3")
                        hint: "MP3"
                        // "lossy", NOT "mp3": that is the value the shared
                        // filter predicate names (`QualityFilter::Lossy`).
                        selected: root.qualityFilter === "lossy"
                        onPicked: QbzPurchases.setQualityFilter("lossy")
                    }
                }

                // Genre — ALBUMS ONLY, from the catalog's own genre on each
                // purchased album (the tracks page carries none). Absent
                // entirely while the listing has no genres to offer.
                Column {
                    visible: root.tab !== "tracks" && root.genres.length > 0
                    width: parent ? parent.width : 0
                    spacing: 4
                    FilterSectionLabel { text: root.t("Genre") }
                    QualityRadioRow {
                        label: root.t("All genres")
                        selected: root.genreFilter === ""
                        onPicked: QbzPurchases.setGenreFilter("")
                    }
                    Repeater {
                        model: root.genres
                        delegate: QualityRadioRow {
                            required property var modelData
                            label: modelData
                            selected: root.genreFilter === modelData
                            onPicked: QbzPurchases.setGenreFilter(modelData)
                        }
                    }
                }

                // Downloads.
                Column {
                    width: parent ? parent.width : 0
                    spacing: 6
                    FilterSectionLabel { text: root.t("Downloads") }
                    FilterCheckRow {
                        label: root.t("Hide downloaded")
                        checked: root.hideDownloaded
                        onToggled: QbzPurchases.setHideDownloaded(!root.hideDownloaded)
                    }
                }
            }
        }

        // --- Grid / list (ALBUMS ONLY) --------------------------------------
        // The glyph shows the OTHER mode, like Tauri and the .slint (:145-165).
        Rectangle {
            id: viewBtn
            visible: root.tab !== "tracks"
            width: root.kioskHost ? 64 : 30
            height: root.kioskHost ? 64 : 30
            radius: 6
            anchors.verticalCenter: parent.verticalCenter
            color: viewArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
            QbzIcon {
                name: root.viewMode === "grid" ? "list" : "layout-grid"
                width: 16
                height: 16
                anchors.centerIn: parent
                tintName: viewArea.containsMouse ? "textPrimary" : "muted"
            }
            MouseArea {
                id: viewArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: QbzPurchases.setViewMode(
                    root.viewMode === "grid" ? "list" : "grid")
            }
        }
    }
}
