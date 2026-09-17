// Blacklist Manager — QML port of crates/qbz-ui/ui/settings/
// BlacklistManagerView.slint (1114 lines). A FULL-PAGE content view, not a
// settings subview: the Slint mounts it in AppShell (shell/AppShell.slint:604)
// and it is reached only from the Settings > Blacklist "Manage" button, exactly
// as here (route id "blacklist").
//
// Three tabs (Artists u64 ids · Albums String ids · Recommendations, the
// reco-scoped "Not interested" dismissals), each with the SAME four
// mutually-exclusive body branches in a load-bearing order:
//   1 loading · 2 FULL count == 0 (empty) · 3 FULL count > 0 but the filtered
//   list is empty (no results) · 4 the list.
// Branch 2 vs 3 is the whole reason the document's `count` / `albumCount` /
// `dismissedCount` are the FULL list lengths and never the filtered ones
// (blacklist_manager.rs:72, 104, 148).
//
// Everything the view shows comes out of ONE document, QbzBlacklist
// .blacklistJson — filtering, ordering, the date strings and the has-notes
// flag are all Rust-side (src/blacklist_qt.rs), and QML never filters. The tab
// index and the search query live in Rust too (TAB / QUERY), so leaving and
// re-entering the manager keeps both, like Slint's BlacklistState.
//
// Deltas vs the Slint, each one deliberate and stated:
// - NO per-view Back chrome (the Slint's NavButtons row, :351-354). Nav history
//   is the global HeaderBar in this port — the same treatment
//   settings/SettingsView.qml:24-25 already documents for the identical case.
//   ADR-004 is satisfied by the HeaderBar.
// - The old per-view `UiFocusState.text-input-focused` write is unnecessary:
//   AppShell/KioskShell compute the active TextInput and pass that gate through
//   the app-wide QbzHotkeys dispatcher before any binding is considered.
// - The list is a recycling ListView, not a Flickable over every row
//   (TRACK-RULES §6 — a list that can be large is born windowed), so the
//   Slint's `list-hover-rows` scrollbar reveal-on-row-hover is dropped:
//   QbzScrollBar reveals on scroll + gutter hover/press instead, and a
//   per-row hover counter across a recycling delegate is cost for a cosmetic
//   cue.
// - Mutation feedback uses the app-wide QbzToast host through `toast_qt`;
//   the settled `blacklistChanged` republish also corrects optimistic glyphs.
//
// Nav re-entry: nav_qt::back()/forward() republish `currentView` and run NO
// per-view load, so this view calls QbzBlacklist.reload() in
// Component.onCompleted to match Slint's reload-on-apply-entry
// (main.rs:3299-3305).

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"

Rectangle {
    id: root

    // KIOSK HOST (2026-09-07, K6). Default FALSE = the desktop page, byte for
    // byte: the chrome is a Column at x24/y24 and the body starts 14px under
    // it, exactly the arithmetic this file has always had.
    //
    // The kiosk panel is 784x256 at the Pi floor. The desktop arithmetic
    // leaves the body 0-39px against a 68px row pitch, so the manager can
    // never draw ONE row (K0b finding #6). Two scroll axes are not an option
    // (a ListView nested in a Flickable steals every vertical drag), so the
    // kiosk arm folds the whole chrome into `rowList`'s ListView.header and
    // its empty/loading/no-results states into its footer: ONE flickable, the
    // header scrolls away, and the rows own the full panel. The chrome and
    // the state block are declared ONCE, as Components, and mounted either in
    // the desktop position or inside the list.
    property bool kioskHost: false

    // Transparent while the ambient background is active — the frosted content
    // panel shows through (HomeView.qml:53 and its twelve siblings).
    color: root.ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn

    // Round to the AppShell content-frame bezel (Radius.md): QML clips are
    // rectangular, so the frame's rounding never reaches the view — the view's
    // own fill must round instead. A literal in all 14 painting view roots.
    radius: 12

    QbzTheme { id: theme }

    // ============================== data ==================================

    readonly property var doc: {
        try {
            return JSON.parse(QbzBlacklist.blacklistJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property int activeTab: root.doc.activeTab || 0
    // `!== false`, NOT `=== true`: BlacklistState.enabled defaults to true
    // (state.slint:2489), so reading a `{}` first-frame document the other way
    // flashes the amber "filtering is disabled" banner on every mount.
    readonly property bool filterEnabled: root.doc.enabled !== false
    readonly property string searchQuery: root.doc.searchQuery || ""

    readonly property int artistCount: root.doc.count || 0
    readonly property int albumCount: root.doc.albumCount || 0
    readonly property int dismissedCount: root.doc.dismissedCount || 0
    /// The FULL count for the active tab — what separates "empty" from
    /// "no results", and what the count badge and Clear-All gate read.
    readonly property int fullCount: root.activeTab === 0 ? root.artistCount
        : root.activeTab === 1 ? root.albumCount : root.dismissedCount

    /// The already-filtered rows for the active tab.
    readonly property var listRows: root.activeTab === 0 ? (root.doc.artists || [])
        : root.activeTab === 1 ? (root.doc.albums || []) : (root.doc.dismissed || [])

    /// The four body branches, evaluated in the reference's order.
    readonly property int bodyBranch: QbzBlacklist.blacklistLoading ? 1
        : root.fullCount === 0 ? 2
        : root.listRows.length === 0 ? 3 : 4

    Component.onCompleted: QbzBlacklist.reload()

    function tabTabs(rev) {
        return [
            { "id": "artists", "label": QbzSession.tr("Artists", rev) },
            { "id": "albums", "label": QbzSession.tr("Albums", rev) },
            { "id": "reco", "label": QbzSession.tr("Recommendations", rev) }
        ]
    }

    // ------------------------- album cover window -------------------------
    // Covers are NEVER in the document: albums[].artPath is the already-cached
    // path ("" when unresolved) and the misses are fetched for the VISIBLE
    // window only. Arrivals land on the existing QbzLibrary.libraryArtworkReady
    // signal keyed by the cover URL and are coalesced into ONE rebind per frame
    // — rebinding per arrival is quadratic in the page (the documented
    // views/ArtistView.qml:265-288 fix).
    property var coverMap: ({})
    property var _coverInbox: ({})

    Timer {
        id: coverFlush
        interval: 16
        repeat: false
        onTriggered: {
            var m = Object.assign({}, root.coverMap, root._coverInbox)
            root._coverInbox = ({})
            // A rebind needs a NEW object reference (same-ref assignment is not
            // a change in QML).
            root.coverMap = m
        }
    }
    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            root._coverInbox[key] = path
            if (!coverFlush.running) coverFlush.start()
        }
    }

    Timer {
        id: coverWindow
        interval: 80
        repeat: false
        onTriggered: root.reportCoverWindow()
    }
    /// Row pitch is 64 + 4 spacing = 68 on desktop, 76 + 4 = 80 in kiosk
    /// (BlacklistRow.qml's own frame height). Computed arithmetically rather
    /// than with indexAt(), which is what the port's other windowed lists do
    /// (LibraryView.qml:1474) — the rows are a fixed height, so the two agree.
    readonly property int rowPitch: root.kioskHost ? 80 : 68
    function reportCoverWindow() {
        if (root.activeTab !== 1)
            return
        var m = root.listRows
        if (m.length === 0)
            return
        // In kiosk the chrome rides in ListView.header, so contentY starts
        // NEGATIVE of the header height; clamping at 0 keeps the first window
        // anchored on row 0 instead of asking for a negative index.
        var top = Math.max(0, rowList.contentY)
        var first = Math.max(0, Math.floor(top / root.rowPitch) - 2)
        var last = Math.min(m.length - 1,
                            Math.ceil((top + rowList.height) / root.rowPitch) + 2)
        var urls = []
        for (var i = first; i <= last; i++) {
            var u = m[i].coverUrl || ""
            if (u !== "" && (m[i].artPath || "") === "" && !root.coverMap[u])
                urls.push(u)
        }
        if (urls.length === 0)
            return
        QbzBlacklist.artworkWindow(JSON.stringify(urls))
    }

    // ============================ chrome ==================================
    // The Slint outer VerticalLayout is padding 24 / spacing 14 with the body
    // at vertical-stretch: 1. QML has no layout here (this port uses none), so
    // the fixed head is a Column and the body fills what is left.

    // The desktop chrome host. Same x/y/width the Column had; a Loader with
    // only its width set takes the item's implicitHeight, so `head.height` is
    // still the Column's content height and the body arithmetic below is
    // unchanged. `sourceComponent: null` in kiosk — the chrome is the list's
    // header there and must exist exactly once.
    Loader {
        id: head
        x: 24
        y: 24
        width: root.width - 48
        sourceComponent: root.kioskHost ? null : chromeComponent
    }

    Component {
    id: chromeComponent
    Column {
        id: chromeCol
        // Desktop: the Loader sizes this to `head.width`. Kiosk: the item is
        // ListView.header, which does NOT stretch its header, so it takes the
        // view width itself.
        width: chromeCol.ListView.view ? chromeCol.ListView.view.width : head.width
        spacing: 14
        // Desktop = 0 on both, so the Column's implicitHeight — and therefore
        // `head.height` — is exactly what it was.
        topPadding: root.kioskHost ? 4 : 0
        bottomPadding: root.kioskHost ? 12 : 0

        // ---- header (:357-378) -------------------------------------------
        Row {
            spacing: 12
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: "blind-eye"
                width: 24
                height: 24
                tintName: "muted"
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("Blacklist", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: theme.fontSection
                font.weight: theme.weightBold
            }
        }

        // ---- tab bar (:381-464) — ADR-008, no pills ----------------------
        // The shared control. Its signal carries the tab ID STRING, not an
        // index (QbzTabBar.qml:24, emitted :97), so the handler maps it; a
        // handler written for an index passes `undefined` to an i32 invokable
        // and every click is a runtime error `cargo check` cannot see.
        QbzTabBar {
            kioskHost: root.kioskHost
            tabs: root.tabTabs(QbzSession.trRev)
            activeId: root.activeTab === 1 ? "albums"
                : root.activeTab === 2 ? "reco" : "artists"
            onSelected: function (id) {
                QbzBlacklist.setTab(id === "artists" ? 0 : id === "albums" ? 1 : 2)
            }
        }

        // ---- description, per tab (:467-477) -----------------------------
        Text {
            width: parent.width
            text: root.activeTab === 0
                ? QbzSession.tr("Blacklisted artists are hidden from search results, radio, suggestions, and similar artists.", QbzSession.trRev)
                : root.activeTab === 1
                    ? QbzSession.tr("Blocked albums are hidden from search, discovery, and listings — even when their artist is allowed.", QbzSession.trRev)
                    : QbzSession.tr("Dismissed artists only leave your Recommendations — they still appear in search and everywhere else.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
            wrapMode: Text.WordWrap
        }

        // ---- controls row (:480-646) — spacing 12 ------------------------
        Item {
            width: parent.width
            height: root.kioskHost ? 44 : 34

            Row {
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
                spacing: 12

                // (a) Enable/Disable toggle (:485-522). The ONLY enable toggle
                // in the feature — the Settings row deliberately has none
                // (ContentFilteringSettings.slint:13-16).
                Rectangle {
                    id: toggleChip
                    width: toggleRow.implicitWidth + 28
                    height: root.kioskHost ? 44 : 34
                    radius: theme.radiusSm
                    color: toggleArea.containsMouse ? theme.surfaceElevatedHover
                                                    : theme.surfaceElevated
                    Row {
                        id: toggleRow
                        x: 14
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 8
                        QbzIcon {
                            anchors.verticalCenter: parent.verticalCenter
                            name: root.filterEnabled ? "eye" : "eye-off"
                            width: 18
                            height: 18
                            tintName: root.filterEnabled ? "accent" : "muted"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.filterEnabled
                                ? QbzSession.tr("Enabled", QbzSession.trRev)
                                : QbzSession.tr("Disabled", QbzSession.trRev)
                            color: root.filterEnabled ? theme.accent : theme.textMuted
                            font.pixelSize: root.kioskHost
                                ? theme.fontLegal * 1.2 : theme.fontLegal
                        }
                    }
                    MouseArea {
                        id: toggleArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzBlacklist.toggleEnabled()
                    }
                }

                // (b) Search (:525-587) — search-as-you-type, NO debounce:
                // every edit re-pushes the Rust-filtered rows. The shared
                // control's search arm; the width override replaces its
                // 240 default.
                //
                // A PLAIN BINDING on `text`, which is also what re-seeds the box
                // on re-entry (the Rust QUERY survives navigation, so the
                // last-published document already carries it and the binding
                // shows it before `reload()` lands). This used to be an
                // imperative `syncSearchField()` because `clearSearch()` wrote
                // `root.text = ""` and destroyed exactly this binding on the
                // first click of the ×; the control no longer touches `text` at
                // all (QbzLineEdit.qml:66-92), so the workaround is gone. Its
                // focus-loss re-seed Binding is what keeps the field from
                // fighting the per-keystroke republish.
                QbzLineEdit {
                    id: searchField
                    anchors.verticalCenter: parent.verticalCenter
                    kioskHost: root.kioskHost
                    searchMode: true
                    // Desktop keeps the 280 override verbatim. In kiosk the
                    // row shares a 760px panel with a taller toggle, the
                    // Clear-All chip and the right-anchored count, so the
                    // field takes the measured slack instead of a constant —
                    // that is what keeps the count badge from overlapping it
                    // at the 800x480 floor.
                    width: root.kioskHost
                        ? Math.max(140, chromeCol.width - toggleChip.width
                            - (clearChip.visible ? clearChip.width + 12 : 0)
                            - countBadge.width - 36)
                        : 280
                    placeholder: ""
                    text: root.searchQuery
                    onEdited: function (v) { QbzBlacklist.searchChanged(v) }
                }

                // (c) Clear All (:591-627) — only when the ACTIVE tab's FULL
                // count > 0, and never on tab 2 (the Recommendations tab has no
                // bulk clear; undo is per row). It opens the confirm, it does
                // NOT mutate.
                //
                // controls/SettingsButton.qml was considered and rejected: its
                // `width: Math.max(160, …)` floor against the Slint's
                // content + 28 (~105px for "Clear All" + a 16px glyph) would
                // put a 160px pill next to a 34px toggle, and it always draws a
                // 1px border and hovers to surfaceHover where this button is
                // borderless and hovers to dangerBg.
                Rectangle {
                    id: clearChip
                    visible: root.activeTab === 0 ? root.artistCount > 0
                        : root.activeTab === 1 ? root.albumCount > 0 : false
                    width: clearRow.implicitWidth + 28
                    height: root.kioskHost ? 44 : 34
                    radius: theme.radiusSm
                    color: clearArea.containsMouse ? theme.dangerBg
                                                   : theme.surfaceElevated
                    Row {
                        id: clearRow
                        x: 14
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 8
                        QbzIcon {
                            anchors.verticalCenter: parent.verticalCenter
                            name: "trash-2"
                            width: 16
                            height: 16
                            // NOT "danger" — no such tint exists; `favorite`
                            // (#ef4444) is the port-wide substitution and
                            // favorite/trash-2.svg is on the qrc floor.
                            tintName: "favorite"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("Clear All", QbzSession.trRev)
                            color: theme.danger
                            font.pixelSize: root.kioskHost
                                ? theme.fontLegal * 1.2 : theme.fontLegal
                        }
                    }
                    MouseArea {
                        id: clearArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: clearConfirm.open()
                    }
                }
            }

            // (d) count badge (:629-645) — the FULL count, right-aligned. Tab 2
            // says "artists" too.
            Text {
                id: countBadge
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: root.activeTab === 1
                    ? QbzSession.tr("{} albums", QbzSession.trRev).replace("{}", root.albumCount)
                    : QbzSession.tr("{} artists", QbzSession.trRev)
                        .replace("{}", root.activeTab === 0 ? root.artistCount
                                                            : root.dismissedCount)
                color: theme.textMuted
                font.pixelSize: root.kioskHost
                    ? theme.fontLegal * 1.2 : theme.fontLegal
                horizontalAlignment: Text.AlignRight
            }
        }

        // ---- disabled warning banner (:649-678) --------------------------
        // The shared control, variant "warning" (its default). The copy goes in
        // `title` because that is the TONE-coloured line (WarningBanner.qml:58);
        // `body` is textSecondary, which is wrong for this banner. Accepted
        // control deltas: radius 8 vs Radius.md 12, glyph `info` vs
        // `circle-alert` (which exists in NO tint dir and is not in MASTERS —
        // the substitution WarningBanner.qml:5-8 already documents), semibold
        // vs regular, and 10%/30% tone alphas vs warning-bg/warning-border's
        // ~15%/35%.
        WarningBanner {
            visible: !root.filterEnabled
            variant: "warning"
            title: QbzSession.tr("Blacklist filtering is disabled. Blacklisted artists are shown everywhere until you re-enable it.", QbzSession.trRev)
        }
    }
    }

    // The three non-list body branches, declared ONCE. Desktop mounts this
    // filling `bodyHost` (so every `anchors.centerIn: parent` centres on the
    // body exactly as before); kiosk mounts it as `rowList`'s footer, where it
    // takes the view width and a fixed band so it lands under the chrome
    // instead of being centred behind it.
    Component {
        id: statesComponent
        Item {
            id: statesItem
            width: statesItem.ListView.view
                ? statesItem.ListView.view.width : (parent ? parent.width : 0)
            height: statesItem.ListView.view
                ? (root.bodyBranch === 4 ? 0 : 200)
                : (parent ? parent.height : 0)

            // 1) Loading. The Slint stacks the spinner and the label with no
            // spacing (two centred HorizontalLayouts in a VerticalLayout).
            Column {
                visible: root.bodyBranch === 1
                anchors.centerIn: parent
                spacing: 0
                QbzSpinner {
                    anchors.horizontalCenter: parent.horizontalCenter
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    // U+2026, not three dots — an exact-byte msgid.
                    text: QbzSession.tr("Loading blacklist…", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                }
            }

            // 2) Empty, per tab (:702-732, :801-831, :898-928). The shared
            // control with its four optional metric props set to the Slint's
            // numbers: 48px glyph at 0.3 opacity, a MUTED regular-weight title.
            QbzEmptyState {
                visible: root.bodyBranch === 2
                anchors.centerIn: parent
                iconName: root.activeTab === 2 ? "thumbs-down" : "blind-eye"
                iconSize: 48
                iconOpacity: 0.3
                titleMuted: true
                titleWeight: theme.weightRegular
                title: root.activeTab === 0
                    ? QbzSession.tr("No blacklisted artists", QbzSession.trRev)
                    : root.activeTab === 1
                        ? QbzSession.tr("No blocked albums", QbzSession.trRev)
                        : QbzSession.tr("No dismissed artists", QbzSession.trRev)
                body: root.activeTab === 0
                    ? QbzSession.tr("To hide an artist, go to their page and use the menu.", QbzSession.trRev)
                    : root.activeTab === 1
                        ? QbzSession.tr("To block an album, open its menu and choose Block this album.", QbzSession.trRev)
                        : QbzSession.tr("To stop seeing an artist in Recommendations, open its card menu and choose Not interested.", QbzSession.trRev)
            }

            // 3) No results — identical on all three tabs, and no body line
            // (the control hides `body` when it is "").
            QbzEmptyState {
                visible: root.bodyBranch === 3
                anchors.centerIn: parent
                iconName: "search"
                iconSize: 48
                iconOpacity: 0.3
                titleMuted: true
                titleWeight: theme.weightRegular
                title: QbzSession.tr("No results for \"{}\"", QbzSession.trRev)
                    .replace("{}", root.searchQuery)
            }
        }
    }

    // ============================== body ==================================
    Item {
        id: bodyHost
        // Kiosk trades the desktop 24px page gutter for 12 — at 784x256 the
        // gutters alone are 19% of the panel.
        x: root.kioskHost ? 12 : 24
        y: root.kioskHost ? 12 : head.y + head.height + 14
        width: root.width - (root.kioskHost ? 24 : 48)
        height: Math.max(0, root.height - y - (root.kioskHost ? 12 : 24))

        Loader {
            anchors.fill: parent
            sourceComponent: root.kioskHost ? null : statesComponent
        }

        // 4) The list. THREE-edge anchors + an explicit width, never
        // `anchors.fill` + `width` — the two are mutually exclusive, the anchor
        // wins, the Slint's 18px scrollbar reserve silently never applies AND
        // Qt logs a `.qml:` warning the gate greps for.
        ListView {
            id: rowList
            // In kiosk the list is the ONLY scroller — it carries the chrome
            // in its header and the loading/empty/no-results states in its
            // footer, so it stays mounted on every branch. The model is
            // emptied instead of the view being hidden, which also drops the
            // stale rows a `loading` republish would otherwise leave under the
            // spinner.
            visible: root.kioskHost || root.bodyBranch === 4
            anchors.left: parent.left
            anchors.top: parent.top
            anchors.bottom: parent.bottom
            width: parent.width - (root.kioskHost ? 0 : 18)
            model: (root.kioskHost && root.bodyBranch !== 4) ? [] : root.listRows
            header: root.kioskHost ? chromeComponent : null
            footer: root.kioskHost ? statesComponent : null
            spacing: 4
            clip: true
            // ~2 rows of pre-instantiation past the viewport.
            cacheBuffer: 128
            boundsBehavior: Flickable.StopAtBounds

            onContentYChanged: coverWindow.restart()
            onModelChanged: coverWindow.restart()
            // The window is computed from contentY + height, and `height` is 0
            // until the Loader has sized the view — without this the first
            // dispatch after a mount asks for 3 rows instead of a screenful.
            onHeightChanged: coverWindow.restart()
            Component.onCompleted: coverWindow.restart()

            delegate: BlacklistRow {
                required property var modelData
                width: rowList.width
                kioskHost: root.kioskHost
                arm: root.activeTab === 0 ? "artist"
                    : root.activeTab === 1 ? "album" : "dismissed"
                row: modelData
                coverSource: root.activeTab === 1
                    ? (root.coverMap[modelData.coverUrl] || modelData.artPath || "")
                    : ""
                onSelectRequested: {
                    if (root.activeTab === 1)
                        QbzBlacklist.openAlbum(modelData.id)
                    else
                        QbzBlacklist.openArtist(modelData.id)
                }
                onRemoveRequested: {
                    if (root.activeTab === 0)
                        QbzBlacklist.removeArtist(modelData.id)
                    else if (root.activeTab === 1)
                        QbzBlacklist.removeAlbum(modelData.id)
                    else
                        QbzBlacklist.removeDismissed(modelData.id)
                }
            }
        }
        // The ListScrollbar replica — 14px gutter, inset 4 from the pane's
        // right edge, which is exactly the 18px the list gave back.
        // Back/forward scroll memory (controls/ScrollMemory.qml): reports
        // this container's offset while it is the live page, and restores it
        // when a back/forward step arms this route.
        ScrollMemory { target: rowList; scope: "blacklist" }
        QbzScrollBar {
            // The control's own rule is `visible: maxScroll > 0` (its auto-hide,
            // = the Slint ListScrollbar's `auto-hide: true`). Assigning `visible`
            // here REPLACES that binding, so the overflow test has to be carried
            // over or a short list gets a full-height thumb on gutter hover
            // (Sidebar.qml:383 combines the two the same way).
            visible: (root.kioskHost || root.bodyBranch === 4)
                && rowList.contentHeight > rowList.height
            target: rowList
            anchors.right: parent.right
            anchors.rightMargin: 4
            anchors.top: parent.top
            anchors.bottom: parent.bottom
        }
    }

    // ======================== Clear-All confirm ============================
    // Declared LAST: declaration order IS z-order, and it also carries z: 3100
    // (ADR-009's floor is 3000; 3100 is the layer the port's other in-pane
    // modal scrim uses). Cancel / Esc / a backdrop click all dismiss WITHOUT
    // mutating.
    QbzConfirmModal {
        id: clearConfirm
        anchors.fill: parent
        kioskHost: root.kioskHost
        title: QbzSession.tr("Clear Blacklist?", QbzSession.trRev)
        body: root.activeTab === 0
            ? QbzSession.tr("This will remove all {} blacklisted artists. This cannot be undone.", QbzSession.trRev)
                .replace("{}", root.artistCount)
            : QbzSession.tr("This will remove all {} blocked albums. This cannot be undone.", QbzSession.trRev)
                .replace("{}", root.albumCount)
        confirmLabel: QbzSession.tr("Clear All", QbzSession.trRev)
        danger: true
        onConfirmed: {
            if (root.activeTab === 0)
                QbzBlacklist.clearAllArtists()
            else
                QbzBlacklist.clearAllAlbums()
        }
    }
}
