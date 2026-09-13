// Local album detail — QML port of album/LocalAlbumView.slint, a ROUTED
// PAGE (not a pane inside the Local Library view, which is how this port
// used to render it).
//
// Homologated 1:1 with the Qobuz AlbumView: whole-page Flickable, the same
// header proportions and action row, the toolbar (quality badge + track
// search), the column header, and source-aware track rows. The intentional
// differences vs the Qobuz page are the Slint's own: no label/awards
// sidebar, no Qobuz context menus, and the local-only version picker.
//
// Local actions ONLY: play all / shuffle / edit tags / add to playlist /
// add to Mixtape. A multi-artist album gets the "+N more artists" expander;
// a multi-disc album gets the disc dividers with their per-disc ⋯ menu.
//
// ONE deliberate ADDITION over the reference (owner, 2026-07-31): the
// toolbar's multi-select toggle and its bulk bar. The Slint puts those on the
// QOBUZ album page (AlbumPageView.slint:752-808) and not on the local one,
// which is an asymmetry, not a design — the local page is where the owner
// asked for them. See the `multiSelect` block below for the wiring.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"
import "local"

Rectangle {
    id: root

    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    radius: 12

    // Same reach as Qobuz AlbumView: through the divider, the following
    // spacer and half of the track-toolbar band.
    readonly property int atmoReach: 1 + 8 + 26

    QbzTheme { id: theme }

    // ---------------------------- document -------------------------------
    function parseDoc(json, fallback) {
        if (json === "") return fallback
        try { return JSON.parse(json) } catch (e) { return fallback }
    }
    readonly property var doc: parseDoc(QbzLocal.localAlbumJson, null)
    readonly property var album: doc ? doc.album : null
    readonly property var tracks: doc ? doc.tracks : []
    readonly property var versions: album && album.versions ? album.versions : []
    // Same global preference as the Qobuz AlbumView. The document value is
    // only the cold-start fallback; settingsJson makes a toggle in any open
    // album update every other album immediately.
    readonly property bool compactHeaderPref: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                var d = JSON.parse(raw)
                if (d.compactAlbumHeader !== undefined)
                    return d.compactAlbumHeader === true
            } catch (e) { /* fall through to the document copy */ }
        }
        return doc && doc.compactHeader === true
    }
    readonly property bool headerGradientPref: {
        var raw = QbzBridge.settingsJson
        if (raw && raw.length > 2) {
            try {
                var d = JSON.parse(raw)
                if (d.albumHeaderGradient !== undefined)
                    return d.albumHeaderGradient === true
            } catch (e) { /* fall through to the document copy */ }
        }
        return !doc || doc.headerGradient !== false
    }
    readonly property bool headerAtmoOn: headerGradientPref && !ambientOn
    readonly property bool headerLight: headerGradientPref || ambientOn
    readonly property color hdrStrong: headerLight ? "#ffffff" : theme.textPrimary
    readonly property color hdrBody: headerLight ? "#e0ffffff" : theme.textSecondary
    readonly property bool hdrOverlay: headerLight
    property bool headerLayoutReady: false
    property int headerLayoutEpoch: 0
    property real headerSavedOffset: 0
    property bool headerSnapshotValid: false
    function toggleCompactHeader() {
        root.headerSavedOffset = flick.contentY - flick.originY
        root.headerSnapshotValid = true
        QbzBridge.settingsBool("compact-album-header", !root.compactHeaderPref)
    }
    onCompactHeaderPrefChanged: {
        if (!root.headerLayoutReady)
            return
        var savedOffset = root.headerSnapshotValid
            ? root.headerSavedOffset
            : flick.contentY - flick.originY
        root.headerSnapshotValid = false
        var epoch = ++root.headerLayoutEpoch
        Qt.callLater(function () {
            if (epoch !== root.headerLayoutEpoch)
                return
            var minY = flick.originY
            var maxY = minY + Math.max(0, flick.contentHeight - flick.height)
            flick.contentY = Math.max(minY,
                Math.min(minY + savedOffset, maxY))
        })
    }
    readonly property var allArtists: {
        if (!album) return []
        var raw = (album.allArtists || "").split(",")
        var out = []
        for (var i = 0; i < raw.length; i++) {
            var n = raw[i].trim()
            if (n !== "" && out.indexOf(n) < 0) out.push(n)
        }
        return out
    }
    // The Slint's `info-line` — built in Rust there, derived here from the
    // fields the album row already carries.
    readonly property string infoLine: {
        if (!album) return ""
        var parts = []
        if ((album.year || "") !== "") parts.push(album.year)
        parts.push((album.trackCount || 0) + " "
                   + QbzSession.tr("tracks", QbzSession.trRev))
        if ((album.duration || "") !== "") parts.push(album.duration)
        if ((album.format || "") !== "") parts.push(album.format.toUpperCase())
        return parts.join("  •  ")
    }
    readonly property string compactInfoLine: {
        if (!album) return ""
        var parts = []
        if ((album.year || "") !== "") parts.push(album.year)
        parts.push((album.trackCount || 0) + " "
                   + QbzSession.tr("tracks", QbzSession.trRev))
        return parts.join("  •  ")
    }

    property string trackQuery: ""
    // Client-side track search: an album's track list is bounded, so the
    // Slint's LocalAlbumActions.search is a pure view filter here.
    property real _deriveMs: 0
    // Cuánto tarda la vista en existir SIN sus filas: si el costo no está en
    // las filas ni en el derive, tiene que estar aquí (portada, versiones,
    // lista de artistas, toolbar).
    property double _mountedAt: Date.now()
    Component.onCompleted: {
        console.info(albTiming, "[albtiming] arbol de la vista construido en "
            + (Date.now() - root._mountedAt) + "ms")
        root.headerLayoutReady = true
    }
    readonly property var visibleTracks: {
        var _dt = Date.now()
        var _r = root._deriveImpl()
        // El trabajo, medido AQUÍ. La versión anterior guardaba el tiempo en
        // un Qt.callLater, que corre en la siguiente vuelta del event loop —
        // así que medía "cuánto tardó el loop en desocuparse", no el filtro.
        // Salía pegado a to-idle porque era el mismo número con otro nombre.
        root._deriveMs = Date.now() - _dt
        return _r
    }
    function _deriveImpl() {
        if (trackQuery === "") return tracks
        var q = trackQuery.toLowerCase()
        var out = []
        for (var i = 0; i < tracks.length; i++) {
            if ((tracks[i].title || "").toLowerCase().indexOf(q) >= 0) out.push(tracks[i])
        }
        return out
    }
    // ------------------------- multi-select ------------------------------
    // The Slint's LOCAL album page has no bulk bar; its QOBUZ one does
    // (album/AlbumPageView.slint:752-808 — the 30x30 square toggle in the
    // toolbar beside the search box, then MultiSelectBar over the rows).
    // Owner call 2026-07-31: close that asymmetry here, in the local page.
    //
    // Selection lives in QML, exactly as the two Local Library tabs keep
    // theirs (LocalLibraryView.qml:684-720) — the ids go down per action, so
    // `select-all` / `clear` never reach Rust. Scope is "track": the album
    // detail's rows ARE `LocalState.detail_raw` (local_album_actions.rs:154
    // caches every version's rows there), which is one of the two caches
    // `local_bulk::resolve_blocking` reads for that scope, so a Plex row with
    // no DB id resolves too.
    property bool multiSelect: false
    property var selected: ({})
    readonly property int selectedCount: Object.keys(selected).length
    // A republish means the track list changed underneath the selection — a
    // different album, or the version picker swapping every row for another
    // physical copy's. Both invalidate the ids, so the selection goes and
    // select mode with it (the same "leaving drops the selection" contract
    // `local_bulk::set_select_mode` documents for the tree rail).
    onDocChanged: { multiSelect = false; selected = ({}); sel.anchorId = "" }
    /// Excel-style selection lives in ONE place — controls/SelectionModel.qml
    /// holds the anchor and the Shift-range rule; this view keeps owning its
    /// map. Ranges run over `visibleTracks`, not `tracks`: the search box is a
    /// view filter, and a range over rows the user cannot see is not a range
    /// they asked for — the same call `bulkAction`'s select-all already makes.
    SelectionModel { id: sel }
    function toggleSelected(id, mods) {
        selected = sel.next(selected, id, visibleTracks,
                            mods === undefined ? Qt.NoModifier : mods)
    }
    /// Selected ids in the order they are ON SCREEN, so a bulk enqueue lands
    /// in disc/track order. `Object.keys` on the selection map would sort the
    /// numeric-looking keys ascending by row id, which is the INSERT order of
    /// the scan, not the album's. Iterates `tracks`, not `visibleTracks`, so a
    /// selection made before typing in the search box still enqueues whole.
    function selectedIdsInOrder() {
        var out = []
        for (var i = 0; i < tracks.length; i++) {
            if (selected[tracks[i].id] === true) out.push(tracks[i].id)
        }
        return out
    }
    function bulkAction(action) {
        if (action === "clear") { selected = ({}); sel.anchorId = ""; return }
        if (action === "select-all") {
            // What the user can SEE: the search box is a view filter, so
            // "select all" means the filtered set (LocalLibraryView.qml:715).
            var s = {}
            for (var i = 0; i < visibleTracks.length; i++) s[visibleTracks[i].id] = true
            selected = s
            return
        }
        QbzLocal.bulkAction("track", JSON.stringify(selectedIdsInOrder()), action)
    }

    // --- Ctrl+A / Escape hotkeys interface (2026-08-03 hotkeys-port §4.6) --
    // The duck-typed seam the AppShell router calls.
    readonly property bool multiSelectOn: root.multiSelect
    function selectAll() {
        if (!root.multiSelect) root.multiSelect = true
        root.bulkAction("select-all")
    }
    function exitMultiSelectMode() {
        // Same "leaving drops the selection" contract the toolbar toggle
        // carries (:371-373).
        if (root.multiSelect) { root.multiSelect = false; root.selected = ({}) }
    }

    // Disc divider before the first row of each disc on a multi-disc album
    // (0 = flat list, as the Slint's disc-header-number).
    // ---- INSTRUMENTACIÓN (qbz.nav.timing) -------------------------------
    // Dos intentos fallaron en esta vista: la ventana en las filas bajó
    // 2242 -> 1485 ms, y quitar el O(n²) del disc-header no movió nada
    // (1417 ms). Deja de razonar y mide: cuántas filas se construyen de
    // verdad, cuánto cuesta el derive, y cuánto tarda cada tramo.
    LoggingCategory {
        id: albTiming
        name: "qbz.nav.timing"
        defaultLogLevel: LoggingCategory.Warning
    }
    property double _t0: Date.now()
    property int _rowsBuilt: 0
    property int _hdrsBuilt: 0
    // El contador anterior era ACUMULATIVO y nunca se reiniciaba, así que
    // "253 filas sobre 247 tracks" podía significar dos cosas opuestas: banda
    // rota construyendo todo, o banda buena con el usuario recorriendo el
    // álbum entero. No discriminaba nada. Ahora se reinicia con cada lista y
    // se reporta en una ventana FIJA, antes de que nadie haga scroll.
    onVisibleTracksChanged: {
        root._rowsBuilt = 0
        root._hdrsBuilt = 0
        settleProbe.restart()
    }
    Timer {
        id: settleProbe
        interval: 900
        repeat: false
        onTriggered: console.info(albTiming,
            "[albtiming] tracks=" + root.visibleTracks.length
            + " derive=" + root._deriveMs + "ms"
            + " filas en 900ms=" + root._rowsBuilt
            + " encabezados=" + root._hdrsBuilt
            + " multiDisc=" + root.multiDisc
            + " versiones=" + (root.versions ? root.versions.length : -1))
    }

    /// Does this album span more than one disc? Computed ONCE per track list
    /// instead of once per row.
    ///
    /// It used to live inside `discHeader(i)`, which is called for every row —
    /// so the scan ran N times over N tracks. O(n²), and the early `break`
    /// hid it on exactly the albums that did not matter: a multi-disc album
    /// bails on the first track of disc 2, while a SINGLE-disc album never
    /// bails and walks the whole list every time. On a 247-track single-disc
    /// album that is ~61,000 iterations, and it was most of the 1485 ms this
    /// view still spent settling after its rows were already windowed.
    ///
    /// The local `list` binding matters too: `visibleTracks` is a `var` on the
    /// root, so each `visibleTracks[j]` in the old loop was a property lookup
    /// through the QML object rather than a plain array index.
    readonly property bool multiDisc: {
        var list = root.visibleTracks
        for (var j = 0; j < list.length; j++) {
            if ((list[j].disc || 1) > 1) return true
        }
        return false
    }

    function discHeader(i) {
        var t = visibleTracks[i]
        if (!t) return 0
        if (!root.multiDisc) return 0
        if (i === 0) return t.disc || 1
        return (visibleTracks[i - 1].disc || 1) !== (t.disc || 1) ? (t.disc || 1) : 0
    }

    // Cover — the same id-keyed artwork channel every local surface uses.
    property var artMap: ({})
    property string headerColor: ""
    property string headerAtmosphere: ""
    property string headerAtmosphereKey: ""
    property string headerAtmospherePath: ""
    function requestHeaderAtmosphere(key, path) {
        if (!key || !path)
            return
        if (root.headerAtmosphereKey === key
                && root.headerAtmospherePath === path)
            return
        root.headerAtmosphereKey = key
        root.headerAtmospherePath = path
        QbzLocal.albumHeaderAtmosphere(key, path)
    }
    Connections {
        target: QbzLocal
        function onLocalArtworkReady(key, path) {
            var m = root.artMap
            m[key] = path
            root.artMap = Object.assign({}, m)
            if (root.album && key === (root.album.artKey || ""))
                root.requestHeaderAtmosphere(key, path)
        }
        function onLocalAlbumAtmosphereReady(key, path, tint, atmosphere) {
            if (!root.album || key !== (root.album.artKey || "")
                    || key !== root.headerAtmosphereKey
                    || path !== root.headerAtmospherePath)
                return
            root.headerColor = tint
            root.headerAtmosphere = atmosphere
        }
    }
    /// The disc rows the document publishes for a MULTI-disc album
    /// (local_album_actions.rs::disc_rows). Empty on a single-disc album.
    readonly property var discRows: (root.doc && root.doc.discs) || []
    function discInfo(n) {
        for (var i = 0; i < root.discRows.length; i++)
            if ((root.discRows[i].disc || 0) === n)
                return root.discRows[i]
        return null
    }
    function discArt(disc) {
        if (!disc) return ""
        return (disc.artKey && root.artMap[disc.artKey])
            || disc.cover || ""
    }
    /// TRUE when the discs genuinely have DIFFERENT covers.
    ///
    /// The id-keyed path handles embedded and remote art; `DiscRow.cover` is
    /// the direct local-file fallback. This still guards the honest case — a
    /// box that really ships one shared cover — where drawing N copies is noise.
    readonly property bool discArtDistinct: {
        var seen = {}
        for (var i = 0; i < root.discRows.length; i++) {
            var p = root.discArt(root.discRows[i])
            if (p === "") return false
            if (seen[p]) return false
            seen[p] = true
        }
        return root.discRows.length > 1
    }
    function requestHeaderArtwork() {
        var keys = []
        if (album && album.artKey) keys.push(album.artKey)
        for (var i = 0; i < discRows.length; i++)
            if (discRows[i].artKey) keys.push(discRows[i].artKey)
        if (keys.length > 0) QbzLocal.artworkWindow(JSON.stringify(keys))
    }
    onAlbumChanged: {
        var key = root.album ? (root.album.artKey || "") : ""
        if (key !== root.headerAtmosphereKey) {
            root.headerColor = ""
            root.headerAtmosphere = ""
            root.headerAtmosphereKey = ""
            root.headerAtmospherePath = ""
        }
        root.requestHeaderArtwork()
        var path = key ? (root.artMap[key] || "") : ""
        if (path !== "") root.requestHeaderAtmosphere(key, path)
    }
    onDiscRowsChanged: requestHeaderArtwork()

    // ------------------------- skeleton pulse ----------------------------
    // ONE 900ms Timer for the whole page. GATING RULE: freeze on NOT VISIBLE
    // (view hidden / window minimized), NEVER on lost focus — a tiling
    // desktop keeps windows visible and unfocused.
    property bool skelPhase: false
    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    readonly property bool pageLoading: QbzLocal.localAlbumLoading
        && root.tracks.length === 0
    // Local artwork resolution DROPS keys with no cover (local_artwork.rs),
    // so the cover placeholder MUST be bounded or an album with no embedded
    // art shimmers forever. Same constant as LocalLibraryView.artSettleMs.
    readonly property int artSettleMs: 2500
    readonly property bool coverPending: root.album
        ? ((root.album.artKey || "") !== ""
           && (root.artMap[root.album.artKey] || "") === "")
        : false
    Timer {
        interval: 900
        repeat: true
        running: (root.pageLoading || root.coverPending)
            && root.visible && root.windowShowing
        onTriggered: root.skelPhase = !root.skelPhase
    }

    // ============================ page ===================================
    Flickable {
        id: flick
        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: page.height + 100
        boundsBehavior: Flickable.StopAtBounds

        // Exact shared Qobuz/artist atmosphere component. It lives inside
        // the scroll content and paints first, so the header scrolls over it
        // without adding another clipping or shader layer.
        HeaderGradient {
            x: 0
            y: 0
            width: flick.width
            height: page.y + headerDivider.y + root.atmoReach
            tint: root.headerColor
            atmosphere: root.headerAtmosphere
            active: root.headerAtmoOn
        }

        Column {
            id: page
            x: 32
            y: 11
            width: parent.width - 64
            spacing: 0

            // ---- History navigation ----
            // NOTHING here, by design. LocalAlbumView.slint:207 does mount
            // `NavButtons { }`, but NavButtons.slint:5-8 is a ZERO-SIZE empty
            // placeholder: "the back / forward / collapse buttons moved to the
            // app header (HeaderBar, floating right) … the real buttons live
            // once, in the header". Drawing real chevrons here was a Tauri-era
            // leftover — the page-level nav from before the app had global
            // history — and views/AlbumView.qml already gets this right.
            // The 22px spacer stays: it is the .slint's own
            // `Rectangle { height: 22px; }` (:211), which follows a zero-high
            // NavButtons, so the header lands exactly where it does today.
            Item { width: 1; height: 22 }

            // ---- Album header ----
            // Placeholder in the header's own proportions (224px cover, 32px
            // gap — LocalAlbumHeader.qml:32/46) until the document lands.
            QbzSkeleton {
                visible: root.pageLoading && !root.album
                variant: "header"
                width: parent.width
                height: root.compactHeaderPref ? 112 : 224
                coverSize: root.compactHeaderPref ? 112 : 224
                coverGap: root.compactHeaderPref ? 20 : 32
                compactHeader: root.compactHeaderPref
                actionCount: 5
                actionSize: root.compactHeaderPref ? 28 : 32
                phase: root.skelPhase
            }
            LocalAlbumHeader {
                visible: !(root.pageLoading && !root.album)
                width: parent.width
                album: root.album
                allArtists: root.allArtists
                infoLine: root.infoLine
                compactInfoLine: root.compactInfoLine
                compact: root.compactHeaderPref
                versions: root.versions
                coverSource: root.album ? (root.artMap[root.album.artKey] || "") : ""
                coverPending: root.coverPending
                skelPhase: root.skelPhase
                artSettleMs: root.artSettleMs
                strongColor: root.hdrStrong
                bodyColor: root.hdrBody
                overlay: root.hdrOverlay
                onOpenArtist: function (name) { root.openArtist(name) }
                onCompactToggled: root.toggleCompactHeader()
            }

            Item { width: 1; height: 20 }
            Rectangle { id: headerDivider; width: parent.width; height: 1; color: "transparent" }
            Item { width: 1; height: 8 }

            // ---- Loading ----
            // The track list in the shape it will arrive in (50px rows, no
            // artwork column on this page), replacing the 36px spinner the
            // Slint mounts here. ONE instance, ONE animator.
            QbzSkeleton {
                visible: root.pageLoading
                variant: "rowList"
                width: parent.width
                height: visible ? 280 : 0
                rowH: 50
                rowGap: 0
                rowArt: false
                phase: root.skelPhase
            }

            // ---- Toolbar: quality badge + track search + select toggle ----
            // AlbumPageView.slint:686-786 numbers: 52px band, the search box
            // cut from 280 to 168 "so the toolbar gives room to the square
            // select button beside it", 16px gap, a 30x30 radius-6 square
            // (NOT a round CircleAction) that goes accent while select mode
            // is on.
            Item {
                width: parent.width
                height: 52
                QualityBadgeFull {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    tier: root.album ? (root.album.qualityTier || "") : ""
                    detail: root.album ? (root.album.qualityDetail || "") : ""
                }
                Row {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 16
                    Rectangle {
                        anchors.verticalCenter: parent.verticalCenter
                        width: 168
                        height: 34
                        radius: 6
                        border.width: 1
                        border.color: theme.borderSubtle
                        color: theme.surfaceElevated
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 10
                            anchors.rightMargin: 10
                            spacing: 7
                            QbzIcon {
                                name: "search"
                                width: 14
                                height: 14
                                anchors.verticalCenter: parent.verticalCenter
                                tintName: "muted"
                            }
                            Item {
                                width: parent.width - 21
                                height: parent.height
                                clip: true
                                TextInput {
                                    QbzTextEditMenu { }
                                    id: searchInput
                                    anchors.fill: parent
                                    color: theme.textPrimary
                                    font.pixelSize: 13
                                    verticalAlignment: Text.AlignVCenter
                                    selectByMouse: true
                                    onTextEdited: root.trackQuery = text
                                }
                                Text {
                                    visible: searchInput.text === ""
                                    anchors.fill: parent
                                    text: QbzSession.tr("Search tracks...", QbzSession.trRev)
                                    color: theme.textMuted
                                    font.pixelSize: 13
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }
                        }
                    }
                    // Multi-select toggle. Unlike the Qobuz AlbumView.qml's
                    // (dimmed and inert — that bridge has no bulk seam), this
                    // one is LIVE: QbzLocal.bulkAction already carries the
                    // "track" scope this page needs.
                    Rectangle {
                        anchors.verticalCenter: parent.verticalCenter
                        width: 30
                        height: 30
                        radius: 6
                        border.width: 1
                        border.color: root.multiSelect ? theme.accent : theme.borderSubtle
                        color: (root.multiSelect || selectArea.containsMouse)
                            ? theme.surfaceHover
                            : theme.surfaceElevated
                        QbzIcon {
                            name: "square-check-big"
                            width: 15
                            height: 15
                            anchors.centerIn: parent
                            tintName: root.multiSelect
                                ? "accent"
                                : (selectArea.containsMouse ? "textPrimary" : "secondary")
                        }
                        MouseArea {
                            id: selectArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                root.multiSelect = !root.multiSelect
                                if (!root.multiSelect) root.selected = ({})
                            }
                        }
                    }
                }
            }

            // ---- Bulk action bar ----
            // Only actions with a LIVE backend, in a context where they mean
            // something. Dropped from the Slint album bar's seven:
            //   * add-to-favorites / remove-favorites — local hearts are not
            //     wired at all (local_bulk.rs's arm is a log-only no-op, and
            //     the store is keyed by file path behind a private handle);
            //   * make-offline — these tracks ARE the local files.
            // Added: add-to-mixtape, which the two Local Library bars already
            // offer and which `local_bulk::apply` serves live.
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
                    { "id": "clear", "label": QbzSession.tr("Clear", QbzSession.trRev), "icon": "x", "danger": false, "needsSelection": true },
                ]
                onAction: function (id) { root.bulkAction(id) }
            }
            // The Slint's 8px gutter under the bar (AlbumPageView.slint:808).
            Item { visible: root.multiSelect; width: 1; height: 8 }

            // ---- Column header ----
            // rows/TrackListHeader.qml, on rows/TrackCols.qml geometry — the
            // same object the LocalTrackRows below lay their cells out from.
            // (LocalAlbumView.slint:501-540 hardcodes a second set here —
            // 16px gaps, Duration 80, Quality 80 — that its own rows do not
            // use; the port does not copy that hazard.)
            //
            // `trailingReserve: 72` is the slice LocalTrackRow spends OUTSIDE
            // the shared row: it narrows the shared TrackRow by 26 (source
            // glyph gutter) + 46 (its own re-drawn ⋯: 32 + one 14px gap), so
            // the labelled columns end 72px short of the row's right edge.
            // Same constant, one place: LocalTrackRow.qml:82.
            TrackListHeader {
                width: parent.width
                bandHeight: 40
                labelSpacing: 0.5
                showFavorite: false
                showMenu: false
                trailingReserve: 72
            }

            // ---- Track list ----
            //
            // WINDOWED. The header here used to say an album's track count is
            // "bounded, so this mounts like the Slint" — and it IS bounded,
            // but bounded is not cheap. Measured on a 247-track album:
            // `localalbum built=39ms` with `to-idle=2242ms`.
            //
            // The reference gets away with one element per track because a
            // Slint element is cheap; a QML delegate is not, which is the
            // recurring mistranslation in this port. Worse than the open cost:
            // the search field filters `visibleTracks`, so every KEYSTROKE
            // handed the Repeater a new model and rebuilt all 247 rows.
            //
            // The slot keeps its full height so the page's scroll geometry is
            // unchanged whether or not the row inside exists; only the row is
            // windowed.
            Column {
                id: trackList
                width: parent.width
                spacing: 0
                /// This list's top in the page Flickable's content coordinates.
                /// One mapToItem for the whole list, re-evaluated on layout —
                /// the per-row test below is then plain arithmetic on `y`,
                /// which the Column has already computed exactly (disc headers
                /// included).
                readonly property real topInFlick:
                    trackList.mapToItem(flick.contentItem, 0, 0).y
                Repeater {
                    model: root.visibleTracks
                    delegate: Column {
                        id: trackBlock
                        required property var modelData
                        required property int index
                        width: page.width
                        spacing: 0

                        // Disc divider + its per-disc ⋯ menu.
                        //
                        // LOADER, not `visible:`. This block carries a
                        // CardMenu — a Popup with its own Repeater — and it
                        // was built for EVERY track so that two or three disc
                        // boundaries could show one. On a 247-track album that
                        // is 247 popups constructed to display none.
                        //
                        // Measured on the owner's largest local album:
                        // `localalbum built=39ms` but `to-idle=2242ms`, i.e.
                        // the mount was instant and the settle took 2.2 s. The
                        // header above this list says an album's track count
                        // is "bounded" so it can mount like the Slint — and it
                        // is bounded, but bounded is not the same as cheap,
                        // which is what that assumption missed.
                        Loader {
                            width: parent.width
                            active: root.discHeader(trackBlock.index) > 0
                            visible: active
                            onLoaded: root._hdrsBuilt++              // INSTRUM.
                            sourceComponent: Item {
                            width: parent.width
                            height: 40
                            // The disc's OWN cover, when the discs differ.
                            // Silent on a box with one shared cover — see
                            // `discArtDistinct`.
                            RoundedImage {
                                id: discThumb
                                x: 12
                                width: 28
                                height: 28
                                radius: theme.radiusSm
                                anchors.verticalCenter: parent.verticalCenter
                                visible: root.discArtDistinct && source !== ""
                                // Already a percent-encoded file:// url —
                                // artwork_qt::file_url built it, because these
                                // folder names contain '#'.
                                source: {
                                    var d = root.discInfo(root.discHeader(trackBlock.index))
                                    return root.discArt(d)
                                }
                            }
                            Text {
                                x: discThumb.visible ? 12 + 28 + 10 : 12
                                anchors.verticalCenter: parent.verticalCenter
                                // "Disc 2" alone when the box does not name its
                                // discs — the behaviour this row has always had —
                                // and "Disc 2 — Das Rheingold" when it does.
                                text: {
                                    var n = root.discHeader(trackBlock.index)
                                    var base = QbzSession.tr("Disc", QbzSession.trRev) + " " + n
                                    var d = root.discInfo(n)
                                    return (d && d.title) ? base + " — " + d.title : base
                                }
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightSemibold
                                font.letterSpacing: 0.5
                            }
                            Rectangle {
                                x: parent.width - 44
                                width: 32
                                height: 32
                                radius: theme.radiusSm
                                anchors.verticalCenter: parent.verticalCenter
                                color: discArea.containsMouse ? theme.surfaceElevated : "transparent"
                                QbzIcon {
                                    name: "ellipsis"
                                    width: 16
                                    height: 16
                                    anchors.centerIn: parent
                                    tintName: discArea.containsMouse ? "textPrimary" : "muted"
                                }
                                MouseArea {
                                    id: discArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: function (mouse) {
                                        discMenu.openAtCursor(discArea, mouse.x, mouse.y)
                                    }
                                }
                                CardMenu {
                                    id: discMenu
                                    menuWidth: 200
                                    entries: [
                                        { "label": QbzSession.tr("Play", QbzSession.trRev), "icon": "play-fill", "action": "play" },
                                        { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" },
                                        { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" },
                                        { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" },
                                    ]
                                    onPicked: function (a) {
                                        QbzLocal.albumDiscAction(root.discHeader(trackBlock.index), a)
                                    }
                                }
                            }
                            }
                        }

                        // 50 px reserved whether or not the row is built, so
                        // windowing cannot move the page under the user.
                        Item {
                            id: rowSlot
                            width: page.width
                            height: 50
                            /// This row's top in Flickable content coords.
                            /// `rowSlot.y` — NOT `trackBlock.height`; see
                            /// the twin in AlbumView. Measuring the slot
                            /// against its own parent's derived height was a
                            /// dependency cycle, and every row read as in-band.
                            readonly property real topY:
                                trackList.topInFlick + trackBlock.y + rowSlot.y
                            /// One screenful of slack each way, so a flick
                            /// reveals rows that already exist.
                            readonly property bool inBand:
                                rowSlot.topY > flick.contentY - flick.height
                                && rowSlot.topY < flick.contentY + 2 * flick.height
                            Loader {
                                anchors.fill: parent
                                active: rowSlot.inBand
                                onLoaded: root._rowsBuilt++          // INSTRUM.
                                sourceComponent: LocalTrackRow {
                                    width: page.width
                                    // Same as the Qobuz album page
                                    // (AlbumView.qml) — the two track lists
                                    // should not disagree about this.
                                    zebra: true
                                    item: trackBlock.modelData
                                    number: trackBlock.modelData.number > 0
                                        ? trackBlock.modelData.number : trackBlock.index + 1
                                    showAlbum: false
                                    showArtwork: false
                                    // The checkbox LocalTrackRow already draws over
                                    // the number cell for the Tracks tab — no fork,
                                    // no new component (rule 5).
                                    selectMode: root.multiSelect
                                    checked: root.selected[trackBlock.modelData.id] === true
                                    onPlayRequested: QbzLocal.albumSelectedAction(
                                        "play", trackBlock.modelData.id)
                                    onEnqueueRequested: function (m) {
                                        QbzLocal.enqueue("track", trackBlock.modelData.id, m)
                                    }
                                    onToggleSelect: function (mods) { root.toggleSelected(trackBlock.modelData.id, mods) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: flick; scope: "localalbum" }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: flick
        visible: flick.contentHeight > flick.height
    }

    // Local/Plex artists have no catalog id, so "go to artist" is a NAME
    // route into the Local Library Artists tab (the Slint's source-aware
    // open-artist).
    function openArtist(name) {
        QbzLocal.openArtistByName(name)
        QbzShell.navigateTo("local")
    }
}
