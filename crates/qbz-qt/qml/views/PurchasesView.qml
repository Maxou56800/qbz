// PurchasesView — the My-Purchases library surface, route id "purchases".
// Port of purchases/PurchasesView.slint (:1300-1470 is the page itself), which
// is in turn the 1:1 port of `PurchasesView.svelte`.
//
// Region by region (contract §15.1):
//   1. header       94x94 accent-gradient tile + the `shopping-bag` glyph at 32
//   2. region notice  dismissible, per user, and its wording is COMMERCIAL
//   3. sticky nav   QbzTabBar (Albums / Tracks, with counts) + expandable search
//   4. toolbar      NOT the same on both tabs — see views/purchases/PurchasesToolbar
//   5. body         albums grid/list, or the track rows
//   6. states       loading · error + retry · empty
//
// ── WHAT IS DELIBERATELY NOT HERE ────────────────────────────────────────
// No pagination, no infinite scroll, no track context menu, no double-click,
// no multi-select, no drag, no Escape handling: none of it exists in the
// reference (§1). Two CSS classes in the Tauri original (`.playing` on list
// rows, `.track-row.failed`) have NO rule in the only stylesheet — they are
// dead, and porting them as visible states would add behaviour that never
// existed. There is also NO offline placeholder: §15.1-6 is explicit that
// Tauri has none and that none was ruled in.
//
// ── STATE LIFETIME (§10-F) ───────────────────────────────────────────────
// Tab, sort, direction, grouping, grid/list and the search text all die when
// the screen is left. `QbzPurchases.leave()` fires from this component's
// destruction, which is the moment the route arm unloads it. The four
// PERSISTED preferences (hide-unavailable, hide-downloaded, quality filter,
// region-notice-dismissed) are the only survivors, and they live in Rust.
//
// ── THE SEARCH DEBOUNCE IS OURS ──────────────────────────────────────────
// §G.1: QML owns the 300 ms debounce and `search()` is the fire. It is a
// ONE-SHOT Timer, which the repaint-pulse rule allows — a repeating one would
// not be. The call REFETCHES BOTH types from the server (§5); it is not a
// local filter over the active tab.
//
// ── WHY THE BODY IS NOT AlbumCollection ──────────────────────────────────
// See the header of views/purchases/PurchaseGridCard.qml. In one line: the
// shared card/row pair hardwires its click to `QbzAlbum.openAlbum`, which would
// make the purchase-album route unreachable, and neither has an arm for the
// UNAVAILABLE state this screen is built around. The .slint reached the same
// conclusion for the same reasons and drew its own cells too.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"
import "purchases"

Rectangle {
    id: root
    property bool kioskHost: false

    // Round to the AppShell content-frame bezel (the literal in all painting
    // view roots — QML clips are rectangular, so the frame's rounding never
    // reaches the view and the view's own fill must round).
    radius: 12
    color: root.ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // ── the document ──────────────────────────────────────────────────────
    // GUARDED: a raw JSON.parse in a binding throws on the pre-publish frame.
    // `purchasesRev` is a pure binding dependency — it is bumped on EVERY
    // publish so this re-parses even when the JSON text is byte-identical.
    function _parse(json, rev) {
        try {
            return JSON.parse(json)
        } catch (e) {
            return ({})
        }
    }
    readonly property var doc: root._parse(QbzPurchases.listJson,
                                           QbzPurchases.purchasesRev)

    readonly property string tab: root.doc.tab || "albums"
    readonly property var toolbarDoc: root.doc.toolbar || ({})
    readonly property string group: root.toolbarDoc.group || "off"
    readonly property string viewMode: root.toolbarDoc.viewMode || "grid"
    readonly property var albums: root.doc.albums || []
    readonly property var tracks: root.doc.tracks || []
    readonly property var counts: root.doc.counts || ({})
    readonly property string search: root.doc.search || ""
    readonly property bool loading: root.doc.loading === true
    readonly property string loadError: root.doc.error || ""

    // NOT `onAlbums`: with a sibling property called `albums` on this same
    // object, QML files `onAlbums: …` as a signal HANDLER — no error, no
    // warning, the initializer never binds and the property sits at `false`
    // forever. That single name blanked the Albums tab for every account with
    // real purchases (2026-09-01; reproduced standalone with the `qml`
    // runtime, the identical expression under another name gives `true`).
    // scripts/qml-audits/qml_on_prefixed_property_audit.py now fails the
    // build on any `on<Upper>` property name.
    readonly property bool albumsTab: root.tab !== "tracks"
    readonly property var rows: root.albumsTab ? root.albums : root.tracks
    readonly property bool grouped: root.group !== "off" && root.group !== ""

    // ── entry / exit ──────────────────────────────────────────────────────
    // `openList()` carries the `metadataLoaded` guard on the Rust side (§5), so
    // a re-entry — or a second caller — costs nothing.
    Component.onCompleted: QbzPurchases.openList()
    Component.onDestruction: QbzPurchases.leave()

    // ── search: 300 ms, one shot ──────────────────────────────────────────
    property string pendingQuery: ""
    Timer {
        id: searchDebounce
        interval: 300
        repeat: false
        onTriggered: QbzPurchases.search(root.pendingQuery)
    }

    // ── grouping ──────────────────────────────────────────────────────────
    // `list_json` publishes FLAT arrays only — §G.2 has no grouped section, and
    // `purchases_qt.rs::build_list_doc` says so in as many words ("GROUPING IS
    // NOT DONE HERE … the QML host owns the bucketing"). The rules below are
    // the ones that comment spells out, so the two halves cannot drift:
    //
    //   album key   artist name, or alphaGroupKey(title)
    //   track key   alphaGroupKey(title) / performer / album title, else "Unknown"
    //   order       by key, ascending
    //
    // It is presentation-only: search, sort and filtering all stay in Rust, and
    // the rows inside a bucket keep the order Rust handed them, so the toolbar's
    // sort still decides what comes first WITHIN a section.
    function _alphaKey(s) {
        var v = (s || "").trim()
        if (v === "")
            return "#"
        var c = v.charAt(0).toUpperCase()
        // The reference's exact test: anything that is not A-Z — a digit, a
        // bracket, Cyrillic, kana — shares the "#" bucket. Widening it here
        // would silently disagree with the Rust-side rule this mirrors.
        return (c >= "A" && c <= "Z") ? c : "#"
    }
    function _bucket(list, keyOf) {
        var byKey = {}
        var keys = []
        for (var i = 0; i < list.length; i++) {
            var k = keyOf(list[i])
            if (byKey[k] === undefined) {
                byKey[k] = []
                keys.push(k)
            }
            byKey[k].push(list[i])
        }
        keys.sort(function (a, b) { return a.localeCompare(b) })
        var out = []
        for (var j = 0; j < keys.length; j++)
            out.push({ "title": keys[j], "items": byKey[keys[j]] })
        return out
    }
    /// Albums: "alpha" (by album title) | "artist".
    function albumGroups() {
        if (!root.grouped)
            return []
        if (root.group === "artist")
            return root._bucket(root.albums, function (a) {
                return (a.artist || "").trim() || root.t("Unknown")
            })
        return root._bucket(root.albums, function (a) { return root._alphaKey(a.title) })
    }
    /// Tracks: "album" | "artist" | "name" (by track title initial).
    function trackGroups() {
        if (!root.grouped)
            return []
        if (root.group === "album")
            return root._bucket(root.tracks, function (tr) {
                return (tr.album || "").trim() || root.t("Unknown")
            })
        if (root.group === "artist")
            return root._bucket(root.tracks, function (tr) {
                return (tr.artist || "").trim() || root.t("Unknown")
            })
        return root._bucket(root.tracks, function (tr) { return root._alphaKey(tr.title) })
    }

    function openAlbum(id) {
        if (!id || id === "")
            return
        // §G.1, in THIS order: the controller records the detail first, then
        // the route changes.
        QbzPurchases.openAlbum(id)
        QbzShell.navigateTo("purchase-album")
    }

    // ══════════════════════════════════════════════════════════════════════
    Flickable {
        id: flick
        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: page.implicitHeight
        boundsBehavior: Flickable.StopAtBounds

        Column {
            id: page
            width: parent.width
            leftPadding: 24
            rightPadding: 24
            topPadding: 24
            // The port's scroll runway: the last row must clear the player bar.
            bottomPadding: 100
            spacing: 16

            readonly property int contentW: Math.max(0, page.width - 48)

            // ── 1. Header ─────────────────────────────────────────────────
            //
            // OWNER RULING, 2026-08-16: plain title, no icon tile.
            //
            // §15.1-1 specified the reference's 94x94 accent-gradient tile with
            // a 32px shopping-bag in it, and a title at `fontTitle` (25). That
            // was a faithful port of Tauri's own header — and faithful is what
            // made it wrong here, because NOTHING else in this application looks
            // like that. Library opens straight into its toolbar with no page
            // title at all; My QBZ and the label header title themselves at
            // `fontSection` (18) / bold. Purchases stood out as the one page
            // shouting its own name over a large coloured square.
            //
            // So this diverges from the contract deliberately: app-internal
            // consistency beats fidelity to a header the reference happened to
            // draw that way. The `shopping-bag` glyph still identifies the
            // destination in the sidebar and the title bar, which is where a
            // section icon belongs in this shell.
            Text {
                width: page.contentW
                text: root.t("My Purchases")
                color: theme.textPrimary
                font.pixelSize: theme.fontSection
                font.weight: theme.weightBold
                elide: Text.ElideRight
            }

            // ── 2. Region notice ──────────────────────────────────────────
            // The copy must NOT be reworded to blame the network: §2.5b proved
            // the endpoint answers 200 in a blocked region, so the limitation
            // this strip describes is COMMERCIAL.
            Rectangle {
                id: regionNotice
                visible: root.doc.regionNoticeVisible === true
                width: page.contentW
                height: visible ? noticeRow.implicitHeight + 24 : 0
                radius: 8
                // The WarningBanner "info" recipe: the accent at 10 % over a
                // 30 % border, derived from the live token rather than frozen
                // into a literal.
                color: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.10)
                border.width: 1
                border.color: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.30)

                Row {
                    id: noticeRow
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 12
                    spacing: 10

                    QbzIcon {
                        name: "info"
                        width: 18
                        height: 18
                        tintName: "accent"
                    }
                    Text {
                        width: Math.max(0, parent.width - 18 - 24 - 20)
                        text: root.t("Purchases are downloaded as DRM-free files. Availability depends on your Qobuz region.")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontBody
                        wrapMode: Text.WordWrap
                    }
                    Rectangle {
                        width: 24
                        height: 24
                        radius: 4
                        color: dismissArea.containsMouse ? theme.surfaceElevated : "transparent"
                        QbzIcon {
                            name: "x"
                            width: 14
                            height: 14
                            anchors.centerIn: parent
                            tintName: "muted"
                        }
                        MouseArea {
                            id: dismissArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzPurchases.dismissRegionNotice()
                        }
                    }
                }
            }

            // ── 3. Sticky nav bar ─────────────────────────────────────────
            // STICKY, and done with a `transform` rather than a `y` write: the
            // Column owns this item's `y`, so pinning it by assignment would
            // fight the positioner. A Translate leaves the layout alone and
            // costs one binding on `contentY` — ONE item, not one per delegate,
            // and the window is already being repainted while a scroll is in
            // flight, so it adds no present of its own.
            Item {
                id: navRow
                width: page.contentW
                height: root.kioskHost ? 64 : 34
                // Above the toolbar and the body, which scroll UNDER it.
                z: 5

                readonly property bool stuck: flick.contentY > navRow.y

                transform: Translate {
                    y: Math.max(0, flick.contentY - navRow.y)
                }

                // The backdrop, and only once pinned: at rest it would paint an
                // opaque band over the page for nothing. It bleeds into the
                // page's 24px gutters and over the 16px Column gap above so
                // nothing peeks around it.
                Rectangle {
                    x: -24
                    y: -16
                    width: parent.width + 48
                    height: parent.height + 24
                    color: navRow.stuck
                        ? (theme.ambientOn ? theme.surfaceMainA50 : theme.surfaceMain)
                        : "transparent"
                }

                QbzTabBar {
                    visible: !root.kioskHost
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    counts: true
                    underline: true
                    activeId: root.tab
                    tabs: [
                        { "id": "albums", "label": root.t("Albums"),
                          "count": root.counts.albums || 0 },
                        { "id": "tracks", "label": root.t("Tracks"),
                          "count": root.counts.tracks || 0 },
                    ]
                    onSelected: function (id) { QbzPurchases.setTab(id) }
                }

                Row {
                    visible: root.kioskHost
                    height: 64
                    spacing: 8
                    Repeater {
                        model: root.kioskHost ? [{id: "albums", label: root.t("Albums")}, {id: "tracks", label: root.t("Tracks")}] : []
                        delegate: Rectangle {
                            required property var modelData
                            width: Math.max(96, kioskTabLabel.implicitWidth + 24)
                            height: 64
                            color: root.tab === modelData.id ? (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated) : "transparent"
                            Text { id: kioskTabLabel; anchors.centerIn: parent; text: parent.modelData.label; color: theme.textPrimary; font.pixelSize: 18 }
                            MouseArea { anchors.fill: parent; onClicked: QbzPurchases.setTab(parent.modelData.id) }
                        }
                    }
                }

                QbzLineEdit {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    searchMode: true
                    kioskHost: root.kioskHost
                    expandable: !root.kioskHost
                    width: root.kioskHost ? Math.min(280, navRow.width * 0.42) : collapsedSize
                    openWidth: 220
                    // Bound, never assigned: the control's own clear latch
                    // depends on the host re-seeding through this binding.
                    text: root.search
                    placeholder: root.t("Search purchases…")
                    onEdited: function (v) {
                        root.pendingQuery = v
                        searchDebounce.restart()
                    }
                }
            }

            // ── 4. Toolbar ────────────────────────────────────────────────
            PurchasesToolbar {
                kioskHost: root.kioskHost
                width: page.contentW
                tab: root.tab
                toolbar: root.toolbarDoc
            }

            // ── 5/6. Body and its states ──────────────────────────────────

            // Loading — one shared composite skeleton shaped like the active
            // collection. The bridge marks loading before its first publish,
            // so the account-empty state can never flash ahead of this block.
            Item {
                visible: root.loading
                width: page.contentW
                height: visible ? Math.max(260, Math.min(620, flick.height - 180)) : 0
                QbzSkeleton {
                    anchors.fill: parent
                    visible: parent.visible
                    selfDrive: true
                    variant: root.albumsTab && root.viewMode !== "list"
                        ? "cardGrid" : "rowList"
                    cellW: root.albumsTab ? 178 : parent.width
                    cellH: root.albumsTab ? 248 : 58
                    cardW: 162
                    cardH: 232
                    rowH: root.albumsTab ? 60 : 56
                    rowGap: root.albumsTab ? 4 : 2
                    rowArtSize: root.albumsTab ? 48 : 40
                }
            }

            // Search keeps the settled rows in place while its server-side
            // refetch runs; a compact progress indicator avoids replacing the
            // whole collection with a second first-load skeleton.
            Item {
                visible: !root.loading && root.doc.searching === true
                width: page.contentW
                height: visible ? 52 : 0
                QbzSpinner {
                    size: 28
                    anchors.centerIn: parent
                }
            }

            // Error + retry (the LibraryView.qml:920-959 block).
            Column {
                visible: !root.loading && root.loadError !== ""
                width: page.contentW
                spacing: 10
                Item { width: 1; height: 48 }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: root.t("Couldn't load purchases. Check your connection.")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                    horizontalAlignment: Text.AlignHCenter
                }
                Rectangle {
                    width: retryText.implicitWidth + 28
                    height: 32
                    radius: 6
                    anchors.horizontalCenter: parent.horizontalCenter
                    color: retryArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                    border.width: 1
                    border.color: theme.borderSubtle
                    Text {
                        id: retryText
                        anchors.centerIn: parent
                        text: root.t("Retry")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontLegal
                    }
                    MouseArea {
                        id: retryArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        // §G.1 has no dedicated reload; `openList()` is the one
                        // entry point that re-runs the active tab's load.
                        onClicked: QbzPurchases.openList()
                    }
                }
            }

            // Empty. Two different sentences on purpose: with a query in the
            // box "No purchases yet" would tell the user they own nothing,
            // which is a lie about their account rather than about the filter.
            // Neither line accuses the network (§15.1-6).
            Column {
                visible: !root.loading && root.doc.searching !== true
                    && root.loadError === "" && root.rows.length === 0
                width: page.contentW
                spacing: 0
                Item { width: 1; height: 60 }
                QbzEmptyState {
                    anchors.horizontalCenter: parent.horizontalCenter
                    iconName: root.search !== ""
                        ? "search"
                        : (root.albumsTab ? "shopping-bag" : "music")
                    iconSize: 48
                    titleMuted: true
                    titleWeight: theme.weightRegular
                    title: root.search !== ""
                        ? root.t("No results")
                        : (root.albumsTab ? root.t("No purchases yet")
                                         : root.t("No purchased tracks yet"))
                }
            }

            // --- ALBUMS, flat -------------------------------------------------
            PurchaseAlbumsCollection {
                visible: !root.loading && root.loadError === "" && root.albumsTab
                    && !root.grouped && root.albums.length > 0
                width: page.contentW
                albums: (root.albumsTab && !root.grouped) ? root.albums : []
                viewMode: root.viewMode
                flick: flick
                onOpenAlbum: function (id) { root.openAlbum(id) }
            }

            // --- ALBUMS, grouped ----------------------------------------------
            Column {
                visible: !root.loading && root.loadError === "" && root.albumsTab
                    && root.grouped && root.albums.length > 0
                width: page.contentW
                spacing: 16
                Repeater {
                    model: (root.albumsTab && root.grouped) ? root.albumGroups() : []
                    delegate: Column {
                        required property var modelData
                        width: page.contentW
                        spacing: 12
                        Text {
                            width: parent.width
                            text: modelData.title
                            color: theme.textPrimary
                            font.pixelSize: theme.fontHeading
                            font.weight: theme.weightSemibold
                            elide: Text.ElideRight
                        }
                        PurchaseAlbumsCollection {
                            width: parent.width
                            albums: modelData.items
                            viewMode: root.viewMode
                            flick: flick
                            onOpenAlbum: function (id) { root.openAlbum(id) }
                        }
                    }
                }
            }

            // --- TRACKS, flat -------------------------------------------------
            PurchaseTracksCollection {
                visible: !root.loading && root.loadError === "" && !root.albumsTab
                    && !root.grouped && root.tracks.length > 0
                width: page.contentW
                tracks: (!root.albumsTab && !root.grouped) ? root.tracks : []
                flick: flick
                onPlayRequested: function (id) { QbzPlayer.playTrack(id) }
            }

            // --- TRACKS, grouped ----------------------------------------------
            Column {
                visible: !root.loading && root.loadError === "" && !root.albumsTab
                    && root.grouped && root.tracks.length > 0
                width: page.contentW
                spacing: 16
                Repeater {
                    model: (!root.albumsTab && root.grouped) ? root.trackGroups() : []
                    delegate: Column {
                        required property var modelData
                        width: page.contentW
                        spacing: 8
                        Text {
                            width: parent.width
                            text: modelData.title
                            color: theme.textPrimary
                            font.pixelSize: theme.fontHeading
                            font.weight: theme.weightSemibold
                            elide: Text.ElideRight
                        }
                        PurchaseTracksCollection {
                            width: parent.width
                            tracks: modelData.items
                            flick: flick
                            onPlayRequested: function (id) { QbzPlayer.playTrack(id) }
                        }
                    }
                }
            }
        }
    }

    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: flick; scope: "purchases" }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: flick
    }
}
