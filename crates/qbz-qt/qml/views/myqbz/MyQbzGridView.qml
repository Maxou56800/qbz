// MyQBZ index grid — ONE view for BOTH routes: `"mixtapes"`
// (myqbz/MixtapesView.slint, 222 lines) and `"collections"`
// (myqbz/CollectionsView.slint, 237 lines). The two .slint files are identical
// apart from the title, the model, the create kind and ONE extra kind-filter
// QbzSelect, so they are one file with a `kind` property — duplicating them
// would be the AlbumCard/ArtistCard mistake the track rules name.
// AppShell mounts it for both route ids and sets `kind`.
//
// Geometry, all off the .slint: page padding 16 top / 32 right / 100 bottom /
// 32 left (:57-60), a 40px header with 16px spacing (:65-66), a 16px spacer
// (:88), a 30px toolbar flush RIGHT with 8px spacing (:126-130), another 16px
// spacer (:169), 208x296 cards on a 20px grid gap, 64px list rows with 4px
// spacing, and the 80/16/80 empty block (:91-118).
//
// Toolbar order, left to right: search -> sort -> [kind filter, Collections
// only] -> view toggle. The view toggle's segments are LIST FIRST, grid second
// (MyQbzShared.slint:57-70) — not the grid-first order ViewModeToggle uses.
//
// NO context menu on a card or a row, no drag, no hover animation, no
// per-view back button (the shell owns the global < >). Slint declares none of
// them; do not add them.
//
// ── KNOWN DIVERGENCES, recorded so the next reader does not re-discover them ─
//
// 1. `QbzLineEdit` is NOT 1:1 with `primitives/ExpandableSearch.slint`, in
//    five measurable ways that are all PRE-EXISTING and all app-wide (the
//    Local Library toolbars, the tree rail, the folder detail and the queue
//    ride the same control): leading glyph 13 vs 14 at `sm`
//    (QbzLineEdit.qml:51 vs ExpandableSearch.slint:131), input + placeholder
//    12px vs Typography.body 15 (:148,:179 vs :114,:119), row spacing 7 vs 8
//    (:126 vs :95), focus border `accent` vs `focus-ring` (:115 vs :88), and
//    the X's hover fill `surfaceHover` vs `surface-elevated` (:196 vs :143).
//    Fixing them is a separate app-wide commit with its own visual review; a
//    MyQBZ commit must not change the Local Library search boxes.
// 2. Both selects are `sm: true` with the .slint's RAW `menu-width` (180 /
//    200), which `QbzSelect` turns into a 140 / 160 control — 30px tall, r6,
//    no outline, 12px label, 14px chevron (QbzSelect.slint:88,102-105,126,
//    147-148,299). The open PANEL used to be 40px wider than the reference's
//    (`Math.max(popupWidth, menuWidth)` against QbzSelect.slint:89-91's
//    `max(popup-width, eff-width)`); that is FIXED in the shared control now —
//    `Math.max(popupWidth, width)`. It moves only the three `sm` selects in
//    the tree, all of them MyQBZ's (grid sort 180->140, kind filter 200->160,
//    detail sort 170->130); the Local Library toolbar's select is not `sm` and
//    the three `popupWidth` callers are not either, so nothing else changes.
// 3. Slint scrolls the WHOLE page (header and toolbar included) in one
//    Flickable; here the header and toolbar are static and only the body
//    scrolls, because the body must be a windowed GridView/ListView and
//    nesting one inside a page Flickable is not an option. Same shape as
//    views/LibraryView.qml and views/LocalLibraryView.qml.
// 4. Scroll-position restore uses the shared ScrollMemory instances below,
//    separately for the grid and list bodies.
// 5. Slint reads `MyQbzState.loading` in neither grid; the cardGrid skeleton
//    below is the same deliberate improvement LibraryView.qml:1163-1225
//    documents. The empty state stays gated on `!loading`, and the skeleton is
//    ARMED after 250ms so a fresh (empty) account does not flash a page of
//    fake cards on its way to "No mixtapes yet" — see `skelArmed`.
// 6. One live instance serves BOTH routes, so per-route UI state is reset by
//    hand on `kind` change (`onKindChanged`), and the two body views take a
//    QML-side model (`activeItems`) so a republish does not reset the viewport
//    and the inactive view builds no delegates.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../theme"

Rectangle {
    id: root

    /// "mixtape" | "collection" — set by the AppShell route arm.
    property string kind: "mixtape"
    readonly property bool isMix: root.kind === "mixtape"
    /// The bridge's grid discriminator (QbzMyQbz.gridSearch / gridSetSort /
    /// gridSetView take "mixtapes" | "collections").
    readonly property string gridId: root.isMix ? "mixtapes" : "collections"

    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    // A LITERAL 12, as in all 14 painting view roots: QML clips are
    // RECTANGULAR, so the view must round its own fill or the shell bezel
    // reads square (AppShell.qml:188-258).
    radius: 12

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // ONE guarded parse per document. A raw JSON.parse inside a binding throws
    // on the pre-publish frame and takes the whole view down
    // (PlaylistView.qml:55-61).
    readonly property var doc: parseDoc()
    function parseDoc() {
        try {
            return JSON.parse(root.isMix ? QbzMyQbz.mixtapesJson
                                         : QbzMyQbz.collectionsJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property var items: root.doc.cards || []
    // The model the two body views actually consume. NOT `root.items`
    // directly, for two reasons that both showed up as "the grid fights me":
    //
    //  1. SCROLL RESET. `items` is a FRESH JS array out of `JSON.parse` on
    //     every publish, and assigning a new array to a view's `model` is a
    //     full model reset — the view drops to `contentY 0`. Rust publishes
    //     three times per visit (`set_loading(true)` -> `apply` ->
    //     `artwork_pass`, the last one behind a NETWORK round trip), again on
    //     every `reload_grids()` / `republish_all()`, and again on every
    //     keystroke in the search box. So the user scrolled, and a beat later
    //     the artwork pass yanked them back to the top. `applyItems()` carries
    //     the viewport across the reset, which is what Slint gets for free by
    //     diffing its VecModel (MixtapesView.slint:36-50 even restores it).
    //  2. DOUBLE DELEGATES. `visible: false` does NOT stop delegate creation,
    //     so the inactive view used to build the whole card set too: 12 cards
    //     meant 24 `MyQbzCard`s, 24 mosaics and TWO artwork dispatches per
    //     card. The ternaries below hand the model to the ACTIVE view only,
    //     which is Slint's `if …mix-view == "grid"` / `== "list"`
    //     (MixtapesView.slint:186,202) — exactly one subtree alive.
    property var activeItems: []
    property real keepGridY: 0
    property real keepListY: 0
    // Set while a route flip is in flight: the viewport must NOT be carried
    // from Mixtapes into Collections (finding 5 — one instance serves both).
    property bool suppressKeep: false

    function applyItems() {
        if (!root.suppressKeep) {
            root.keepGridY = grid.contentY
            root.keepListY = list.contentY
        }
        root.activeItems = root.items
        // `forceLayout()` flushes the pending re-layout synchronously, so
        // `contentHeight` below is the NEW one and the clamp is honest.
        if (grid.visible) {
            grid.forceLayout()
            grid.contentY = root.clampY(grid, root.keepGridY)
        }
        if (list.visible) {
            list.forceLayout()
            list.contentY = root.clampY(list, root.keepListY)
        }
    }
    function clampY(view, y) {
        var max = Math.max(0, view.contentHeight + view.bottomMargin - view.height)
        return Math.max(0, Math.min(y, max))
    }
    function releaseKeep() { root.suppressKeep = false }

    onItemsChanged: root.applyItems()
    Component.onCompleted: root.applyItems()

    // Mixtapes and Collections are ONE live component (AppShell.qml:199-223
    // only flips `kind`), so every scrap of per-route UI state has to be reset
    // by hand or it bleeds across the flip: the expandable search stayed OPEN
    // showing the other route's query, and the viewport carried over.
    // `suppressKeep` covers both possible orderings of this handler and the
    // `items` re-parse that the same `kind` change triggers.
    onKindChanged: {
        root.suppressKeep = true
        root.keepGridY = 0
        root.keepListY = 0
        grid.contentY = 0
        list.contentY = 0
        searchBox.open = false
        Qt.callLater(root.releaseKeep)
    }
    readonly property bool loading: root.doc.loading === true
    readonly property string search: root.doc.search || ""
    readonly property string sortField: root.doc.sort || "position"
    readonly property string viewMode: root.doc.view || "grid"
    readonly property string kindFilter: root.doc.kindFilter || "all"

    // Slint's own predicate (:81, plus Collections' kind-filter arm at
    // CollectionsView.slint:116): the toolbar and the header "+ New" appear
    // together, and when there is genuinely nothing the empty-state CTA is the
    // SINGLE call to action — never both.
    readonly property bool populated: root.items.length > 0
        || root.search !== ""
        || (!root.isMix && root.kindFilter !== "all")

    readonly property int sortIndex: root.sortField === "name" ? 1
        : root.sortField === "items" ? 2
        : root.sortField === "updated" ? 3 : 0
    readonly property int kindIndex: root.kindFilter === "collection" ? 1
        : root.kindFilter === "artist_collection" ? 2 : 0

    // Grid pitch: the 208px card + the 20px gap on both axes.
    readonly property int cardW: 208
    readonly property int cardH: 184 + 12 + 12 + 18 + 2 * 20 + 18 + 12   // 296
    readonly property int gridGap: 20

    // Content rows (the static header block above the scrolling body).
    readonly property int headerTop: 16
    readonly property int toolbarTop: 16 + 40 + 16                       // 72
    readonly property int bodyTop: root.populated ? root.toolbarTop + 30 + 16 : root.toolbarTop

    // --- skeleton pulse: ONE Timer for the whole view, frozen when the view
    // is not visible or the window is minimized/hidden — NEVER on lost focus
    // (a tiling desktop keeps windows visible and unfocused). Copied from
    // PlaylistView.qml:150-179.
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    // The skeleton is ARMED, not immediate. A first load with nothing to show
    // yet is indistinguishable from an account that simply has no mixtapes, so
    // the old immediate `loading && items.length === 0` flashed a full page of
    // fake cards at every empty account before "No mixtapes yet" resolved. The
    // grid docs come out of local SQLite in a few ms, so a 250ms arming delay
    // removes the flash entirely while a genuinely slow load still gets its
    // skeleton. (Slint has no skeleton at all here — divergence 5.)
    readonly property bool gridLoadingEmpty: root.loading && root.items.length === 0
    property bool skelArmed: false
    onGridLoadingEmptyChanged: if (!root.gridLoadingEmpty) root.skelArmed = false
    Timer {
        interval: 250
        repeat: false
        running: root.gridLoadingEmpty && !root.skelArmed
            && root.visible && root.windowShowing
        onTriggered: root.skelArmed = true
    }
    readonly property bool gridPending: root.gridLoadingEmpty && root.skelArmed
    Timer {
        interval: 900
        repeat: true
        running: root.gridPending && root.visible && root.windowShowing
        onTriggered: root.skelPhase = !root.skelPhase
    }

    // ============================ HEADER =============================
    Item {
        id: header
        x: 32
        y: root.headerTop
        width: Math.max(0, root.width - 64)
        height: 40

        // Mixtapes <-> Collections, as a TAB BAR rather than a page title —
        // the Discover and Library pattern, where the tabs stand in for the
        // heading instead of sitting under one. The two pages are separate
        // ROUTES that share this component (ContentRouter hands it `kind`), so
        // a tab click navigates rather than flipping a local property: that
        // keeps the sidebar highlight, the back stack and the `kind` Binding
        // all agreeing on where the user is, and the page transition is the
        // same one every other route change gets.
        QbzTabBar {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: Math.max(0, parent.width - (root.populated ? 28 + 16 : 0))
            // The ids ARE the routes, so nothing has to translate between the
            // tab and the navigation.
            tabs: [
                { "id": "mixtapes", "label": root.t("Mixtapes") },
                { "id": "collections", "label": root.t("Collections") }
            ]
            activeId: root.isMix ? "mixtapes" : "collections"
            onSelected: function (id) {
                if (id === (root.isMix ? "mixtapes" : "collections"))
                    return
                QbzShell.navigateTo(id)
            }
        }
        NewActionBtn {
            visible: root.populated
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            onClicked: QbzMyQbz.createOpen(root.kind)
        }
    }

    // ============================ TOOLBAR ============================
    // Flush right: the Slint layout floats it with a leading stretch spacer.
    Row {
        id: toolbar
        visible: root.populated
        anchors.right: parent.right
        anchors.rightMargin: 32
        y: root.toolbarTop
        height: 30
        spacing: 8
        layoutDirection: Qt.LeftToRight

        QbzLineEdit {
            // Named so the route flip can collapse it — see onKindChanged.
            id: searchBox
            anchors.verticalCenter: parent.verticalCenter
            searchMode: true
            expandable: true
            sm: true
            elevated: false
            openWidth: 196
            placeholder: root.t("Search")
            text: root.search
            // No debounce in the Slint (:137) — do not add one.
            onEdited: function (v) { QbzMyQbz.gridSearch(root.gridId, v) }
        }
        QbzSelect {
            anchors.verticalCenter: parent.verticalCenter
            // `sm` = 30px tall, r6, no outline, 12px label, 14px chevron, and
            // 40px narrower — so `menuWidth` is the .slint's RAW menu-width and
            // the control lands at 180 - 40 = 140 (QbzSelect.slint:88,102-105).
            sm: true
            menuWidth: 180
            options: [root.t("Position"), root.t("Name"), root.t("Items"),
                      root.t("Recently updated")]
            currentIndex: root.sortIndex
            onSelected: function (i) {
                QbzMyQbz.gridSetSort(root.gridId,
                    i === 1 ? "name" : i === 2 ? "items" : i === 3 ? "updated" : "position")
            }
        }
        // Kind filter — Collections only (CollectionsView.slint:159-174).
        QbzSelect {
            visible: !root.isMix
            anchors.verticalCenter: parent.verticalCenter
            sm: true
            // 200 - 40 = 160 effective (CollectionsView.slint:160).
            menuWidth: 200
            options: [root.t("All kinds"), root.t("Collections"),
                      root.t("Artist Collections")]
            currentIndex: root.kindIndex
            onSelected: function (i) {
                QbzMyQbz.gridSetKindFilter(
                    i === 1 ? "collection" : i === 2 ? "artist_collection" : "all")
            }
        }
        QbzSegToggle {
            anchors.verticalCenter: parent.verticalCenter
            // LIST first, grid second (MyQbzShared.slint:57-70).
            segments: [{ "id": "list", "icon": "list" },
                       { "id": "grid", "icon": "layout-grid" }]
            mode: root.viewMode
            // The ViewToggle metric set, NOT the control's Local defaults: a
            // 60x30 r6 surface-elevated well holding two `ToggleButton { sm }`
            // EDGE TO EDGE — 30x30 each, no gap, 15px glyph, active fill
            // surface-hover (MyQbzShared.slint:47-48 "active = surface-hover,
            // NEVER accent"), hover surface-elevated and tint text-primary /
            // text-muted (ToggleButton.slint:19-20,24-28,32-33,38). `segRadius`
            // is 6, not the reference's 0, because a QML clip is rectangular —
            // see the note in QbzSegToggle.qml.
            segWidth: 30
            segHeight: 30
            segRadius: 6
            segSpacing: 0
            glyphSize: 15
            activeFill: theme.surfaceHover
            hoverFill: theme.surfaceElevated
            activeTint: "textPrimary"
            idleTint: "muted"
            onSetMode: function (v) { QbzMyQbz.gridSetView(root.gridId, v) }
        }
    }

    // ========================== EMPTY STATE ==========================
    // 80 top / 16 spacing / 80 bottom, every child horizontally centred
    // (:91-118). Gated on `!loading` so the skeleton owns the first frames.
    Column {
        visible: !root.populated && !root.loading
        y: root.toolbarTop + 80
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: 16

        CollectionMosaic {
            anchors.horizontalCenter: parent.horizontalCenter
            size: 160
            // The EFFECTIVE kind override: it picks the glyph (and, for a
            // collection, would pick the 3x3 rule — moot at coverCount 0).
            emptyKind: root.isMix ? "mixtape" : "collection"
        }
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: root.isMix ? root.t("No mixtapes yet") : root.t("No collections yet")
            color: theme.textSecondary
            font.pixelSize: theme.fontSection
            font.weight: theme.weightSemibold
        }
        NewActionBtn {
            anchors.horizontalCenter: parent.horizontalCenter
            onClicked: QbzMyQbz.createOpen(root.kind)
        }
    }

    // ============================= BODY ==============================
    Item {
        id: body
        x: 32
        y: root.bodyTop
        width: Math.max(0, root.width - 64)
        height: Math.max(0, root.height - root.bodyTop)
        // Load-bearing: the GridView below is 20px WIDER than this host (see
        // the column-count note on it) and must be cut off here.
        clip: true

        // Search / filter produced nothing: the header "+ New" and the whole
        // toolbar STAY mounted and the body becomes an 80px band (:172-183).
        Text {
            visible: root.populated && root.items.length === 0 && !root.loading
            width: parent.width
            height: 80
            text: root.t("No results found")
            color: theme.textMuted
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
        }

        // First load, nothing to show yet.
        QbzSkeleton {
            anchors.fill: parent
            visible: root.gridPending
            variant: "cardGrid"
            phase: root.skelPhase
            cellW: root.cardW + root.gridGap
            cellH: root.cardH + root.gridGap
            cardW: root.cardW
            cardH: root.cardH
        }

        // ------------------------------ GRID --------------------------
        GridView {
            id: grid
            visible: root.viewMode === "grid" && root.items.length > 0
            // COLUMN-COUNT PARITY TRAP. Slint computes
            //   floor((W + gap) / (cardW + gap))
            // (MixtapesView.slint:187-188) — the TRAILING gap is not reserved.
            // GridView computes floor(W / cellWidth), which is one column
            // fewer at every exact boundary. So the view is `gridGap` wider
            // than its host and the host clips: the extra 20px is the gap that
            // is never painted.
            width: parent.width + root.gridGap
            height: parent.height
            cellWidth: root.cardW + root.gridGap
            cellHeight: root.cardH + root.gridGap
            cacheBuffer: 2 * (root.cardH + root.gridGap)
            reuseItems: true
            // Clears the player bar, the .slint's 100px page bottom padding.
            bottomMargin: 100
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            // Only the ACTIVE view carries a model — see `activeItems`.
            model: root.viewMode === "grid" ? root.activeItems : []

            delegate: MyQbzCard {
                required property var modelData
                width: root.cardW
                item: modelData
                onClicked: QbzMyQbz.openCard(modelData.id)
            }
        }

        // ------------------------------ LIST --------------------------
        ListView {
            id: list
            visible: root.viewMode === "list" && root.items.length > 0
            anchors.fill: parent
            // Ten rows of pitch 68 (64 + the 4px spacing), spec 01 §5.5.
            cacheBuffer: 68 * 10
            reuseItems: true
            spacing: 4
            bottomMargin: 100
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            model: root.viewMode === "list" ? root.activeItems : []

            delegate: MyQbzCard {
                required property var modelData
                width: list.width
                compact: true
                item: modelData
                onClicked: QbzMyQbz.openCard(modelData.id)
            }
        }

    }

    // Thin auto-hiding scrollbars (the ListScrollbar replica), one per body
    // view — LibraryView.qml:1443-1459's shape. Children of the ROOT, not of
    // `body`, so they sit OUTSIDE the 32px content padding (the .slint pins the
    // bar at `parent.width - width - 4`, MixtapesView.slint:213-221, and the
    // 32px right pad exists precisely so the grid never rides under it).
    //
    // VERTICALLY they track `body`, not the root. In Slint the bar and the
    // flickable are the same box, so the mapping is exact; here the body starts
    // 118px down. Spanning the full root height over-sized the thumb by ~18%
    // and mapped the track's top 118px — visually the header/toolbar band — to
    // content that is not there, so a click in the gutter beside the TOOLBAR
    // scrolled the grid.
    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: grid; scope: QbzShell.currentView }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: body.top
        anchors.bottom: body.bottom
        target: grid
        visible: grid.visible && grid.contentHeight > grid.height
    }
    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: list; scope: QbzShell.currentView }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: body.top
        anchors.bottom: body.bottom
        target: list
        visible: list.visible && list.contentHeight > list.height
    }

    // The "+ New" header/empty-state action (MyQbzShared.slint:24-42): a 28x28
    // surface-elevated CIRCLE with a plus glyph — never an accent fill, and
    // never the deleted blue PrimaryCta. Inline because both call sites are in
    // this file.
    component NewActionBtn: Rectangle {
        id: newBtn
        signal clicked()
        width: 28
        height: 28
        radius: 14
        color: newArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
        QbzIcon {
            anchors.centerIn: parent
            name: "plus"
            width: 15
            height: 15
            tintName: "textPrimary"
        }
        MouseArea {
            id: newArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: newBtn.clicked()
        }
    }
}
