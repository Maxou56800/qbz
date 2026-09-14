// LibraryToolbar — two rows: navigation/actions/selects above, an always-open
// search and independent opt-in filter switches below. Narrow panes scroll
// each control row horizontally instead of covering neighbouring controls.
//
// EXTRACTED from views/LibraryView.qml (track rule 2 — the file was 1,881
// lines). Everything it reads and writes goes through `view`, the LibraryView
// instance, exactly as views/local/LocalToolbar.qml does for LocalLibraryView.
// Toolbar choices are written through `view.setPref(...)` rather than straight
// onto the property, because they PERSIST (library_prefs.rs -> the same
// favorites_ui.json the Slint build writes).

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"
import "../../assets/release-sort.js" as ReleaseSort

Item {
    id: root

    readonly property var sortOptions: {
        var tab = view.activeTab
        var options = []
        function add(value, label) { options.push({value: value, label: label}) }
        // Every tab now leads with an explicit Default (the resting order),
        // so there is always a clear way back to the un-sorted state.
        add("default", QbzSession.tr("Default", QbzSession.trRev))
        add("date", QbzSession.tr("Date added", QbzSession.trRev))
        if (tab === "playlists") add("updated", QbzSession.tr("Date updated", QbzSession.trRev))
        add("title", QbzSession.tr("Alphabetical", QbzSession.trRev))
        if (tab === "tracks") add("release", QbzSession.tr("Release", QbzSession.trRev))
        if (["all", "tracks", "albums"].indexOf(tab) >= 0)
            add("artist", QbzSession.tr("Artist", QbzSession.trRev))
        if (tab === "tracks" || tab === "albums") {
            add("label", QbzSession.tr("Label", QbzSession.trRev))
            add("genre", QbzSession.tr("Genre", QbzSession.trRev))
            add("release-date", QbzSession.tr("Release date", QbzSession.trRev))
        }
        if (tab === "playlists") add("track-count", QbzSession.tr("Track count", QbzSession.trRev))
        if (["tracks", "albums", "playlists"].indexOf(tab) >= 0)
            add("duration", QbzSession.tr("Duration", QbzSession.trRev))
        return options
    }

    /// The LibraryView root.
    property var view: null

    QbzTheme { id: theme }

    // The active sort differs from the tab's resting default → the sort select
    // highlights its outline and names the sort in a tooltip. "all" rests at
    // date-added descending; every other tab rests at its "default" option.
    readonly property bool sortIsNonDefault: {
        if (view.activeTab === "all")
            return !(view.sortBy === "date" && !view.sortAsc)
        return ReleaseSort.fieldOf(view.activeSort) !== "default"
    }
    readonly property string sortActiveTooltip: {
        if (!root.sortIsNonDefault) return ""
        var field = ReleaseSort.fieldOf(view.activeSort)
        var lbl = ""
        for (var i = 0; i < root.sortOptions.length; i++)
            if (root.sortOptions[i].value === field) { lbl = root.sortOptions[i].label; break }
        if (lbl === "") return ""
        return lbl + (ReleaseSort.ascending(view.activeSort) ? "  ↑" : "  ↓")
    }

    readonly property bool singleRow: view.activeTab === "labels" || view.activeTab === "playlists"
        || view.activeTab === "artists"
    readonly property real rowGap: 8
    readonly property real secondRowY: 25 + Math.max(tabRow.height, 30) / 2 + rowGap
    readonly property real fullSearchWidth: Math.min(240, Math.max(140, root.width * 0.22))
    height: singleRow ? 50 : secondRowY + searchBox.height + rowGap
    // Compact only the count badges; genre and sort retain their labels.
    readonly property bool compactChrome: width < 1040

    // Small toolbar toggle button (ToggleButton sm): 30px, active = accent.
    component ToolToggle: Rectangle {
        property string name: ""
        property bool active: false
        signal clicked()
        width: 30
        height: 30
        radius: 6
        color: active ? (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
             : ttArea.containsMouse ? theme.surfaceHover : "transparent"
        QbzIcon {
            name: parent.name
            width: 16
            height: 16
            anchors.centerIn: parent
            tintName: parent.active ? "accent"
                   : ttArea.containsMouse ? "textPrimary" : "secondary"
        }
        MouseArea {
            id: ttArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: parent.clicked()
        }
    }

    // Filter-by-genre trigger (FavoritesView.slint's FavGenreButton, sm):
    // accent fill + "N genres" while the "library-all" selection is active.
    component GenreToolButton: Rectangle {
        id: gtb
        readonly property bool active: root.view.genreCount > 0
        width: gtbRow.implicitWidth
        height: 30
        radius: 6
        // Homologated with the selects (QbzSelect): the same ambient-aware
        // rest fill and, when a genre is applied, the SAME accent OUTLINE —
        // not an accent fill — so the three toolbar controls read identically
        // in their modified state.
        color: gtbArea.containsMouse ? theme.surfaceHover
             : (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
        border.width: gtb.active ? 1 : 0
        border.color: gtb.active ? theme.accent : "transparent"
        Row {
            id: gtbRow
            anchors.centerIn: parent
            height: parent.height
            leftPadding: 10
            rightPadding: 12
            spacing: 7
            QbzIcon {
                name: "list-filter"
                width: 13
                height: 13
                anchors.verticalCenter: parent.verticalCenter
                // Same pair-of-halves fix as controls/BrowseGenreButton.qml:
                // the glyph said "primary" (legacy alias of a literal
                // #ffffff) next to an accent-text label, i.e. a white glyph
                // beside a black label on the pale-accent themes.
                // favorites/FavoritesView.slint:301 + :309 are consistently
                // #ffffff, but that white is 1.70:1 on high-contrast, 1.74
                // on ikari, 1.82 on wcag-dark and under 2.6:1 on 16 of the
                // 35 palettes — deliberate divergence from both lines.
                // theme/QbzTheme.qml, "ON AN ACCENT FILL".
                tintName: gtb.active ? "accent" : "secondary"
            }
            Text {
                visible: true
                anchors.verticalCenter: parent.verticalCenter
                text: root.view.genreCount === 0
                    ? QbzSession.tr("Filter by genre", QbzSession.trRev)
                    : root.view.genreCount === 1
                        ? QbzSession.tr("1 genre", QbzSession.trRev)
                        : QbzSession.tr("{} genres", QbzSession.trRev)
                            .replace("{}", root.view.genreCount)
                // The colour twin of the glyph's tint above — accent-text on
                // 34 of the 35 palettes.
                color: gtb.active ? theme.accent : theme.textSecondary
                font.pixelSize: 12
            }
        }
        MouseArea {
            id: gtbArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                libFilterTip.exit()
                root.view.toggleGenrePopup()
            }
            onEntered: libFilterTip.enter()
            onExited: libFilterTip.exit()
        }

        // The genre chip is the toolbar's filter affordance, so it carries the
        // whole bar's summary: the genres it owns, plus the source switches
        // that live next to it and are just as invisible once set.
        QbzFilterTip {
            id: libFilterTip
            ownerKey: "library-all-filter"
            anchor: gtb
            groups: {
                var out = []
                var names = (root.view.genreDoc.names || {})[root.view.genreContext] || []
                if (names.length > 0)
                    out.push({ group: QbzSession.tr("Genre", QbzSession.trRev),
                               values: names })
                var ex = root.view.filterSummaryGroups || []
                for (var i = 0; i < ex.length; i++)
                    out.push(ex[i])
                return out
            }
        }
    }

    // --- Group-by selects (FavoritesView.slint:604-670, 760-775).
    // One control, three tabs: each carries its own option set and writes its
    // own pref. The reference draws a QbzSelect per tab; this is the same menu
    // shape the sort popups beside it already use, so the toolbar stays
    // visually of a piece.
    // Now a QbzSelect (sm) like the sort control beside it: one select style,
    // ambient-aware, with the same popup. `options` are {value,label} — the
    // primitive reads `.label`; the value maps back through the picked index.
    // The outline highlights once a real grouping (not the resting "off") is on.
    component GroupSelect: QbzSelect {
        id: gsRoot
        property string current: "off"
        signal picked(string value)
        visible: false
        sm: true
        menuWidth: 180
        popupWidth: 180
        currentIndex: {
            for (var i = 0; i < gsRoot.options.length; i++)
                if (gsRoot.options[i].value === gsRoot.current) return i
            return 0
        }
        outlineActive: gsRoot.current !== "off"
        onSelected: function (index) {
            if (index >= 0 && index < gsRoot.options.length)
                gsRoot.picked(gsRoot.options[index].value)
        }
    }

    // Navigation and labelled selects share the first row.
    Flickable {
        id: topChrome
        width: parent.width
        height: 52
        contentWidth: Math.max(width, topControls.x + topControls.width + 32)
        contentHeight: height
        flickableDirection: Flickable.HorizontalFlick
        boundsBehavior: Flickable.StopAtBounds
        clip: true

    Rectangle {
        x: 32
        y: 25 - height / 2
        width: tabRow.width
        height: tabRow.height
        // Ambient-aware like the tab bar it hosts (the strip read opaque
        // under the ambient field while Discover's and Local's had alpha).
        color: (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
        radius: 6
        Row {
            id: tabRow
            padding: 3
            spacing: 4
            QbzTabBar {
                counts: !root.compactChrome
                underline: true
                activeId: root.view.activeTab
                tabs: [
                    { "id": "all", "label": QbzSession.tr("All", QbzSession.trRev), "count": root.view.counts.all || 0 },
                    { "id": "tracks", "label": QbzSession.tr("Tracks", QbzSession.trRev), "count": root.view.tabTotals.tracks || 0 },
                    { "id": "albums", "label": QbzSession.tr("Releases", QbzSession.trRev), "count": root.view.tabTotals.albums || 0 },
                    { "id": "artists", "label": QbzSession.tr("Artists", QbzSession.trRev), "count": root.view.counts.artists || 0 },
                    { "id": "playlists", "label": QbzSession.tr("Playlists", QbzSession.trRev), "count": root.view.tabTotals.playlists || 0 },
                    { "id": "labels", "label": QbzSession.tr("Labels", QbzSession.trRev), "count": root.view.counts.labels || 0 },
                ]
                onSelected: function (id) { root.view.activeTab = id }
            }
        }
    }

    // Per-tab controls.
    Row {
        id: topControls
        x: Math.max(32 + tabRow.width + 16, topChrome.width - width - 32)
        y: 25 - height / 2
        height: 30
        spacing: 8

        Item {
            id: firstRowSearchSlot
            visible: root.singleRow
            width: searchBox.width
            height: 30
        }

        GenreToolButton {
            visible: ["all", "tracks", "albums"].indexOf(root.view.activeTab) >= 0
                && root.view.tabHasItems
        }
        GroupSelect {
            visible: root.view.activeTab === "tracks" && root.view.tabHasItems
            current: root.view.tracksGroup
            options: [
                { "value": "off", "label": QbzSession.tr("Group: Off", QbzSession.trRev) },
                { "value": "album", "label": QbzSession.tr("Group: Album", QbzSession.trRev) },
                { "value": "artist", "label": QbzSession.tr("Group: Artist", QbzSession.trRev) },
                { "value": "name", "label": QbzSession.tr("Group: Name", QbzSession.trRev) },
            ]
            onPicked: function (v) { root.view.setPref("tracksGroup", v) }
        }
        GroupSelect {
            visible: root.view.activeTab === "albums" && root.view.tabHasItems
            current: root.view.albumsGroup
            options: [
                { "value": "off", "label": QbzSession.tr("Group: Off", QbzSession.trRev) },
                { "value": "alpha", "label": QbzSession.tr("Group: A-Z", QbzSession.trRev) },
                { "value": "artist", "label": QbzSession.tr("Group: Artist", QbzSession.trRev) },
                { "value": "year", "label": QbzSession.tr("Group: Year", QbzSession.trRev) },
                { "value": "decade", "label": QbzSession.tr("Group: Decade", QbzSession.trRev) },
            ]
            onPicked: function (v) { root.view.setPref("albumsGroup", v) }
        }
        // Grid grouping is independent of the chosen sort.
        GroupSelect {
            visible: root.view.activeTab === "artists" && root.view.tabHasItems
                && root.view.artistsView === "grid"
            current: root.view.artistsGroup
            options: [
                { "value": "off", "label": QbzSession.tr("Group: Off", QbzSession.trRev) },
                { "value": "alpha", "label": QbzSession.tr("Group: A-Z", QbzSession.trRev) },
            ]
            onPicked: function (v) { root.view.setPref("artistsGroup", v) }
        }

        QbzSelect {
            id: sortSelect
            sm: true
            // Collapsed control is a compact "Sort" pill; the list stays wide.
            menuWidth: 140
            popupWidth: 200
            leadingIcon: "arrow-down-up"
            placeholderText: QbzSession.tr("Sort", QbzSession.trRev)
            outlineActive: root.sortIsNonDefault
            tooltipHost: libTips
            tooltipText: root.sortActiveTooltip
            tooltipKey: "library-sort"
            options: root.sortOptions
            searchable: options.length > 8
            currentIndex: {
                // On "all" the resting order is date-desc; map that to the
                // explicit Default row so the list highlights it (and the
                // outline stays off) at the default.
                if (root.view.activeTab === "all"
                        && root.view.sortBy === "date" && !root.view.sortAsc) {
                    for (var j = 0; j < options.length; ++j)
                        if (options[j].value === "default") return j
                }
                var field = ReleaseSort.fieldOf(root.view.activeSort)
                for (var i = 0; i < options.length; ++i)
                    if (options[i].value === field) return i
                return 0
            }
            sortDirection: ReleaseSort.ascending(root.view.activeSort) ? "asc" : "desc"
            onSelected: function(index) { root.view.pickSort(options[index].value) }
        }
        QbzSelect {
            visible: root.view.activeTab === "playlists"
            sm: true
            menuWidth: 180
            outlineActive: root.view.playlistsSubTab !== "all"
            options: [QbzSession.tr("All", QbzSession.trRev),
                QbzSession.tr("By you", QbzSession.trRev),
                QbzSession.tr("By Qobuz", QbzSession.trRev),
                QbzSession.tr("By others", QbzSession.trRev)]
            currentIndex: Math.max(0, ["all", "you", "qobuz", "others"].indexOf(root.view.playlistsSubTab))
            onSelected: function (index) {
                root.view.playlistsSubTab = ["all", "you", "qobuz", "others"][index]
            }
        }
        Rectangle {
            visible: rightButtons.width > 0
            width: 1
            height: 20
            anchors.verticalCenter: parent.verticalCenter
            color: theme.borderSubtle
        }
        Row {
            id: rightButtons
            height: 30
            spacing: 8
            Row {
                id: actionRow
                height: 30
                spacing: 8

                function randomVisibleId(kind) {
                    var pool = []
                    var rows = root.view.visibleRows
                    for (var i = 0; i < rows.length; i++)
                        if (rows[i].kind === kind) pool.push(rows[i])
                    if (pool.length === 0) return null
                    return pool[Math.floor(Math.random() * pool.length)]
                }

                // Tracks — play all / shuffle all / select multiple.
                QbzIconButton {
                    visible: root.view.activeTab === "tracks" && root.view.tabHasItems
                    btnSize: 30
                    name: "play-fill"
                    onClicked: QbzLibrary.libraryPlayAll(
                        JSON.stringify(root.view.visibleTrackIds()), false)
                }
                QbzIconButton {
                    visible: root.view.activeTab === "tracks" && root.view.tabHasItems
                    btnSize: 30
                    name: "shuffle"
                    onClicked: QbzLibrary.libraryPlayAll(
                        JSON.stringify(root.view.visibleTrackIds()), true)
                }
                ToolToggle {
                    visible: root.view.activeTab === "tracks" && root.view.tabHasItems
                    name: "square-check-big"
                    active: root.view.tracksMultiSelect
                    onClicked: root.view.setTracksMultiSelect(!root.view.tracksMultiSelect)
                }
                // Albums — play a RANDOM album (the reference's `albums_shuffle`,
                // which picks one visible album, not a shuffled play of every album).
                QbzIconButton {
                    visible: root.view.activeTab === "albums" && root.view.tabHasItems
                    btnSize: 30
                    name: "shuffle"
                    onClicked: {
                        var pick = actionRow.randomVisibleId("album")
                        if (pick) QbzPlayer.playAlbum(pick.id)
                    }
                }
                // Albums multi-select — LIST mode ONLY (owner 2026-07-24,
                // FavoritesView.slint:512-523): the grid card has no checkbox slot.
                ToolToggle {
                    visible: root.view.activeTab === "albums" && root.view.tabHasItems
                        && root.view.albumsView === "list"
                    name: "square-check-big"
                    active: root.view.albumsMultiSelect
                    onClicked: root.view.setAlbumsMultiSelect(!root.view.albumsMultiSelect)
                }
                // Artists — open a random artist.
                QbzIconButton {
                    visible: root.view.activeTab === "artists" && root.view.tabHasItems
                    btnSize: 30
                    name: "shuffle"
                    onClicked: {
                        var pick = actionRow.randomVisibleId("artist")
                        if (pick) QbzArtist.openArtist(pick.id)
                    }
                }
                // Playlists — play a random playlist.
                QbzIconButton {
                    visible: root.view.activeTab === "playlists" && root.view.tabHasItems
                    btnSize: 30
                    name: "shuffle"
                    onClicked: {
                        var pick = actionRow.randomVisibleId("playlist")
                        if (pick) QbzBridge.openPlaylist(pick.id)
                    }
                }
                // Labels — open a random label's landing.
                QbzIconButton {
                    visible: root.view.activeTab === "labels" && root.view.tabHasItems
                    btnSize: 30
                    name: "shuffle"
                    onClicked: {
                        var pick = actionRow.randomVisibleId("label")
                        if (pick) QbzHome.openLabel(pick.id)
                    }
                }
            }

            ToolToggle {
                visible: root.view.activeTab === "all"
                name: root.view.viewMode === "list" ? "layout-grid" : "list"
                active: false
                onClicked: root.view.viewMode = root.view.viewMode === "list" ? "grid" : "list"
            }
            // Albums grid / list toggle (FavoritesView.slint:697-705 ViewToggle).
            ToolToggle {
                visible: root.view.activeTab === "albums" && root.view.tabHasItems
                name: root.view.albumsView === "list" ? "layout-grid" : "list"
                active: false
                onClicked: root.view.setPref("albumsView",
                    root.view.albumsView === "list" ? "grid" : "list")
            }
            // Playlists grid / list toggle (FavoritesView.slint:737-745).
            ToolToggle {
                visible: root.view.activeTab === "playlists" && root.view.tabHasItems
                name: root.view.playlistsView === "list" ? "layout-grid" : "list"
                active: false
                onClicked: root.view.setPref("playlistsView",
                    root.view.playlistsView === "list" ? "grid" : "list")
            }
            // Artists grid / sidepanel toggle (FavoritesView.slint:780-793 —
            // `active: false` there too, "keeps the two icon states visually
            // identical").
            ToolToggle {
                visible: root.view.activeTab === "artists" && root.view.tabHasItems
                name: root.view.artistsView === "sidepanel" ? "layout-grid" : "list"
                active: false
                onClicked: root.view.setPref("artistsView",
                    root.view.artistsView === "sidepanel" ? "grid" : "sidepanel")
            }
        }
    }

    } // topChrome

    // Search remains visible, including when a restriction produces no rows.
    QbzLineEdit {
        id: searchBox
        objectName: "librarySearch"
        parent: root.singleRow ? firstRowSearchSlot : root
        x: root.singleRow ? 0 : 34
        y: root.singleRow ? 0 : root.secondRowY
        width: root.fullSearchWidth * (root.singleRow ? 0.6 : 1)
        searchMode: true
        expandable: false
        sm: true
        text: root.view.activeTab === "all" ? root.view.search : root.view.tabSearch
        placeholder: root.view.activeTab === "tracks" ? QbzSession.tr("Search your tracks", QbzSession.trRev)
            : root.view.activeTab === "albums" ? QbzSession.tr("Search your releases", QbzSession.trRev)
            : root.view.activeTab === "playlists" ? QbzSession.tr("Search your playlists", QbzSession.trRev)
            : root.view.activeTab === "artists" ? QbzSession.tr("Search your artists", QbzSession.trRev)
            : root.view.activeTab === "labels" ? QbzSession.tr("Search your labels", QbzSession.trRev)
            : QbzSession.tr("Search your library", QbzSession.trRev)
        onEdited: function (value) {
            if (root.view.activeTab === "all") root.view.setAllSearch(value)
            else root.view.tabSearch = value
        }
    }

    // Compact Library-only switch: glyph travels inside the thumb.
    component SourceSwitch: Rectangle {
        id: sourceSwitch
        property string label: ""
        property string explanation: ""
        property string glyph: ""
        property bool checked: false
        signal toggled(bool value)
        width: 46
        height: 26
        anchors.verticalCenter: parent.verticalCenter
        radius: 13
        color: checked ? theme.accent : theme.surfaceHover
        activeFocusOnTab: true
        border.width: activeFocus ? 2 : 0
        border.color: theme.accent
        Accessible.role: Accessible.CheckBox
        Accessible.name: label
        Accessible.description: explanation
        Accessible.checked: checked
        Accessible.onToggleAction: sourceSwitch.toggled(!checked)
        Keys.onPressed: function (event) {
            if (event.key === Qt.Key_Space || event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                if (!event.isAutoRepeat) sourceSwitch.toggled(!checked)
                event.accepted = true
            }
        }
        Rectangle {
            width: 20
            height: 20
            radius: 10
            x: sourceSwitch.checked ? sourceSwitch.width - width - 3 : 3
            y: 3
            color: theme.textPrimary
            QbzIcon {
                anchors.centerIn: parent
                width: 15
                height: 15
                visible: sourceSwitch.glyph !== ""
                name: sourceSwitch.glyph
                tintName: theme.isDark ? "black" : "white"
            }
            Text {
                anchors.centerIn: parent
                visible: sourceSwitch.glyph === ""
                text: "Hi-Res"
                font.pixelSize: 7
                font.weight: Font.DemiBold
                color: theme.surfaceMain
            }
        }
        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onPressed: sourceSwitch.forceActiveFocus()
            onClicked: sourceSwitch.toggled(!sourceSwitch.checked)
        }
        HoverHandler { onHoveredChanged: hovered ? switchTip.enter() : switchTip.exit() }
        QbzFilterTip {
            id: switchTip
            ownerKey: "library-switch-" + sourceSwitch.label
            anchor: sourceSwitch
            groups: [{group: sourceSwitch.label, values: [sourceSwitch.explanation]}]
        }
    }

    Flickable {
        id: filterChrome
        x: 34 + searchBox.width + 20
        width: Math.max(0, root.width - x - 32)
        y: root.secondRowY
        visible: !root.singleRow
        height: 30
        clip: true
        contentWidth: Math.max(width, switches.width)
        contentHeight: height
        flickableDirection: Flickable.HorizontalFlick
        boundsBehavior: Flickable.StopAtBounds
        Row {
            id: switches
            x: Math.max(0, filterChrome.width - width)
            height: 30
            spacing: 12
            readonly property bool sourceTab: root.view.activeTab === "all"
                || root.view.activeTab === "tracks" || root.view.activeTab === "albums"
            SourceSwitch {
                visible: switches.sourceTab
                glyph: "shopping-bag"
                label: QbzSession.tr("Purchases only", QbzSession.trRev)
                explanation: QbzSession.tr("Show only purchased items. Combines with the other enabled filters.", QbzSession.trRev)
                checked: root.view.showPurchases
                onToggled: function (value) { root.view.setShowPurchases(value) }
            }
            SourceSwitch {
                visible: switches.sourceTab
                glyph: "heart"
                label: QbzSession.tr("Favorites only", QbzSession.trRev)
                explanation: QbzSession.tr("Show only favorites. Combines with the other enabled filters.", QbzSession.trRev)
                checked: root.view.showFavorites
                onToggled: function (value) { root.view.setShowFavorites(value) }
            }
            SourceSwitch {
                visible: root.view.activeTab === "all"
                glyph: "user-plus"
                label: QbzSession.tr("Following only", QbzSession.trRev)
                explanation: QbzSession.tr("Show only followed items. Combines with the other enabled filters.", QbzSession.trRev)
                checked: root.view.showFollowing
                onToggled: function (value) { root.view.setShowFollowing(value) }
            }
            SourceSwitch {
                visible: root.view.activeTab === "tracks" || root.view.activeTab === "albums"
                label: QbzSession.tr("Hi-Res only", QbzSession.trRev)
                explanation: QbzSession.tr("Show only Hi-Res items. Combines with the other enabled filters.", QbzSession.trRev)
                checked: root.view.hiresOnly
                onToggled: function (value) { root.view.setHiresOnly(value) }
            }
            SourceSwitch {
                visible: root.view.activeTab === "all"
                glyph: "hard-drive"
                label: QbzSession.tr("Hide local albums from library", QbzSession.trRev)
                explanation: QbzSession.tr("Hide local albums here. They remain available in Local Library.", QbzSession.trRev)
                checked: !root.view.showLocal
                onToggled: function (value) { root.view.setShowLocal(!value) }
            }
        }
    }

    function closeTabSearch() {
        searchBox.text = Qt.binding(function () {
            return root.view.activeTab === "all" ? root.view.search : root.view.tabSearch
        })
    }

    // Shared hover-tooltip overlay for this toolbar (the sort control names its
    // active sort here when it differs from the tab default). Mounted last so
    // it paints above the controls; QueueView uses the same pattern.
    QbzTooltip {
        id: libTips
        anchors.fill: parent
        z: 4000
    }
}
