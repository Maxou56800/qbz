// Label landing page — QML port of label/LabelPageView.slint.
//
// A circular-portrait header with description + Follow, a JUMP TO strip, a
// Popular Tracks list with a 5 -> 20 -> 50 reveal, and the Releases /
// Critics' Picks / Playlists / Artists / More Labels carousels.
//
// SCROLLING-PAGE CONVENTION (AlbumView / ArtistView / PlaylistView): the
// whole page scrolls in one Flickable whose Column pads 32 / 32 / top 11 /
// bottom 100. The .slint's NavButtons row is dropped (back/forward live in
// the shell header, HeaderBar.qml:235) but its 22px spacer is KEPT, so the
// header sits where every other detail page's does.
//
// POC-NOTEs, each deliberate:
// - JUMP TO is NOT sticky. ArtistView.qml:1204 documents the same call for
//   the .slint's sticky clamp; matching it keeps the two detail pages
//   consistent. This is the one accepted visual delta.
// - NO multi-select — and the ORIGINAL reason is now STALE on both halves:
//   rows/TrackRow.qml:116 DOES have the `selectMode` arm, and add-to-playlist
//   DOES have a bridge seam (QbzPlaylistPicker.openForTracks,
//   src/playlist_picker_bridge.rs). What is still missing is only the
//   selection STATE and the bar itself (LibraryView.qml's `tracksSelected`
//   map is the pattern to copy). Until then the select disc renders DIMMED
//   and inert (the ArtistView "Radio" precedent) and the bar is absent.
//   Per-ROW add-to-playlist and add-to-mixtape are both LIVE on this page
//   already, through the shared TrackRow menu.
// - NO "In library" tab. The Qt library feed carries no label id, so
//   `libraryCount` is always 0 and the SegmentedTabBar never mounts (see
//   src/label_qt.rs). A toggle that switches to an empty tab is worse.
// - Share is live through the Rust clipboard bridge and shared toast host.
// - Back/forward scroll restore is handled by ScrollMemory below.
//
// LOGO FIT — a CONSCIOUS deviation: both .slint label views draw the logo
// `image-fit: cover` (LabelPageView.slint:116). This port draws it CONTAIN,
// the way qml/cards/LabelCard.qml already does, because label logos are
// wordmarks on transparent backgrounds and cropping one to a circle cuts the
// name off. The Slint behaviour is cover; this is not it, on purpose.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../cards"
import "../controls"
import "../rows"
import "../theme"
import "../kiosk"

Rectangle {
    id: root
    property bool kioskHost: false

    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    radius: 12

    QbzTheme { id: theme }

    readonly property var doc: {
        try {
            return JSON.parse(QbzHome.labelJson)
        } catch (e) {
            return {}
        }
    }
    readonly property var topTracks: doc.topTracks || []
    readonly property var releases: doc.releases || []
    readonly property var critics: doc.critics || []
    readonly property var playlists: doc.playlists || []
    readonly property var artists: doc.artists || []
    readonly property var moreLabels: doc.moreLabels || []
    readonly property var jumpTabs: doc.jumpTabs || []

    property string activeJumpTab: "about"
    /// Popular Tracks progressive reveal: 5 -> 20 -> 50 (the Tauri steps).
    property int previewCount: 5
    property string loadedLabelId: ""

    // Reset the per-label view state when the id actually changes (the
    // document is republished on the artwork pass too — ArtistView's
    // syncArtistState rule).
    onDocChanged: {
        var id = doc.id || ""
        if (id === root.loadedLabelId)
            return
        root.loadedLabelId = id
        root.previewCount = 5
        root.activeJumpTab = "about"
        root.setMultiSelect(false)
        flick.contentY = 0
    }

    // --- Multi-select (Popular Tracks) -------------------------------------
    // Same shape as AlbumView/PlaylistView: selection in QML, select-all /
    // clear local, the rest via QbzPlayer.bulkTracksAction.
    property bool multiSelect: false
    property var selected: ({})
    readonly property int selectedCount: Object.keys(root.selected).length
    readonly property bool multiSelectOn: root.multiSelect
    function setMultiSelect(on) {
        root.multiSelect = on
        if (!on) { root.selected = ({}); sel.anchorId = "" }
    }
    /// Excel-style selection lives in ONE place — controls/SelectionModel.qml
    /// holds the anchor and the Shift-range rule; this view keeps owning its
    /// map. `mods` is the mouse event's modifiers, forwarded by the row; a
    /// caller with no event (the checkbox, a keyboard path) may omit it.
    SelectionModel { id: sel }
    function toggleSelected(id, mods) {
        root.selected = sel.next(root.selected, id, root.topTracks,
                                 mods === undefined ? Qt.NoModifier : mods)
    }
    function selectedIdsInOrder() {
        var rows = root.topTracks
        var out = []
        for (var i = 0; i < rows.length; i++)
            if (root.selected[rows[i].id] === true) out.push(rows[i].id)
        return out
    }
    function bulkAction(action) {
        if (action === "select-all") {
            var m = {}
            var rows = root.topTracks
            for (var i = 0; i < rows.length; i++) m[rows[i].id] = true
            root.selected = m
            return
        }
        if (action === "clear") { root.selected = ({}); sel.anchorId = ""; return }
        var ids = root.selectedIdsInOrder()
        if (ids.length === 0) return
        QbzPlayer.bulkTracksAction(JSON.stringify(ids), action, "label", String(doc.id || ""))
        if (action !== "add-to-playlist" && action !== "add-to-mixtape")
            root.selected = ({})
    }
    // Ctrl+A / Escape hotkey router seam (AppShell duck-types these).
    function selectAll() {
        if (!root.multiSelect) root.setMultiSelect(true)
        root.bulkAction("select-all")
    }
    function exitMultiSelectMode() {
        if (root.multiSelect) root.setMultiSelect(false)
    }

    function scrollToSection(id) {
        root.activeJumpTab = id
        if (id === "about") {
            flick.contentY = 0
            return
        }
        for (var i = 0; i < catalog.children.length; i++) {
            var c = catalog.children[i]
            if (c.anchorId !== undefined && c.anchorId === id) {
                var p = c.mapToItem(flick.contentItem, 0, 0)
                flick.contentY = Math.max(0, p.y - 48)
                return
            }
        }
    }

    // --- Share ------------------------------------------------------------
    // The QML `Loader`/`TextEdit`/`copy()` clipboard hack that used to live
    // here is GONE. Its header said "there is no toast seam, so the copy is
    // silent" — both halves of that are now false (`toast_qt.rs`, and
    // `share_qt.rs` since the artist Share landed), and the hack was not
    // merely silent: it formatted `https://open.qobuz.com/label/{id}`, a host
    // with no `/label/` route, so the link it copied did not resolve at all.
    // The header ⋯ entry calls `QbzHome.labelShare(id)` and Rust owns the URL,
    // the clipboard and the "Link copied" toast — 1:1 with main.rs:13164-13178.

    // ============================ shared rail =============================
    // ONE horizontal rail for all five carousels; `kind` picks the card, the
    // way the .slint mounts Carousel / PlaylistCarousel / ArtistCarousel.
    component CardRail: Column {
        id: cr
        property string title: ""
        property var items: []
        /// "album" | "playlist" | "artist" | "label"
        property string kind: "album"
        property bool showViewAll: false
        signal viewAllClicked()

        width: parent ? parent.width : 0
        spacing: 12

        readonly property int perPage: Math.max(1, Math.floor((crList.width + 32) / 232))
        readonly property int step: perPage * 232
        readonly property real maxScroll: Math.max(0, crList.contentWidth - crList.width)

        QbzSectionHeader {
            kioskHost: root.kioskHost
            title: cr.title
            leftEnabled: crList.contentX > 1
            rightEnabled: crList.contentX < cr.maxScroll - 1
            showViewAll: cr.showViewAll
            onViewAllClicked: cr.viewAllClicked()
            onPageLeft: crList.contentX = Math.max(0, crList.contentX - cr.step)
            onPageRight: crList.contentX = Math.min(cr.maxScroll, crList.contentX + cr.step)
        }
        Item {
            width: parent.width
            height: root.kioskHost && (cr.kind === "album" || cr.kind === "playlist") ? 64 : 246
            ListView {
                id: crList
                anchors.fill: parent
                orientation: ListView.Horizontal
                spacing: 32
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                cacheBuffer: 0
                model: !root.kioskHost || (cr.mapToItem(page, 0, 0).y < flick.contentY + flick.height && cr.mapToItem(page, 0, cr.height).y > flick.contentY) ? cr.items : []
                delegate: Item {
                    id: crCell
                    required property var modelData
                    width: root.kioskHost ? 340 : 200
                    height: root.kioskHost && (cr.kind === "album" || cr.kind === "playlist") ? 64 : 246

                    KioskCoverSource { id: cellArt; remote: root.kioskHost ? (crCell.modelData.artUrl || "") : ""; local: crCell.modelData.artPath || ""; edge: 44 }
                    Component { id: crKioskAlbum; AlbumListRow { kioskHost: true; showLabel: false; item: crCell.modelData; artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "") } }
                    Component { id: crKioskPlaylist; PlaylistListRow { kioskHost: true; item: crCell.modelData; artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "") } }
                    Component {
                        id: crAlbum
                        AlbumCard {
                            albumId: crCell.modelData.id
                            title: crCell.modelData.title
                            artist: crCell.modelData.artist
                            artistId: crCell.modelData.artistId
                            genre: crCell.modelData.genre
                            year: crCell.modelData.year
                            qualityTier: crCell.modelData.qualityTier
                            qualityDetail: crCell.modelData.qualityDetail || ""
                            ribbon: crCell.modelData.ribbon || ""
                            ribbonKind: crCell.modelData.ribbonKind || ""
                            artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "")
                            isPinned: crCell.modelData.isPinned === true
                            // The pin payload's display snapshot — the REMOTE
                            // url (artPath is the local cache path). Without
                            // it a label release pinned from here landed in
                            // the Home Pinned rail as a grey placeholder.
                            artworkUrl: crCell.modelData.artUrl || ""
                            // Stamped on the row by label_qt; false made the
                            // glyph lie and inverted the first click.
                            isFavorite: crCell.modelData.isFavorite === true
                        }
                    }
                    Component {
                        id: crPlaylist
                        PlaylistCard {
                            // artworkUrl defaults to `item.artUrl` (the card's
                            // own arm), so only the pin state is handed over.
                            item: crCell.modelData
                            artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "")
                            isPinned: crCell.modelData.isPinned === true
                        }
                    }
                    Component {
                        id: crArtist
                        ArtistCard {
                            item: crCell.modelData
                            artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "")
                            isPinned: crCell.modelData.isPinned === true
                            artworkUrl: crCell.modelData.artUrl || ""
                            // LabelPageView.slint:524 passes card-follow-mode
                            // "none" here — and now that ArtistCard's follow
                            // arm is LIVE (its `hasFollowSeam: false` was a
                            // factual error: library_toggle_favorite routes
                            // "artist"), this line is what actually keeps the
                            // chip off this carousel instead of a constant.
                            followMode: "none"
                        }
                    }
                    Component {
                        id: crLabel
                        LabelCard {
                            item: crCell.modelData
                            artSource: root.kioskHost ? cellArt.source : (crCell.modelData.artPath || "")
                        }
                    }
                    Loader {
                        anchors.fill: parent
                        sourceComponent: root.kioskHost && cr.kind === "album" ? crKioskAlbum
                            : root.kioskHost && cr.kind === "playlist" ? crKioskPlaylist
                            : cr.kind === "playlist" ? crPlaylist
                            : cr.kind === "artist" ? crArtist
                            : cr.kind === "label" ? crLabel : crAlbum
                    }
                }
            }
        }
    }

    // ============================ offline gate ============================
    QbzOfflinePlaceholder {
        visible: QbzSession.offline
        anchors.centerIn: parent
        showSettingsAction: true
        onSettingsClicked: QbzShell.navigateTo("settings")
    }

    // ============================ the page ================================
    Flickable {
        id: flick
        anchors.fill: parent
        visible: !QbzSession.offline
        clip: true
        contentWidth: width
        contentHeight: page.implicitHeight
        boundsBehavior: Flickable.StopAtBounds

        Column {
            id: page
            width: parent.width
            leftPadding: 32
            rightPadding: 32
            topPadding: 11
            bottomPadding: 100
            spacing: 0

            // The .slint's NavButtons row is not drawn (shell header owns
            // back/forward) but its 22px spacer is kept.
            Item { width: 1; height: 22 }

            // --- Header ---------------------------------------------------
            Row {
                width: parent.width - 64
                spacing: 32

                Rectangle {
                    width: 180
                    height: 180
                    radius: 90
                    color: theme.surfaceElevated
                    clip: true

                    QbzIcon {
                        visible: (root.doc.imageUrl || "") === ""
                        name: "disc-3"
                        width: 60
                        height: 60
                        anchors.centerIn: parent
                        tintName: "muted"
                    }
                    // Contain-fit inside the square inscribed in the circle
                    // (180 / sqrt(2) ~= 127 -> margin 26) — see the header
                    // note on the logo-fit deviation.
                    RoundedImage {
                        anchors.fill: parent
                        anchors.margins: 26
                        source: root.doc.artPath || ""
                        radius: 0
                        fit: "contain"
                    }
                }

                Column {
                    width: parent.width - 180 - 32
                    anchors.top: parent.top
                    anchors.topMargin: 8
                    spacing: 0

                    Text {
                        text: QbzSession.tr("LABEL", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 10
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.5
                    }
                    Item { width: 1; height: 6 }
                    Text {
                        width: parent.width
                        text: root.doc.name || ""
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }

                    Item { visible: (root.doc.description || "") !== ""; width: 1; height: 12 }
                    Text {
                        visible: (root.doc.description || "") !== ""
                        // .slint: `width: root.width - 296px`.
                        width: Math.max(0, root.width - 296)
                        text: root.doc.descriptionShort || ""
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    Item { visible: root.doc.descriptionTruncated === true; width: 1; height: 4 }
                    Text {
                        visible: root.doc.descriptionTruncated === true
                        text: QbzSession.tr("Read more", QbzSession.trRev)
                        color: readMoreArea.containsMouse ? theme.accentHover : theme.accent
                        font.pixelSize: theme.fontLegal
                        MouseArea {
                            id: readMoreArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                var shell = root.parent
                                while (shell && shell.openTextModal === undefined)
                                    shell = shell.parent
                                if (!shell) return
                                // The .slint modal heading is @tr("About"),
                                // not the label name.
                                shell.openTextModal(
                                    QbzSession.tr("About", QbzSession.trRev),
                                    root.doc.description || "")
                            }
                        }
                    }

                    Item { width: 1; height: 18 }

                    // Action row — NO Play button (owner call, .slint:171-179:
                    // Popular Tracks already carries one and it does the same).
                    Row {
                        width: parent.width
                        spacing: 12
                        QbzCircleAction {
                            id: followBtn
                            readonly property bool following: root.doc.isFollowing === true
                            name: following ? "check" : "user-plus"
                            active: following
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzHome.labelFollow()
                        }
                        QbzCircleAction {
                            name: "shuffle"
                            btnEnabled: root.topTracks.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzHome.labelShuffle()
                        }
                        QbzCircleAction {
                            id: headerMoreBtn
                            name: "ellipsis"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: function (mouse) {
                                headerMenu.openAtCursor(headerMoreBtn, mouse.x, mouse.y)
                            }
                        }
                        // Clamped stretch (ArtistView.qml:1161 — an unclamped
                        // negative width silently reflows the whole row).
                        Item {
                            width: Math.max(0, parent.width - 3 * 32 - 3 * 12)
                            height: 1
                        }
                    }
                }
            }

            CardMenu {
                id: headerMenu
                menuWidth: 224
                entries: [
                    { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next",
                      "enabled": root.topTracks.length > 0 },
                    { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later",
                      "enabled": root.topTracks.length > 0 },
                    { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue",
                      "enabled": root.topTracks.length > 0 },
                    { "label": QbzSession.tr("Share", QbzSession.trRev), "icon": "link", "action": "share" },
                ]
                onPicked: function (a) {
                    // Share goes through Rust (share_qt::share_label): it owns
                    // the long-lived clipboard AND the "Link copied" toast the
                    // reference raises (main.rs:13164-13178). The URL is built
                    // there too — this call site used to format it here and
                    // got the HOST wrong (see the header note above).
                    if (a === "share")
                        QbzHome.labelShare(root.doc.id || "")
                    else
                        QbzHome.labelTopAction(a)
                }
            }

            // --- JUMP TO ---------------------------------------------------
            // The .slint reserves 52px here for its sticky overlay; this port
            // puts the bar itself in the flow (see the header POC-NOTE).
            Item { visible: root.jumpTabs.length > 1; width: 1; height: visible ? 8 : 0 }
            QbzJumpNavBar {
                visible: root.jumpTabs.length > 1
                width: parent.width - 64
                padH: 0
                showSearch: false
                // surface-main @ bar-alpha (0.3), LabelPageView.slint:593 —
                // see the note on the identical mount in ArtistView.
                barBg: root.ambientOn ? theme.surfaceMainA30 : theme.surfaceMain
                tabs: root.jumpTabs
                activeTabId: root.activeJumpTab
                onTabClicked: function (id) { root.scrollToSection(id) }
            }

            // --- Catalog ---------------------------------------------------
            Column {
                id: catalog
                width: parent.width - 64
                spacing: 0

                // First-paint block.
                Item {
                    visible: QbzHome.labelLoading && root.topTracks.length === 0
                    width: parent.width
                    height: visible ? 280 : 0
                    Column {
                        anchors.centerIn: parent
                        spacing: 18
                        QbzSpinner {
                            size: 36
                            anchors.horizontalCenter: parent.horizontalCenter
                        }
                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: QbzSession.tr("Loading…", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 13
                        }
                    }
                }

                // Multi-select bulk bar — INLINE above the Popular Tracks
                // header (bar in flow, content below).
                QbzMultiSelectBar {
                    visible: root.multiSelect
                    width: parent.width
                    selectedCount: root.selectedCount
                    actions: [
                        { "id": "select-all", "label": QbzSession.tr("Select all", QbzSession.trRev), "icon": "square-check-big", "danger": false, "needsSelection": false },
                        { "id": "play-next", "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "danger": false, "needsSelection": true },
                        { "id": "play-later", "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "danger": false, "needsSelection": true },
                        { "id": "queue", "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "danger": false, "needsSelection": true },
                        { "id": "add-to-playlist", "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "danger": false, "needsSelection": true },
                        { "id": "add-to-mixtape", "label": QbzSession.tr("Add to Mixtape/Collection", QbzSession.trRev), "icon": "cassette-tape", "danger": false, "needsSelection": true },
                        { "id": "add-to-favorites", "label": QbzSession.tr("Add to Library", QbzSession.trRev), "icon": "heart", "danger": false, "needsSelection": true },
                        { "id": "make-offline", "label": QbzSession.tr("Make available offline", QbzSession.trRev), "icon": "cloud-download", "danger": false, "needsSelection": true },
                        { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true }
                    ]
                    onAction: function (id) { root.bulkAction(id) }
                }

                // --- Popular Tracks -------------------------------------
                // The WHOLE block is gated on a non-empty list (.slint:313).
                Column {
                    property string anchorId: "popular"
                    visible: root.topTracks.length > 0
                    width: parent.width
                    spacing: 0

                    Row {
                        width: parent.width
                        spacing: 12
                        Text {
                            width: Math.max(0, parent.width - 3 * 12 - 44 - 32 - 32)
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("Popular Tracks", QbzSession.trRev)
                            color: theme.textPrimary
                            font.pixelSize: theme.fontHeading
                            font.weight: theme.weightSemibold
                            elide: Text.ElideRight
                        }
                        QbzCircleAction {
                            primary: true
                            name: "play-fill"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzHome.labelPlayTop()
                        }
                        // Multi-select toggle — live since the bulk-bar port.
                        QbzCircleAction {
                            name: "square-check-big"
                            active: root.multiSelect
                            btnEnabled: root.topTracks.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: root.setMultiSelect(!root.multiSelect)
                        }
                        QbzCircleAction {
                            id: topMoreBtn
                            name: "ellipsis"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: function (mouse) {
                                topMenu.openAtCursor(topMoreBtn, mouse.x, mouse.y)
                            }
                        }
                    }
                    CardMenu {
                        id: topMenu
                        menuWidth: 224
                        entries: [
                            { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" },
                            { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" },
                            { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" },
                        ]
                        onPicked: function (a) { QbzHome.labelTopAction(a) }
                    }

                    Item { width: 1; height: 10 }

                    Item {
                        id: trackWindow
                        width: parent.width
                        readonly property int total: Math.min(root.previewCount, root.topTracks.length)
                        readonly property int pitch: root.kioskHost ? 64 : 50
                        readonly property real viewportTop: flick.contentY - mapToItem(page, 0, 0).y
                        readonly property int first: root.kioskHost ? Math.min(total, Math.max(0, Math.floor(viewportTop / pitch) - 1)) : 0
                        readonly property int last: root.kioskHost ? Math.min(total, Math.max(first, Math.ceil((viewportTop + flick.height) / pitch) + 1)) : total
                        height: total * pitch
                    Repeater {
                        // Numeric growth preserves the five existing delegates
                        // and creates only the newly revealed rows. The old
                        // full-array model built up to 50 TrackRows at first
                        // paint and merely gave 45 of them height zero.
                        model: trackWindow.last - trackWindow.first
                        delegate: TrackRow {
                            required property int index
                            width: parent ? parent.width : 0
                            height: root.kioskHost ? 64 : 50
                            kioskHost: root.kioskHost
                            y: (trackWindow.first + index) * trackWindow.pitch
                            // SMOOTH REVEAL (owner, 2026-08-02: "que la
                            // aparicion de lo que se cargue, sea smooth").
                            // This site is CLIENT-SIDE. Numeric model growth
                            // adds only the requested rows; each new delegate
                            // flips `revealed` after creation and dissolves in.
                            // There is no appended network tail or threshold.
                            //
                            // 220ms / OutCubic = the content duration this
                            // round standardises on (controls/QbzLoadMore
                            // .qml uses 180ms for its skeleton).
                            // Collapse stays instant because numeric shrink
                            // removes the tail in the same frame. Holding its
                            // layout open for a fade-out would leave a large
                            // blank band and then jump.
                            property bool revealed: false
                            opacity: revealed ? 1.0 : 0.0
                            Behavior on opacity {
                                NumberAnimation { duration: 220; easing.type: Easing.OutCubic }
                            }
                            Component.onCompleted: revealed = true
                            item: root.topTracks[trackWindow.first + index] || ({})
                            number: trackWindow.first + index + 1
                            showArtwork: true
                            showAlbum: true
                            selectMode: root.multiSelect
                            checked: root.selected[item.id] === true
                            onToggleSelect: function (mods) { root.toggleSelected(item.id, mods) }
                            onPlayRequested: QbzHome.labelPlayTrack(item.id)
                            onEnqueueRequested: function (mode) {
                                QbzHome.labelEnqueueTrack(item.id, mode)
                            }
                            // MyQBZ "Add to mixtape" — the HOST builds the
                            // AddItem array (TrackRow does not know
                            // itemType/source).
                            //
                            // SOURCE: a label page is a Qobuz catalog surface.
                            // These rows are the `/label/page` response's
                            // `top_tracks` mapped to track rows
                            // (label_qt.rs:280-287) — there is no local label
                            // library and no non-Qobuz row can reach this
                            // Repeater, so `item.id` is a Qobuz catalog
                            // id by construction of the document.
                            onMixtapeRequested: QbzMyQbzAdd.open(JSON.stringify([{
                                "itemType": "track", "source": "qobuz",
                                "sourceItemId": item.id,
                                "title": item.title || "",
                                "subtitle": item.artist || "",
                                "artworkUrl": item.artUrl || ""  // label_qt serializes the shared
                                // TrackRow, which carries artUrl; only artPath is
                                // patched on top for this page's 36px art cell., "year": null, "trackCount": null
                            }]))
                        }
                    }

                    }
                    // Reveal control: 5 -> 20 -> 50, then back to 5
                    // (LabelPageView.slint:439-466). This was the fifth
                    // hand-rolled copy of the plain Load-more shape; it is
                    // now controls/QbzLoadMore.qml, arm (e) in that file's
                    // inventory — same 28-tall box, same 13px centred label,
                    // same hover textPrimary / textSecondary, same
                    // `implicitWidth + 24` hit box. Idle pixels unchanged.
                    //
                    // CLIENT-SIDE, so no fetch to wait on: `busy` stays
                    // false and `skeleton` stays "none", which is what keeps
                    // a placeholder from appearing under a button whose
                    // rows are revealed in the same frame. The smooth half
                    // of the owner's request lives on the TrackRow delegate
                    // above, not here (see the SMOOTH REVEAL note there).
                    Item { visible: root.topTracks.length > 5; width: 1; height: visible ? 4 : 0 }
                    QbzLoadMore {
                        visible: root.topTracks.length > 5
                        width: parent.width
                        buttonHeight: root.kioskHost ? 64 : 28
                        skeleton: "none"
                        busy: false
                        // The label ternary is verbatim from the copy this
                        // replaces: "Load more" while the reveal can still
                        // grow, "View less" once it cannot (both are
                        // existing msgids — no new strings).
                        label: (root.previewCount < 50
                                && root.topTracks.length > root.previewCount)
                            ? QbzSession.tr("Load more", QbzSession.trRev)
                            : QbzSession.tr("View less", QbzSession.trRev)
                        onClicked: {
                            if (root.previewCount < 50
                                && root.topTracks.length > root.previewCount)
                                root.previewCount = root.previewCount === 5 ? 20 : 50
                            else
                                root.previewCount = 5
                        }
                    }
                }

                // --- Releases ---------------------------------------------
                Column {
                    property string anchorId: "releases"
                    visible: root.releases.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    CardRail {
                        title: QbzSession.tr("Releases", QbzSession.trRev)
                        items: root.releases
                        kind: "album"
                        showViewAll: true
                        onViewAllClicked: QbzHome.labelOpenReleases()
                    }
                }

                // --- Critics' Picks ---------------------------------------
                Column {
                    property string anchorId: "critics"
                    visible: root.critics.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    CardRail {
                        title: QbzSession.tr("Critics' Picks", QbzSession.trRev)
                        items: root.critics
                        kind: "album"
                    }
                }

                // --- Playlists ---------------------------------------------
                Column {
                    property string anchorId: "playlists"
                    visible: root.playlists.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    CardRail {
                        title: QbzSession.tr("Playlists", QbzSession.trRev)
                        items: root.playlists
                        kind: "playlist"
                    }
                }

                // --- Artists ------------------------------------------------
                Column {
                    property string anchorId: "artists"
                    visible: root.artists.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    CardRail {
                        title: QbzSession.tr("Artists", QbzSession.trRev)
                        items: root.artists
                        kind: "artist"
                    }
                }

                // --- More Labels --------------------------------------------
                // NOTE the msgid: the carousel is "More labels" (lowercase l)
                // and the JUMP TO tab is "More Labels" — two distinct msgids
                // in the .slint, kept distinct here.
                Column {
                    property string anchorId: "labels"
                    visible: root.moreLabels.length > 0
                    width: parent.width
                    spacing: 0
                    Item { width: 1; height: 32 }
                    CardRail {
                        title: QbzSession.tr("More labels", QbzSession.trRev)
                        items: root.moreLabels
                        kind: "label"
                    }
                }
            }
        }
    }

    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: flick; scope: "label" }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: flick
    }
}
