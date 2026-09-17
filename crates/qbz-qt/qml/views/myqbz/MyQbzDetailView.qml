// MyQbzDetailView — the My QBZ Mixtape / Collection DETAIL route
// ("mixtapedetail"), the port of myqbz/MixtapeDetailView.slint:596-1272.
// Hero + sticky toolbar + body (list / grid / expanded) + the bulk bar.
// ONE document: `QbzMyQbz.detailJson` (spec 02 §5.2 `DetailDoc`).
//
// No per-view back button — the shell owns the global < > nav (.slint :613).
//
// --- PAGE FRAME, and the ONE structural deviation ------------------------
// The .slint puts hero + toolbar + body inside a single Flickable
// (:631-660: padding 16 / 32 / 100 / 32, spacing 0, top-aligned), so its hero
// scrolls away. Here the hero + toolbar are a FIXED header and only the body
// scrolls, which is the port's established shape for a WINDOWED body
// (`views/LibraryView.qml:1269`, `views/local/LocalTracksTab.qml:135`).
//
// Why, since 1:1 is the bar: an artist_collection holds 200+ items and every
// row carries a QualityBadgeFull, so the body MUST be a recycling
// ListView/GridView (spec 01 §15.1, non-negotiable). A recycling view cannot
// be content-sized inside an outer Flickable, and `ListView.header` is not
// available to the GRID arm either — the grid is hosted in a clipped Item and
// given a corrected width to reproduce the app album grid's column count
// (see the grid arm), which would offset and over-widen a header inside it. The
// paddings, the 32px hero→body gap and the 10px toolbar tail are all kept;
// the toolbar simply became literally sticky, which is what §7.2 calls it.
//
// --- PER-ROW COLLAPSIBLE ROWS (owner-authorised divergence) -------------
// In the DETAILS arm every album / playlist row carries a chevron that opens
// ITS OWN inline track list (`MyQbzDetailRow.qml`, col 0). NEITHER the .slint
// NOR the Tauri build had this — both render inline tracks only as a whole-list
// view MODE, explicitly "no chevron". The owner asked for the accordion anyway
// (2026-07-30); do not "restore" it toward either reference.
// It is ADDITIVE on machinery that already existed: Rust already published
// per-row `canExpand` / `inlineTracks` / `expandLoading` / `tracksLoaded`, so
// the accordion added exactly one thing — `rowOpen`, THE single notion of open.
// There is deliberately no second, QML-side definition of "is this row open".
// DEFAULT + PERSISTENCE, owner 2026-08-01: the details arm opens with every row
// CLOSED (the segment no longer force-opens anything) and the rows the user
// opens are remembered per collection across view switches and restarts
// (`collection_open_rows.json`). Rust gates the published `rowOpen` on the
// details arm itself, so the other arms still render no inline block.
// SCOPE, owner 2026-07-31: the chevron belongs to the DETAILS arm only — the
// plain LIST arm carries no affordance and is the reference's 8-column row
// again (`rowExpander` below). The accordion itself stays; only its reach
// changed, and `rowOpen` is still the one notion of open.
//
// --- THE MODEL IS PATCHED, NEVER REPLACED, AND PARSED ONCE --------------
// `QbzMyQbz` carries no signals: every Rust-side change arrives as a whole new
// `detailJson`. `refresh()` is the single entry point: ONE `JSON.parse` per
// republish (spec 01 §15.2), feeding both `doc` and `syncRows` from the same
// object so the two can never be a republish out of step.
// A naive `model: JSON.parse(...).items` would push a NEW
// array into the view on every `resolve_items` badge, every `ensure_expanded`
// batch and every checkbox — and `QQuickItemView::setModel()` resets contentY
// to 0 (and can SIGSEGV from inside its own teardown; the analysis is at
// `views/LibraryView.qml:174-221`). So `syncRows()` keeps the array identity
// whenever the DERIVATION did not change (same collection, same positions in
// the same order) and patches the row objects in place, bumping `patchRev` —
// the notifier a plain JS object mutation does not have. The delegates read
// `patchRev` (see MyQbzDetailRow.qml's header). The array is replaced only on
// a real re-derive (sort / filter / search / another collection), which is
// exactly when Slint also rebuilds and the scroll reset is correct.
//
// --- THE GRID ARM MOUNTS THE STANDARD CARD ------------------------------
// `cards/AlbumCard.qml`, not a My QBZ card. The port used to carry
// `MyQbzDetailCard.qml`, a 1:1 of the reference's local `DetailCard`
// (MixtapeDetailView.slint:495-594); the owner rejected it (2026-07-31,
// "quiero el de siempre con su overlay") and it is DELETED. Select mode is the
// CARD's (`selectMode`/`selected`/`selectToggled` — the port of the reference
// card's own `select-mode`, discover/AlbumCard.slint:83), because it has to
// hide the pin and the hover action row as well as draw the tick; only "Remove
// from collection" is host knowledge and it rides AlbumCard's
// `extraMenuEntries` tail.
//
// Reused, not rebuilt: rows/TrackRow.qml (inline tracks), cards/AlbumCard,
// controls/QualityBadgeFull, controls/CardMenu, controls/QbzContextMenu,
// controls/QbzSegToggle, controls/QbzMultiSelectBar, controls/QbzToolButton,
// controls/QbzLoadingDots, controls/QbzLineEdit, controls/QbzSelect,
// controls/QbzCircleAction, cards/CollectionMosaic, views/local/SelectCheck,
// theme/QbzScrollBar, theme/QbzSpinner.
//
// `controls/QbzLineEdit.qml` is NOT 1:1 with `primitives/ExpandableSearch.slint`
// — five pre-existing, app-wide divergences (leading glyph 13 vs 14 at `sm`,
// input + placeholder 12px vs 15, row spacing 7 vs 8, focus border `accent`
// vs `focus-ring`, X hover `surfaceHover` vs `surface-elevated`). Shipped as
// they are: fixing them changes the Local Library toolbars too (spec 01 §3.2).

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../rows"
import "../../theme"
import "../local"

Rectangle {
    id: root

    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    radius: 12

    QbzTheme { id: theme }

    function trs(s) { return QbzSession.tr(s, QbzSession.trRev) }

    // ---------------------- file-private components ----------------------

    /// One 10px column label of the 8-column header (.slint :1123-1129).
    component ColHead: Text {
        color: theme.textMuted
        font.pixelSize: 10
        font.weight: theme.weightSemibold
        font.letterSpacing: 1.2
        height: 34
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    /// A filter-popup section header (.slint :968-978): 20px, 10px semibold,
    /// letter-spacing 0.8, left-aligned.
    component SectionHead: Text {
        width: parent ? parent.width : 0
        height: 20
        color: theme.textMuted
        font.pixelSize: 10
        font.weight: theme.weightSemibold
        font.letterSpacing: 0.8
        verticalAlignment: Text.AlignVCenter
    }

    /// One filter row (.slint `MenuOption`, :47-77): 30px, r4, hover
    /// `surface-hover`, padding 8, spacing 8, a leading `SelectCheck` and a
    /// 13px label that goes accent when selected. The WHOLE row is clickable,
    /// not just the box.
    component MenuOption: Rectangle {
        id: mo
        property string label: ""
        property bool selected: false
        signal chosen()

        width: parent ? parent.width : 0
        height: 30
        radius: 4
        color: moArea.containsMouse ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 8
            SelectCheck {
                anchors.verticalCenter: parent.verticalCenter
                on: mo.selected
                onToggled: mo.chosen()
            }
            Text {
                width: Math.max(0, parent.width - 21)
                height: parent.height
                text: mo.label
                color: mo.selected ? theme.accent : theme.textPrimary
                font.pixelSize: theme.fontLegal
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: moArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: mo.chosen()
        }
    }

    // ------------------------------ document ------------------------------
    function parseDoc() {
        try {
            return JSON.parse(QbzMyQbz.detailJson)
        } catch (e) {
            return ({})
        }
    }
    /// ONE parse per republish (spec 01 §15.2). `refresh()` is the only writer
    /// and the only caller of `syncRows`, so `doc` and `rows` can never be a
    /// republish out of step — the earlier shape had a `readonly` binding here
    /// AND a second `parseDoc()` inside the change handler, i.e. two parses of
    /// a document that in expanded mode carries every item's inline track list.
    /// The initial value is still a BINDING so the very first frame is already
    /// correct (a plain `({})` would flash the not-found state for a frame);
    /// the first `refresh()` assignment replaces it.
    property var doc: root.parseDoc()

    readonly property bool loading: doc.loading === true
    readonly property bool found: doc.found === true
    // The collapsible My QBZ opt-in gates the sidebar hide/show toggle in the
    // hero overflow — the concept is meaningless while the flat entry is used.
    // GUARDED parse, the NavFlyout.qml:65-70 precedent.
    readonly property bool collapsibleMyqbzOn: {
        try { return JSON.parse(QbzBridge.settingsJson).collapsibleMyqbz === true }
        catch (e) { return false }
    }
    readonly property string kind: doc.kind || ""
    readonly property int itemCount: doc.itemCount || 0
    readonly property string viewMode: doc.viewMode || "list"
    readonly property bool selectMode: doc.selectMode === true
    readonly property int selectedCount: doc.selectedCount || 0

    // --- Escape hotkeys interface (2026-08-03 hotkeys-port §4.6), EXIT-ONLY.
    // The selection lives in RUST (myqbz_detail_qt.rs positions) and its bulk
    // bar carries no select-all arm, so this view deliberately exposes NO
    // selectAll() — the AppShell capability reporter reads that as
    // "Ctrl+A does not apply here". Escape-exit is available:
    // detailToggleSelectMode off ALSO clears the selection
    // (myqbz_detail_qt.rs:1902-1917).
    readonly property bool multiSelectOn: root.selectMode
    function exitMultiSelectMode() {
        if (root.selectMode) QbzMyQbz.detailToggleSelectMode()
    }

    readonly property int filterCount: doc.filterCount || 0
    readonly property bool hasAnyFilter: doc.hasAnyFilter === true

    /// Does the body carry the per-row accordion chevron?
    ///
    /// ONLY the DETAILS arm — the `rows-3` segment, the one whose whole point
    /// is showing each item's tracks (the segment id stays `expanded`, which is
    /// now only the arm's name: it no longer means "open all"). The owner asked
    /// for the chevron out of the plain LIST arm (2026-07-31); it is not a
    /// removal of the accordion, which stays his feature, and `rowOpen` stays
    /// the ONE notion of open with Rust as its only writer. Rust AND-s the
    /// published `rowOpen` with this same arm, so list mode cannot render an
    /// open row and no state is orphaned by hiding the affordance — the user's
    /// open set survives the switch instead of being cleared by it.
    readonly property bool rowExpander: root.viewMode === "expanded"

    // --------------------------- the row model -----------------------------
    property var rows: []
    property string rowsKey: ""
    property int patchRev: 0

    /// Identity of the current derivation: the collection plus the visible
    /// positions in order. `position` is the stable persisted key (spec 02
    /// §5.2), so this changes exactly when Slint would rebuild its model.
    function rowsSignature(d) {
        var it = d.items || []
        var s = (d.id || "") + "#" + it.length
        for (var i = 0; i < it.length; i++) s += "," + it[i].position
        return s
    }
    function syncRows(d) {
        var fresh = d.items || []
        var sig = root.rowsSignature(d)
        if (sig === root.rowsKey && root.rows.length === fresh.length) {
            for (var i = 0; i < fresh.length; i++) Object.assign(root.rows[i], fresh[i])
            root.patchRev = root.patchRev + 1
            return
        }
        root.rows = fresh
        root.rowsKey = sig
        root.patchRev = root.patchRev + 1
        // `ensureExpanded` is idempotent (cached rows are skipped). Rust fires
        // it from `detailSetViewMode("expanded")`; the OTHER entry is an
        // INITIAL OPEN whose restored view-pref is already `expanded`
        // (spec 01 §7.5), which no invokable covers. Read off `d`, not the
        // `doc` binding, which may not have re-evaluated yet.
        if (d.loading !== true && d.found === true && d.viewMode === "expanded"
            && root.expandedFor !== (d.id || "")) {
            root.expandedFor = d.id || ""
            QbzMyQbz.ensureExpanded()
        }
    }
    property string expandedFor: ""
    /// The single republish entry point: parse once, publish `doc`, then patch
    /// or replace `rows` off THAT same object. Never read `root.doc` here — the
    /// order between a binding on `doc` and this handler is not defined, which
    /// is why the parse happens locally and is handed to both.
    function refresh() {
        var d = root.parseDoc()
        root.doc = d
        root.syncRows(d)
    }
    /// Merge a PARTIAL row batch from `QbzMyQbz.detailRowsPatched`.
    ///
    /// Rust no longer republishes the whole document for a one-row change
    /// (resolved quality, an inline track list landing, a chevron toggle) — it
    /// sends `{ id, rows: [ { position, sourceItemId, ...changed } ] }`. The
    /// document in Rust stays authoritative; this is purely the cheap path to
    /// the SAME live row objects `syncRows` patches, so `patchRev` is bumped
    /// ONCE for the batch and every delegate binding re-evaluates exactly like
    /// after a republish.
    ///
    /// `position` is the join column because it is the stable persisted key
    /// (spec 02 §5.2) and it survives a re-derive; the array INDEX does not.
    /// A batch addressed at another collection, or at rows the current
    /// derivation filtered out, is silently dropped — the same guard Rust
    /// makes before building it.
    ///
    /// Being a signal, it is a DELTA: an instance mounted after it was emitted
    /// never saw it. That is what `QbzMyQbz.detailResync()` below is for.
    function applyRowPatch(patchJson) {
        var p
        try {
            p = JSON.parse(patchJson)
        } catch (e) {
            return
        }
        if (!p || !p.rows || p.rows.length === 0) return
        if ((p.id || "") !== (root.doc.id || "")) return
        var byPosition = ({})
        for (var i = 0; i < root.rows.length; i++)
            byPosition[root.rows[i].position] = root.rows[i]
        var touched = false
        for (var j = 0; j < p.rows.length; j++) {
            var patch = p.rows[j]
            var target = byPosition[patch.position]
            if (target === undefined) continue
            Object.assign(target, patch)
            touched = true
        }
        if (touched) root.patchRev = root.patchRev + 1
    }

    Connections {
        target: QbzMyQbz
        function onDetailJsonChanged() { root.refresh() }
        function onDetailRowsPatched(patchJson) { root.applyRowPatch(patchJson) }
    }
    // `doc`'s initial binding has already parsed at this point — reuse it
    // instead of parsing a third time.
    //
    // Then ask Rust to re-hand the document. This view is a `Loader` child
    // (`shell/AppShell.qml:192`), so nav-away DESTROYS it and nav-back builds a
    // NEW instance whose `detailJson` is whatever was last fully published —
    // every row patch since (resolved quality badges, loaded inline tracks,
    // which rows are open) went to the dead instance. One full publish per
    // mount closes that, and it is the only full publish the patch channel
    // still costs.
    Component.onCompleted: {
        root.syncRows(root.doc)
        QbzMyQbz.detailResync()
    }

    // --- ONE loading-dots clock for the whole view (spec 01 §7.5 / §15.5) --
    // Slint animates each row's LoadingDots off `animation-tick`; a windowed
    // list of hundreds of rows must not. Gating rule, copied from
    // `views/PlaylistView.qml:156-179`: freeze when not visible or the window
    // is minimized / hidden, NEVER on lost focus (a tiling desktop keeps
    // windows visible and unfocused).
    property int dotPhase: 0
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    readonly property bool anyResolving: {
        if (root.patchRev < 0) return false      // `patchRev` dependency
        var r = root.rows
        for (var i = 0; i < r.length; i++) {
            if (r[i].qualityResolving === true && (r[i].qualityTier || "") === "") return true
        }
        return false
    }
    Timer {
        interval: 300
        repeat: true
        running: root.anyResolving && root.visible && root.windowShowing
        onTriggered: root.dotPhase = (root.dotPhase + 1) % 3
    }

    // ============================== LOADING ==============================
    QbzSpinner {
        visible: root.loading
        anchors.centerIn: parent
        size: 36
    }

    // ============================= NOT FOUND =============================
    Text {
        visible: !root.loading && !root.found
        anchors.centerIn: parent
        text: root.trs("Not found")
        color: theme.textMuted
        font.pixelSize: theme.fontLink
    }

    // =============================== LOADED ==============================
    Item {
        id: page
        visible: !root.loading && root.found
        anchors.fill: parent

        // --------------------------- fixed header -------------------------
        Column {
            id: headerCol
            x: 32
            y: 16
            width: parent.width - 64
            spacing: 0

            // --- Hero (.slint :663-861) -------------------------------
            Row {
                width: parent.width
                spacing: 32

                // Rust pre-downscales the hero cells (3 cols -> _50, else
                // _150 — `myqbz_detail.rs:375`), so the mosaic only lays out.
                CollectionMosaic {
                    anchors.bottom: parent.bottom
                    size: 186
                    kind: root.kind
                    itemCount: root.itemCount
                    hasCustomCover: root.doc.hasCustomCover === true
                    customCoverPath: root.doc.customCoverPath || ""
                    coverCount: root.doc.coverCount || 0
                    urls: root.doc.heroCellUrls || []
                    paths: root.doc.heroCellPaths || []
                }

                Column {
                    width: Math.max(0, parent.width - 186 - 32)
                    anchors.bottom: parent.bottom
                    spacing: 8

                    Text {
                        text: root.doc.kindLabel || ""
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.2
                    }
                    // 18px, NOT 25 — .slint :709-713 calls the old 25px a
                    // homologation bug against the other detail heroes.
                    Text {
                        width: parent.width
                        text: root.doc.name || ""
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightBold
                        wrapMode: Text.WordWrap
                    }
                    Text {
                        visible: (root.doc.description || "") !== ""
                        width: Math.min(root.width - 280, 720)
                        text: root.doc.description || ""
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLink
                        wrapMode: Text.WordWrap
                    }
                    Text {
                        text: root.doc.meta || ""
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                    Item { width: 1; height: 4 }

                    // EXACTLY four controls (.slint :735-859).
                    Row {
                        spacing: 8
                        QbzCircleAction {
                            name: "play-fill"
                            primary: true
                            btnEnabled: root.itemCount > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzMyQbz.playAll()
                        }
                        QbzCircleAction {
                            name: "shuffle"
                            btnEnabled: root.itemCount > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzMyQbz.shuffle()
                        }
                        // ASSET GAP: `turntable` is not baked yet (spec 01 §14).
                        QbzCircleAction {
                            name: "turntable"
                            btnEnabled: root.itemCount > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzMyQbz.djMix()
                        }
                        Item {
                            id: overflowAnchor
                            width: 32
                            height: 32
                            anchors.verticalCenter: parent.verticalCenter
                            // Self-sizing (32px, the non-primary arm), so the
                            // anchor Item is exactly its box.
                            QbzCircleAction {
                                name: "ellipsis"
                                // Slint anchors this 220px panel `x: 0,
                                // y: 36` on the 32px disc (:787-788), so it
                                // hugs the trigger's LEFT edge —
                                // `openBelowRight` would throw it 188px left.
                                onClicked: heroMenu.openBelowLeft(overflowAnchor)
                            }
                        }
                    }
                }
            }

            Item { width: 1; height: 32 }

            // --- Empty collection (.slint :866-877) — the hero stays, the
            // toolbar and the body do not (:880) ----------------------------
            Item {
                visible: root.itemCount === 0
                width: parent.width
                height: visible ? 80 : 0
                Text {
                    anchors.centerIn: parent
                    text: root.trs("No items yet. Add albums, tracks, or playlists from their detail pages.")
                    color: theme.textMuted
                    font.pixelSize: 13
                }
            }

            // --- Sticky toolbar (.slint :885-1086) ------------------------
            Item {
                visible: root.itemCount > 0
                width: parent.width
                height: visible ? 30 : 0

                Row {
                    anchors.right: parent.right
                    height: parent.height
                    spacing: 8

                    QbzLineEdit {
                        anchors.verticalCenter: parent.verticalCenter
                        searchMode: true
                        expandable: true
                        sm: true
                        elevated: false
                        openWidth: 196
                        placeholder: root.trs("Search")
                        text: root.doc.search || ""
                        onEdited: function (v) { QbzMyQbz.detailSearch(v) }
                    }

                    QbzSelect {
                        anchors.verticalCenter: parent.verticalCenter
                        sm: true
                        menuWidth: 170
                        options: [root.trs("Position"), root.trs("Name"),
                                  root.trs("Year"), root.trs("Tracks")]
                        currentIndex: (root.doc.sort || "position") === "name" ? 1
                            : (root.doc.sort || "position") === "year" ? 2
                            : (root.doc.sort || "position") === "tracks" ? 3 : 0
                        onSelected: function (i) {
                            QbzMyQbz.detailSetSort(i === 1 ? "name" : i === 2 ? "year"
                                : i === 3 ? "tracks" : "position")
                        }
                    }

                    // Filter — the LABELED arm, so `fillActive` (accent FILL).
                    QbzToolButton {
                        id: filterBtn
                        anchors.verticalCenter: parent.verticalCenter
                        name: "list-filter"
                        label: root.filterCount > 0
                            ? (root.filterCount === 1 ? root.trs("1 filter")
                               : root.trs("{} filters").replace("{}", root.filterCount))
                            : root.trs("Filter")
                        active: root.filterCount > 0
                        fillActive: true
                        onClicked: filterMenu.openBelowLeft(filterBtn)
                    }

                    // Reset — icon-only, so accent BORDER, never a fill.
                    QbzToolButton {
                        visible: root.hasAnyFilter
                        anchors.verticalCenter: parent.verticalCenter
                        name: "rotate-ccw"
                        fillActive: false
                        onClicked: QbzMyQbz.detailResetFilters()
                    }

                    QbzToolButton {
                        anchors.verticalCenter: parent.verticalCenter
                        name: "square-check-big"
                        active: root.selectMode
                        fillActive: false
                        onClicked: QbzMyQbz.detailToggleSelectMode()
                    }

                    QbzSegToggle {
                        anchors.verticalCenter: parent.verticalCenter
                        width: 90
                        mode: root.viewMode
                        segments: [{ "id": "list", "icon": "list" },
                                   { "id": "grid", "icon": "layout-grid" },
                                   { "id": "expanded", "icon": "rows-3" }]
                        // The 3-segment ViewToggle metric set, NOT the control's
                        // Local defaults: MixtapeDetailView.slint:1058-1084 is a
                        // 90x30 r6 surface-elevated well holding THREE
                        // `ToggleButton { sm }` edge to edge — 30x30 each, no
                        // gap, 15px glyph, active fill surface-hover (never
                        // accent), hover surface-elevated, tint text-primary /
                        // text-muted (ToggleButton.slint:19-20,24-28,32-33,38).
                        // `segRadius` 6 rather than the reference's 0 because a
                        // QML clip is rectangular — see QbzSegToggle.qml.
                        segWidth: 30
                        segHeight: 30
                        segRadius: 6
                        segSpacing: 0
                        glyphSize: 15
                        activeFill: theme.surfaceHover
                        hoverFill: theme.surfaceElevated
                        activeTint: "textPrimary"
                        idleTint: "muted"
                        onSetMode: function (m) { QbzMyQbz.detailSetViewMode(m) }
                    }
                }
            }

            Item { visible: root.itemCount > 0; width: 1; height: visible ? 10 : 0 }
        }

        // ----------------------------- the body ----------------------------
        Item {
            id: body
            visible: root.itemCount > 0
            anchors.top: headerCol.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.leftMargin: 32
            anchors.rightMargin: 32
            // NOT `bottomMargin: 100`. The .slint's 100px is `padding-bottom`
            // INSIDE the Flickable (:657) — scroll RUNWAY past the last row,
            // clearing the player bar. As an anchor margin here it shrank the
            // ListView/GridView VIEWPORT instead: the last row was clipped
            // mid-height and ~85px of dead band sat under it, unusable and
            // unscrollable, on every collection. The runway now lives on the
            // flickables themselves (`bottomMargin: 100` below), which is what
            // the sibling `MyQbzGridView.qml:336,358` already does right.
            anchors.bottomMargin: 0

            // ================= BODY: LIST / EXPANDED =================
            Column {
                id: listHost
                anchors.fill: parent
                visible: root.viewMode !== "grid"
                spacing: 0

                Rectangle { width: parent.width; height: 1; color: theme.surfaceElevated }

                // Column header (.slint :1114-1135) — the 8th cell is a bare
                // 40px spacer, not a label (:1130).
                Item {
                    width: parent.width
                    height: 34
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 12
                        // The accordion chevron's leading cell (see
                        // MyQbzDetailRow.qml). Unlabelled — a header for a
                        // disclosure control would be noise — but while the
                        // details arm mounts it, it MUST be here at exactly the
                        // row's 24px or the eight reference columns below drift
                        // 36px out of the header. `visible: false` drops the
                        // cell AND its 12px gap, exactly like the row's.
                        Item { visible: root.rowExpander; width: 24; height: 1 }
                        ColHead { width: 40; text: "#"; horizontalAlignment: Text.AlignHCenter }
                        ColHead {
                            // Paired with MyQbzDetailRow's `titleColWidth`:
                            // 24 padding + 592 fixed cells + 84 gaps = 700,
                            // plus the chevron's 24 + 12 while it is mounted.
                            width: Math.max(0,
                                listHost.width - (root.rowExpander ? 736 : 700))
                            text: root.trs("Item")
                        }
                        ColHead { width: 140; text: root.trs("Type") }
                        ColHead { width: 80; text: root.trs("Source") }
                        ColHead { width: 160; text: root.trs("Quality") }
                        ColHead { width: 72; text: root.trs("Tracks"); horizontalAlignment: Text.AlignRight }
                        ColHead { width: 60; text: root.trs("Year"); horizontalAlignment: Text.AlignRight }
                        Item { width: 40; height: 1 }
                    }
                }

                Rectangle { width: parent.width; height: 1; color: theme.surfaceElevated }

                Item {
                    width: parent.width
                    height: Math.max(0, listHost.height - 36)

                    ListView {
                        id: itemList
                        anchors.fill: parent
                        clip: true
                        boundsBehavior: Flickable.StopAtBounds
                        cacheBuffer: 500
                        reuseItems: true
                        // The .slint's 100px page bottom padding, as scroll
                        // runway past the last row (clears the player bar).
                        bottomMargin: 100
                        model: root.rows

                        // EXPANDED mode is this SAME recycled list with the
                        // inline block unhidden — a nested ListView inside a
                        // recycled delegate cannot size itself (spec 02 §9.1),
                        // and switching the delegate per mode would reset
                        // contentY. The per-row ACCORDION rides the identical
                        // machinery: the outer list stays recycled and windowed
                        // whatever is open, and an open row's tracks stay a
                        // bounded `Repeater` inside a `Column` delegate, which
                        // is content-sized and therefore safe. Do not "upgrade"
                        // that Repeater to a nested ListView.
                        delegate: Column {
                            id: rowCell
                            required property var modelData
                            required property int index
                            width: itemList.width
                            spacing: 0

                            /// THE LIVE ROW, and the reason every read below goes
                            /// through it instead of `modelData`.
                            ///
                            /// `model: root.rows` is a JavaScript array, and
                            /// QQmlDelegateModel SNAPSHOTS such a model — the
                            /// delegate's `modelData` is the engine's own copy of
                            /// the element, not the object in `root.rows`. So
                            /// `syncRows`' in-place `Object.assign` patch (the
                            /// whole point of which is to update a row WITHOUT
                            /// reassigning the array and throwing the user's
                            /// scroll position to the top) mutated objects the
                            /// delegates could never see. Bumping `patchRev` then
                            /// dutifully re-evaluated every binding — against the
                            /// stale copy.
                            ///
                            /// Measured: 34 items resolved Rust-side with correct
                            /// tiers and 34 publishes, 71 QML refreshes, and the
                            /// Quality column stayed on its skeleton forever.
                            /// Expanded mode rendered nothing for the same reason
                            /// (`inlineTracks` is patched in exactly this way).
                            ///
                            /// Indexing the live array closes it: `patchRev` is
                            /// the dependency, `root.rows[index]` is the object
                            /// Rust actually patched. `modelData` stays the
                            /// fallback for the frame where the array and the
                            /// delegate model are momentarily out of step.
                            readonly property var live: root.patchRev >= 0
                                ? (root.rows[index] || modelData) : modelData

                            /// ONE notion of open, and it is Rust's `rowOpen`.
                            ///
                            /// It used to be `viewMode === "expanded" &&
                            /// canExpand`, i.e. a second, QML-side definition
                            /// of the same thing. With the accordion there are
                            /// two writers (the chevron and the segment) and
                            /// two definitions would disagree the first time a
                            /// row was closed inside expanded mode — so the
                            /// view mode is not read here at all. Rust publishes
                            /// `rowOpen` only for a row the USER opened while
                            /// the details arm is up; `canExpand` is implied,
                            /// because a non-expandable row is never marked
                            /// open.
                            ///
                            /// Through `live`, like every other row read — see
                            /// the note above.
                            readonly property bool expanded: root.patchRev >= 0
                                && rowCell.live.rowOpen === true
                            readonly property var inlineTracks: (root.patchRev >= 0)
                                ? (rowCell.live.inlineTracks || []) : []
                            readonly property bool expandLoading: root.patchRev >= 0
                                && rowCell.live.expandLoading === true
                            readonly property bool tracksLoaded: root.patchRev >= 0
                                && rowCell.live.tracksLoaded === true

                            MyQbzDetailRow {
                                width: parent.width
                                item: rowCell.live
                                ordinal: rowCell.index + 1
                                selectMode: root.selectMode
                                showExpander: root.rowExpander
                                dotPhase: root.dotPhase
                                rev: root.patchRev
                                detailKind: root.kind
                            }

                            // Inline tracks (.slint :1151-1218): padding
                            // 52 / 12 / 4 / 8, spacing 0.
                            //
                            // The reference's 52 is `12 padding + 40 ordinal
                            // cell`, i.e. exactly where the row's artwork
                            // column starts; the chevron cell adds `24 + 12` in
                            // front of the ordinal, so the same alignment is 88
                            // whenever the chevron is mounted. The right inset
                            // stays 12 either way (width - x - 12). Keep it tied
                            // to the row's template — off by one cell and the
                            // inline block stops hanging under its own title.
                            //
                            // Only the details arm can have an open row today
                            // (see `rowExpander`), so the 88 branch is the live
                            // one; the 52 keeps the two definitions honest.
                            Item {
                                visible: rowCell.expanded
                                width: parent.width
                                height: visible ? inlineCol.height + 12 : 0
                                Column {
                                    id: inlineCol
                                    x: root.rowExpander ? 88 : 52
                                    y: 4
                                    width: Math.max(0, parent.width - inlineCol.x - 12)
                                    spacing: 0

                                    Item {
                                        visible: rowCell.expandLoading
                                            && rowCell.inlineTracks.length === 0
                                        width: parent.width
                                        height: visible ? 32 : 0
                                        QbzSpinner { anchors.centerIn: parent; size: 16 }
                                    }
                                    Item {
                                        visible: !rowCell.expandLoading && rowCell.tracksLoaded
                                            && rowCell.inlineTracks.length === 0
                                        width: parent.width
                                        height: visible ? 32 : 0
                                        Text {
                                            anchors.centerIn: parent
                                            text: root.trs("No results found")
                                            color: theme.textMuted
                                            font.pixelSize: 12
                                        }
                                    }
                                    // Sub-header for the expanded track list.
                                    // The item's OWN columns (Item/Type/Source/
                                    // Quality/Tracks/Year) name the collection
                                    // rows, not these child tracks — so a small,
                                    // muted, column-aligned label band sits over
                                    // the inline tracks instead. Reuses the
                                    // shared TrackListHeader with the SAME arms
                                    // the inline TrackRow sets (menu only), so
                                    // "#", "Title", "Duration", "Quality" land on
                                    // their columns by construction. Shown only
                                    // once the tracks have loaded (never over the
                                    // spinner or the empty state).
                                    Item {
                                        visible: !rowCell.expandLoading
                                            && rowCell.tracksLoaded
                                            && rowCell.inlineTracks.length > 0
                                        width: parent.width
                                        height: visible ? 22 : 0
                                        TrackListHeader {
                                            anchors.left: parent.left
                                            anchors.right: parent.right
                                            anchors.verticalCenter: parent.verticalCenter
                                            showArtwork: false
                                            showSource: false
                                            showAlbum: false
                                            showFavorite: false
                                            showDownload: false
                                            showMenu: true
                                            showReorder: false
                                        }
                                        // Discreet hairline under the labels.
                                        Rectangle {
                                            anchors.left: parent.left
                                            anchors.right: parent.right
                                            anchors.bottom: parent.bottom
                                            height: 1
                                            color: theme.surfaceElevated
                                        }
                                    }
                                    Repeater {
                                        model: rowCell.inlineTracks
                                        delegate: TrackRow {
                                            required property var modelData
                                            required property int index
                                            width: inlineCol.width
                                            item: modelData
                                            // `number` is a DELEGATE property,
                                            // never an item field (spec 02 §5.2).
                                            number: index + 1
                                            showArtwork: false
                                            showFavorite: false
                                            showDownload: false
                                            showMenu: true
                                            // `showAlbum` is LEFT AT ITS DEFAULT
                                            // (false) — TrackRow.slint:43 does the
                                            // same and MixtapeDetailView does not
                                            // override it, so there is no album
                                            // LINK on these rows.
                                            draggable: false
                                            menuShowRemove: false
                                            // `primitives/TrackRow.slint:125`
                                            // zebras UNCONDITIONALLY on odd
                                            // `index` (there is no `zebra`
                                            // property to turn it off), so the
                                            // inline block IS striped in the
                                            // reference. Qt gates it and
                                            // defaults false; `number % 2 === 0`
                                            // with `number = index + 1` stripes
                                            // exactly the same rows.
                                            zebra: true
                                            // Go-to must route through the PARENT
                                            // item, not the track's own ids —
                                            // spec 01 §7.5 / OQ#9.
                                            routeGoToExternally: true
                                            onPlayRequested: QbzMyQbz.inlineTrackAction(
                                                rowCell.live.sourceItemId || "",
                                                modelData.id || "", "play")
                                            // `next|later|queue` -> the wire's
                                            // `play-next|play-later|queue`.
                                            // Unmapped is a SILENT no-op.
                                            onEnqueueRequested: function (mode) {
                                                QbzMyQbz.inlineTrackAction(
                                                    rowCell.live.sourceItemId || "",
                                                    modelData.id || "",
                                                    mode === "next" ? "play-next"
                                                        : mode === "later" ? "play-later" : "queue")
                                            }
                                            onGoToRequested: function (goKind) {
                                                if (goKind === "artist")
                                                    QbzMyQbz.openArtist(
                                                        rowCell.live.source || "",
                                                        modelData.artist || "",
                                                        modelData.artistId || "")
                                                else
                                                    QbzMyQbz.inlineTrackAction(
                                                        rowCell.live.sourceItemId || "",
                                                        modelData.id || "", "go-to-album")
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                }
            }

            // ======================= BODY: GRID =======================
            // THE STANDARD `cards/AlbumCard.qml`, at its app-wide size, with
            // its own hover overlay and its own ⋯ / right-click menu.
            //
            // This REPLACES the port's `MyQbzDetailCard.qml` (deleted), which
            // was a faithful 1:1 of the reference's local `DetailCard`
            // (MixtapeDetailView.slint:495-594): a 150px-ish hand-rolled tile
            // with no overlay buttons, no quality badge, no source badge and no
            // menu. The owner ruled against that card (2026-07-31): "quiero el
            // de siempre con su overlay". So this arm is a DELIBERATE, OWNER-
            // ORDERED divergence from the .slint, and the .slint's `auto-fill
            // minmax(150,1fr)` / gap-20 arithmetic went with it.
            //
            // Geometry is now the one every other album grid in the app uses
            // (`views/AlbumCollection.qml:44-46`): a fixed 200x246 card in a
            // 200x266 cell with a 24px gap. AlbumCard hardcodes its 200px
            // width, so this grid cannot be fluid — matching the app is both
            // the correct and the only option.
            Item {
                id: gridHost
                anchors.fill: parent
                visible: root.viewMode === "grid"
                clip: true

                readonly property int cardWidth: 200
                readonly property int cardHeight: 266
                readonly property int cardGap: 24

                GridView {
                    id: itemGrid
                    x: 12
                    y: 0
                    // The app grid takes `floor((avail + gap) / (card + gap))`
                    // columns over `avail = width - 24` (the 12px content inset
                    // on both sides); GridView takes `floor(width / cellWidth)`.
                    // Since the gap and the two insets are both 24, handing it
                    // the UNINSET width makes the two agree exactly — the same
                    // correction the old 150/20 arm made with `+ 20 - 24`.
                    width: Math.max(0, gridHost.width)
                    height: gridHost.height
                    cellWidth: gridHost.cardWidth + gridHost.cardGap
                    cellHeight: gridHost.cardHeight + gridHost.cardGap
                    cacheBuffer: 500
                    reuseItems: true
                    boundsBehavior: Flickable.StopAtBounds
                    // Same runway as the list arm — see `body`'s note.
                    bottomMargin: 100
                    model: root.rows

                    delegate: Item {
                        id: gcell
                        required property var modelData
                        required property int index
                        width: gridHost.cardWidth
                        height: gridHost.cardHeight

                        // Live row, not the delegate model's snapshot — see the
                        // list delegate's `live` note. AlbumCard has no `rev`
                        // property, so EVERY leaf below carries the
                        // `root.patchRev >= 0` guard itself: that is what makes
                        // `patchRev` a captured dependency of the binding, and
                        // an in-place `Object.assign` patch fires no notifier of
                        // its own. Always true; never "simplify" it away.
                        readonly property var live: root.patchRev >= 0
                            ? (root.rows[index] || modelData) : modelData
                        readonly property int cPosition: root.patchRev >= 0 ? (gcell.live.position || 0) : 0
                        readonly property string cItemId: root.patchRev >= 0 ? (gcell.live.sourceItemId || "") : ""
                        readonly property string cItemType: root.patchRev >= 0 ? (gcell.live.itemType || "") : ""
                        readonly property string cSource: root.patchRev >= 0 ? (gcell.live.source || "") : ""
                        readonly property string cTitle: root.patchRev >= 0 ? (gcell.live.title || "") : ""
                        readonly property string cSubtitle: root.patchRev >= 0 ? (gcell.live.subtitle || "") : ""
                        readonly property string cYear: root.patchRev >= 0 ? (gcell.live.yearText || "") : ""
                        readonly property string cQuality: root.patchRev >= 0 ? (gcell.live.qualityTier || "") : ""
                        readonly property string cDetail: root.patchRev >= 0 ? (gcell.live.qualityDetail || "") : ""
                        /// The GRID rung of the cover, NOT the list row's.
                        /// `artPath` is the 50px thumbnail every list row draws
                        /// at 40px; this cell's artwork well is 200px, so
                        /// reading `artPath` here upscaled it 4x — the owner's
                        /// "la calidad de la portada es menor que la del
                        /// albumcard". Rust publishes both rungs per row
                        /// (`myqbz_detail_qt::DetailRow::art_url_large`) so the
                        /// list/grid toggle stays instant: re-deriving one rung
                        /// per view mode would republish the document, and a
                        /// republish resets the view's scroll offset.
                        readonly property string cArt: root.patchRev >= 0 ? (gcell.live.artPathLarge || "") : ""
                        /// The REMOTE url of that same rung — AlbumCard's
                        /// `artworkUrl`, i.e. the pin payload and the block
                        /// snapshot, never the `file://` cache path.
                        readonly property string cArtUrl: root.patchRev >= 0 ? (gcell.live.artUrlLarge || "") : ""
                        readonly property string cSourceKind: root.patchRev >= 0 ? (gcell.live.sourceKind || "") : ""
                        readonly property bool cSelected: root.patchRev >= 0 && gcell.live.selected === true
                        readonly property bool cFavorite: root.patchRev >= 0 && gcell.live.isFavorite === true
                        readonly property string cCardIdentity:
                            gcell.cItemType + ":" + gcell.cItemId
                        /// Is THIS cell's `sourceItemId` a Qobuz catalog album
                        /// id? The per-item gate for the card's catalog-only
                        /// affordances (heart and "Block this album"), and the
                        /// QML twin of `myqbz_detail_qt::is_catalog_album` — the
                        /// two must agree for the favorite seed. Pinning has a
                        /// separate string-key gate below so local albums can
                        /// participate too.
                        ///
                        /// The STORED source (`source`, "qobuz" | "local"), not
                        /// the resolved `sourceKind`: the stored word is what
                        /// makes the id a catalog id. And the TYPE, because a
                        /// Qobuz track item carries a track id and a Qobuz
                        /// playlist item a playlist id — hearting either as
                        /// "album" would hit a different entity.
                        readonly property bool cCatalogAlbum: gcell.cSource === "qobuz"
                            && gcell.cItemType === "album" && gcell.cItemId !== ""
                        /// track → music, playlist → list-music, else disc —
                        /// the reference card's empty-well glyph (.slint
                        /// :522-526), carried over on AlbumCard's opt-in
                        /// `placeholderIcon`.
                        readonly property string cGlyph: gcell.cItemType === "track" ? "music"
                            : (gcell.cItemType === "playlist" ? "list-music" : "disc")

                        /// RE-ESTABLISH the two self-mutating bindings whenever
                        /// this delegate changes row.
                        ///
                        /// `AlbumCard` writes its OWN `isFavorite` / `isPinned`
                        /// — optimistically in `toggleFavorite()` /
                        /// `togglePin()`, and again from its
                        /// `libraryFavoriteChanged` / `pinChanged` Connections —
                        /// and a QML property assignment DESTROYS the binding
                        /// that fed it. On a non-recycling host that is
                        /// harmless: the delegate dies with its row. This grid
                        /// sets `reuseItems: true`, so the very same AlbumCard
                        /// instance is handed the NEXT row, with a dead
                        /// `isFavorite` binding — it would keep the previous
                        /// album's filled heart, and clicking it would toggle
                        /// the new album the wrong way (glyph says "remove", the
                        /// call adds). Same for the pin badge.
                        ///
                        /// This is the idiom the port already uses for exactly
                        /// this hazard on the cards that own an `item`
                        /// (`cards/TrackCard.qml:49`, `cards/PlaylistCard.qml:117`,
                        /// `rows/TrackRow.qml:233`, `views/library/FeedListRow.qml:39`);
                        /// AlbumCard takes flat properties instead of an `item`,
                        /// so the restore belongs to the recycling HOST. Keyed
                        /// on the type + id pair, which is derived from
                        /// `index` and therefore changes on every reuse.
                        onCCardIdentityChanged: gcell.rebindCardState()
                        function rebindCardState() {
                            // The change signal can fire while the delegate is
                            // still being built, before the child below exists;
                            // at that point nothing has broken a binding yet, so
                            // skipping is the correct answer, not a workaround.
                            if (!gcard)
                                return
                            gcard.isFavorite = Qt.binding(function () { return gcell.cFavorite })
                            gcard.isPinned = Qt.binding(function () {
                                return gcell.cItemType === "album"
                                    ? QbzLibrary.pinState("album", gcell.cItemId)
                                    : false
                            })
                        }

                        AlbumCard {
                            id: gcard
                            // localMode = ROUTING: open / play / enqueue go to
                            // the host signals below, because a My QBZ item can
                            // be a Plex or local album, a track or a playlist
                            // and only `QbzMyQbz` knows how to open and play
                            // one. TRUE for every cell, no exceptions.
                            localMode: true
                            // …but the catalog-only affordances are decided PER
                            // ITEM, which is the whole point of the split (see
                            // AlbumCard's `catalogAffordances`). A My QBZ
                            // container is multi-source per ROW: a Qobuz album
                            // cell's id IS a catalog album id and its heart /
                            // pin / block are perfectly valid. Local/server
                            // albums below opt back into pin independently;
                            // tracks/playlists still keep album affordances
                            // absent rather than dead.
                            catalogAffordances: gcell.cCatalogAlbum
                            // The heterogeneous grid also uses AlbumCard for
                            // tracks/playlists. Quick View belongs only to its
                            // real album rows, including local/server albums.
                            quickViewAffordance: gcell.cItemType === "album"
                            pinAffordance: gcell.cItemType === "album"
                            // Seeded by Rust behind the SAME predicate, then
                            // settled by the card itself on
                            // `QbzLibrary.libraryFavoriteChanged` / `pinChanged`
                            // — so a heart flipped here, on the album page or
                            // on a Home rail agrees everywhere, and no toggle
                            // republishes this document.
                            isFavorite: gcell.cFavorite
                            isPinned: pinAffordance
                                ? QbzLibrary.pinState("album", gcell.cItemId) : false
                            albumId: gcell.cItemId
                            title: gcell.cTitle
                            artist: gcell.cSubtitle
                            // No artist link: the reference grid card's subtitle
                            // is a plain Text (.slint :578-583), and a My QBZ
                            // subtitle can be a Plex/local artist whose id would
                            // route QbzArtist.openArtist into a Qobuz lookup.
                            artistId: ""
                            genre: ""
                            year: gcell.cYear
                            qualityTier: gcell.cQuality
                            qualityDetail: gcell.cDetail
                            artSource: gcell.cArt
                            // The pin payload / block snapshot url (see
                            // `cArtUrl`). Without it a pinned My QBZ album
                            // lands in the Pinned rail as a placeholder.
                            artworkUrl: gcell.cArtUrl
                            pinArtworkUrl: artworkUrl !== "" ? artworkUrl : artSource
                            // Badge ON: a My QBZ container is multi-source BY
                            // DEFINITION and the sibling LIST arm gives every
                            // row a Source column (col 4, the same
                            // `sourceKind` through the same `SourceIcon`), so
                            // the grid arm hiding it would be the odd one out.
                            // All four kinds `sourceKind` can carry —
                            // qobuz / plex / local / offline — draw a mark;
                            // only "" (nothing resolved yet) draws none, which
                            // is AlbumCard's `source !== ""` gate.
                            source: gcell.cSourceKind
                            showSourceBadge: true
                            placeholderIcon: gcell.cGlyph
                            // The first menu entry's NOUN. `open_item` sends
                            // "album" AND "track" to the album page and only
                            // "playlist" elsewhere (myqbz_detail_qt.rs:1893-1901,
                            // 1:1 with qbz/src/main.rs:6275-6288), so "Open
                            // album" is accurate on an album and on a track and
                            // wrong on exactly one arm. A label, not an action:
                            // the routing below is unchanged.
                            openLabel: gcell.cItemType === "playlist"
                                ? root.trs("Open playlist") : ""
                            // The container action the card cannot know about.
                            // Same msgid, same icon and same destructive styling
                            // as the list row's last entry
                            // (MyQbzDetailRow.qml:457-458 == .slint :481-486),
                            // after the same separator. It applies to a
                            // Collection, a Mixtape and an artist Collection
                            // alike: `remove_item` removes by POSITION from
                            // whatever container is open
                            // (myqbz_edit_qt.rs:360-362), so there is no arm
                            // here that could render and no-op.
                            extraMenuEntries: [
                                { "sep": true },
                                { "label": QbzSession.tr("Remove from collection", QbzSession.trRev),
                                  "icon": "trash-2", "action": "remove", "danger": true },
                            ]
                            onExtraMenuAction: function (a) {
                                if (a === "remove") QbzMyQbz.removeItem(gcell.cPosition)
                            }
                            // SELECT MODE is the card's, not this host's
                            // (discover/AlbumCard.slint:83 declares
                            // `select-mode` and :179-239 hides the pin and the
                            // whole hover action row behind it). Overlaying a
                            // tick from here — the shape
                            // `views/local/LocalAlbumCollection.qml` used until
                            // 2026-08-01 — can only add the indicator: it leaves
                            // the hover
                            // PLAY button live under the selection, so hovering
                            // a card you meant to tick and hitting play starts
                            // the album instead. The owner's own instruction
                            // for this round was to extend AlbumCard with the
                            // selection checkbox rather than bolt one on.
                            selectMode: root.selectMode
                            selected: gcell.cSelected
                            onSelectToggled: function (mods) {
                                QbzMyQbz.detailToggleItemSelect(gcell.cPosition,
                                    (mods & Qt.ShiftModifier) !== 0)
                            }
                            // Card body click, non-select mode only (the card
                            // routes a select-mode click to `selectToggled`).
                            onOpenRequested: QbzMyQbz.openItem(gcell.cSource,
                                gcell.cItemType, gcell.cItemId)
                            onPlayRequested: QbzMyQbz.playItem(gcell.cItemId)
                            // AlbumCard's `next|later|queue` -> the wire's
                            // `play-next|play-later|add-to-queue`
                            // (myqbz_play_qt.rs:84-91), the same mapping the
                            // row menu hands `itemAction`.
                            onEnqueueRequested: function (mode) {
                                QbzMyQbz.itemAction(gcell.cItemId,
                                    mode === "next" ? "play-next"
                                        : mode === "later" ? "play-later" : "add-to-queue")
                            }
                        }
                    }
                }
            }
        }

        // The .slint floats its ListScrollbar at `parent.width - 14 - 4`
        // (:1226-1235), i.e. INSIDE the page's 32px right padding. Mounted
        // here, outside `body`, because the grid host clips.
        // Back/forward scroll memory (controls/ScrollMemory.qml): reports
        // this container's offset while it is the live page, and restores it
        // when a back/forward step arms this route.
        ScrollMemory { target: itemList; scope: "mixtapedetail" }
        QbzScrollBar {
            target: itemList
            visible: root.itemCount > 0 && root.viewMode !== "grid"
                && itemList.contentHeight > itemList.height
            anchors.right: parent.right
            anchors.rightMargin: 4
            anchors.top: body.top
            anchors.bottom: body.bottom
        }
        // Back/forward scroll memory (controls/ScrollMemory.qml): reports
        // this container's offset while it is the live page, and restores it
        // when a back/forward step arms this route.
        ScrollMemory { target: itemGrid; scope: "mixtapedetail" }
        QbzScrollBar {
            target: itemGrid
            visible: root.itemCount > 0 && root.viewMode === "grid"
                && itemGrid.contentHeight > itemGrid.height
            anchors.right: parent.right
            anchors.rightMargin: 4
            anchors.top: body.top
            anchors.bottom: body.bottom
        }
    }

    // ========================= BULK ACTION BAR ==========================
    // A SIBLING of the scroller, floating over the last 100px of scroll runway
    // (which is now inside the flickables, not carved out of the viewport —
    // see `body`). `parent.width - 18 - 22` is asymmetric on purpose
    // (.slint :1253).
    QbzMultiSelectBar {
        visible: root.selectMode && root.selectedCount > 0
        x: 18
        y: parent.height - height - 12
        width: parent.width - 40
        selectedCount: root.selectedCount
        actions: root.bulkActions()
        onAction: function (id) { QbzMyQbz.bulkAction(id) }
    }

    // Order is the .slint's (:1258-1266) and it puts LATER before NEXT — the
    // opposite of the row menu. Do not "fix" it.
    function bulkActions() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        return [
            { "id": "add-to-queue", "label": t("Add to queue", r), "icon": "list-end",
              "danger": false, "needsSelection": true },
            { "id": "play-later", "label": t("Play later", r), "icon": "list-plus",
              "danger": false, "needsSelection": true },
            { "id": "play-next", "label": t("Play next", r), "icon": "list-start",
              "danger": false, "needsSelection": true },
            { "id": "add-to-playlist", "label": t("Add to playlist", r), "icon": "list-music",
              "danger": false, "needsSelection": true },
            { "id": "add-to-mixtape", "label": t("Add to Mixtape/Collection", r),
              "icon": "cassette-tape", "danger": false, "needsSelection": true },
            { "id": "remove-selected", "label": t("Remove", r), "icon": "trash-2",
              "danger": true, "needsSelection": true },
            { "id": "clear", "label": t("Clear", r), "icon": "x",
              "danger": false, "needsSelection": true },
        ]
    }

    // ========================== HERO ⋯ MENU =============================
    CardMenu {
        id: heroMenu
        menuWidth: 220
        entries: root.heroMenuModel()
        onPicked: function (a) { root.heroMenuAction(a) }
    }

    function heroMenuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        // ASSET GAP: `image` is not baked yet (spec 01 §14).
        var m = [
            { "label": t("Rename", r), "icon": "pen-line", "action": "rename" },
            { "label": t("Edit description", r), "icon": "pen-line", "action": "description" },
            { "label": t("Upload cover", r), "icon": "image", "action": "upload-cover" },
        ]
        if (root.doc.hasCustomCover === true)
            m.push({ "label": t("Clear custom cover", r), "icon": "x", "action": "clear-cover" })
        m.push({ "sep": true })
        // The label is the OTHER mode (.slint :831-837).
        m.push({ "label": (root.doc.playMode || "in_order") === "in_order"
                    ? t("Album shuffle", r) : t("In order", r),
                 "icon": "shuffle", "action": "play-mode" })
        if (root.kind !== "artist_collection")
            m.push({ "label": root.kind === "mixtape"
                        ? t("Convert to Collection", r) : t("Convert to Mixtape", r),
                     "icon": "cassette-tape", "action": "convert" })
        // Sidebar visibility — only meaningful with the collapsible tree on.
        // The label reflects the CURRENT state (the mechanism playlists carry).
        if (root.collapsibleMyqbzOn) {
            m.push({ "sep": true })
            m.push({ "label": root.doc.hidden === true
                        ? t("Show in sidebar", r) : t("Hide from sidebar", r),
                     "icon": root.doc.hidden === true ? "eye" : "eye-off",
                     "action": "toggle-hidden" })
        }
        m.push({ "sep": true })
        m.push({ "label": t("Delete", r), "icon": "trash-2", "action": "delete", "danger": true })
        return m
    }

    function heroMenuAction(a) {
        if (a === "rename") QbzMyQbz.openRename()
        else if (a === "description") QbzMyQbz.openDescription()
        else if (a === "upload-cover") QbzMyQbz.uploadCover()
        else if (a === "clear-cover") QbzMyQbz.removeCover()
        else if (a === "play-mode") QbzMyQbz.togglePlayMode()
        else if (a === "convert") QbzMyQbz.convertKind()
        else if (a === "toggle-hidden")
            QbzMyQbz.setCollectionHidden(root.doc.id, root.doc.hidden !== true)
        else if (a === "delete") QbzMyQbz.openDelete()
    }

    // ========================= FILTER POPUP =============================
    // `QbzContextMenu` HARDCODES padding 5 / `surfaceMain` / r8 /
    // `borderMuted` and its content Column's spacing is unreachable through
    // the `menuContent` alias, so the panel, the padding and the row spacing
    // are all written here (spec 01 §3.3). Its default
    // `CloseOnPressOutside` is what the .slint asks for (:952-956): these are
    // radio / checkbox rows that must survive several toggles.
    QbzContextMenu {
        id: filterMenu
        menuWidth: 200
        padding: 4
        background: Rectangle {
            color: theme.surfaceMain
            radius: 8
            border.width: 1
            border.color: theme.surfaceElevated
        }
        Column {
            width: parent ? parent.width : 0
            spacing: 1

            SectionHead { text: root.trs("Type") }
            MenuOption {
                label: root.trs("All types")
                selected: (root.doc.typeFilter || "all") === "all"
                onChosen: QbzMyQbz.detailSetTypeFilter("all")
            }
            MenuOption {
                label: root.trs("Album")
                selected: root.doc.typeFilter === "album"
                onChosen: QbzMyQbz.detailSetTypeFilter("album")
            }
            MenuOption {
                label: root.trs("Track")
                selected: root.doc.typeFilter === "track"
                onChosen: QbzMyQbz.detailSetTypeFilter("track")
            }
            MenuOption {
                label: root.trs("Playlist")
                selected: root.doc.typeFilter === "playlist"
                onChosen: QbzMyQbz.detailSetTypeFilter("playlist")
            }
            Rectangle {
                width: parent.width
                height: 1
                color: theme.surfaceElevated
            }
            SectionHead { text: root.trs("Source") }
            MenuOption {
                label: root.trs("Qobuz")
                selected: root.doc.srcQobuz === true
                onChosen: QbzMyQbz.detailToggleSourceFilter("qobuz")
            }
            MenuOption {
                label: root.trs("Plex")
                selected: root.doc.srcPlex === true
                onChosen: QbzMyQbz.detailToggleSourceFilter("plex")
            }
            MenuOption {
                label: root.trs("Local")
                selected: root.doc.srcLocal === true
                onChosen: QbzMyQbz.detailToggleSourceFilter("local")
            }
        }
    }

}
