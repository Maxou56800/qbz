// Purchased-album detail page — screen 2 of Purchases
// (contract §6 / §15.2, recon 02-tauri-frontend §B.2).
//
// Built from views/AlbumView.qml: same page shell (Flickable + 32px-padded
// Column + QbzScrollBar), same 224px cover frame, the same rows/TrackRow +
// rows/TrackListHeader table on rows/TrackCols geometry, the same disc
// separators. What it adds over that template is the five things §15.2 lists:
//
//   1. the FORMAT dropdown (QbzSelect) — and the re-scoping it triggers is
//      the most surprising behaviour on this screen: every green check, the
//      progress section and Add-to-Library are all recomputed for the newly
//      selected format. NONE of that is computed here. The document says
//      `downloaded` per track, `progress` and `addToLibraryVisible`; this
//      file renders exactly what it is handed, so the re-scope is one
//      republish and cannot drift from the controller's own answer.
//   2. a DETERMINATE progress bar, `completed / total` — counted in TRACKS,
//      never bytes (§4.2). This is the ONLY place a percentage appears.
//   3. ADD TO LIBRARY, which is a SEQUENCE and not a banner (§6): the click
//      is one `addToLibrary()` call and the controller owns adding the
//      folder, clearing the download state, the toast and the scan.
//   4. DOWNLOAD GOODIES — album-level only, never per-track, and ABSENT
//      (not disabled, not an empty state) when the list is empty (§14.3).
//   5. track titles through `formatTrackTitle` (§10-C).
//
// --- The data seam ------------------------------------------------------
// `QbzPurchases.albumJson`, re-parsed on `QbzPurchases.purchasesRev` (§G.3).
// The rev is read purely as a binding dependency so a byte-identical
// republish still re-parses. Every field is read defensively: the document
// defaults to "{}" and QML parses it on the first frame.
//
// --- Per-track download state -------------------------------------------
// `status` is the existing four-part vocabulary — 0 none · 1 queued ·
// 2 downloading · 3 ready · 4 failed — drawn with the SAME glyph set
// TrackRow's offline column uses (TrackRow.qml:782-835), so a purchase
// download looks identical to a cache download. It does NOT ride the
// `trackCacheStatusChanged` channel (§G.4): that channel is keyed by
// track id alone and drives the OFFLINE-CACHE indicator, and a track can be
// both cached offline and purchase-downloaded. Purchase status travels
// inside `album_json`, which is per-album and cannot collide.
//
// --- Why the status cell lives OUTSIDE the shared TrackRow --------------
// The views/local/LocalTrackRow.qml pattern: the shared row is narrowed by
// exactly the 28px cell + 14px gap it does not draw, and the cell is drawn
// in the gutter that frees. TrackListHeader reserves the same slice through
// `trailingReserve: 42`, so the header and the rows stay the same width by
// construction. The alternative — teaching TrackRow a second, purchase-only
// download column — would fork a file five other surfaces share.
//
// --- Animation ----------------------------------------------------------
// Every spinner on this page is ONE phase advanced on the shared shell pulse
// (QbzShell.pulseMs, ~30 Hz) and frozen when nothing is in flight. Qt Quick
// has no partial redraws, so a component-local RotationAnimation here would
// bill a whole-window present per display frame for the life of a download.

import QtQuick
import QtQuick.Controls
import QtQuick.Window
import com.blitzfc.qbz
import "../controls"
import "../rows"
import "../theme"
import "../kiosk"
import "../assets/kiosk-art.js" as KioskArt

Rectangle {
    id: root
    property bool kioskHost: false

    // Transparent while the ambient background is active — AlbumView's rule.
    color: ambientOn ? "transparent" : theme.surfaceMain
    readonly property bool ambientOn: theme.ambientOn
    // Rounds to the AppShell content-frame bezel (QML clips are rectangular,
    // so the frame's rounding never reaches the view — the fill must round).
    radius: 12

    QbzTheme { id: theme }

    function t(s) { return QbzSession.tr(s, QbzSession.trRev) }

    readonly property var kioskTrackRows: {
        if (!root.kioskHost) return []
        var result = []
        root.discs.forEach(function(d, discIndex) {
            var tracks = (d.tracks || []).filter(root.matches)
            if (root.multiDisc && tracks.length) result.push({kind: "disc", number: d.number || discIndex + 1})
            tracks.forEach(function(track, i) { result.push({kind: "track", track: track, ordinal: i}) })
        })
        return result
    }

    // ---- The document (§G.3) --------------------------------------------
    // `rev` is a binding dependency ONLY: it is bumped on every publish so a
    // republish of byte-identical JSON still re-parses here.
    readonly property var doc: root._parse(QbzPurchases.albumJson, QbzPurchases.purchasesRev)
    function _parse(raw, rev) {
        try { return JSON.parse(raw || "{}") } catch (e) { return ({}) }
    }

    readonly property bool loading: doc.loading === true
    readonly property string errorText: doc.error || ""
    readonly property bool ready: !root.loading && root.errorText === ""

    readonly property var formats: doc.formats || []
    readonly property var goodies: doc.goodies || []
    readonly property var discs: doc.discs || []
    readonly property var progress: doc.progress || ({})
    readonly property var copies: doc.copies || []
    readonly property var batchResult: doc.batchResult || ({})
    readonly property bool downloadable: doc.downloadable !== false
    property int shownBatchSerial: 0
    property var copyMenuTarget: ({})

    function copyStateLabel(state) {
        if (state === "complete") return root.t("Complete")
        if (state === "partial") return root.t("Partial")
        if (state === "missing") return root.t("Missing files")
        if (state === "changed") return root.t("Changed files")
        if (state === "unreachable") return root.t("Unavailable")
        if (state === "unreadable") return root.t("Unreadable files")
        return state || ""
    }
    function copyFormatLabel(formatId) {
        for (var i = 0; i < root.formats.length; i++)
            if (Number(root.formats[i].id) === Number(formatId))
                return root.formats[i].label || ("#" + String(formatId))
        return "#" + String(formatId)
    }
    function copyTitle(copy) {
        var folder = copy.folderLabel || ""
        var format = root.copyFormatLabel(copy.formatId)
        if (folder === "") return format
        if (format === "" || folder.toLowerCase().indexOf(format.toLowerCase()) >= 0)
            return folder
        return folder + "  ·  " + format
    }
    function batchResultBody() {
        var result = root.batchResult
        var text = root.t("{} of {} tracks available in this folder.")
            .replace("{}", result.available || 0)
            .replace("{}", result.total || 0)
        if (Number(result.failed || 0) > 0)
            text += "\n" + root.t("{} failed").replace("{}", result.failed)
        if (Number(result.cancelled || 0) > 0)
            text += "\n" + root.t("{} cancelled").replace("{}", result.cancelled)
        return text
    }

    Connections {
        target: QbzPurchases
        function onPurchasesRevChanged() {
            var result = root.doc.batchResult || ({})
            var serial = Number(result.serial || 0)
            if (serial > 0 && serial !== root.shownBatchSerial) {
                root.shownBatchSerial = serial
                copiesModal.close()
                batchModal.open()
            }
            if (copiesModal.opened && root.copies.length === 0)
                copiesModal.close()
        }
    }

    // Disc headers render only on a multi-disc album (recon §B.2.2-7:
    // `isMultiDisc = discGroups.size > 1`).
    readonly property bool multiDisc: root.discs.length > 1

    // The dropdown's index. `formats` order IS the dropdown order and index 0
    // is the default (§G.3), so an id the document has not caught up with yet
    // resolves to 0 rather than to a blank control.
    readonly property int formatIndex: {
        var want = doc.selectedFormatId
        for (var i = 0; i < root.formats.length; i++)
            if (root.formats[i].id === want) return i
        return 0
    }
    // "✓" after every format whose download is COMPLETE for the whole album
    // (`FormatRow.downloaded`, from the registry): the dropdown then says
    // which versions are already on disk without opening each one.
    readonly property var formatLabels: {
        var out = []
        for (var i = 0; i < root.formats.length; i++)
            out.push((root.formats[i].label || "")
                     + (root.formats[i].downloaded === true ? "  ✓" : ""))
        return out
    }

    // The track filter's text and its matcher (title / performer, case-folded).
    property string query: ""
    function matches(track) {
        var q = root.query.trim().toLowerCase()
        if (q === "")
            return true
        var t = (track.title || "").toLowerCase()
        var a = (track.artist || "").toLowerCase()
        return t.indexOf(q) >= 0 || a.indexOf(q) >= 0
    }
    readonly property int trackTotal: {
        var n = 0
        for (var d = 0; d < root.discs.length; d++)
            n += (root.discs[d].tracks || []).length
        return n
    }
    readonly property int matchCount: {
        var n = 0
        for (var d = 0; d < root.discs.length; d++) {
            var ts = root.discs[d].tracks || []
            for (var i = 0; i < ts.length; i++)
                if (root.matches(ts[i])) n++
        }
        return n
    }

    // Click-to-play is gated on `streamable`, which on THIS screen defaults
    // FALSE (§G.3 / §2.6): these tracks come from the catalog album, not from
    // the purchases list. The album Play button therefore only appears when
    // at least one track can actually be played — a hero button that plays
    // nothing is worse than no hero button.
    readonly property bool anyStreamable: {
        for (var d = 0; d < root.discs.length; d++) {
            var ts = root.discs[d].tracks || []
            for (var i = 0; i < ts.length; i++)
                if (ts[i].streamable === true) return true
        }
        return false
    }
    readonly property bool anyCompleteHealthyCopy: {
        for (var i = 0; i < root.copies.length; i++)
            if (root.copies[i].state === "complete") return true
        return false
    }
    readonly property bool anyPlayable: root.anyStreamable || root.anyCompleteHealthyCopy

    // ---- The one animation clock ----------------------------------------
    // Advanced on the shared shell pulse and ONLY while something is actually
    // in flight; a frozen page writes nothing, so it dirties nothing.
    readonly property bool anyTrackBusy: {
        for (var d = 0; d < root.discs.length; d++) {
            var ts = root.discs[d].tracks || []
            for (var i = 0; i < ts.length; i++) {
                var st = ts[i].status
                if (st === 1 || st === 2) return true
            }
        }
        return false
    }
    readonly property bool spinning: root.visible
        && (root.loading || root.progress.active === true || root.anyTrackBusy)
    property real spinDeg: 0
    Connections {
        target: QbzShell
        function onPulseMsChanged() {
            // 12° per ~30Hz tick = one revolution per second, the rate
            // theme/QbzSpinner.qml animates at.
            if (root.spinning)
                root.spinDeg = (root.spinDeg + 12) % 360
        }
    }

    // ---- Track titles (§10-C) -------------------------------------------
    /// `formatTrackTitle` — the Tauri helper (src/lib/utils/trackTitle.ts:26-31):
    /// the version, when there is one, is appended in parentheses. Hard-coding
    /// `title.trim()` here is what made the latent #360 regression unreachable
    /// in the reference, so it is a real function even though `version` is
    /// empty on most releases.
    function formatTrackTitle(title, version) {
        var base = (title || "").trim()
        var v = (version || "").trim()
        return v === "" ? base : base + " (" + v + ")"
    }

    /// M:SS. The shared TrackRow renders `item.duration` as a STRING and the
    /// document carries SECONDS, so a raw hand-off prints "245" in the
    /// duration column.
    function fmtTrackDuration(secs) {
        var s = Math.max(0, Math.floor(secs || 0))
        var m = Math.floor(s / 60)
        var r = s % 60
        return m + ":" + (r < 10 ? "0" : "") + r
    }

    /// Album total — `1h 12m` / `12m`, the reference's own units
    /// (PurchaseAlbumDetailView.svelte:140-145). They are not localised there
    /// either; inventing two new msgids for them is not this port's call.
    function fmtAlbumDuration(secs) {
        var s = Math.max(0, Math.floor(secs || 0))
        var h = Math.floor(s / 3600)
        var m = Math.floor((s % 3600) / 60)
        return h > 0 ? (h + "h " + m + "m") : (m + "m")
    }

    /// The purchase date, long month — the detail screen's format
    /// (`month: 'long'`, recon §B.2.2-3). 0 = never rendered.
    function fmtPurchased(ts) {
        if (!ts || ts <= 0) return ""
        return Qt.formatDate(new Date(ts * 1000), Locale.LongFormat)
    }

    /// The meta line: purchase date · label · year · genre, joined only over
    /// the parts that exist. (The reference emits a leading "·" when the
    /// genre is present and the date is not — that is a bug, not a behaviour.)
    readonly property string metaLine: {
        var parts = []
        var p = root.fmtPurchased(doc.purchasedAt)
        if (p !== "") parts.push(root.t("Purchased on") + " " + p)
        if ((doc.label || "") !== "") parts.push(doc.label)
        if ((doc.year || "") !== "") parts.push(doc.year)
        if ((doc.genre || "") !== "") parts.push(doc.genre)
        return parts.join("   •   ")
    }

    // ---- Artwork ---------------------------------------------------------
    // The url-keyed pipeline every other view uses: the url goes out on
    // QbzShell.sidebarArtworkWindow and the decoded path comes back on
    // QbzLibrary.libraryArtworkReady, keyed by that same url.
    readonly property string coverUrl: root.kioskHost ? KioskArt.sizedUrl(root.doc.artworkUrl || "", Math.ceil(112 * Screen.devicePixelRatio)) : (root.doc.artworkUrl || "")
    property var coverMap: ({})
    property string dispatchedCover: ""
    function dispatchCover() {
        var url = root.coverUrl
        if (url === "" || url === root.dispatchedCover) return
        root.dispatchedCover = url
        QbzShell.sidebarArtworkWindow(JSON.stringify([url]))
    }
    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            var m = root.coverMap
            m[key] = path
            root.coverMap = Object.assign({}, m)
        }
    }
    Component.onCompleted: root.dispatchCover()
    onDocChanged: root.dispatchCover()
    onCoverUrlChanged: root.dispatchCover()

    // ======================== the page ====================================
    Flickable {
        id: pageFlick
        anchors.fill: parent
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

            Item { width: 1; height: 22 }

            // --- Loading ---------------------------------------------------
            // The reference's loading state is a centred 32px spinner, not a
            // skeleton (recon §B.2.2-2) — and a spinner is honest here,
            // because the detail is ONE request graph that lands in one piece
            // rather than the staged passes AlbumView shows placeholders for.
            Item {
                visible: root.loading
                width: parent.width - 64
                height: visible ? 220 : 0
                QbzIcon {
                    anchors.centerIn: parent
                    name: "loader-circle"
                    width: 32
                    height: 32
                    tintName: "accent"
                    rotation: root.spinDeg
                }
            }

            // --- Error + retry (LibraryView.qml:920-959) -------------------
            Column {
                visible: !root.loading && root.errorText !== ""
                width: parent.width - 64
                spacing: 10
                Item { width: 1; height: 60 }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: root.t("Couldn't load purchases. Check your connection.")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                }
                // No detail sub-line. The two-line shape borrowed from
                // LibraryView only earns its second line when the document
                // carries something MORE SPECIFIC than the headline; this
                // controller only ever emits the same generic sentence, so
                // printing it twice just says it twice.
                Rectangle {
                    // §G.1 has no dedicated reload; `openAlbum(id)` re-records
                    // the detail and reloads it, which is the retry. With no
                    // id there is nothing to retry, so the button is absent
                    // rather than dead.
                    visible: (root.doc.id || "") !== ""
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
                        onClicked: QbzPurchases.openAlbum(root.doc.id)
                    }
                }
            }

            // --- Album header ---------------------------------------------
            Row {
                visible: root.ready
                width: parent.width - 64
                spacing: 32

                Rectangle {
                    width: root.kioskHost ? 112 : 224
                    height: width
                    radius: 12
                    color: theme.surfaceElevated
                    // No clip: RoundedImage confines itself on both arms, and
                    // a clip is an unconditional batch root.
                    Loader {
                        anchors.fill: parent
                        active: root.downloadable
                        sourceComponent: root.kioskHost ? kioskCover : desktopCover
                    }
                    Component { id: kioskCover; KioskArtwork { source: root.coverMap[root.coverUrl] || ""; radius: 12 } }
                    Component { id: desktopCover; RoundedImage { source: root.coverMap[root.coverUrl] || ""; radius: 12 } }
                    // An album the account cannot download shows the warning
                    // in place of the cover (recon §B.2.2-3) — the one place
                    // this screen says "unavailable", and it says it about the
                    // PURCHASE, never about the network.
                    Column {
                        visible: !root.downloadable
                        anchors.centerIn: parent
                        spacing: 8
                        QbzIcon {
                            anchors.horizontalCenter: parent.horizontalCenter
                            name: "triangle-alert"
                            width: 18
                            height: 18
                            tintName: "muted"
                        }
                        Text {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: root.t("Unavailable")
                            color: theme.textMuted
                            font.pixelSize: 12
                        }
                    }
                }

                Column {
                    width: parent.width - (root.kioskHost ? 112 : 224) - 32
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 0

                    // `fontSection`, matching AlbumView.qml:626 exactly — this
                    // header is a copy of that one and the size had drifted to
                    // `fontTitle` (25), which would have made a PURCHASED album
                    // shout louder than an ordinary one on the very next screen.
                    Text {
                        width: parent.width
                        text: root.doc.title || ""
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }
                    Item { width: 1; height: 4 }
                    Text {
                        // `width: fit-content` in the reference — the hover
                        // target must be the NAME, not the whole line, or the
                        // pointer turns into a link half a screen away from it.
                        width: Math.min(implicitWidth, parent.width)
                        text: root.doc.artist || ""
                        color: (root.doc.artistId || "") !== ""
                            ? (artistArea.containsMouse ? theme.accentHover : theme.accent)
                            : theme.textPrimary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                        MouseArea {
                            id: artistArea
                            anchors.fill: parent
                            enabled: (root.doc.artistId || "") !== ""
                            hoverEnabled: true
                            cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                            onClicked: QbzArtist.openArtist(root.doc.artistId)
                        }
                    }
                    Item { width: 1; height: 8 }
                    Text {
                        visible: root.metaLine !== ""
                        width: parent.width
                        text: root.metaLine
                        color: theme.textMuted
                        font.pixelSize: 14
                        elide: Text.ElideRight
                    }
                    Item { visible: root.metaLine !== ""; width: 1; height: 6 }
                    // The album's stored quality, through the shared chip
                    // rather than a hand-rolled "24-bit / 96 kHz" string —
                    // QualityBadgeFull hides ITSELF on an empty tier, so a
                    // document that does not carry one costs no empty band.
                    QualityBadgeFull {
                        tier: root.doc.qualityTier || ""
                        detail: root.doc.qualityDetail || ""
                    }
                    // Gated on the same tier the badge is: the badge hides
                    // ITSELF and a Column skips an invisible child, so an
                    // ungated spacer would leave 6px of nothing behind it.
                    Item {
                        visible: (root.doc.qualityTier || "") !== ""
                        width: 1
                        height: 6
                    }
                    Text {
                        width: parent.width
                        text: {
                            var n = root.doc.tracksCount || 0
                            var s = root.t("{} tracks").replace("{}", n)
                            var d = root.fmtAlbumDuration(root.doc.duration)
                            return (root.doc.duration || 0) > 0 ? (s + "  ·  " + d) : s
                        }
                        color: theme.textMuted
                        font.pixelSize: 14
                        elide: Text.ElideRight
                    }

                    Item { width: 1; height: 20 }

                    // --- Action row --------------------------------------
                    // Remote download actions follow the current entitlement,
                    // but an existing local copy remains playable/manageable
                    // if that catalog edition later becomes unavailable.
                    Row {
                        visible: !root.kioskHost && (root.downloadable || root.copies.length > 0)
                        width: parent.width
                        spacing: 12

                        // On THIS screen the download is the point and Play
                        // is the convenience, so the accent goes to the
                        // cloud and both share one intermediate size — the
                        // 44px primary Play of the catalog album page read as
                        // the main event here (smoke 2026-09-02).
                        QbzCircleAction {
                            visible: root.anyPlayable
                            primary: false
                            diameterOverride: 38
                            name: "play-fill"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzPurchases.playAlbum()
                        }
                        IconTextButton {
                            visible: root.downloadable
                            label: root.t("Download another copy")
                            iconName: "cloud-download"
                            btnEnabled: !(root.progress.active === true)
                                        && root.formats.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: {
                                if (root.progress.active !== true && root.formats.length > 0)
                                    QbzPurchases.downloadAlbum()
                            }
                        }
                        IconTextButton {
                            visible: root.copies.length > 0
                            label: root.t("Local purchase copies") + " (" + root.copies.length + ")"
                            iconName: "hard-drive"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: copiesModal.open()
                        }
                        // THE FORMAT DROPDOWN. Options in document order —
                        // that order IS the dropdown order and index 0 is the
                        // default. Choosing one re-scopes every downloaded
                        // mark, the progress section and Add-to-Library; none
                        // of that is computed here, it arrives in the next
                        // republish.
                        QbzSelect {
                            visible: root.downloadable && root.formats.length > 0
                            anchors.verticalCenter: parent.verticalCenter
                            menuWidth: 200
                            options: root.formatLabels
                            currentIndex: root.formatIndex
                            enabled: !(root.progress.active === true)
                            onSelected: function (i) {
                                if (i >= 0 && i < root.formats.length)
                                    QbzPurchases.setFormat(root.formats[i].id)
                            }
                        }
                        Text {
                            visible: root.downloadable && root.formats.length === 0
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.t("No downloadable formats available")
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                        }
                        // The album is ALREADY in the Local Library: jump to
                        // it. `localAlbumId` is the download folder the
                        // library indexed (purchases_qt::resolve_local_album),
                        // which is exactly the id the local album page opens.
                        IconTextButton {
                            visible: (root.doc.localAlbumId || "") !== ""
                            anchors.verticalCenter: parent.verticalCenter
                            label: root.t("Open in Local Library")
                            iconName: "hard-drive"
                            onClicked: {
                                QbzLocal.openAlbum(root.doc.localAlbumId)
                                QbzShell.navigateTo("localalbum")
                            }
                        }
                        // GOODIES — album-level only, never per-track, and
                        // completely absent when the list is empty (§14.3).
                        // Not disabled, not an empty state: the owner's
                        // account will never populate this, so a wrong empty
                        // state would be a permanent wrong nobody can see.
                        IconTextButton {
                            visible: root.downloadable
                                     && root.goodies.length > 0
                                     && !goodieCount.visible
                            anchors.verticalCenter: parent.verticalCenter
                            label: root.t("Download goodies")
                            iconName: "cloud-download"
                            btnEnabled: !(root.progress.active === true)
                            onClicked: QbzPurchases.downloadGoodies()
                        }
                        // Goodies are counted SEPARATELY from tracks (§14.3), so
                        // they get their own readout rather than a slot in the
                        // track bar. Without it a multi-item booklet download is
                        // indistinguishable from a click that did nothing until
                        // the final toast lands, and the natural reaction is to
                        // click again.
                        Row {
                            id: goodieCount
                            readonly property var gp: root.doc.goodiesProgress || ({})
                            visible: goodieCount.gp.active === true
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6
                            QbzIcon {
                                anchors.verticalCenter: parent.verticalCenter
                                name: "loader-circle"
                                width: 14
                                height: 14
                                tintName: "accent"
                                rotation: root.spinDeg
                            }
                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.t("Downloading {}/{}...")
                                          .replace("{}", goodieCount.gp.completed || 0)
                                          .replace("{}", goodieCount.gp.total || 0)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                            }
                        }
                    }

                    // --- Action row --------------------------------------
                    // Remote download actions follow the current entitlement,
                    // but an existing local copy remains playable/manageable
                    // if that catalog edition later becomes unavailable.
                    Flow {
                        visible: root.kioskHost && (root.downloadable || root.copies.length > 0)
                        width: parent.width
                        spacing: 12

                        // On THIS screen the download is the point and Play
                        // is the convenience, so the accent goes to the
                        // cloud and both share one intermediate size — the
                        // 44px primary Play of the catalog album page read as
                        // the main event here (smoke 2026-09-02).
                        QbzCircleAction {
                            visible: root.anyPlayable
                            primary: false
                            diameterOverride: 64
                            name: "play-fill"
                            anchors.verticalCenter: undefined
                            onClicked: QbzPurchases.playAlbum()
                        }
                        IconTextButton {
                            height: 64
                            visible: root.downloadable
                            label: root.t("Download another copy")
                            iconName: "cloud-download"
                            btnEnabled: !(root.progress.active === true)
                                        && root.formats.length > 0
                            anchors.verticalCenter: undefined
                            onClicked: {
                                if (root.progress.active !== true && root.formats.length > 0)
                                    QbzPurchases.downloadAlbum()
                            }
                        }
                        IconTextButton {
                            height: 64
                            visible: root.copies.length > 0
                            label: root.t("Local purchase copies") + " (" + root.copies.length + ")"
                            iconName: "hard-drive"
                            anchors.verticalCenter: undefined
                            onClicked: copiesModal.open()
                        }
                        // THE FORMAT DROPDOWN. Options in document order —
                        // that order IS the dropdown order and index 0 is the
                        // default. Choosing one re-scopes every downloaded
                        // mark, the progress section and Add-to-Library; none
                        // of that is computed here, it arrives in the next
                        // republish.
                        QbzSelect {
                            kioskHost: true
                            visible: root.downloadable && root.formats.length > 0
                            anchors.verticalCenter: undefined
                            menuWidth: 200
                            options: root.formatLabels
                            currentIndex: root.formatIndex
                            enabled: !(root.progress.active === true)
                            onSelected: function (i) {
                                if (i >= 0 && i < root.formats.length)
                                    QbzPurchases.setFormat(root.formats[i].id)
                            }
                        }
                        Text {
                            visible: root.downloadable && root.formats.length === 0
                            anchors.verticalCenter: undefined
                            text: root.t("No downloadable formats available")
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                        }
                        // The album is ALREADY in the Local Library: jump to
                        // it. `localAlbumId` is the download folder the
                        // library indexed (purchases_qt::resolve_local_album),
                        // which is exactly the id the local album page opens.
                        IconTextButton {
                            height: 64
                            visible: (root.doc.localAlbumId || "") !== ""
                            anchors.verticalCenter: undefined
                            label: root.t("Open in Local Library")
                            iconName: "hard-drive"
                            onClicked: {
                                QbzLocal.openAlbum(root.doc.localAlbumId)
                                QbzShell.navigateTo("localalbum")
                            }
                        }
                        // GOODIES — album-level only, never per-track, and
                        // completely absent when the list is empty (§14.3).
                        // Not disabled, not an empty state: the owner's
                        // account will never populate this, so a wrong empty
                        // state would be a permanent wrong nobody can see.
                        IconTextButton {
                            height: 64
                            visible: root.downloadable
                                     && root.goodies.length > 0
                                     && !kioskGoodieCount.visible
                            anchors.verticalCenter: undefined
                            label: root.t("Download goodies")
                            iconName: "cloud-download"
                            btnEnabled: !(root.progress.active === true)
                            onClicked: QbzPurchases.downloadGoodies()
                        }
                        // Goodies are counted SEPARATELY from tracks (§14.3), so
                        // they get their own readout rather than a slot in the
                        // track bar. Without it a multi-item booklet download is
                        // indistinguishable from a click that did nothing until
                        // the final toast lands, and the natural reaction is to
                        // click again.
                        Row {
                            id: kioskGoodieCount
                            readonly property var gp: root.doc.goodiesProgress || ({})
                            visible: kioskGoodieCount.gp.active === true
                            anchors.verticalCenter: undefined
                            spacing: 6
                            QbzIcon {
                                anchors.verticalCenter: undefined
                                name: "loader-circle"
                                width: 14
                                height: 14
                                tintName: "accent"
                                rotation: root.spinDeg
                            }
                            Text {
                                anchors.verticalCenter: undefined
                                text: root.t("Downloading {}/{}...")
                                          .replace("{}", kioskGoodieCount.gp.completed || 0)
                                          .replace("{}", kioskGoodieCount.gp.total || 0)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                            }
                        }
                    }

                    // The chosen destination, shown so "Choose folder" is not a
                    // button whose effect is invisible.
                    Item {
                        visible: destPath.visible
                        width: 1
                        height: 8
                    }
                    Text {
                        id: destPath
                        visible: root.downloadable && (root.doc.destination || "") !== ""
                        width: parent.width
                        text: root.doc.destination || ""
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        elide: Text.ElideMiddle
                    }

                    // What Play actually does. A DSD purchase streams as CD
                    // quality (the catalog has no DSD stream), so the first
                    // smoke saw a "downgrade" badge on a Hi-Res album and
                    // wondered which one was lying: neither — Play is the
                    // stream until the file is on disk, and the purchased
                    // copy after that (purchase_playback_qt).
                    Item {
                        visible: streamNote.visible
                        width: 1
                        height: 8
                    }
                    Text {
                        id: streamNote
                        visible: root.downloadable && root.anyStreamable
                        width: parent.width
                        text: root.t("Until the purchased file is downloaded, Play streams the best quality Qobuz offers for this album. Once it is on disk, Play uses your copy.")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                }
            }

            // --- Download progress (§15.2-2) -------------------------------
            // Determinate, `completed / total`, counted in TRACKS and never in
            // bytes (§4.2). The ONLY percentage on the whole feature.
            Item { visible: progressBox.visible; width: 1; height: 24 }
            Rectangle {
                id: progressBox
                // The reference renders this section while downloading, when the
                // album finished, AND after a cancel — `downloadingAll ||
                // allComplete || wasCancelled`. Without the third arm a cancel
                // makes the whole box VANISH, which reads as "nothing ever
                // happened" rather than "you stopped it".
                visible: root.ready
                         && (root.progress.active === true
                             || root.progress.allComplete === true
                             || root.progress.cancelled === true)
                width: parent.width - 64
                height: visible ? 66 : 0
                radius: theme.radiusSm
                color: theme.surfaceCard
                border.width: 1
                border.color: theme.borderSubtle

                readonly property int completed: root.progress.completed || 0
                readonly property int total: root.progress.total || 0
                readonly property bool done: root.progress.allComplete === true
                readonly property bool cancelled: root.progress.cancelled === true && !progressBox.done
                readonly property real fraction:
                    progressBox.total > 0
                        ? Math.max(0, Math.min(1, progressBox.completed / progressBox.total))
                        : 0

                Column {
                    anchors.fill: parent
                    anchors.leftMargin: 14
                    anchors.rightMargin: 14
                    anchors.topMargin: 12
                    anchors.bottomMargin: 12
                    spacing: 6

                    // 32px, not the label's own line height: the Cancel
                    // control is an IconTextButton and those are 32 tall, so a
                    // 20px band would let it hang out of the box.
                    Item {
                        width: parent.width
                        height: 32

                        Row {
                            anchors.left: parent.left
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6
                            QbzIcon {
                                visible: progressBox.done
                                anchors.verticalCenter: parent.verticalCenter
                                name: "check"
                                width: 14
                                height: 14
                                tintName: "accent"
                            }
                            QbzIcon {
                                visible: progressBox.cancelled
                                anchors.verticalCenter: parent.verticalCenter
                                name: "x"
                                width: 14
                                height: 14
                                tintName: "muted"
                            }
                            // The spinner turns only while work is actually in
                            // flight — a cancelled album must not keep spinning.
                            QbzIcon {
                                visible: !progressBox.done && !progressBox.cancelled
                                anchors.verticalCenter: parent.verticalCenter
                                name: "loader-circle"
                                width: 14
                                height: 14
                                tintName: "accent"
                                rotation: root.spinDeg
                            }
                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                text: progressBox.done
                                    ? root.t("Download complete")
                                    : progressBox.cancelled
                                        ? root.t("Download cancelled ({}/{})")
                                              .replace("{}", progressBox.completed)
                                              .replace("{}", progressBox.total)
                                        : root.t("Downloading {}/{}...")
                                              .replace("{}", progressBox.completed)
                                              .replace("{}", progressBox.total)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                            }
                        }
                        // Cancel is offered only while the album loop is
                        // running. A failed track does NOT stop it (§4.2b) —
                        // it keeps its own retry control and the album carries
                        // on — so this button means "stop the rest", not
                        // "something went wrong".
                        IconTextButton {
                            visible: root.progress.active === true
                            anchors.right: parent.right
                            anchors.verticalCenter: parent.verticalCenter
                            label: root.t("Cancel")
                            iconName: "x"
                            danger: true
                            onClicked: QbzPurchases.cancelAlbumDownload()
                        }
                    }

                    // The bar. NO `Behavior on width`: it is fed by the
                    // document, and an animator on a data-fed property turns
                    // one republish into a display-rate run of whole-window
                    // presents.
                    Rectangle {
                        width: parent.width
                        height: 4
                        radius: 2
                        color: theme.surfaceElevated
                        Rectangle {
                            width: Math.round(parent.width * progressBox.fraction)
                            height: parent.height
                            radius: parent.radius
                            color: progressBox.done ? theme.success : theme.accent
                        }
                    }
                }
            }

            // --- Divider ---------------------------------------------------
            Item { visible: root.ready; width: 1; height: 24 }
            Rectangle {
                visible: root.ready
                width: parent.width - 64
                height: 1
                color: theme.borderSubtle
            }
            Item { visible: root.ready; width: 1; height: 8 }

            // --- Track table -----------------------------------------------
            Column {
                visible: root.ready
                width: parent.width - 64
                spacing: 0

                // Track filter — the Local Library album page's "Filter
                // tracks" box, so a 30-track purchase is searchable in place.
                // Presentation-only: rows that do not match collapse to 0px.
                Item {
                    visible: root.discs.length > 0
                    width: parent.width
                    height: visible ? 44 : 0
                    QbzLineEdit {
                        id: trackFilter
                        kioskHost: root.kioskHost
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 260
                        sm: true
                        searchMode: true
                        placeholder: root.t("Filter tracks")
                        text: root.query
                        onEdited: function (v) { root.query = v }
                    }
                    Text {
                        anchors.right: trackFilter.left
                        anchors.rightMargin: 12
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.query.trim() !== ""
                        text: root.matchCount + " / " + root.trackTotal
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                }

                // Column header. The arms MIRROR the delegate's arms below —
                // no heart, no shared offline column, no ⋯ — and
                // `trailingReserve` is the 28px status cell plus its 14px gap,
                // i.e. exactly what the delegate spends outside the shared
                // row. The cloud glyph that heads that column is drawn here
                // because the reserve slot is, by design, an unlabelled Item.
                Item {
                    // A labelled header over nothing is worse than no header.
                    visible: root.discs.length > 0
                    width: parent.width
                    height: visible ? 40 : 0
                    TrackListHeader {
                        kioskHost: root.kioskHost
                        anchors.fill: parent
                        bandHeight: 40
                        labelSpacing: 0.5
                        showFavorite: false
                        showDownload: false
                        showMenu: false
                        trailingReserve: 42
                    }
                    QbzIcon {
                        x: parent.width - 40 + 7
                        anchors.verticalCenter: parent.verticalCenter
                        name: "cloud-download"
                        width: 14
                        height: 14
                        tintName: "muted"
                    }
                }

                Item {
                    id: kioskTracks
                    visible: root.kioskHost
                    width: parent.width
                    height: root.kioskHost ? root.kioskTrackRows.length * 64 : 0
                    readonly property real viewTop: {
                        var probe = pageFlick.contentHeight
                        return pageFlick.contentY - kioskTracks.mapToItem(pageFlick.contentItem, 0, 0).y
                    }
                    readonly property int first: Math.max(0, Math.floor(viewTop / 64) - 1)
                    readonly property int last: Math.min(root.kioskTrackRows.length, Math.ceil((viewTop + pageFlick.height) / 64) + 1)
                    Repeater {
                        model: root.kioskHost ? root.kioskTrackRows.slice(kioskTracks.first, Math.max(kioskTracks.first, kioskTracks.last)) : []
                        delegate: Item {
                            id: purchaseRow
                            required property var modelData
                            required property int index
                            x: 0
                            y: (kioskTracks.first + index) * 64
                            width: kioskTracks.width
                            height: 64
                            readonly property var track: modelData.track || ({})
                            readonly property int status: track.status || (track.downloaded ? 3 : 0)
                            Text { visible: purchaseRow.modelData.kind === "disc"; anchors.verticalCenter: parent.verticalCenter; text: root.t("Disc") + " " + purchaseRow.modelData.number; color: theme.textMuted; font.pixelSize: 18 }
                            Loader {
                                anchors.fill: parent
                                active: purchaseRow.modelData.kind === "track"
                                sourceComponent: Component {
                                    Item {
                                        TrackRow {
                                            kioskHost: true
                                            width: parent.width - rowDownload.width - 8
                                            item: ({id: purchaseRow.track.id || "", title: root.formatTrackTitle(purchaseRow.track.title, purchaseRow.track.version), artist: purchaseRow.track.artist || "", duration: root.fmtTrackDuration(purchaseRow.track.duration), qualityTier: purchaseRow.track.qualityTier || "", qualityDetail: purchaseRow.track.qualityDetail || ""})
                                            number: purchaseRow.track.trackNumber || (purchaseRow.modelData.ordinal + 1)
                                            showFavorite: false; showDownload: false; showMenu: false; draggable: false
                                            clickPlays: purchaseRow.track.streamable === true
                                            onPlayRequested: { if (purchaseRow.track.streamable) QbzPlayer.playAlbumFrom(root.doc.id || "", purchaseRow.track.id || "") }
                                        }
                                        SettingsButton {
                                            id: rowDownload
                                            anchors.right: parent.right
                                            kioskHost: true
                                            btnHeight: 64
                                            minWidth: 96
                                            text: purchaseRow.status === 4 ? root.t("Retry") : purchaseRow.status === 3 ? "✓" : purchaseRow.status === 1 || purchaseRow.status === 2 ? "…" : root.t("Download")
                                            enabled: root.downloadable && root.formats.length > 0 && purchaseRow.status !== 1 && purchaseRow.status !== 2
                                            onClicked: { if (purchaseRow.status === 4) QbzPurchases.retryTrack(purchaseRow.track.id || ""); else QbzPurchases.downloadTrack(purchaseRow.track.id || "") }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Discs. The document is ALREADY grouped (§G.3), in the order
                // the reference's first-seen bucketing produces, so nothing is
                // regrouped or re-sorted here.
                Repeater {
                    model: root.kioskHost ? [] : root.discs
                    delegate: Column {
                        id: discBlock
                        required property var modelData
                        required property int index
                        width: parent ? parent.width : 0
                        spacing: 0

                        // Disc separator — multi-disc albums only, exactly as
                        // AlbumView and the reference both gate it.
                        Item {
                            visible: root.multiDisc
                            width: parent.width
                            height: visible ? 40 : 0
                            Text {
                                x: 12
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.t("Disc") + " "
                                      + (discBlock.modelData.number || (discBlock.index + 1))
                                color: theme.textMuted
                                font.pixelSize: 13
                                font.weight: theme.weightSemibold
                                font.letterSpacing: 0.5
                            }
                        }

                        Repeater {
                            model: discBlock.modelData.tracks || []
                            delegate: Item {
                                id: trackRow
                                required property var modelData
                                required property int index
                                width: parent ? parent.width : 0
                                readonly property bool shown: root.matches(modelData)
                                visible: shown
                                height: shown ? 50 : 0

                                readonly property bool streamable: modelData.streamable === true
                                readonly property string trackId: modelData.id || ""
                                /// The row's live download state. A persisted
                                /// `downloaded` — which the controller has
                                /// already re-scoped to the SELECTED format —
                                /// reads as `ready`, so the green check
                                /// disappears the moment the dropdown moves to
                                /// a format this track was not downloaded in.
                                /// A transient status always wins, because a
                                /// download in flight is newer than the
                                /// registry.
                                readonly property int status: {
                                    var s = modelData.status
                                    if (s === undefined || s === 0)
                                        return modelData.downloaded === true ? 3 : 0
                                    return s
                                }

                                // The item the shared row draws. Built rather
                                // than passed through for three reasons: the
                                // title needs `formatTrackTitle`, the duration
                                // is SECONDS here and TrackRow renders it as a
                                // string, and the artist line is shown only
                                // when the performer differs from the album
                                // artist (recon §B.2.2-7).
                                readonly property var rowItem: ({
                                    "id": trackRow.trackId,
                                    "title": root.formatTrackTitle(trackRow.modelData.title,
                                                                   trackRow.modelData.version),
                                    "artist": (trackRow.modelData.artist || "") !== ""
                                              && trackRow.modelData.artist !== (root.doc.artist || "")
                                                  ? trackRow.modelData.artist : "",
                                    "duration": root.fmtTrackDuration(trackRow.modelData.duration),
                                    "qualityTier": trackRow.modelData.qualityTier || "",
                                    "qualityDetail": trackRow.modelData.qualityDetail || ""
                                })

                                TrackRow {
                                    // Narrowed by exactly the 28px status cell
                                    // + the 14px gap this row draws outside
                                    // the shared one (LocalTrackRow's pattern).
                                    width: trackRow.width - 42
                                    item: trackRow.rowItem
                                    number: trackRow.modelData.trackNumber > 0
                                        ? trackRow.modelData.trackNumber : (trackRow.index + 1)
                                    zebra: true
                                    showFavorite: false
                                    showDownload: false
                                    showMenu: false
                                    // Not a drag source, and `draggable` is
                                    // also the "this id is a Qobuz catalog id"
                                    // predicate that arms the shared row's own
                                    // OFFLINE-cache column — which must stay
                                    // dark here, because purchase state and
                                    // cache state are different features that
                                    // must not overwrite each other (§G.4).
                                    draggable: false
                                    // Double-click / play disc play only where
                                    // the catalog says the track is streamable;
                                    // the guard is here, not on the row.
                                    onPlayRequested: {
                                        if (trackRow.streamable)
                                            QbzPlayer.playAlbumFrom(root.doc.id || "",
                                                                    trackRow.trackId)
                                    }
                                }

                                // --- The purchase download cell ------------
                                // Same glyph set, sizes and tints as the
                                // shared row's offline column
                                // (TrackRow.qml:782-835) so the two features
                                // read identically — on a DIFFERENT channel.
                                Item {
                                    x: trackRow.width - 40
                                    width: 28
                                    height: 28
                                    anchors.verticalCenter: parent.verticalCenter

                                    // Gated on the SAME predicate the click is:
                                    // on an album the account cannot download,
                                    // or with no synthesized format, the
                                    // reference renders nothing here at all
                                    // (recon §B.2.3, last row) rather than a
                                    // cloud that does nothing.
                                    QbzIcon {
                                        visible: trackRow.status === 0 && dlArea.enabled
                                        anchors.centerIn: parent
                                        name: "cloud-download"
                                        width: 16
                                        height: 16
                                        tintName: dlArea.containsMouse ? "textPrimary" : "muted"
                                    }
                                    QbzIcon {
                                        visible: trackRow.status === 1 || trackRow.status === 2
                                        anchors.centerIn: parent
                                        name: "loader-circle"
                                        width: 16
                                        height: 16
                                        tintName: "accent"
                                        rotation: root.spinDeg
                                    }
                                    QbzIcon {
                                        visible: trackRow.status === 3
                                        anchors.centerIn: parent
                                        name: "circle-check-big"
                                        width: 16
                                        height: 16
                                        tintName: "accent"
                                    }
                                    // Failed keeps its RETRY control and the
                                    // album keeps going (§4.2b): the glyph is
                                    // the control, exactly as the reference
                                    // makes the warning triangle clickable.
                                    QbzIcon {
                                        visible: trackRow.status === 4
                                        anchors.centerIn: parent
                                        name: "triangle-alert"
                                        width: 16
                                        height: 16
                                        tintName: "favorite"
                                    }
                                    MouseArea {
                                        id: dlArea
                                        anchors.fill: parent
                                        // Nothing to download on an album the
                                        // account cannot download, and nothing
                                        // to download into with no format.
                                        enabled: root.downloadable
                                                 && root.formats.length > 0
                                                 && trackRow.status !== 1
                                                 && trackRow.status !== 2
                                        hoverEnabled: true
                                        cursorShape: enabled ? Qt.PointingHandCursor
                                                             : Qt.ArrowCursor
                                        ToolTip.visible: containsMouse && trackRow.status === 4
                                        ToolTip.text: root.t("Retry")
                                        ToolTip.delay: 350
                                        onClicked: {
                                            if (trackRow.status === 4)
                                                QbzPurchases.retryTrack(trackRow.trackId)
                                            else
                                                QbzPurchases.downloadTrack(trackRow.trackId)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Nothing to list. Reached when the album resolved but carries
                // no tracks — rare, and the alternative is a labelled header
                // over blank space.
                // (No `anchors.horizontalCenter` — this is a Column child, and
                // the Column owns x. QbzEmptyState centres its own contents.)
                QbzEmptyState {
                    visible: root.ready && root.discs.length === 0
                    width: parent.width
                    iconName: "shopping-bag"
                    title: root.t("No purchased tracks yet")
                }
            }
        }
    }

    // Thin auto-hiding scrollbar (ListScrollbar).
    // Back/forward scroll memory (controls/ScrollMemory.qml): reports
    // this container's offset while it is the live page, and restores it
    // when a back/forward step arms this route.
    ScrollMemory { target: pageFlick; scope: "purchase-album" }
    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: pageFlick
    }

    Popup {
        id: copiesModal
        parent: Overlay.overlay
        x: 0
        y: 0
        width: parent ? parent.width : 0
        height: parent ? parent.height : 0
        padding: 0
        z: 3100
        modal: true
        dim: false
        closePolicy: Popup.CloseOnEscape

        background: Rectangle { color: "#bf000000" }

        contentItem: Item {
            MouseArea {
                anchors.fill: parent
                onClicked: copiesModal.close()
                onWheel: function (wheel) { wheel.accepted = true }
            }

            Rectangle {
                id: copiesCard
                width: Math.min(copiesModal.width - 48, 840)
                height: Math.min(copiesModal.height - 80,
                                 Math.max(180, 96 + root.copies.length * 84))
                anchors.centerIn: parent
                radius: theme.radiusMd
                color: theme.surfaceCard
                border.width: 1
                border.color: theme.borderSubtle
                clip: true

                MouseArea {
                    anchors.fill: parent
                    onWheel: function (wheel) { wheel.accepted = true }
                }

                Text {
                    id: copiesTitle
                    anchors.left: parent.left
                    anchors.leftMargin: 24
                    anchors.right: copiesClose.left
                    anchors.rightMargin: 16
                    anchors.top: parent.top
                    anchors.topMargin: 20
                    text: root.t("Local purchase copies")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightBold
                    elide: Text.ElideRight
                }

                Rectangle {
                    id: copiesClose
                    width: 32
                    height: 32
                    radius: theme.radiusSm
                    anchors.right: parent.right
                    anchors.rightMargin: 16
                    anchors.top: parent.top
                    anchors.topMargin: 14
                    color: copiesCloseArea.containsMouse ? theme.surfaceHover : "transparent"
                    Accessible.role: Accessible.Button
                    Accessible.name: root.t("Close")
                    Accessible.onPressAction: copiesModal.close()
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 16
                        height: 16
                        tintName: "muted"
                    }
                    MouseArea {
                        id: copiesCloseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: copiesModal.close()
                    }
                }

                Rectangle {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.topMargin: 64
                    height: 1
                    color: theme.borderSubtle
                }

                ListView {
                    id: copiesList
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    anchors.leftMargin: 20
                    anchors.rightMargin: 20
                    anchors.topMargin: 76
                    anchors.bottomMargin: 16
                    spacing: 6
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds
                    model: root.copies

                    delegate: Rectangle {
                        id: copyRow
                        required property var modelData
                        width: copiesList.width - 8
                        height: 78
                        radius: theme.radiusSm
                        color: theme.surfaceElevated
                        border.width: 1
                        border.color: theme.borderSubtle

                        Column {
                            anchors.left: parent.left
                            anchors.right: copyActions.left
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 4
                            Text {
                                width: parent.width
                                text: root.copyTitle(copyRow.modelData)
                                color: theme.textPrimary
                                font.pixelSize: theme.fontBody
                                font.weight: theme.weightSemibold
                                elide: Text.ElideMiddle
                            }
                            Text {
                                width: parent.width
                                text: copyRow.modelData.folderPath
                                      || copyRow.modelData.folderLabel || ""
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideMiddle
                            }
                            Text {
                                width: parent.width
                                text: root.copyStateLabel(copyRow.modelData.state) + "  ·  "
                                      + root.t("{} of {} tracks")
                                          .replace("{}", copyRow.modelData.healthyTracks || 0)
                                          .replace("{}", copyRow.modelData.totalTracks || 0)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideRight
                            }
                        }

                        Row {
                            id: copyActions
                            anchors.right: parent.right
                            anchors.rightMargin: 8
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 8
                            Text {
                                visible: copyRow.modelData.inLocalLibrary === true
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.t("In Local Library")
                                color: theme.success
                                font.pixelSize: theme.fontLegal
                            }
                            IconTextButton {
                                visible: copyRow.modelData.continuable === true
                                         && Number(copyRow.modelData.formatId) === Number(root.doc.selectedFormatId)
                                label: root.t("Continue copy")
                                iconName: "cloud-download"
                                btnEnabled: root.progress.active !== true
                                onClicked: {
                                    copiesModal.close()
                                    QbzPurchases.continueCopy(copyRow.modelData.copyId || "")
                                }
                            }
                            IconTextButton {
                                visible: copyRow.modelData.inLocalLibrary !== true
                                         && Number(copyRow.modelData.libraryFolderId || 0) <= 0
                                         && Number(copyRow.modelData.downloadedTracks || 0) > 0
                                label: root.t("Add to Local Library")
                                iconName: "hard-drive"
                                onClicked: {
                                    copiesModal.close()
                                    QbzPurchases.addCopyToLibrary(copyRow.modelData.copyId || "")
                                }
                            }
                            Rectangle {
                                width: 32
                                height: 32
                                radius: theme.radiusSm
                                color: copyMoreArea.containsMouse ? theme.surfaceHover : "transparent"
                                Accessible.role: Accessible.Button
                                Accessible.name: root.t("More options")
                                Accessible.onPressAction: {
                                    root.copyMenuTarget = copyRow.modelData
                                    copyMenu.openBelowRight(copyMoreArea)
                                }
                                QbzIcon {
                                    anchors.centerIn: parent
                                    name: "ellipsis"
                                    width: 16
                                    height: 16
                                    tintName: "muted"
                                }
                                MouseArea {
                                    id: copyMoreArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        root.copyMenuTarget = copyRow.modelData
                                        copyMenu.openBelowRight(copyMoreArea)
                                    }
                                }
                            }
                        }
                    }

                }

                QbzScrollBar {
                    anchors.right: parent.right
                    anchors.rightMargin: 4
                    anchors.top: copiesList.top
                    anchors.bottom: copiesList.bottom
                    target: copiesList
                }
            }
        }
    }

    QbzContextMenu {
        id: copyMenu
        z: 3300
        menuWidth: 230

        Rectangle {
            width: parent.width
            height: 33
            radius: theme.radiusSm
            color: showLocationArea.containsMouse ? theme.surfaceHover : "transparent"
            QbzIcon { x: 9; anchors.verticalCenter: parent.verticalCenter; name: "folder-open"; width: 15; height: 15; tintName: "muted" }
            Text { x: 32; anchors.verticalCenter: parent.verticalCenter; text: root.t("Show location"); color: theme.textPrimary; font.pixelSize: theme.fontLegal }
            MouseArea {
                id: showLocationArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    copyMenu.close()
                    QbzPurchases.showCopyLocation(root.copyMenuTarget.copyId || "")
                }
            }
        }

        Rectangle {
            visible: Number(root.copyMenuTarget.libraryFolderId || 0) > 0
            width: parent.width
            height: visible ? 33 : 0
            radius: theme.radiusSm
            color: removeLibraryArea.containsMouse ? theme.dangerHover : "transparent"
            QbzIcon { x: 9; anchors.verticalCenter: parent.verticalCenter; name: "hard-drive"; width: 15; height: 15; tintName: "danger" }
            Text { x: 32; anchors.verticalCenter: parent.verticalCenter; text: root.t("Remove from Local Library"); color: theme.danger; font.pixelSize: theme.fontLegal }
            MouseArea {
                id: removeLibraryArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    copyMenu.close()
                    QbzPurchases.removeCopyFromLibrary(root.copyMenuTarget.copyId || "")
                }
            }
        }

        Rectangle {
            visible: root.copyMenuTarget.inLocalLibrary !== true
                     && Number(root.copyMenuTarget.libraryFolderId || 0) <= 0
            width: parent.width
            height: visible ? 33 : 0
            radius: theme.radiusSm
            color: forgetRecordArea.containsMouse ? theme.dangerHover : "transparent"
            QbzIcon { x: 9; anchors.verticalCenter: parent.verticalCenter; name: "trash-2"; width: 15; height: 15; tintName: "danger" }
            Text { x: 32; anchors.verticalCenter: parent.verticalCenter; text: root.t("Forget download record"); color: theme.danger; font.pixelSize: theme.fontLegal }
            MouseArea {
                id: forgetRecordArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    copyMenu.close()
                    QbzPurchases.forgetCopyRecord(root.copyMenuTarget.copyId || "")
                }
            }
        }
    }

    QbzConfirmModal {
        id: batchModal
        anchors.fill: parent
        title: root.t("Download finished")
        body: root.batchResultBody()
        confirmLabel: root.t("Add to Local Library")
        cancelLabel: root.t("Not now")
        danger: false
        onConfirmed: {
            var copyId = root.batchResult.copyId || ""
            QbzPurchases.dismissBatchResult(copyId)
            QbzPurchases.addCopyToLibrary(copyId)
        }
        onCancelled: QbzPurchases.dismissBatchResult(root.batchResult.copyId || "")
    }
}
