// The Local Library album collection — the Qt equivalent of
// discover/AlbumCollectionView.slint as LocalLibraryView.slint mounts it
// (:1255 Albums, :1601 Folders-flat, :2409 Artists detail): grid or list,
// grouped or flat, with the source badge and multi-select.
//
// WHY A CHUNK MODEL AND NOT A GridView + Repeater
// The Slint calls this a "chunked grid" and it is the one surface with the
// documented 16k-row freeze. Here EVERY arm is a single windowing ListView
// over a flat "entry" array:
//     { t: 0, label }                 -> group header (34px)
//     { t: 1, items: [...], base: n } -> one visual row of cards, or one
//                                        list row; `base` is the row's
//                                        first index in the flat album array
// so grouping costs one O(n) rebuild when the inputs change (never per
// frame) and the mounted delegate count stays proportional to the viewport,
// not to the library. Artwork is reported from the mounted entry range only,
// via the host's windowing helpers.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../theme"

Item {
    id: root

    /// The LocalLibraryView root — artMap + queueWindowReport live there.
    property var view: null
    /// Stable id for this cover surface in the host's window registry. Three
    /// instances of this component can be alive at once (Albums, Folders-flat,
    /// the Artists detail grid), and they must not share a slot.
    property string surface: "collection"
    /// Scroll-memory scope for this list ("local:albums" / "local:folders"),
    /// empty when the host is not the page (the artist detail pane).
    property string scrollScope: ""
    /// Flat album rows (already searched/sorted/filtered by the host).
    property var rows: []
    /// [{ letter, items: [...] }] — only read when `grouped`.
    property var groups: []
    property bool grouped: false
    property string viewMode: "grid"      // "grid" | "list"
    property bool showSource: true
    property bool selectMode: false
    /// { albumId: true } — the host owns the selection set.
    property var selected: ({})
    /// Only the Albums tab opts into the phase-F1 native stream. The other
    /// two collection users keep their existing bounded arrays.
    property bool nativeSurface: false
    property bool nativeActive: false
    property var nativeModel: null
    property string nativeJumpsJson: "[]"

    /// The scrollbar belongs on the WINDOW's right edge, but hosts inset
    /// their content (LocalAlbumsTab pads 32, plus 20 more to clear the A-Z
    /// strip), which dragged the bar visibly inward compared with the rest of
    /// the app. Both are measured from the window's right edge:
    /// `scrollBarInset` is where the bar should land (LibraryView uses 4, or
    /// 34 with the strip up) and `hostRightInset` is how far this component's
    /// own right edge already is from it. Defaults of 0 leave callers that
    /// have not opted in exactly where they were.
    property int scrollBarInset: 0
    property int hostRightInset: 0

    signal openRequested(string id)
    signal playRequested(string id)
    signal enqueueRequested(string id, string mode)
    /// `modifiers` rides through from the card's mouse event — Shift is
    /// what turns a click into a range (controls/SelectionModel.qml).
    signal toggleSelect(string id, int modifiers)
    signal nativeToggleSelect(int index, int modifiers)

    QbzTheme { id: theme }

    readonly property int cellW: 220
    readonly property int cellH: 266
    readonly property int headerH: 34
    readonly property int listRowH: 56
    /// Columns, or 0 while the width is still UNKNOWN.
    ///
    /// Zero is not a degenerate case to be clamped away — it is the honest
    /// answer, and clamping it to 1 is what put a full-length ONE-COLUMN grid
    /// on screen at every mount. `Component.onCompleted` builds synchronously
    /// (deliberately, so the grid is not empty for a frame), and at that moment
    /// `width` is still 0, so `Math.max(1, …)` said "one column" and the whole
    /// album list was chunked one-per-row. The real width arrives a moment
    /// later, `onColsChanged` fires, and the entire model is thrown away and
    /// rebuilt — the reflow the owner sees, and it is visible for exactly as
    /// long as chunking every album twice takes.
    ///
    /// A list view really is one column, so that arm keeps its 1.
    readonly property int cols: viewMode === "grid"
        ? (width > 0 ? Math.max(1, Math.floor(width / cellW)) : 0) : 1

    /// Stand-in for a grid slot with no album — the last row of a group is
    /// rarely full, and since the cell count is now CONSTANT (that is what
    /// makes recycling free) those tail slots are real, live cells with no
    /// data behind them. `visible: false` does not stop a binding from
    /// evaluating, so every `modelData.x` on an empty slot threw a TypeError
    /// once per evaluation — a log flood during any scroll. Binding against
    /// this frozen object instead keeps the slot silent AND avoids gating the
    /// card behind a Loader, which would reintroduce exactly the construction
    /// churn the constant cell count removed.
    readonly property var emptySlot: ({
        "id": "", "title": "", "artist": "", "year": "",
        "qualityTier": "", "artKey": "", "source": "", "sourceRaw": "",
        "sources": [], "isFavorite": false, "favoriteable": false
    })

    // ---------------------------------------------------------------------
    // Chunk model
    // ---------------------------------------------------------------------
    // `flat` is the album array in display order (the window report indexes
    // into it); `entries` is what the ListView actually mounts.
    property var flat: []
    property var entries: []
    /// [{ letter, index }] with `index` = the ENTRY index of that letter's
    /// header — what AlphaStrip needs to scroll here.
    property var alphaJumps: []

    // Counts and times every rebuild so a smoke run can FALSIFY the coalescing:
    // one line per keystroke means it holds, two means it does not. Measured
    // around the work itself, never from a deferred callback.
    LoggingCategory {
        id: colTiming
        name: "qbz.nav.timing"
        defaultLogLevel: LoggingCategory.Warning
    }
    property int rebuildCount: 0

    function parseNativeJumps() {
        try { return JSON.parse(nativeJumpsJson || "[]") }
        catch (e) { return [] }
    }

    function rebuild() {
        // No width yet: build NOTHING rather than something guaranteed wrong.
        // One empty frame beats a full-length one-column grid that has to be
        // discarded — and in the common case the width IS known here, so this
        // never fires and the synchronous first build is unchanged.
        if (cols <= 0) return
        if (nativeActive) {
            flat = []
            entries = []
            alphaJumps = parseNativeJumps()
            report()
            return
        }
        var _t0 = Date.now()
        var out = []
        var flatOut = []
        var jumps = []
        var per = cols
        var i, j
        function pushChunk(items) {
            for (j = 0; j < items.length; j += per) {
                out.push({ "t": 1, "base": flatOut.length + j,
                           "items": items.slice(j, j + per) })
            }
            for (j = 0; j < items.length; j++) flatOut.push(items[j])
        }
        if (grouped) {
            for (i = 0; i < groups.length; i++) {
                var g = groups[i]
                if (!g.items || g.items.length === 0) continue
                jumps.push({ "letter": g.letter, "index": out.length })
                // The header carries the flat index its group STARTS at.
                // Without it the window report below fell back to 0 whenever
                // the first visible entry was a header — so scrolling in
                // grouped mode asked for artwork from album 0 through the
                // current letter, hundreds of covers at a time, and then
                // evicted them again. (Found by an independent review; the
                // fallback reads as harmless until you notice what `0` means
                // to `queueWindowReport`.)
                out.push({ "t": 0, "label": g.letter, "base": flatOut.length })
                pushChunk(g.items)
            }
        } else {
            pushChunk(rows)
        }
        flat = flatOut
        entries = out
        alphaJumps = jumps
        root.rebuildCount += 1
        console.info(colTiming, "[coltiming] rebuild #" + root.rebuildCount
            + " surface=" + root.surface + " grouped=" + root.grouped
            + " cols=" + per
            + " albums=" + flatOut.length + " entries=" + out.length
            + " in " + (Date.now() - _t0) + "ms")
        report()
    }
    // `rows` and `groups` are TWO bindings over ONE query: the host derives
    // `albumsVisible` from the search box, then `albumsGrouped` FROM
    // `albumsVisible` (LocalLibraryView.qml:378-380). So a single keystroke
    // changes both, and with one handler each this rebuilt twice — and the
    // first of the two was WRONG as well as wasted, because it chunked the new
    // rows against the previous groups. Grouping being off does not save it:
    // `groupRows` returns a FRESH `[]` every time, which is still a change.
    //
    // Coalesced into one rebuild per event-loop turn. A zero-interval Timer
    // rather than `Qt.callLater`, because dedup then depends on the callback
    // being the same function object every time, and `restart()` needs no such
    // assumption — same shape as `artFlush` in LocalLibraryView.
    Timer {
        id: rebuildCoalescer
        interval: 0
        repeat: false
        onTriggered: root.rebuild()
    }
    function scheduleRebuild() { rebuildCoalescer.restart() }
    onRowsChanged: scheduleRebuild()
    onGroupsChanged: scheduleRebuild()
    onGroupedChanged: scheduleRebuild()
    onViewModeChanged: { scheduleRebuild(); scheduleNativeReset() }
    onColsChanged: { scheduleRebuild(); scheduleNativeReset() }
    onNativeActiveChanged: { scheduleRebuild(); reportSoon() }
    onNativeJumpsJsonChanged: if (nativeActive) alphaJumps = parseNativeJumps()

    Timer {
        id: nativeQueryCoalescer
        interval: 0
        repeat: false
        onTriggered: root.resetNativeQuery()
    }
    function scheduleNativeReset() {
        if (nativeSurface && cols > 0 && view
                && view.albumsFilter.favorite !== true)
            nativeQueryCoalescer.restart()
    }
    function resetNativeQuery() {
        if (!nativeSurface || !view || cols <= 0
                || view.albumsFilter.favorite === true) return
        QbzLocal.albumsNativeReset(
            view.albumsSearch,
            view.albumsSort,
            view.albumsGroup,
            JSON.stringify(view.filter || ({})),
            cols)
    }
    Connections {
        target: root.nativeSurface ? root.view : null
        function onAlbumsSearchChanged() { root.scheduleNativeReset() }
        function onAlbumsSortChanged() { root.scheduleNativeReset() }
        function onAlbumsGroupChanged() { root.scheduleNativeReset() }
        function onFilterChanged() { root.scheduleNativeReset() }
    }
    Connections {
        target: root.nativeSurface ? QbzLocal : null
        function onLocalAlbumModeChanged() { root.scheduleNativeReset() }
    }
    // The first build stays synchronous — deferring it would show one frame of
    // empty grid on every mount.
    Component.onCompleted: { rebuild(); reportSoon(); scheduleNativeReset() }
    Component.onDestruction: if (view) view.releaseWindow(root.surface)

    // ---------------------------------------------------------------------
    // Window report — the MOUNTED entry band, mapped back to flat indices.
    // ---------------------------------------------------------------------
    // TRIGGERS. A report used to leave here on `contentY` and on the model
    // swap alone, and BOTH of those fire while this surface is still hidden:
    // the tab body is mounted behind `localAlbumsLoading` and the rows land
    // in the same pass, so the one report that mattered was thrown away by
    // the `!visible` guard below and nothing was requested until the user
    // moved the list by a pixel. Every state change that can alter the
    // mounted band now reports: mount, becoming visible, the model rebuild,
    // a viewport resize, and the scroll.
    // Only wheel/motion reports can be skipped. Model, layout and artwork
    // notifications always refresh the window, including late native pages.
    property string _motionBand: ""
    property double _motionReportedAt: 0
    function report(motionOnly) {
        if (!view || !list) return
        var count = nativeActive && nativeModel ? nativeModel.totalCount : entries.length
        if (!list.visible || count === 0 || width <= 0) {
            root._motionBand = ""
            view.releaseWindow(root.surface)
            return
        }
        var first = list.indexAt(4, list.contentY + 1)
        var last = list.indexAt(4, list.contentY + Math.max(1, list.height) - 1)
        if (first < 0) first = 0
        if (last < 0) last = Math.min(count - 1, first + 8)
        var visibleFirst = first
        // These delegates already exist in cacheBuffer. Resolve their covers
        // too, so Qt can decode them before the next wheel step reveals them.
        if (root.viewMode === "grid") {
            first = Math.max(0, first - 2)
            last = Math.min(count - 1, last + 2)
        }
        var band = first + ":" + last + ":" + visibleFirst
        var now = Date.now()
        if (motionOnly === true && root._motionBand === band
                && now - root._motionReportedAt < 1000) return
        root._motionBand = band
        root._motionReportedAt = now
        if (nativeActive && nativeModel) {
            var resident = []
            for (var offset = 0; offset <= last - first; offset++) {
                var entryIndex = visibleFirst + offset
                if (entryIndex > last) entryIndex = first + entryIndex - last - 1
                var entry = nativeModel.rowAt(entryIndex)
                if (!entry || entry.loading || !entry.items) continue
                for (var itemIndex = 0; itemIndex < entry.items.length; itemIndex++)
                    resident.push(entry.items[itemIndex])
            }
            if (resident.length > 0)
                view.queueWindowReport(resident, 0, resident.length - 1, root.surface)
            else
                view.releaseWindow(root.surface)
            return
        }
        var lo = entries[first]
        var hi = entries[last]
        // Both entry kinds now carry `base`, so a header at either edge of
        // the viewport reports ITS group's position instead of collapsing to
        // the top of the library.
        var loIdx = lo ? (lo.base || 0) : 0
        var hiIdx = hi
            ? (hi.t === 1 ? hi.base + hi.items.length - 1 : Math.max(loIdx, hi.base || 0))
            : loIdx
        view.queueWindowReport(flat, loIdx, hiIdx, root.surface)
    }

    /// Report NOW, then again once layout has settled. `indexAt` answers -1
    /// until the ListView has laid its delegates out, which is exactly the
    /// state a just-mounted / just-shown list is in — the immediate call gets
    /// the covers moving off the conservative fallback band, the second one
    /// corrects it to the real viewport.
    Timer {
        id: reportSettle
        interval: 50
        repeat: false
        onTriggered: root.report()
    }
    function reportSoon() {
        report()
        reportSettle.restart()
    }

    onVisibleChanged: {
        if (visible) reportSoon()
        else if (view) view.releaseWindow(root.surface)
    }
    onHeightChanged: report()
    Connections {
        target: root.view
        function onArtworkRefresh() { root.reportSoon() }
    }

    function jumpToEntry(entryIndex) {
        list.positionViewAtIndex(entryIndex, ListView.Beginning)
    }

    ListView {
        id: list
        anchors.fill: parent
        clip: true
        cacheBuffer: root.viewMode === "grid" ? root.cellH * 2 : root.listRowH * 10
        boundsBehavior: Flickable.StopAtBounds
        // Recycle rows instead of destroying and rebuilding them on every
        // scroll step. Safe only NOW: the project's own note (PmListRow /
        // PmFolderMenu) is that `reuseItems` plus a per-row menu gives every
        // pooled delegate its own Popup — so the menus above had to become
        // lazy FIRST. In the other order this would install that bug.
        reuseItems: true
        model: root.nativeActive && root.nativeModel ? root.nativeModel : root.entries
        onContentYChanged: root.report(true)
        onModelChanged: root.report()
        onHeightChanged: root.report()
        onVisibleChanged: if (visible) root.reportSoon()

        delegate: Loader {
            required property var modelData
            width: list.width
            height: modelData.t === 0 ? root.headerH
                 : root.viewMode === "grid" ? root.cellH : root.listRowH
            sourceComponent: modelData.loading ? loadingComp
                : modelData.t === 0 ? headerComp
                : root.viewMode === "grid" ? cardRowComp : listRowComp
            onModelDataChanged: root.reportSoon()

            Component {
                id: loadingComp
                QbzSkeleton {
                    variant: root.viewMode === "grid" ? "cardGrid" : "rowList"
                    cellW: root.cellW
                    cellH: root.cellH
                    rowH: root.listRowH
                    rowGap: 0
                    rowArtSize: 40
                    phase: root.view ? root.view.skelPhase : false
                }
            }

            Component {
                id: headerComp
                Item {
                    Text {
                        x: 2
                        anchors.verticalCenter: parent.verticalCenter
                        text: modelData.label
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                    }
                }
            }

            // One visual row of album cards.
            Component {
                id: cardRowComp
                Row {
                    id: cardRow
                    /// The row entry from the ListView delegate (the Loader's
                    /// `modelData`), captured here because the Repeater below
                    /// shadows that name inside its own delegates.
                    readonly property var rowEntry: modelData
                    spacing: root.cellW - 200
                    // FIXED CELL COUNT — `root.cols`, a NUMBER, not the row's
                    // item array.
                    //
                    // This is what makes the ListView's `reuseItems` actually
                    // pay. Pooling recycles the row SHELL, but the old
                    // `model: modelData.items` handed this Repeater a new
                    // array on every recycle, so it destroyed and rebuilt its
                    // AlbumCards anyway — and the card is where the cost is:
                    // ~29 items, a RoundedImage with TWO render targets (its
                    // own layer plus the mask), image probes, a skeleton.
                    // The shell was reused and the expensive part was not.
                    //
                    // With a constant count the cells are created once and
                    // survive recycling; a scrolled or re-filtered row only
                    // reassigns `item`, which is a binding update instead of a
                    // construction. Same shape as the reference, where cheap
                    // slots stay alive and only the data moves.
                    Repeater {
                        model: root.cols
                        delegate: Item {
                            id: cardCell
                            required property int index
                            /// The album for this slot, or `null` on the last
                            /// row when it is not full. A null slot renders
                            /// nothing and keeps its width, so the row's
                            /// spacing does not collapse.
                            readonly property var modelData:
                                (cardRow.rowEntry && cardRow.rowEntry.items)
                                    ? (cardRow.rowEntry.items[cardCell.index] || null)
                                    : null
                            /// `modelData` answers "is this slot filled?" and
                            /// stays null so `visible` keeps working. `slot`
                            /// is what the bindings read: same object when the
                            /// slot is filled, `root.emptySlot` when it is not.
                            readonly property var slot:
                                cardCell.modelData || root.emptySlot
                            property string artTracePendingKey: ""
                            Connections {
                                target: root.view && root.view.artTimingEnabled
                                    && cardCell.artTracePendingKey !== "" ? root.Window.window : null
                                function onFrameSwapped() {
                                    if (cardCell.slot.artKey === cardCell.artTracePendingKey)
                                        root.view.traceArt("frame", cardCell.artTracePendingKey)
                                    cardCell.artTracePendingKey = ""
                                }
                            }
                            visible: cardCell.modelData !== null
                            width: 200
                            height: 246

                            // AlbumCard updates these two properties
                            // optimistically, which intentionally replaces the
                            // bindings below. The card instances in this fixed
                            // Repeater survive ListView row reuse, so restore
                            // the bindings whenever a different slot object is
                            // installed; otherwise the next album could inherit
                            // the previous card's heart/pin state.
                            function restoreMutableBindings() {
                                albumCard.isFavorite = Qt.binding(function () {
                                    return root.view
                                        ? root.view.albumFavorite(cardCell.slot)
                                        : cardCell.slot.isFavorite === true
                                })
                                albumCard.isPinned = Qt.binding(function () {
                                    return QbzLibrary.pinState(
                                        "album", cardCell.slot.id)
                                })
                            }
                            function scheduleMutableRestore() {
                                mutableRestore.expected = cardCell.slot
                                mutableRestore.restart()
                            }
                            onSlotChanged: cardCell.scheduleMutableRestore()
                            Component.onCompleted:
                                cardCell.scheduleMutableRestore()

                            // A queued Qt.callLater closure can outlive this
                            // recycled delegate and then call into an invalid
                            // QML context. A child timer is cancelled with its
                            // owner, so delayed binding repair cannot escape
                            // the card cell's lifetime.
                            Timer {
                                id: mutableRestore
                                interval: 0
                                repeat: false
                                property var expected: null
                                onTriggered: {
                                    if (cardCell.slot === expected)
                                        cardCell.restoreMutableBindings()
                                }
                            }

                            AlbumCard {
                                id: albumCard
                                localMode: true
                                localPlaylistAffordance: true
                                quickViewAffordance: true
                                pinAffordance: true
                                albumId: cardCell.slot.id
                                title: cardCell.slot.title
                                artist: cardCell.slot.artist
                                artistId: ""
                                genre: ""
                                year: cardCell.slot.year
                                qualityTier: cardCell.slot.qualityTier
                                qualityDetail: cardCell.slot.qualityDetail || ""
                                artSource: cardCell.slot.artPath || (root.view
                                    ? (root.view.artMap[cardCell.slot.artKey] || "") : "")
                                artFadeMs: root.view && root.view.immediateArtwork ? 0 : 200
                                onArtworkReadyChanged: {
                                    if (root.view && root.view.artTimingEnabled && artworkReady) {
                                        root.view.traceArt("ready", cardCell.slot.artKey)
                                    }
                                }
                                onArtworkRevealedChanged: {
                                    if (root.view && root.view.artTimingEnabled && artworkRevealed) {
                                        root.view.traceArt("revealed", cardCell.slot.artKey)
                                        cardCell.artTracePendingKey = cardCell.slot.artKey
                                    }
                                }
                                pinArtworkUrl: artSource
                                isPinned: QbzLibrary.pinState("album", albumId)
                                // LocalLibraryView.slint:1267 `show-source-badge`
                                // — the card takes the raw source word and the
                                // BADGE is what the flag switches (blanking
                                // `source` would also silence the card's
                                // source-aware menu gates).
                                //
                                // `sourceRaw` FIRST (contract §D.1). It carries
                                // `qobuz_purchase` and nothing else; `source`
                                // folds that into "offline" so the source chips
                                // keep filtering it, which means the card's gold
                                // purchase chip (AlbumCard.qml:588) only ever
                                // fires off the raw word. All FOUR badge sites
                                // do this — grid and list must not disagree
                                // about the same album.
                                source: cardCell.slot.sourceRaw
                                    || cardCell.slot.source
                                sources: cardCell.slot.sources || []
                                showSourceBadge: root.showSource
                                hostFavorite: cardCell.slot.favoriteable === true
                                isFavorite: root.view
                                    ? root.view.albumFavorite(cardCell.slot)
                                    : cardCell.slot.isFavorite === true
                                extraMenuEntries: [
                                    { "label": QbzSession.tr("Album info", QbzSession.trRev),
                                      "icon": "info", "action": "album-info" }
                                ]
                                onExtraMenuAction: function (action) {
                                    if (action === "album-info")
                                        QbzLocal.bulkAction("album",
                                            JSON.stringify([String(cardCell.slot.id)]), action)
                                }
                                onFavoriteRequested: if (root.view) {
                                    root.view.toggleAlbumFavorite(
                                        cardCell.slot, albumCard.artSource)
                                }
                                // SELECT MODE IS THE CARD'S (AlbumCard's
                                // `selectMode`/`selected`/`selectToggled`, the
                                // port of discover/AlbumCard.slint:83, :179-197,
                                // :207, :239, :465). This host used to paint its
                                // OWN 22px tick on top of the card at top-left —
                                // a THIRD geometry, and one that could only add
                                // the indicator: the card's hover PLAY button
                                // stayed live underneath it, so hovering a card
                                // you meant to tick and hitting play started the
                                // album. The card hides the whole hover action
                                // row and the pin badge in select mode and draws
                                // the reference's 24px tick in the reference's
                                // corner (top-right); all three move together,
                                // which is exactly what a host overlay cannot
                                // do. Do not re-add one.
                                selectMode: root.selectMode
                                selected: root.nativeActive
                                    ? cardCell.slot.selected === true
                                    : root.selected[cardCell.slot.id] === true
                                // The card's modifiers, straight through: the
                                // paged grid (299dcde5b) passed NoModifier here,
                                // so a Shift-click on a card was a toggle.
                                onSelectToggled: function (mods) {
                                    if (root.nativeActive)
                                        root.nativeToggleSelect(cardCell.slot.nativeIndex, mods)
                                    else
                                        root.toggleSelect(cardCell.slot.id, mods)
                                }
                                // Non-select mode only — the card routes a
                                // select-mode click to `selectToggled` and never
                                // emits this while ticking.
                                onOpenRequested: root.openRequested(cardCell.slot.id)
                                onPlayRequested: root.playRequested(cardCell.slot.id)
                                onEnqueueRequested: function (m) {
                                    root.enqueueRequested(cardCell.slot.id, m)
                                }
                            }
                            // Observe the card's actual derivative, so the placeholder
                            // neither decodes the original again nor retires before
                            // the displayed image is ready. The source probe remains
                            // only for the diagnostic baseline comparison.
                            // settleMs is mandatory here: local artwork
                            // resolution drops keys with no cover, so an
                            // artless album would otherwise shimmer forever
                            // (see LocalLibraryView.artSettleMs).
                            QbzSkeleton {
                                variant: "art"
                                width: 200
                                height: 200
                                pending: root.view
                                    ? root.view.artWanted(cardCell.slot.artKey) : false
                                coverReady: root.view && root.view.immediateArtwork
                                    ? albumCard.artworkReady : false
                                handoverFadeMs: root.view && root.view.immediateArtwork ? 0 : 180
                                coverSource: root.view && !root.view.immediateArtwork
                                    ? root.view.artPathOf(cardCell.slot.artKey) : ""
                                phase: root.view ? root.view.skelPhase : false
                                settleMs: root.view ? root.view.artSettleMs : 0
                                settleHold: root.view ? root.view.artPulse : false
                            }
                            // RIGHT-only, declared after the card so every
                            // left click still reaches the card's own areas.
                            MouseArea {
                                id: cardRc
                                anchors.fill: parent
                                acceptedButtons: Qt.RightButton
                                onClicked: function (mouse) {
                                    albumCard.openAlbumMenu(cardRc, mouse.x, mouse.y)
                                }
                            }
                            // (The multi-select tick that used to sit here is
                            //  gone — it is AlbumCard's `selectMode` now; see
                            //  the note on the card above.)
                        }
                    }
                }
            }

            // One list row.
            Component {
                id: listRowComp
                LocalAlbumRow {
                    width: list.width
                    view: root.view
                    item: modelData.items[0]
                    artSource: root.view
                        ? (modelData.items[0].artPath
                           || root.view.artMap[modelData.items[0].artKey] || "") : ""
                    isFavorite: root.view
                        ? root.view.albumFavorite(modelData.items[0])
                        : modelData.items[0].isFavorite === true
                    showSource: root.showSource
                    selectMode: root.selectMode
                    checked: root.nativeActive
                        ? modelData.items[0].selected === true
                        : root.selected[modelData.items[0].id] === true
                    onOpened: root.openRequested(modelData.items[0].id)
                    onPlayRequested: root.playRequested(modelData.items[0].id)
                    onEnqueueRequested: function (m) {
                        root.enqueueRequested(modelData.items[0].id, m)
                    }
                    onFavoriteRequested: if (root.view) {
                        root.view.toggleAlbumFavorite(modelData.items[0], artSource)
                    }
                    onToggleSelect: function (mods) {
                        if (root.nativeActive)
                            root.nativeToggleSelect(modelData.items[0].nativeIndex, mods)
                        else
                            root.toggleSelect(modelData.items[0].id, mods)
                    }
                }
            }
        }
    }

    // Back/forward scroll memory — OPT-IN, because this component is mounted
    // by three different hosts (the Albums tab, the Folders tab and the artist
    // detail pane) and only the two that ARE the page should own a scroll
    // scope. The default "" leaves the third inert.
    ScrollMemory { target: list; scope: root.scrollScope }

    QbzScrollBar {
        anchors.right: parent.right
        // Negative when the host insets its content — the bar overhangs to
        // reach the real edge. Safe: `root` is a plain Item and does not clip
        // (the `clip: true` in this file is on the ListView, a sibling).
        anchors.rightMargin: root.scrollBarInset - root.hostRightInset
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: list
        visible: list.contentHeight > list.height
    }
}
