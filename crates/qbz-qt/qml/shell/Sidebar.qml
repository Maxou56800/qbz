// Left navigation sidebar — QML port of crates/qbz-ui/ui/shell/Sidebar.slint.
//
// Three states (ShellState.sidebar-state): 0 = open 240px (icon + label),
// 1 = mini (icons only), 2 = closed 0px. Width animates 160ms ease-in-out;
// the header's panel-left button cycles (QbzShell.cycleSidebar).
//
// MINI RAIL: 64px, 1:1 with Slint (state.slint:4075), holding a 34px row
// (Sidebar.slint:575/652) on symmetric 15px padding. It was 50px for a while —
// an owner deviation that turned out to put the rail's centre 5px left of the
// header control sitting directly above it; see `miniPadLeft` below and
// QbzTheme.sidebarMiniWidth for the measurement. Everything else
// in the mini state is 1:1 with Slint: rows centre their lone glyph, nested
// playlist rows collapse to zero, hovering a row names it in a bubble to the
// right (SidebarTooltip.slint) and clicking a FOLDER opens its playlists in a
// flyout to the right (SidebarFolderPopup.slint) — in mini a folder cannot
// expand in place, so a toggle there would be a control that does nothing.
//
// FILE LENGTH (>500): what remains after the mini folder flyout moved to
// shell/SidebarFolderFlyout.qml and the row context menu to
// shell/SidebarRowMenu.qml is ONE surface with one state machine — the three
// sidebar widths. Every block below is a WIDTH-CONDITIONAL variant of the same
// tree (the nav rows, the PLAYLISTS toolbar, the row delegate and the
// scrollbar each read `root.mini`), so splitting them into files would hand
// each half a `mini` property to keep in sync and put the row delegate's
// drop-target arithmetic — which needs `root`'s own window coordinates — a
// file away from the root it measures. The three things that DID come out are
// the three that are self-contained: both overlays and the hover bubble (the
// shell's shared QbzTooltip). All three are shared single instances declared
// at this root, never one per row: the tree can hold hundreds of rows.
//
// Top-level section nav rows (Discover / Library / Local Library / My QBZ)
// replicate SidebarNavRow: 34px rows, radius 6, 16px icons, 13px/w500
// labels, surface-hover on hover, Discover + Library HIDDEN while offline
// (ADR-010 mount-site gating). The whole block — rows AND the hairline under
// them — is mounted only while `QbzShell.navInSidebar` is ON; with it OFF the
// sections move to the header (HeaderBar.qml) and this sidebar starts at the
// PLAYLISTS toolbar, exactly like Sidebar.slint:724/817.
//
// The rows open the same dropdown FLYOUT their Slint counterparts do
// (SidebarNavRow: hover or click, panel to the RIGHT of the row, headed by
// the section name) — the panel, its behaviour and the entry catalog live in
// the shared NavFlyout.qml, which HeaderBar.qml mounts too, so the sections
// behave identically wherever the nav is hosted. The rows keep the
// SidebarDirectRow active treatment (surface-hover bg + primary text/icon)
// for the current section and while their own menu is open.
// All four sections route into real views, My QBZ included (its Collections /
// Mixtapes entries reach the "collections" / "mixtapes" routes). The disabled
// treatment the NavRow still carries — dimmed to 0.45, pointer-inert, no
// flyout — is the mechanism for a section with no surface behind it, and
// nothing uses it today (a row that clicks into nothing is a defect, not a
// stub). Its leading glyph goes through shell/NavSectionGlyph.qml, because My
// QBZ can carry a user-supplied icon (SidebarNavRow `raw-icon`).
// PURCHASES is the one row in that block that is NOT a section: it is a
// DirectRow (SidebarDirectRow) — no dropdown, no catalog entry, it just
// navigates — and it is triple-gated (opt-in `show_purchases`, hidden while
// offline, and shown only while the sections themselves live here: with the
// navigation in the header it follows them up there). The pref defaults
// OFF, so on a stock install this block is still the same four section rows
// it has always been.
// The playlist/folder tree below the nav IS live (sidebar_qt.rs: load, sort,
// search, expand/collapse, drag-drop target), and so is everything the "..."
// menu offers: New folder opens QbzFolderEdit's create panel (the small
// name-only create modal, controls/FolderModals.qml), Manage playlists routes
// to the Playlist Manager, Import opens the importer and Refresh rebuilds the
// tree. Import is the only row that dims — it is the only Qobuz-only one, so it
// follows the reference's `enabled: !offline`; Refresh stays lit because its
// bridge call is the offline-safe rebuild (see the Repeater's comment). Nothing
// in this file renders a control that no-ops.
//
// Right-pressing a row opens shell/SidebarRowMenu.qml (edit / hide / mixtape /
// move-to-folder). It is suppressed on the mini rail: Slint anchors that menu
// at `row.width - 210px`, which on a 34px rail row is deeply negative and Qt's
// viewport clamp would drop it at x=8 — not the reference's placement, just
// the clamp.

import QtQuick
import QtQuick.Controls
import QtQuick.Effects
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root

    property bool mini: QbzShell.sidebarState === 1

    // Purchases downloading now — `QbzPurchases.activeDownloadsJson`, parsed
    // once per publish (guarded: "[]" on the pre-boot frame).
    readonly property var activeDownloads: {
        try { return JSON.parse(QbzPurchases.activeDownloadsJson || "[]") } catch (e) { return [] }
    }
    /// Mean completion across the active albums, 0..1, for the mini ring.
    readonly property real activeDownloadsOverall: {
        var rows = root.activeDownloads
        if (rows.length === 0) return 0
        var sum = 0
        for (var i = 0; i < rows.length; i++) sum += (rows[i].percent || 0)
        return sum / rows.length / 100
    }
    /// The hover bubble's text: one line per album, "Title — 7/14 · 50%".
    readonly property string activeDownloadsSummary: {
        var rows = root.activeDownloads
        var lines = []
        for (var i = 0; i < rows.length; i++)
            lines.push((rows[i].title || QbzSession.tr("Purchases", QbzSession.trRev))
                + " — " + (rows[i].done || 0) + "/" + (rows[i].total || 0)
                + " · " + (rows[i].percent || 0) + "%")
        return lines.join("\n")
    }
    // The shell's ONE hover-tooltip overlay (controls/QbzTooltip.qml), fed in
    // by AppShell. Null when this component is mounted in isolation, in which
    // case the rail simply shows no bubble.
    property Item tooltip: null

    // ---- Collapsed-rail optical centring ---------------------------------
    // MEASURED, not nudged (release binary, 1600x1000, xcb, pixels sampled off
    // the framebuffer). The finding that mattered: the rail's content was
    // ALREADY symmetric on the sidebar's own geometry — hover pill centre 25.0
    // against a rail centre of 25.0 — and the owner still read it as off. The
    // eye was not comparing the glyphs to the rail. It was comparing them to
    // the header's leading control, the panel-left sidebar toggle, which sits
    // at x = spacingMd(16) and is 28px wide (HeaderBar.qml:225 +
    // QbzNavButton.qml:102) -> centre 30, directly above them. Five pixels
    // apart in one vertical line is visible; symmetric padding does not help.
    //
    // Nor is there a visible rail edge to centre against: the sidebar is
    // surface-card and the content frame's 8px left gutter is surface-card too
    // (AppShell.qml:149 paints the panel; the frame around it is the shell
    // base), so it reads as one uninterrupted band, sampled (26,26,26) from
    // x=1 to x=57 and (15,15,15) from 58 — band centre 29.
    //
    // The rail is back at Slint's 64px, which dissolves the conflict instead
    // of trading one misalignment for another: row centre 32 vs header button
    // 30. See QbzTheme.sidebarMiniWidth for why 50 was the actual defect.
    // Padding is symmetric and DERIVED, so the pair can never drift apart.
    readonly property int miniPadLeft: (theme.sidebarMiniWidth - theme.sidebarMiniRow) / 2
    readonly property int miniPadRight: root.miniPadLeft
    // Which section owns the highlight. DERIVED from the live view (in
    // NavFlyout, shared with the header), not set on click: the same rows can
    // live here or in the header, and history navigation (back/forward) must
    // keep the highlight honest. Slint additionally highlights the row whose
    // menu is open — the rows OR `navFlyout.openId` in for that.
    readonly property string activeNav: navFlyout.activeSection

    // ---- Purchases row gating (contract §7.1, Sidebar.slint:794-805) -----
    // NO new bridge property is needed. `settings_bool`'s tail is
    // unconditional on success — `publish_snapshot()` re-serialises the WHOLE
    // settings document after every successful write (settings_qt.rs:2278-2285)
    // — and the pref is already in it (`showPurchases`). So the Navigation
    // toggle reaches this row through QbzBridge.settingsJson and nothing
    // else is wired.
    //
    // GUARDED parse, the NavFlyout.qml:65-70 precedent: a bare JSON.parse in a
    // binding throws on the pre-publish frame and would take the whole sidebar
    // down with it, not just this row.
    readonly property var settingsDoc: {
        try { return JSON.parse(QbzBridge.settingsJson) } catch (e) { return ({}) }
    }
    // `show_purchases` is the MASTER gate. Purchases then FOLLOWS the other
    // sections (2026-09-13): it sits here while the navigation lives in the
    // sidebar, and in the header beside them otherwise (HeaderBar.qml
    // `purchasesInHeader`) — never the one entry left behind. The pref
    // defaults false: Purchases ships hidden.
    readonly property bool purchasesVisible:
        root.settingsDoc.showPurchases === true
        && !QbzSession.offline
        && QbzShell.navInSidebar

    // Playlist tree state (phase 7).
    property bool searchOpen: false
    // PERSISTED collapse state (owner request 2026-09-10): read live from the
    // republished settingsJson, toggled through `settingsBool`, so a collapsed
    // section stays collapsed across restarts. Default expanded.
    readonly property bool playlistsCollapsed: settingsDoc.sidebarPlaylistsCollapsed === true
    property string activePlaylistId: ""

    // ---- Collapsible My QBZ tree (opt-in) -------------------------------
    // Collapse state persisted the same way as the playlist tree above. The
    // parent "My QBZ" header collapses the whole block; each section collapses
    // its own list.
    readonly property bool myqbzCollapsed: settingsDoc.myqbzCollapsed === true
    readonly property bool myqbzMixtapesCollapsed: settingsDoc.myqbzMixtapesCollapsed === true
    readonly property bool myqbzCollectionsCollapsed: settingsDoc.myqbzCollectionsCollapsed === true
    // The light tree feed (myqbz_qt::publish_sidebar_tree): both grids' rows as
    // {mixtapes:[{id,name,hidden}], collections:[…]}. GUARDED parse, same
    // precedent as `settingsDoc`. Hidden rows are dropped here so the sidebar
    // honours the per-element "Hide from sidebar" flag.
    // EXPLICIT refresh, not a declarative binding: a plain `var` block binding
    // on QbzShell.myqbzTreeJson did not re-evaluate on republish here (the tree
    // stayed empty though Rust logged the feed). The grids drive their model the
    // same way — `onItemsChanged` + `Component.onCompleted` (MyQbzGridView.qml:
    // 152-153) — so mirror that: a Connections handler re-parses on every
    // change, and `Component.onCompleted` seeds whatever is already published.
    // PLAIN arrays assigned together in refreshMyqbzTree — NOT derived
    // bindings. A derived `var` binding on another `var` (myqbzMixtapes:
    // _visibleRows(myqbzTree.mixtapes)) did not re-evaluate when the source was
    // assigned imperatively (the feed arrived — proven in the log — yet the
    // rows stayed empty). Assigning the final arrays directly makes the
    // Repeater/empty-state bindings react the ordinary way.
    property var myqbzMixtapes: []
    property var myqbzCollections: []
    // Raw feed rows (hidden dropped), kept so a search / sort change re-derives
    // the visible arrays without a DB round trip.
    property var _myqbzMixRaw: []
    property var _myqbzColRaw: []
    // Playlists-parity toolbar state: inline search over the tree, and a simple
    // A–Z / Z–A name sort.
    property bool myqbzSearchOpen: false
    property string myqbzSearch: ""
    property bool myqbzSortAsc: true
    function _visibleRows(arr) {
        var out = []
        if (!arr) return out
        for (var i = 0; i < arr.length; ++i)
            if (arr[i] && arr[i].hidden !== true) out.push(arr[i])
        return out
    }
    // Apply the live search filter + name sort to one raw list.
    function _filterSortMyqbz(arr) {
        var q = root.myqbzSearch.trim().toLowerCase()
        var out = []
        for (var i = 0; i < arr.length; ++i) {
            var r = arr[i]
            if (!r) continue
            if (q !== "" && String(r.name).toLowerCase().indexOf(q) === -1) continue
            out.push(r)
        }
        out.sort(function (a, b) {
            var an = String(a.name).toLowerCase()
            var bn = String(b.name).toLowerCase()
            if (an === bn) return 0
            var lt = an < bn
            return root.myqbzSortAsc ? (lt ? -1 : 1) : (lt ? 1 : -1)
        })
        return out
    }
    function _applyMyqbz() {
        root.myqbzMixtapes = _filterSortMyqbz(root._myqbzMixRaw)
        root.myqbzCollections = _filterSortMyqbz(root._myqbzColRaw)
    }
    function refreshMyqbzTree() {
        var t
        try { t = JSON.parse(QbzShell.myqbzTreeJson) }
        catch (e) { t = ({ "mixtapes": [], "collections": [] }) }
        root._myqbzMixRaw = _visibleRows(t.mixtapes)
        root._myqbzColRaw = _visibleRows(t.collections)
        root._applyMyqbz()
    }
    onMyqbzSearchChanged: root._applyMyqbz()
    onMyqbzSortAscChanged: root._applyMyqbz()
    Connections {
        target: QbzShell
        function onMyqbzTreeJsonChanged() { root.refreshMyqbzTree() }
    }
    Component.onCompleted: root.refreshMyqbzTree()
    // Master gate + the two section gates + the standard-entry suppressor.
    readonly property bool myqbzTreeOn: settingsDoc.collapsibleMyqbz === true
    readonly property bool myqbzShowMixtapes: settingsDoc.myqbzShowMixtapes !== false
    readonly property bool myqbzShowCollections: settingsDoc.myqbzShowCollections !== false
    // The collapsible tree REPLACES the flat My QBZ nav entry — its parent
    // header click reaches the same surface — so the two are mutually
    // exclusive: whenever the tree is on, the classic row is suppressed. (No
    // separate toggle; keeping both would be the same entry twice.)
    readonly property bool hideStandardMyqbz: myqbzTreeOn
    // ---- Large-NPB dock space reservation -------------------------------
    // The cover+spectrum dock is an AppShell-ROOT overlay pinned flush to the
    // WINDOW bottom-left, so it extends the bar's height BELOW this sidebar
    // (which stops at npb.top). Only the IN-SIDEBAR portion has to be
    // reserved, and it shrinks when the band is hidden — that is why the
    // height comes from the bridge and not a literal.
    //
    // Without this the playlist list keeps running to the sidebar's bottom
    // edge and the rows sit UNDER the album art (Sidebar.slint:1145 reserves
    // the same way).
    readonly property bool largeDockActive: QbzShell.npbMode === 3 && QbzShell.sidebarState === 0
    readonly property real dockReserve: largeDockActive
        ? Math.max(0, QbzShell.largeDockHeight - theme.npbLargeHeight)
        : 0

    // Flattened entries from the bridge + the url-keyed cover map.
    readonly property var entries: parseEntries(QbzShell.sidebarJson)
    // A republish rebuilds the Repeater's delegates, so the row a bubble is
    // anchored to may be gone or moved and its MouseArea will never report the
    // leave. The overlay outlives the delegate by design (it stores numbers,
    // never the Item), so nothing dangles — but it would linger, so drop it.
    onEntriesChanged: if (root.tooltip) root.tooltip.hideAll()
    property var coverMap: ({})
    function parseEntries(json) {
        var e = JSON.parse(json)
        // Collect every cover url and dispatch the artwork window.
        var urls = []
        for (var i = 0; i < e.length; i++) {
            for (var j = 0; j < e[i].covers.length; j++) urls.push(e[i].covers[j])
        }
        if (urls.length > 0) QbzShell.sidebarArtworkWindow(JSON.stringify(urls))
        return e
    }

    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            var m = root.coverMap
            m[key] = path
            root.coverMap = Object.assign({}, m)
        }
    }

    /// Shader availability for the playlist micro-collage's rounding mask.
    /// Verbatim from cards/PlaylistCollage.qml:120-122 (and
    /// theme/RoundedImage.qml:234-236): the ONLY case the retired "effects
    /// render nothing" doctrine was ever true for is the software/Null
    /// renderer, and `GraphicsInfo.api` answers it per window at runtime
    /// (`api` carries `notify: "apiChanged"`, so this re-evaluates when the
    /// window arrives). Test Software/Null NEGATIVELY — under the RHI an
    /// OpenGL backend reports `GraphicsInfo.OpenGL`. `QbzShell.forceCanvasArt`
    /// (env `QBZ_QT_ROUND_MODE=canvas`, default OFF) is honoured here too, so
    /// a machine where `GraphicsInfo` reports a GPU and the mask still
    /// misbehaves degrades to square corners instead of losing the covers.
    /// `typeof` guard mirrors both files: eager binding, and the sidebar must
    /// stay loadable in an isolated qml6 scene.
    ///
    /// Declared ONCE on this root rather than per delegate: the answer is a
    /// per-window constant and the playlist tree can hold hundreds of rows.
    readonly property bool _noShaders: GraphicsInfo.api === GraphicsInfo.Software
        || GraphicsInfo.api === GraphicsInfo.Null
        || (typeof QbzShell !== "undefined" && QbzShell.forceCanvasArt)

    QbzTheme { id: theme }

    // The section-nav dropdown (catalog + panel + open/close behaviour) —
    // the SAME component HeaderBar.qml mounts. Zero-size and outside the
    // Column so its origin stays this root's origin (the row coordinates are
    // mapped into it). The panel renders in the window overlay, so neither
    // this root's `clip` nor its 240px width can cut it off.
    NavFlyout { id: navFlyout }

    // ---- Collapsed (mini) affordances -----------------------------------
    // In the mini rail a playlist row is an icon and nothing else, so the two
    // things the open sidebar gives for free have to be restored:
    //   - the NAME, as a hover bubble to the row's right (SidebarTooltip.slint);
    //   - a folder's CONTENTS, as a flyout to the row's right, because the
    //     nested rows are hidden in mini (SidebarFolderPopup.slint).
    // Both are ONE shared instance driven by the hovered/clicked row (exactly
    // like Slint's SidebarTooltipState / SidebarFolderPopupState), never one
    // per delegate: the tree can hold hundreds of rows.
    // Neither can live inside this root's bounds, because it clips. The FLYOUT
    // is a QtQuick.Controls Popup, so its content is reparented to the WINDOW
    // overlay — the same reason NavFlyout and HeaderBar's playlists flyout are
    // Popups. The BUBBLE is not a popup at all: it is the shell-level
    // QbzTooltip overlay (`root.tooltip`, fed in by AppShell), for the reasons
    // spelled out where the old ToolTip used to be declared, below.

    // Race-safe show/hide (Slint SidebarRow.show-tooltip / hide-tooltip): the
    // leaving row only clears the bubble if it still owns it, so sliding the
    // pointer straight from one row to the next never blanks it. Both halves
    // now live in the shared overlay, keyed by the entry id.
    function showMiniTip(row, entry) {
        if (!root.mini || !root.tooltip)
            return
        root.tooltip.showRight(row, entry.id, entry.kind === "folder"
            ? entry.name + " (" + entry.count + ")"
            : entry.name)
    }
    function hideMiniTip(id) {
        if (root.tooltip)
            root.tooltip.hide(id)
    }

    // Mini folder click: the flyout, not a toggle (the nested rows are hidden,
    // so toggling is a control that renders and does nothing — Slint opens the
    // popup instead, Sidebar.slint:355-377).
    //
    // It no longer force-expands the folder on the way in. That used to be
    // necessary because the flyout filtered the published `entries`, which
    // carry a folder's children ONLY while it is expanded — a persistent side
    // effect (the folder stayed open once the sidebar was re-opened) the
    // reference does not have. The flyout now asks Rust for a dedicated
    // cache-built document instead (SidebarFolderFlyout.qml / §4.7).
    function openFolderFlyout(row, entry) {
        // The flyout takes the same +6px slot the bubble occupies, and it is a
        // Popup (window overlay) so it would paint straight over it. Slint drops
        // the tooltip explicitly here too (Sidebar.slint:353).
        if (root.tooltip) root.tooltip.hideAll()
        folderFlyout.openFor(entry, row)
    }

    // Cycling the sidebar while any of the three is open would leave it
    // floating over a rail that no longer exists (the bubble self-hides
    // through `root.mini`; the two Popups need telling).
    Connections {
        target: QbzShell
        function onSidebarStateChanged() {
            folderFlyout.close()
            // The row menu is row-anchored and the rows are about to change
            // width — or, going to state 2, stop existing.
            rowMenu.close()
            // The bubble's anchor is a rail row that is about to move or stop
            // existing (SidebarTooltip.slint:14 gates on sidebar-mini for the
            // same reason).
            if (root.tooltip) root.tooltip.hideAll()
        }
    }

    // The hover bubble that names a collapsed row is NOT declared here any
    // more: it is the shell's shared overlay (controls/QbzTooltip.qml, mounted
    // once by AppShell), which this file drives through showMiniTip /
    // hideMiniTip above. Two reasons, in order of severity:
    //
    //  1. THE OLD ONE WAS BROKEN. It was a QtQuick.Controls `ToolTip` with a
    //     custom contentItem, and a ToolTip does not propagate a custom
    //     content's implicit size: measured on Qt 6.11.1, an identical Text
    //     inside a `Popup` gives implicitContentWidth 157.8 and inside a
    //     `ToolTip` gives 1. The Basic style then sizes the popup to
    //     `implicitContentWidth + padding`, so the bubble collapsed to ~13x25
    //     while the Text — which a Popup never clips — kept painting its full
    //     178px: the label floated over the content pane with no background and
    //     a small detached box sat beside it (owner report, reproduced and
    //     pixel-sampled before the change).
    //  2. Slint does not put a popup per call site either. It has ONE overlay
    //     fed by a global channel (SidebarTooltipState, state.slint:1893;
    //     TooltipState, state.slint:4791), precisely so a clipped surface — and
    //     this sidebar clips — can still show a bubble outside its own bounds.

    // Folder flyout (mini rail) and the row context menu — ONE shared instance
    // each, declared at this root, never one per delegate: the tree can hold
    // hundreds of rows. Both are Popups, so their content is reparented to the
    // WINDOW overlay and this root's `clip: true` cannot cut them off.
    SidebarFolderFlyout {
        id: folderFlyout
        onPlaylistActivated: function (id) { root.activePlaylistId = id }
    }

    // The canonical folder list, parsed once. `foldersJson` is the Playlist
    // Manager's property because the manager owns the rich folder record, but
    // it is kept fresh independently of the manager's document cache
    // (refresh_folders()), so the sidebar can read it before — and without —
    // the manager view ever being opened.
    readonly property var pmFolders: {
        try { return JSON.parse(QbzPlaylistManager.foldersJson) } catch (e) { return [] }
    }
    // Hidden folders are dropped EVERYWHERE in the sidebar tree, so the row
    // menu must not offer them, must not count them toward its search
    // threshold and must not let them suppress its empty state. Derived once,
    // here, and read three times over there.
    readonly property var visibleFolders: root.pmFolders.filter(function (f) {
        return f.isHidden !== true
    })

    SidebarRowMenu {
        id: rowMenu
        visibleFolders: root.visibleFolders
    }

    // Right-click menu for a collapsible My QBZ tree row (mixtape/collection):
    // one action, "Hide from sidebar".
    MyQbzSidebarMenu { id: myqbzRowMenu }

    // My QBZ header "+" — create a new Mixtape or Collection (app-global modal).
    CardMenu {
        id: myqbzPlusMenu
        menuWidth: 190
        entries: [
            { "label": QbzSession.tr("New Mixtape", QbzSession.trRev), "icon": "cassette-tape", "action": "mixtape" },
            { "label": QbzSession.tr("New Collection", QbzSession.trRev), "icon": "library-big", "action": "collection" },
        ]
        onPicked: function (a) { QbzShell.myqbzCreateOpen(a) }
    }
    // My QBZ header "..." — a simple name sort plus refresh.
    CardMenu {
        id: myqbzMoreMenu
        menuWidth: 190
        entries: [
            { "label": QbzSession.tr("Name (A-Z)", QbzSession.trRev), "icon": "chevron-down", "action": "az" },
            { "label": QbzSession.tr("Name (Z-A)", QbzSession.trRev), "icon": "chevron-up", "action": "za" },
            { "sep": true },
            { "label": QbzSession.tr("Refresh", QbzSession.trRev), "icon": "refresh-cw", "action": "refresh" },
        ]
        onPicked: function (a) {
            if (a === "az") root.myqbzSortAsc = true
            else if (a === "za") root.myqbzSortAsc = false
            else if (a === "refresh") QbzShell.myqbzRefreshTree()
        }
    }

    // #771: the open width is the user's (240..480, persisted); the other
    // two states keep their fixed rails.
    width: QbzShell.sidebarState === 2 ? 0
         : QbzShell.sidebarState === 1 ? theme.sidebarMiniWidth
         : QbzShell.sidebarWidth
    /// Set by AppShell while its resize handle is pressed: the width follows
    /// the pointer directly, the state snaps still animate.
    property bool resizing: false
    // surface-card @ 0.5 while the ambient background is active (phase 14).
    color: ambientOn ? theme.surfaceCardA50 : theme.surfaceCard
    readonly property bool ambientOn: theme.ambientOn

    // Square edges; clip keeps content from spilling while the width
    // animates (same as the Slint root).
    clip: true

    // While the pointer drives the width the Behavior is off (it would lag
    // the mouse); a STATE snap during that drag (open <-> mini <-> closed)
    // still animates, because the bridge raises `sidebarSnapping` right
    // before it changes the state and lowers it on the next pointer-follow.
    Behavior on width {
        enabled: !root.resizing || QbzShell.sidebarSnapping
        NumberAnimation { duration: 160; easing.type: Easing.InOutQuad }
    }

    // One section-nav row (SidebarNavRow / SidebarDirectRow metrics). Hovering
    // or clicking it opens that section's flyout to the RIGHT of the row.
    component NavRow: Rectangle {
        id: navRow
        // A section from the shared catalog (NavFlyout.sections).
        property var section: null
        readonly property bool isActive: section
            && (root.activeNav === section.id || navFlyout.openId === section.id)
        // A section with no surface behind it renders as unavailable rather
        // than as a live row that clicks into nothing. Every section is live
        // today; the treatment stays for the next stub.
        readonly property bool isEnabled: section && section.enabled
        readonly property bool lit: isActive || (navArea.containsMouse && isEnabled)

        width: parent ? parent.width : 0
        // Mini: a SQUARE row that fills the 34px padded track (theme token,
        // rail 50 - 2*8). Open keeps Slint's 34px (Sidebar.slint:575).
        height: root.mini ? theme.sidebarMiniRow : 34
        radius: 6
        opacity: isEnabled ? 1.0 : 0.45
        color: lit ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: root.mini ? 0 : 8
            anchors.rightMargin: root.mini ? 0 : 8
            spacing: 10

            Item {
                width: root.mini ? parent.width : 16
                height: parent.height
                // Baked glyph, or the section's own raw image when it carries
                // one (My QBZ branding) — Sidebar.slint:601-612.
                NavSectionGlyph {
                    section: navRow.section
                    size: 16
                    anchors.centerIn: parent
                    tintName: navRow.lit ? "textPrimary" : "secondary"
                }
            }
            Text {
                visible: !root.mini
                height: parent.height
                width: parent.width - (root.mini ? 0 : 26)
                text: navRow.section ? navRow.section.label : ""
                color: navRow.lit ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                font.weight: theme.weightMedium
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: navArea
            anchors.fill: parent
            enabled: navRow.isEnabled
            hoverEnabled: navRow.isEnabled
            cursorShape: Qt.PointingHandCursor
            onClicked: {
                navFlyout.openBeside(navRow, navRow.section, root.mini)
                // Opt-in "click lands on the first tab" — a no-op while the
                // Appearance toggle is off, which is the default.
                navFlyout.sectionClicked(navRow.section)
            }
            onContainsMouseChanged: {
                if (containsMouse) {
                    // Hover-to-open; hovering another row overwrites the open
                    // menu (instant switch, single-open by construction).
                    navFlyout.triggerHovered = true
                    navFlyout.openBeside(navRow, navRow.section, root.mini)
                } else if (navRow.section && navFlyout.openId === navRow.section.id) {
                    // Only the row owning the open menu clears the flag.
                    navFlyout.triggerHovered = false
                }
            }
        }
    }

    // A DIRECT destination row — the port of SidebarDirectRow
    // (Sidebar.slint:646-685). Same 34px metrics, same radius, same lit
    // treatment as NavRow above; what it does NOT have is the flyout. It
    // navigates on click, so there is no catalog `section` behind it — which
    // is also why the glyph goes to QbzIcon directly rather than through
    // shell/NavSectionGlyph.qml (that component's whole input IS a section).
    // `shopping-bag` is a registered tint master (icon_tint_qt.rs:261), so it
    // is available in every tint under all 36 themes.
    //
    // IT MUST NEVER TOUCH `navFlyout.triggerHovered`, and the reason is the
    // opposite of the obvious one. The idle-close timer is
    // `running: panel.visible && !panel.menuHovered` (NavFlyout.qml:285), so
    // with nothing open the flag pins nothing. The hazard is setting it while
    // ANOTHER section's panel IS open: it feeds `menuHovered` (:368-369),
    // which halts the 1s idle-close and leaves that flyout hanging open for as
    // long as the pointer rests on this row. Hovering a direct row is not a
    // flyout event at all.
    component DirectRow: Rectangle {
        id: directRow
        // A QbzIcon glyph name (not a section) and its label.
        property string iconName: ""
        property string label: ""
        property bool active: false
        signal activated()

        readonly property bool lit: directRow.active || directArea.containsMouse

        width: parent ? parent.width : 0
        // Mini: the SAME square track the nav rows use, so a direct row and a
        // section row cannot land on different centre lines.
        height: root.mini ? theme.sidebarMiniRow : 34
        radius: 6
        color: directRow.lit ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: root.mini ? 0 : 8
            anchors.rightMargin: root.mini ? 0 : 8
            spacing: 10

            Item {
                width: root.mini ? parent.width : 16
                height: parent.height
                QbzIcon {
                    name: directRow.iconName
                    width: 16
                    height: 16
                    anchors.centerIn: parent
                    tintName: directRow.lit ? "textPrimary" : "secondary"
                }
            }
            Text {
                visible: !root.mini
                height: parent.height
                width: parent.width - (root.mini ? 0 : 26)
                text: directRow.label
                color: directRow.lit ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                font.weight: theme.weightMedium
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: directArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: directRow.activated()
        }
    }

    // A collapsible My QBZ tree HEADER: "My QBZ" / "Mixtapes" / "Collections".
    // TWO hit-areas by owner intent — the TEXT is a link to that surface, the
    // CHEVRON only collapses its list. `indent` nests the section headers under
    // the parent. Muted, discreet; not a full nav row.
    component MyQbzHeader: Rectangle {
        id: mqHeader
        property string label: ""
        property bool expanded: true
        property int indent: 0
        property bool showChevron: true
        signal linkClicked()
        signal toggle()

        width: parent ? parent.width : 0
        height: 30
        radius: 6
        color: (mqLinkArea.containsMouse || mqChevArea.containsMouse)
            ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            anchors.leftMargin: 8 + mqHeader.indent
            anchors.rightMargin: 4
            spacing: 4

            Text {
                id: mqLinkText
                width: parent.width - 26
                height: parent.height
                text: mqHeader.label
                color: mqLinkArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: mqHeader.indent > 0 ? 12 : 13
                font.weight: theme.weightMedium
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
                MouseArea {
                    id: mqLinkArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: mqHeader.linkClicked()
                }
            }
            Rectangle {
                width: 22
                height: 22
                radius: 4
                visible: mqHeader.showChevron
                anchors.verticalCenter: parent.verticalCenter
                color: mqChevArea.containsMouse ? theme.surfaceElevated : "transparent"
                QbzIcon {
                    name: mqHeader.expanded ? "chevron-down" : "chevron-right"
                    width: 14
                    height: 14
                    anchors.centerIn: parent
                    tintName: mqChevArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: mqChevArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: mqHeader.toggle()
                }
            }
        }
    }

    // A collapsible My QBZ tree ITEM: one mixtape/collection. Click opens its
    // detail (openCard); right-click offers "Hide from sidebar".
    component MyQbzItem: Rectangle {
        id: mqItem
        property string itemId: ""
        property string label: ""
        property bool isActive: false

        width: parent ? parent.width : 0
        height: 28
        radius: 6
        color: (mqItemArea.containsMouse || mqItem.isActive)
            ? theme.surfaceHover : "transparent"

        Row {
            anchors.fill: parent
            // Indented past the section header's text so items read as children.
            anchors.leftMargin: 26
            anchors.rightMargin: 8
            spacing: 8

            QbzIcon {
                name: "music"
                width: 13
                height: 13
                anchors.verticalCenter: parent.verticalCenter
                tintName: (mqItemArea.containsMouse || mqItem.isActive) ? "textPrimary" : "muted"
            }
            Text {
                width: parent.width - 21
                height: parent.height
                text: mqItem.label
                color: (mqItemArea.containsMouse || mqItem.isActive)
                    ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 12
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
        MouseArea {
            id: mqItemArea
            anchors.fill: parent
            hoverEnabled: true
            acceptedButtons: Qt.LeftButton | Qt.RightButton
            cursorShape: Qt.PointingHandCursor
            onClicked: function (mouse) {
                if (mouse.button === Qt.RightButton) {
                    var lx = mqItem.width - myqbzRowMenu.menuWidth
                    myqbzRowMenu.openForItem(mqItem.itemId, mqItem, Math.max(0, lx), 26)
                } else {
                    QbzShell.myqbzOpenCard(mqItem.itemId)
                }
            }
        }
    }

    Column {
        anchors.fill: parent
        // Mini left/right are ASYMMETRIC on purpose — see `miniPadLeft` at the
        // top of this file for the measurement. Top stays 8 (nothing above the
        // rail competes with it vertically).
        anchors.leftMargin: root.mini ? root.miniPadLeft : theme.spacingMd
        anchors.rightMargin: root.mini ? root.miniPadRight : theme.spacingMd
        anchors.topMargin: root.mini ? 8 : theme.spacingMd
        // Per-side padding so Large can zero ONLY the bottom (Sidebar.slint:719):
        // the reserved dock band then reaches the true window bottom-left and the
        // cover sits flush (the L corner). Left/right/top are unchanged.
        anchors.bottomMargin: root.largeDockActive ? 0 : (root.mini ? 8 : theme.spacingMd)
        spacing: theme.spacingMd

        // ---- Section nav -------------------------------------------
        Column {
            id: navColumn
            // Section-nav placement: OFF moves the whole block to the header.
            // A Column skips invisible children entirely (no phantom spacing),
            // so this is the `if` mount-site gate Slint uses.
            visible: QbzShell.navInSidebar
            width: parent.width
            spacing: 2

            // The same four sections the header hosts, from the same catalog.
            // Qobuz-only ones are HIDDEN entirely while offline (ADR-010
            // mount-site gating, not rendered-disabled). My QBZ is `qobuz:
            // false` in the catalog, i.e. it stays mounted while offline —
            // Slint does not offline-gate its row either
            // (Sidebar.slint:779-792, unlike the Purchases row right after it).
            Repeater {
                model: navFlyout.sections
                delegate: NavRow {
                    required property var modelData
                    section: modelData
                    // Offline-gated as before, PLUS the opt-in suppressor: with
                    // the collapsible tree on and "Hide standard My QBZ menu
                    // entry" checked, the flat My QBZ section row gives way to
                    // the tree block mounted right below this Repeater.
                    visible: !(modelData.qobuz && QbzSession.offline)
                        && !(modelData.id === "myqbz" && root.hideStandardMyqbz)
                }
            }

            // ---- Collapsible My QBZ tree (opt-in) ----------------------
            // Sits right under the four section rows (My QBZ is the last of
            // them), so its expanded tree reads as that section's contents.
            // Hidden on the mini rail and whenever the opt-in is off. The
            // parent "My QBZ" header collapses the whole block; each section
            // header links to its grid and collapses its own list.
            Column {
                id: myqbzTree
                visible: !root.mini && root.myqbzTreeOn
                width: parent.width
                spacing: 2

                // Parent header TOOLBAR — Playlists-parity: branding icon +
                // title (a link to the My QBZ surface) on the LEFT; search /
                // new / more / collapse FLOATING on the right, their chevron
                // aligned to the playlist carets (rightMargin 4 → the 22px box's
                // 15px icon centres on width-15, same as the row carets and the
                // Mixtapes/Collections sub-chevrons).
                Item {
                    width: parent.width
                    height: 34

                    // Branding icon — user image, else the baked qbz-symbolic.
                    Item {
                        id: myqbzBrandIcon
                        anchors.left: parent.left
                        anchors.leftMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        width: 18
                        height: 18
                        Image {
                            visible: navFlyout.brandingIconPath !== ""
                            source: navFlyout.brandingIconPath
                            width: 16
                            height: 16
                            anchors.centerIn: parent
                            fillMode: Image.PreserveAspectFit
                            mipmap: true
                        }
                        QbzIcon {
                            visible: navFlyout.brandingIconPath === ""
                            name: "qbz-symbolic"
                            width: 16
                            height: 16
                            anchors.centerIn: parent
                            tintName: "secondary"
                        }
                    }

                    // Floating right-hand controls.
                    Row {
                        id: myqbzHeaderBtns
                        anchors.right: parent.right
                        anchors.rightMargin: 4
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 4

                        // Search toggle.
                        Rectangle {
                            width: 22
                            height: 22
                            radius: 4
                            color: (myqbzSearchArea.containsMouse || root.myqbzSearchOpen) ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                name: "search"
                                width: 15
                                height: 15
                                anchors.centerIn: parent
                                tintName: (myqbzSearchArea.containsMouse || root.myqbzSearchOpen) ? "textPrimary" : "muted"
                            }
                            MouseArea {
                                id: myqbzSearchArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    root.myqbzSearchOpen = !root.myqbzSearchOpen
                                    if (!root.myqbzSearchOpen) {
                                        myqbzSearchEdit.text = ""
                                        root.myqbzSearch = ""
                                    } else {
                                        myqbzSearchEdit.forceActiveFocus()
                                    }
                                }
                            }
                        }
                        // New (+).
                        Rectangle {
                            width: 22
                            height: 22
                            radius: 4
                            color: myqbzPlusArea.containsMouse ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                name: "plus"
                                width: 16
                                height: 16
                                anchors.centerIn: parent
                                tintName: myqbzPlusArea.containsMouse ? "textPrimary" : "muted"
                            }
                            MouseArea {
                                id: myqbzPlusArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: myqbzPlusMenu.openBelowLeft(myqbzPlusArea)
                            }
                        }
                        // More (…) — sort + refresh.
                        Rectangle {
                            width: 22
                            height: 22
                            radius: 4
                            color: myqbzMoreArea.containsMouse ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                name: "ellipsis"
                                width: 15
                                height: 15
                                anchors.centerIn: parent
                                tintName: myqbzMoreArea.containsMouse ? "textPrimary" : "muted"
                            }
                            MouseArea {
                                id: myqbzMoreArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: myqbzMoreMenu.openBelowLeft(myqbzMoreArea)
                            }
                        }
                        // Collapse chevron (down = expanded).
                        Rectangle {
                            width: 22
                            height: 22
                            radius: 4
                            color: myqbzChevArea.containsMouse ? theme.surfaceHover : "transparent"
                            QbzIcon {
                                name: root.myqbzCollapsed ? "chevron-right" : "chevron-down"
                                width: 15
                                height: 15
                                anchors.centerIn: parent
                                tintName: myqbzChevArea.containsMouse ? "textPrimary" : "muted"
                            }
                            MouseArea {
                                id: myqbzChevArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzBridge.settingsBool("myqbz-collapsed", !root.myqbzCollapsed)
                            }
                        }
                    }

                    // Title — a link to the My QBZ surface (Collections). Fills
                    // between the icon and the floating controls.
                    Text {
                        visible: !root.myqbzSearchOpen
                        anchors.left: myqbzBrandIcon.right
                        anchors.leftMargin: 8
                        anchors.right: myqbzHeaderBtns.left
                        anchors.rightMargin: 6
                        anchors.verticalCenter: parent.verticalCenter
                        text: navFlyout.brandingLabel
                        color: myqbzTitleArea.containsMouse ? theme.textPrimary : theme.textSecondary
                        font.pixelSize: 13
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                        MouseArea {
                            id: myqbzTitleArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzShell.navigateTo("collections")
                        }
                    }
                    // Inline search field (filters the tree client-side).
                    Rectangle {
                        visible: root.myqbzSearchOpen
                        anchors.left: myqbzBrandIcon.right
                        anchors.leftMargin: 8
                        anchors.right: myqbzHeaderBtns.left
                        anchors.rightMargin: 6
                        anchors.verticalCenter: parent.verticalCenter
                        height: 22
                        radius: 4
                        color: theme.surfaceElevated
                        border.width: 1
                        border.color: theme.borderSubtle
                        TextInput {
                            QbzTextEditMenu { }
                            id: myqbzSearchEdit
                            anchors.fill: parent
                            anchors.leftMargin: 6
                            color: theme.textPrimary
                            font.pixelSize: 12
                            verticalAlignment: Text.AlignVCenter
                            clip: true
                            onTextEdited: root.myqbzSearch = text
                            Text {
                                visible: myqbzSearchEdit.text === ""
                                anchors.fill: parent
                                text: QbzSession.tr("Search My QBZ", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: 12
                                verticalAlignment: Text.AlignVCenter
                            }
                        }
                    }
                }

                // Mixtapes section (gated by its own opt-in).
                MyQbzHeader {
                    visible: !root.myqbzCollapsed && root.myqbzShowMixtapes
                    width: parent.width
                    indent: 12
                    label: QbzSession.tr("Mixtapes", QbzSession.trRev)
                    expanded: !root.myqbzMixtapesCollapsed
                    onLinkClicked: QbzShell.navigateTo("mixtapes")
                    onToggle: QbzBridge.settingsBool("myqbz-mixtapes-collapsed", !root.myqbzMixtapesCollapsed)
                }
                Repeater {
                    model: (!root.myqbzCollapsed && root.myqbzShowMixtapes
                            && !root.myqbzMixtapesCollapsed) ? root.myqbzMixtapes : []
                    delegate: MyQbzItem {
                        required property var modelData
                        width: myqbzTree.width
                        itemId: String(modelData.id)
                        label: String(modelData.name)
                    }
                }
                Text {
                    visible: !root.myqbzCollapsed && root.myqbzShowMixtapes
                        && !root.myqbzMixtapesCollapsed && root.myqbzMixtapes.length === 0
                    width: parent.width
                    leftPadding: 26
                    height: 22
                    text: QbzSession.tr("No mixtapes yet.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 11
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }

                // Collections section (gated by its own opt-in).
                MyQbzHeader {
                    visible: !root.myqbzCollapsed && root.myqbzShowCollections
                    width: parent.width
                    indent: 12
                    label: QbzSession.tr("Collections", QbzSession.trRev)
                    expanded: !root.myqbzCollectionsCollapsed
                    onLinkClicked: QbzShell.navigateTo("collections")
                    onToggle: QbzBridge.settingsBool("myqbz-collections-collapsed", !root.myqbzCollectionsCollapsed)
                }
                Repeater {
                    model: (!root.myqbzCollapsed && root.myqbzShowCollections
                            && !root.myqbzCollectionsCollapsed) ? root.myqbzCollections : []
                    delegate: MyQbzItem {
                        required property var modelData
                        width: myqbzTree.width
                        itemId: String(modelData.id)
                        label: String(modelData.name)
                    }
                }
                Text {
                    visible: !root.myqbzCollapsed && root.myqbzShowCollections
                        && !root.myqbzCollectionsCollapsed && root.myqbzCollections.length === 0
                    width: parent.width
                    leftPadding: 26
                    height: 22
                    text: QbzSession.tr("No collections yet.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 11
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
            }

            // Purchases — LAST in the block and the only non-section row here
            // (Sidebar.slint:794-805). Opt-in, hidden while offline (the
            // surface needs the Qobuz session), and ACTIVE on both purchase
            // routes so the highlight survives the click into a purchased
            // album's detail.
            //
            // The click only navigates. Entering the list is the VIEW's job:
            // `openList()` pairs with `leave()` on the view's own
            // mount/unmount (§G.1), and Back reaches this route without ever
            // passing through this row — a row that pre-loaded would leave the
            // history path unloaded and double-fetch the direct path.
            DirectRow {
                visible: root.purchasesVisible
                iconName: "shopping-bag"
                label: QbzSession.tr("Purchases", QbzSession.trRev)
                active: QbzShell.currentView === "purchases"
                    || QbzShell.currentView === "purchase-album"
                onActivated: QbzShell.navigateTo("purchases")
            }

            // Purchases DOWNLOADING right now. A download is process-wide
            // and survives leaving its page (purchases_qt's store); this is
            // how the user finds it again. Expanded: one row per album with
            // a thin progress bar. Mini: one ring for the whole set, with
            // the per-album detail in its hover bubble. Absent while idle.
            Item {
                id: dlBlock
                readonly property var rows: root.activeDownloads
                visible: root.purchasesVisible && rows.length > 0
                width: parent.width
                height: !visible ? 0 : (root.mini ? theme.sidebarMiniRow : dlCol.implicitHeight)

                Column {
                    id: dlCol
                    visible: !root.mini
                    width: parent.width
                    spacing: 2
                    Repeater {
                        model: root.mini ? [] : dlBlock.rows
                        delegate: Rectangle {
                            id: dlRow
                            required property var modelData
                            width: parent ? parent.width : 0
                            height: 34
                            radius: 6
                            color: dlArea.containsMouse ? theme.surfaceHover : "transparent"
                            Column {
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 4
                                Row {
                                    width: parent.width
                                    spacing: 6
                                    QbzIcon {
                                        name: "cloud-download"
                                        width: 14
                                        height: 14
                                        anchors.verticalCenter: parent.verticalCenter
                                        tintName: "accent"
                                    }
                                    Text {
                                        width: parent.width - 20 - 34
                                        text: dlRow.modelData.title || QbzSession.tr("Purchases", QbzSession.trRev)
                                        color: theme.textSecondary
                                        font.pixelSize: 12
                                        elide: Text.ElideRight
                                        anchors.verticalCenter: parent.verticalCenter
                                    }
                                    Text {
                                        width: 28
                                        text: (dlRow.modelData.percent || 0) + "%"
                                        color: theme.textMuted
                                        font.pixelSize: 11
                                        horizontalAlignment: Text.AlignRight
                                        anchors.verticalCenter: parent.verticalCenter
                                    }
                                }
                                Rectangle {
                                    width: parent.width
                                    height: 3
                                    radius: 1.5
                                    color: theme.borderSubtle
                                    Rectangle {
                                        width: parent.width * Math.max(0, Math.min(1, (dlRow.modelData.percent || 0) / 100))
                                        height: parent.height
                                        radius: parent.radius
                                        color: theme.accent
                                    }
                                }
                            }
                            MouseArea {
                                id: dlArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: {
                                    QbzPurchases.openAlbum(dlRow.modelData.id || "")
                                    QbzShell.navigateTo("purchase-album")
                                }
                            }
                        }
                    }
                }

                // Mini: the ring, with the detail on hover.
                Item {
                    visible: root.mini
                    width: parent.width
                    height: parent.height
                    QbzProgressRing {
                        anchors.centerIn: parent
                        size: 18
                        value: root.activeDownloadsOverall
                    }
                    MouseArea {
                        id: ringArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzShell.navigateTo("purchases")
                    }
                    ToolTip {
                        visible: ringArea.containsMouse
                        delay: 0
                        timeout: -1
                        text: root.activeDownloadsSummary
                        x: parent.width + 8
                        y: Math.round((parent.height - height) / 2)
                        contentItem: Text {
                            text: root.activeDownloadsSummary
                            color: theme.textPrimary
                            font.pixelSize: 11
                            font.weight: theme.weightMedium
                            leftPadding: 9
                            rightPadding: 9
                            topPadding: 6
                            bottomPadding: 6
                        }
                        background: Rectangle {
                            color: theme.surfaceElevated
                            radius: 4
                            border.width: 1
                            border.color: theme.borderMuted
                        }
                    }
                }
            }
        }

        // Hairline under the section nav — mounted with the nav block, not
        // on its own (Sidebar.slint:817 gates it on the same flag).
        Rectangle {
            visible: QbzShell.navInSidebar
            width: parent.width
            height: 1
            color: theme.borderSubtle
        }

        // ---- Playlists header toolbar (hidden in the mini state) -----
        Row {
            visible: !root.mini
            width: parent.width
            height: 22
            spacing: 4

            // Title (hidden while the search input is open).
            Text {
                visible: !root.searchOpen
                width: parent.width - 4 * 26
                height: parent.height
                text: QbzSession.tr("PLAYLISTS", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: 10
                font.letterSpacing: 1
                verticalAlignment: Text.AlignVCenter
            }
            // Inline search input (filters entries, recursive).
            Rectangle {
                visible: root.searchOpen
                width: parent.width - 4 * 26
                height: 22
                radius: 4
                color: theme.surfaceElevated
                border.width: 1
                border.color: theme.borderSubtle
                TextInput {
                    QbzTextEditMenu { }
                    id: searchEdit
                    anchors.fill: parent
                    anchors.leftMargin: 6
                    color: theme.textPrimary
                    font.pixelSize: 12
                    verticalAlignment: Text.AlignVCenter
                    clip: true
                    onTextEdited: QbzShell.sidebarSearch(text)
                    Text {
                        visible: searchEdit.text === ""
                        anchors.fill: parent
                        text: QbzSession.tr("Search playlists", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 12
                        verticalAlignment: Text.AlignVCenter
                    }
                }
            }

            // Search toggle.
            Rectangle {
                width: 22
                height: 22
                radius: 4
                color: (searchBtnArea.containsMouse || root.searchOpen) ? theme.surfaceHover : "transparent"
                QbzIcon {
                    name: "search"
                    width: 15
                    height: 15
                    anchors.centerIn: parent
                    tintName: (searchBtnArea.containsMouse || root.searchOpen) ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: searchBtnArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.searchOpen = !root.searchOpen
                        if (!root.searchOpen) {
                            searchEdit.text = ""
                            QbzShell.sidebarSearch("")
                        } else {
                            searchEdit.forceActiveFocus()
                        }
                    }
                }
            }
            // New playlist (+) — opens controls/PlaylistCreateModal.qml, the
            // port of primitives/CreatePlaylistModal.slint.
            //
            // IT USED TO BE A ONE-CALL SHORTCUT (`QbzShell.createPlaylist()`)
            // that created a playlist named "New Playlist" and navigated to
            // it. That shortcut had no way to set the name, the description,
            // the folder or the public flag — and, the reason it counts as a
            // regression rather than a rough edge, no way to set OFFLINE-ONLY.
            // It derived that from connectivity, so an online user could not
            // create a local playlist at all.
            //
            // NOT offline-gated, and that is deliberate: the modal's own D8
            // arm locks the offline-only toggle ON while the app is offline
            // and creates a LOCAL playlist. Dimming this row would take the
            // ONE way an account-less user makes a playlist away from exactly
            // the state they live in.
            Rectangle {
                width: 22
                height: 22
                radius: 4
                color: plusArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon {
                    name: "plus"
                    width: 16
                    height: 16
                    anchors.centerIn: parent
                    tintName: plusArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: plusArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzPlaylistEdit.openCreate()
                }
            }
            // Sort / more (...) menu.
            Rectangle {
                width: 22
                height: 22
                radius: 4
                color: sortBtnArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon {
                    name: "ellipsis"
                    width: 15
                    height: 15
                    anchors.centerIn: parent
                    tintName: sortBtnArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: sortBtnArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: sortMenu.open()
                }
                Popup {
                    id: sortMenu
                    // Slint puts this panel's left edge at `toolbar.width -
                    // 40px`. Resolved: the open sidebar's content column is
                    // 240 - 2*16 = 208 wide and the "..." button occupies
                    // 160…182 of it, so 208 - 40 = 168 is the button's own
                    // x + 8. This Popup is parented to the BUTTON, not to the
                    // row, so the 1:1 value is x: 8 — the old `parent.width +
                    // 18` (= 40) put it 32px too far right.
                    x: 8
                    y: 26
                    width: 200
                    padding: 6
                    closePolicy: Popup.CloseOnPressOutside
                    background: Rectangle {
                        // OPAQUE UNDER AMBIENT, unlike this root. Sidebar.slint
                        // :920-922 gives the panel the sidebar's own ambient
                        // treatment (surface-card @50%), and it was ported 1:1 —
                        // which made this the ONE menu surface in the app that
                        // goes translucent while the dynamic background is on.
                        // Every other menu/flyout (QbzContextMenu and therefore
                        // CardMenu / SidebarRowMenu / ViewModeMenu / the audio
                        // flyout, plus NavFlyout, SidebarFolderFlyout,
                        // QconnectFlyout, GenreFilterPopup, LocalFilterPopup)
                        // paints a flat opaque surface. A menu is TEXT the user
                        // is reading against artwork that moves under it, not
                        // chrome, so the outlier lost: owner ruling 2026-08-19,
                        // "la lectura se complica con la transparencia".
                        color: theme.surfaceCard
                        radius: 8
                        border.width: 1
                        border.color: theme.borderSubtle
                    }
                    contentItem: Column {
                        spacing: 1
                        Row {
                            leftPadding: 8
                            topPadding: 4
                            bottomPadding: 2
                            spacing: 6
                            QbzIcon { name: "arrow-up-down"; width: 13; height: 13; tintName: "muted" }
                            Text {
                                text: QbzSession.tr("Sort by", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: 11
                                verticalAlignment: Text.AlignVCenter
                            }
                        }
                        Repeater {
                            model: [
                                { "opt": "name", "label": QbzSession.tr("Name (A-Z)", QbzSession.trRev) },
                                { "opt": "recent", "label": QbzSession.tr("Recent", QbzSession.trRev) },
                                { "opt": "tracks", "label": QbzSession.tr("# of tracks", QbzSession.trRev) },
                                { "opt": "playcount", "label": QbzSession.tr("Play Count", QbzSession.trRev) },
                                { "opt": "custom", "label": QbzSession.tr("Custom", QbzSession.trRev) },
                            ]
                            delegate: Rectangle {
                                required property var modelData
                                width: parent ? parent.width : 0
                                height: 30
                                radius: 5
                                color: sortOptArea.containsMouse ? theme.surfaceHover : "transparent"
                                Row {
                                    anchors.fill: parent
                                    anchors.leftMargin: 8
                                    anchors.rightMargin: 8
                                    spacing: 6
                                    Text {
                                        width: parent.width - 20
                                        height: parent.height
                                        text: modelData.label
                                        color: theme.textSecondary
                                        font.pixelSize: 13
                                        font.weight: QbzShell.sidebarSortBy === modelData.opt ? theme.weightSemibold : theme.weightRegular
                                        verticalAlignment: Text.AlignVCenter
                                    }
                                    QbzIcon {
                                        visible: QbzShell.sidebarSortBy === modelData.opt
                                        name: QbzShell.sidebarSortAsc ? "chevron-up" : "chevron-down"
                                        width: 13
                                        height: 13
                                        anchors.verticalCenter: parent.verticalCenter
                                        tintName: "accent"
                                    }
                                }
                                MouseArea {
                                    id: sortOptArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        QbzShell.sidebarSetSort(modelData.opt)
                                        sortMenu.close()
                                    }
                                }
                            }
                        }
                        Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
                        // New folder / Manage playlists / Import / Refresh —
                        // all four LIVE (the Refresh row was missing from this
                        // port entirely and is restored here).
                        //
                        // `on` is the enabled flag and it drives everything at
                        // once: the hover fill, the pointer cursor, the glyph
                        // tint, the 0.4 dim AND whether the MouseArea exists.
                        // There is no way to light one row and leave another
                        // inert without it.
                        //
                        // Import is the only row that can be off, and only
                        // while offline — importing a playlist is Qobuz-only
                        // work.
                        //
                        // REFRESH STAYS LIT OFFLINE (reversed 2026-07-31 on the
                        // owner's smoke). The earlier decision dimmed it
                        // because QbzShell.reloadSidebar() called
                        // crate::reload_sidebar(), which early-returns offline
                        // — a lit row would have been a no-op, and a control
                        // that renders and no-ops is a defect. The bridge now
                        // calls crate::reload_sidebar_including_local()
                        // instead, which offline re-reads folders, folder
                        // membership, the hidden set and the LOCAL playlists
                        // from library.db and republishes (preserving the
                        // cached Qobuz set). That is a real refresh, and it is
                        // precisely the tree an account-less user owns — the
                        // owner hit an empty offline sidebar with no way to
                        // rebuild it.
                        //
                        // Every arm closes the menu FIRST. Slint's PopupWindow
                        // dismisses on any click, including inside itself,
                        // which is why its own items never call close(); Qt's
                        // CloseOnPressOutside does the opposite. This is the
                        // single most common silent divergence in this seam.
                        Repeater {
                            model: [
                                { "icon": "folder-plus", "label": QbzSession.tr("New folder", QbzSession.trRev),
                                  "act": "newFolder", "on": true },
                                { "icon": "library-big", "label": QbzSession.tr("Manage playlists", QbzSession.trRev),
                                  "act": "manage", "on": true },
                                // LIVE. The importer landed with this seam
                                // (src/playlist_import_bridge.rs +
                                // controls/PlaylistImportModal.qml), so contract
                                // §10 Q2 resolves to "land the importer" and
                                // there is no `typeof QbzPlaylistImport` guard
                                // here: the row is a real row, dimmed only
                                // while offline — the only row that dims.
                                { "icon": "import", "label": QbzSession.tr("Import", QbzSession.trRev),
                                  "act": "import", "on": !QbzSession.offline },
                                { "icon": "refresh-cw", "label": QbzSession.tr("Refresh", QbzSession.trRev),
                                  "act": "refresh", "on": true },
                            ]
                            delegate: Rectangle {
                                id: actRow
                                required property var modelData
                                readonly property bool rowEnabled: modelData.on === true
                                width: parent ? parent.width : 0
                                height: 30
                                radius: 5
                                color: (actRow.rowEnabled && actArea.containsMouse)
                                    ? theme.surfaceHover : "transparent"
                                opacity: actRow.rowEnabled ? 1.0 : 0.4
                                Row {
                                    anchors.fill: parent
                                    anchors.leftMargin: 8
                                    anchors.rightMargin: 8
                                    spacing: 8
                                    QbzIcon {
                                        name: actRow.modelData.icon
                                        width: 14
                                        height: 14
                                        anchors.verticalCenter: parent.verticalCenter
                                        tintName: actRow.rowEnabled ? "secondary" : "muted"
                                    }
                                    Text {
                                        width: parent.width - 22
                                        height: parent.height
                                        text: actRow.modelData.label
                                        color: actRow.rowEnabled ? theme.textSecondary : theme.textMuted
                                        font.pixelSize: 13
                                        verticalAlignment: Text.AlignVCenter
                                        elide: Text.ElideRight
                                    }
                                }
                                MouseArea {
                                    id: actArea
                                    anchors.fill: parent
                                    enabled: actRow.rowEnabled
                                    hoverEnabled: actRow.rowEnabled
                                    cursorShape: actRow.rowEnabled
                                        ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onClicked: {
                                        sortMenu.close()
                                        var a = actRow.modelData.act
                                        if (a === "newFolder") QbzFolderEdit.openCreate()
                                        else if (a === "manage") QbzPlaylistManager.navigate()
                                        else if (a === "import") QbzPlaylistImport.open()
                                        else if (a === "refresh") QbzShell.reloadSidebar()
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Collapse chevron (down = expanded, right = collapsed).
            Rectangle {
                width: 22
                height: 22
                radius: 4
                color: collapseArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon {
                    name: root.playlistsCollapsed ? "chevron-right" : "chevron-down"
                    width: 15
                    height: 15
                    anchors.centerIn: parent
                    tintName: collapseArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: collapseArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzBridge.settingsBool("sidebar-playlists-collapsed", !root.playlistsCollapsed)
                }
            }
        }

        // ---- Playlist tree (folders + playlists, sidebar_qt.rs) --------
        Item {
            visible: !root.playlistsCollapsed
            width: parent.width
            // This is the element that FILLS (Sidebar.slint gives the playlist
            // Rectangle `vertical-stretch: 1`): everything above it — the nav
            // block, the hairline, the 22px toolbar — takes its natural height
            // and the list eats the rest, down to the Column's bottom margin.
            // `root.dockReserve` keeps the rows clear of the Large-NPB cover
            // (0 in every other mode). Clamped so a short window shrinks the
            // list to nothing instead of giving it a negative height.
            //
            // There used to be a trailing `Item { height: 0 }` "filler" after
            // this one and a matching `- 16` here to pay for the Column spacing
            // it introduced. Net effect: the list stopped 16px short and the
            // sidebar read as too short against Slint. Both are gone — the
            // list is the last child, so its bottom IS the Column's bottom.
            height: Math.max(0, parent.height - y - root.dockReserve)

            Flickable {
                id: plFlick
                anchors.fill: parent
                clip: true
                contentWidth: width
                contentHeight: plColumn.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: plColumn
                    width: parent.width
                    spacing: 2

                    Repeater {
                        model: root.entries
                        delegate: Rectangle {
                            id: plRow
                            required property var modelData
                            property bool isFolder: modelData.kind === "folder"
                            // Slint SidebarRow.use-collage: opt-OUT setting AND
                            // at least one cover; otherwise the list-music glyph.
                            property bool useCollage: !isFolder
                                && QbzShell.sidebarPlaylistCollage
                                && modelData.covers.length > 0
                            property bool isActive: !isFolder && modelData.id === root.activePlaylistId
                            // Track-drag drop target (Sidebar.slint SidebarRow):
                            // the row self-detects the drop from the shared
                            // window-coord pointer and claims/releases it.
                            property bool dropHot: false
                            function recomputeDrop() {
                                if (isFolder || !QbzShell.dragActive) {
                                    if (dropHot) { QbzShell.dragSetOver(""); dropHot = false }
                                    return
                                }
                                const tl = mapToItem(null, 0, 0)
                                const hot = QbzShell.dragX >= tl.x && QbzShell.dragX <= tl.x + width
                                    && QbzShell.dragY >= tl.y && QbzShell.dragY <= tl.y + height
                                if (hot && !dropHot) QbzShell.dragSetOver(modelData.id)
                                else if (!hot && dropHot) QbzShell.dragSetOver("")
                                dropHot = hot
                            }
                            Connections {
                                target: QbzShell
                                function onDragXChanged() { recomputeDrop() }
                                function onDragYChanged() { recomputeDrop() }
                                function onDragActiveChanged() { recomputeDrop() }
                            }
                            width: plColumn.width
                            // Mini: nested rows collapse to nothing (their
                            // folder opens the flyout instead, Sidebar.slint:
                            // 213-215), top-level rows become the SAME square
                            // the nav rows are — a 34px row in a 34px track.
                            // Slint keeps 32px here; the 2px come from the
                            // narrower rail (theme.sidebarMiniRow).
                            height: root.mini
                                ? (modelData.indent ? 0 : theme.sidebarMiniRow)
                                : 32
                            visible: !(root.mini && modelData.indent)
                            radius: 6
                            // success-bg + success-border while hot. These used
                            // to be the Slint literals "#3fae6a1a" /
                            // "#3fae6a4d" copied verbatim, which is a real
                            // colour bug: Slint's 8-digit form is #RRGGBBAA and
                            // Qt's is #AARRGGBB, so the drop target rendered
                            // olive-brown at 25% alpha instead of success green
                            // at 15% / 35%. The tokens carry the converted
                            // values (theme.successBg = #263fae6a).
                            color: dropHot ? theme.successBg
                                : ((rowArea.containsMouse || isActive) ? theme.surfaceHover : "transparent")
                            border.width: dropHot ? 1 : 0
                            border.color: theme.successBorder

                            Row {
                                anchors.fill: parent
                                anchors.leftMargin: root.mini ? 0 : (modelData.indent ? 24 : 8)
                                anchors.rightMargin: root.mini ? 0 : 6
                                spacing: 8

                                // Leading icon slot: 24px fixed while open (so
                                // every row's name starts at the same x), the
                                // WHOLE row in mini so the lone glyph lands on
                                // the rail's centre line — Slint gets this from
                                // `alignment: mini ? center : stretch`
                                // (Sidebar.slint:230), which a QML Row has no
                                // equivalent for. Without it the slot is the
                                // only visible child, sits at x=0, and the
                                // playlist/folder glyphs render 12px LEFT of
                                // the nav glyphs above them (owner screenshot).
                                // Same idiom as NavRow's slot above.
                                Item {
                                    width: root.mini ? parent.width : 24
                                    height: parent.height
                                    // Folder icon (accent). Every glyph in this
                                    // slot is placed with rounded arithmetic
                                    // (Sidebar.slint:245-246): `centerIn` of an
                                    // odd 15px glyph in an even track lands on a
                                    // half pixel and blurs it.
                                    QbzIcon {
                                        visible: isFolder
                                        name: modelData.expanded && !root.mini ? "folder-open" : "folder"
                                        width: 15
                                        height: 15
                                        x: Math.round((parent.width - width) / 2)
                                        y: Math.round((parent.height - height) / 2)
                                        tintName: "accent"
                                    }
                                    // Playlist micro-collage (or list-music
                                    // glyph) — the port of
                                    // primitives/SidebarPlaylistCollage.slint:
                                    // a 20px box, 3px radius, surface-elevated
                                    // backing, 1px seam.
                                    //
                                    // TWO arms, not one. The reference splits
                                    // on the cover count (.slint:27-68):
                                    // 1-3 covers -> ONE full-bleed cover, 4+ ->
                                    // the 2x2 grid. This used to be an
                                    // unconditional 2x2 of hard-coded 10x10
                                    // tiles with no seam, so a playlist with a
                                    // single cover drew one 10x10 tile in the
                                    // top-left quadrant and left three quarters
                                    // of the box empty.
                                    //
                                    // Covers stay on the host's already-resolved
                                    // `root.coverMap` (filled by the ONE
                                    // `sidebarArtworkWindow` dispatch in
                                    // parseEntries) — this is not a second
                                    // artwork seam.
                                    Item {
                                        id: collage
                                        visible: useCollage
                                        width: 20
                                        height: 20
                                        x: Math.round((parent.width - width) / 2)
                                        y: Math.round((parent.height - height) / 2)

                                        // `n`, straight off SidebarEntry.cover-count
                                        // (.slint:24). sidebar_qt.rs already
                                        // de-duplicates and caps `covers` at 4
                                        // (SidebarEntry::covers), so this is 0..4
                                        // and no dedup pass is owed here.
                                        readonly property int n: useCollage ? plRow.modelData.covers.length : 0
                                        // .slint:25 `gap: 1px`. NOT the mosaic
                                        // card's 2px seam — the micro-collage is
                                        // its own component with its own number.
                                        readonly property real gap: 1

                                        // Backing surface. A Rectangle's OWN
                                        // rounded fill paints correctly; only its
                                        // CHILDREN need the mask below, which is
                                        // exactly the distinction the old code
                                        // missed.
                                        Rectangle {
                                            anchors.fill: parent
                                            radius: 3
                                            color: theme.surfaceElevated
                                        }

                                        // The rounded outer shape as a mask
                                        // texture. `clip: true` in Qt Quick is a
                                        // RECTANGULAR scissor and IGNORES
                                        // `radius`, which is why the old
                                        // `Rectangle { radius: 3; clip: true }`
                                        // shipped square collages: the fill was
                                        // round, the four covers painted on top
                                        // of it were not (owner report). The
                                        // documented fix is the `layer.enabled` +
                                        // `MultiEffect { maskEnabled }` idiom —
                                        // same mechanism, same constants, as
                                        // cards/PlaylistCollage.qml:264-305 and
                                        // theme/RoundedImage.qml.
                                        Item {
                                            id: collageMask
                                            anchors.fill: parent
                                            visible: false
                                            // A layer renders even when its item
                                            // is invisible — that IS the mask
                                            // idiom — so the gate has to be
                                            // `useCollage`, not `visible`.
                                            // PlaylistCollage.qml gates its own
                                            // two layers on `tiles.length > 0`
                                            // for the same reason, and it matters
                                            // MORE here: this delegate is inside
                                            // a Repeater over the whole playlist
                                            // tree, so an ungated pair of layers
                                            // would cost two FBOs on every folder
                                            // row and every glyph row too.
                                            layer.enabled: useCollage && !root._noShaders
                                            layer.smooth: true
                                            Rectangle {
                                                anchors.fill: parent
                                                radius: 3
                                                color: "#ffffff"
                                            }
                                        }

                                        // ONE layer + ONE mask for the whole
                                        // collage, not one per tile. `clip` stays
                                        // on the container because
                                        // PreserveAspectCrop paints OUTSIDE its
                                        // item's bounds unless clipped — it just
                                        // cannot round on its own.
                                        Item {
                                            anchors.fill: parent
                                            clip: true
                                            layer.enabled: useCollage && !root._noShaders
                                            layer.smooth: true
                                            layer.effect: MultiEffect {
                                                maskEnabled: true
                                                maskSource: collageMask
                                                // 0.5 / 1.0 — MEASURED; the full
                                                // table is in
                                                // theme/RoundedImage.qml. Qt's
                                                // mask is a smoothstep CENTRED on
                                                // maskThresholdMin of width
                                                // maskSpreadAtMin, so this is the
                                                // only pair that maps mask alpha
                                                // 0..1 onto 0..1. (0.0, 0.0)
                                                // collapses it to step() and the
                                                // corners come back HARD; (0.0,
                                                // > 0) clamps everything opaque
                                                // and disables the mask outright.
                                                // Do not "restore the defaults".
                                                maskThresholdMin: 0.5
                                                maskSpreadAtMin: 1.0
                                            }

                                            // 1-3 covers -> a single full-bleed
                                            // cover (.slint:28-33). `image-fit:
                                            // cover` = PreserveAspectCrop.
                                            Image {
                                                anchors.fill: parent
                                                visible: collage.n >= 1 && collage.n < 4
                                                // Empty out of arm so the hidden
                                                // branch holds no texture. Reading
                                                // `visible` back gives the
                                                // EFFECTIVE visibility, so this
                                                // also drops the texture for a row
                                                // the mini rail collapses — and
                                                // re-binds when it comes back.
                                                source: visible ? (root.coverMap[plRow.modelData.covers[0]] || "") : ""
                                                fillMode: Image.PreserveAspectCrop
                                                asynchronous: true
                                            }

                                            // 4+ covers -> a 2x2 grid of
                                            // (size - gap) / 2 tiles with the 1px
                                            // seam (.slint:36-68). Grid `spacing`
                                            // IS that seam: 9.5 + 1 + 9.5 = 20.
                                            Grid {
                                                anchors.fill: parent
                                                columns: 2
                                                spacing: collage.gap
                                                Repeater {
                                                    // Empty below four — the arm
                                                    // is not merely hidden, it
                                                    // holds no tiles and no
                                                    // textures.
                                                    model: collage.n >= 4 ? plRow.modelData.covers.slice(0, 4) : []
                                                    delegate: Image {
                                                        required property string modelData
                                                        width: (collage.width - collage.gap) / 2
                                                        height: (collage.height - collage.gap) / 2
                                                        source: root.coverMap[modelData] || ""
                                                        fillMode: Image.PreserveAspectCrop
                                                        asynchronous: true
                                                        // Per-tile scissor: a crop
                                                        // fit overflows its own
                                                        // bounds, and the
                                                        // container's clip is 20px
                                                        // wide — without this a
                                                        // wide cover bleeds into
                                                        // the neighbouring cell
                                                        // instead of being cropped
                                                        // to it (Slint's
                                                        // `image-fit: cover`
                                                        // clips per element).
                                                        clip: true
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    QbzIcon {
                                        visible: !isFolder && !useCollage
                                        name: "list-music"
                                        width: 15
                                        height: 15
                                        x: Math.round((parent.width - width) / 2)
                                        y: Math.round((parent.height - height) / 2)
                                        tintName: (rowArea.containsMouse || isActive) ? "textPrimary" : "muted"
                                    }
                                }
                                // Name (hidden in mini). A Row is a positioner
                                // with no stretch, so this width must reserve
                                // EVERY later cell + its spacing or the excess
                                // is pushed past the row's right edge and the
                                // `clip: true` on the Flickable eats it.
                                // (Sidebar.slint gets this free: its
                                // HorizontalLayout stretches the name cell.)
                                // Reserve: 8 + 24 icon slot always; for a
                                // FOLDER also 8 + 14 chevron, plus 8 + the
                                // count label while it is laid out. The old
                                // flat -22 ignored the count, pushing the
                                // chevron ~15px past the clip — which is why
                                // the caret vanished on exactly the folders
                                // that HAVE children (owner report).
                                Text {
                                    visible: !root.mini
                                    width: Math.max(0, parent.width - 32
                                        - (isFolder
                                            ? 22 + (countLabel.visible
                                                ? countLabel.implicitWidth + 8 : 0)
                                            : 0)
                                        // The local mark is a sibling AFTER
                                        // this Text, so its width has to come
                                        // out of the name's reserve or the
                                        // glyph lands past the row's clip.
                                        - (modelData.isLocal === true ? 20 : 0))
                                    height: parent.height
                                    text: modelData.name
                                    color: (rowArea.containsMouse || isActive) ? theme.textPrimary : theme.textSecondary
                                    font.pixelSize: 13
                                    font.weight: isFolder ? theme.weightSemibold : theme.weightRegular
                                    verticalAlignment: Text.AlignVCenter
                                    elide: Text.ElideRight
                                }
                                // LOCAL playlist mark. A local playlist has no
                                // Qobuz side at all — this is how the row says
                                // so, and it is the only visual difference
                                // between the two kinds in the list.
                                QbzIcon {
                                    visible: !root.mini && modelData.isLocal === true
                                    name: "hard-drive"
                                    width: 12
                                    height: 12
                                    anchors.verticalCenter: parent.verticalCenter
                                    tintName: "muted"
                                }
                                // Folder count (hover) + chevron. The count is
                                // hover-FADED, not hidden — opacity does not
                                // free layout space, so it must stay in the
                                // name's reserve above at all times.
                                Text {
                                    id: countLabel
                                    visible: !root.mini && isFolder && modelData.count > 0
                                    height: parent.height
                                    text: modelData.count
                                    color: theme.textMuted
                                    font.pixelSize: 11
                                    verticalAlignment: Text.AlignVCenter
                                    opacity: rowArea.containsMouse ? 1.0 : 0.0
                                }
                                QbzIcon {
                                    visible: !root.mini && isFolder
                                    name: modelData.expanded ? "chevron-down" : "chevron-right"
                                    width: 14
                                    height: 14
                                    anchors.verticalCenter: parent.verticalCenter
                                    tintName: rowArea.containsMouse ? "textPrimary" : "muted"
                                }
                            }
                            // Active-row redundant shape cue: 3px accent bar.
                            Rectangle {
                                visible: isActive
                                x: 0
                                width: 3
                                height: parent.height - 12
                                anchors.verticalCenter: parent.verticalCenter
                                radius: 1.5
                                color: theme.accent
                            }
                            MouseArea {
                                id: rowArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                // Right-press anywhere on the row opens its
                                // context menu (Sidebar.slint:389-397). The old
                                // hover "..." button was removed upstream
                                // because the scrollbar overlapped it and ate
                                // the click — do not reintroduce it.
                                acceptedButtons: Qt.LeftButton | Qt.RightButton
                                // Mini: the row is an icon, so hovering names
                                // it (Sidebar.slint:342-349). No-op while open.
                                onContainsMouseChanged: {
                                    if (containsMouse) root.showMiniTip(plRow, plRow.modelData)
                                    else root.hideMiniTip(plRow.modelData.id)
                                }
                                onClicked: function (mouse) {
                                    // Slint drops the bubble on click so it
                                    // does not linger behind the flyout.
                                    root.hideMiniTip(plRow.modelData.id)
                                    if (mouse.button === Qt.RightButton) {
                                        // SUPPRESSED ON THE RAIL. Slint anchors
                                        // this menu at `row.width - 210px`,
                                        // which on a 34px rail row is deeply
                                        // negative; Qt's viewport clamp would
                                        // silently drop it at x=8, which is not
                                        // the reference's placement, just the
                                        // clamp.
                                        if (root.mini)
                                            return
                                        var lx = plRow.width - rowMenu.menuWidth
                                        if (isFolder)
                                            rowMenu.openForFolder(plRow.modelData, plRow, lx, 28)
                                        else
                                            rowMenu.openForPlaylist(plRow.modelData, plRow, lx, 28)
                                        return
                                    }
                                    if (isFolder) {
                                        // Mini: the nested rows are hidden, so
                                        // a toggle would change NOTHING on
                                        // screen — the contents open in a
                                        // flyout to the right instead
                                        // (Sidebar.slint:355-377).
                                        if (root.mini) root.openFolderFlyout(plRow, plRow.modelData)
                                        else QbzShell.sidebarToggleFolder(modelData.id)
                                    } else {
                                        root.activePlaylistId = modelData.id
                                        QbzBridge.openPlaylist(modelData.id)
                                    }
                                }
                            }
                        }
                    }

                    // Empty state. NOTE: Slint does NOT gate this on mini
                    // (Sidebar.slint:1077-1082) and word-wraps the sentence
                    // inside the 48px track; keeping the gate here is a
                    // deliberate, owner-visible divergence.
                    Text {
                        visible: !root.mini && root.entries.length === 0 && !QbzSession.offline
                        width: plColumn.width
                        text: QbzSession.tr("No playlists yet.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }
                }
            }

            // List scrollbar (Sidebar.slint:1090-1137). The shared
            // QbzScrollBar — the port's ListScrollbar replica — rather than a
            // third scrollbar implementation. Two accepted deviations from
            // the sidebar's bespoke Slint one: it reveals on SCROLL (900ms
            // auto-hide) instead of on row-hover, and its thumb is the
            // standard 8/10px instead of the sidebar's thinner 3-6px.
            // Placed in the sidebar's right PADDING (negative margin), like
            // Slint's `x: parent.width - width + (mini ? 8 : Spacing.md)`, so
            // the gutter never overlays the rows and cannot eat their clicks.
            //
            // MINI CAVEAT after the optical re-centring above: the right pad is
            // 4px now, so an 8px gutter pinned to the rail's edge shares its
            // left 4px with the rows. That is fine to LOOK at (an 8px-wide,
            // auto-hiding indicator whose thumb grazes the pill's rounded right
            // edge) but not to CLICK through, so the mini instance is
            // `enabled: false`: indicator only, no drag / click-to-position.
            // Nothing is lost — the wheel scrolls the rail, and a 3px-of-travel
            // drag inside a 50px rail was never a real affordance. The open
            // sidebar keeps the full interactive bar.
            QbzScrollBar {
                target: plFlick
                enabled: !root.mini
                width: root.mini ? 8 : 10
                anchors.right: parent.right
                anchors.rightMargin: -(root.mini ? root.miniPadRight : theme.spacingMd)
                anchors.top: parent.top
                anchors.bottom: parent.bottom
            }
        }
    }
}
