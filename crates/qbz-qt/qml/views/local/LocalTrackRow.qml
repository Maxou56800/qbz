// Local track row — the SHARED rows/TrackRow.qml with the local arms set,
// plus the three things the shared row has no arm for:
//
//   * the always-visible SOURCE glyph the Slint renders from
//     `show-source: true` (LocalLibraryView.slint:1489) — drawn OUTSIDE the
//     shared row so TrackRow keeps one column layout for every surface;
//   * the multi-select checkbox (Slint TrackRow's `multi-select-mode`
//     swaps the number cell for it) — overlaid on the number cell here for
//     the same reason: no fork of the shared row, no shared-file edit;
//   * the LOCAL context menu.
//
// THE MENU (this is the `force-local-menu` arm of the unified Slint menu —
// TrackRow.slint:68 -> TrackMenuState -> AppShell.slint:1009-1021 ->
// TrackContextMenu.slint). For a local / Plex / offline row on a library
// surface Slint resolves to: qobuz-actions OFF, favorite OFF, Track info OFF
// (show-track-info rides the Qobuz block), mixtape ON, add-to-playlist ON,
// Go to album / Go to artist ON (`local-goto-actions`). So the full local set
// is: Play now, Play next, Play later, Add to queue, Add to mixtape, Add to
// playlist, Go to album, Go to artist.
//
// Add to playlist is LIVE (QbzPlaylistPicker). It goes through
// `QbzLocal.bulkAction("track", [id], "add-to-playlist")` — the LOCAL-mode
// arm — and not through the picker singleton directly, because only
// `local_bulk::resolve_blocking` can turn a Tracks-table row id into the
// `LocalTrack` that `local_picker_ref_for_track` needs; a Plex row's ref is
// "plex:<rating key>" and its numeric display id means a different track the
// moment anything reads it as a catalog id. The bulk entry point takes a JSON
// id array, so a single row is a one-element array — same code path, no
// second resolver.
//
// Add to mixtape rides the SAME entry point for the same reason
// (`local_bulk.rs::apply` "add-to-mixtape" -> `myqbz_add_qt::open_items(
// track_items_from_local(rows))`), which restores the .slint's order:
// Play now · Play next · Play later · Add to queue · Add to mixtape ·
// Add to playlist · Go to album · Go to artist. Every row that IS shown is
// wired; nothing here renders and no-ops.
//
// The shared row's own ⋯ is turned OFF (`showMenu: false`) because its
// entries are the Qobuz set: its Go-to-album/artist call QbzAlbum.openAlbum /
// QbzArtist.openArtist with CATALOG ids, and a local row carries a local
// group key + a bare artist NAME. The button is re-drawn here, in the exact
// same gutter (the shared row is narrowed by precisely the 32px cell + 14px
// gap it no longer draws), and both it and a right-click open this menu.
//
// Artwork follows the Slint gate (AppearanceState.local-library-track-
// artwork, default OFF) — decoding a cover per row is exactly what froze
// this surface at 16k tracks.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../rows"
import "../../theme"

Item {
    property bool kioskHost: false
    id: root

    property var item: ({})
    property int number: 0
    property bool showAlbum: true
    property bool showArtwork: false
    property string artSource: ""
    /// The LocalLibraryView root (shared skeleton pulse + artwork gate).
    property var view: null
    property bool selectMode: false
    property bool checked: false
    property bool nativeActions: false
    property int nativeIndex: -1
    /// Alternating row tint, forwarded to the shared row. Off by default there
    /// (rows/TrackRow.qml), which is why the local album page had none while
    /// the Qobuz one (AlbumView.qml) did — the flag was simply never handed
    /// through this wrapper.
    property bool zebra: false
    signal playRequested()
    signal enqueueRequested(string mode)
    /// `modifiers` rides straight off the mouse event: Shift is what turns
    /// a click into a range (controls/SelectionModel.qml).
    signal toggleSelect(int modifiers)

    QbzTheme { id: theme }

    height: kioskHost ? 64 : 50

    // The shared TrackRow takes its artwork from `item.artPath`, and a LOCAL
    // track row publishes an empty one (local_rows.rs:284) — the cover comes
    // from the id-keyed artMap instead. Patch it in rather than fork the
    // shared row; the copy only happens when a cover is actually present.
    readonly property var rowItem: root.artSource === ""
        ? root.item
        : Object.assign({}, root.item, { "artPath": root.artSource })
    readonly property string runtimeError: root.view
        ? (root.view.localTrackErrors[
            root.view.localTrackErrorKey(root.item.source || "local", root.item.id)] || "")
        : ""

    // Local rows have a local album GROUP KEY and a bare artist NAME; both
    // routes are name/key routes, so each entry is gated on its own datum.
    readonly property bool canGoAlbum: (item.albumId || "") !== ""
    readonly property bool canGoArtist: (item.artist || "") !== ""
    // Local/network files may write a sidecar or their canonical tag layer;
    // media-server rows use the same editor but are deliberately sidecar-only.
    // Offline Qobuz copies are excluded: their cache file is not user-owned
    // library metadata and has no stable server-album identity.
    readonly property bool canEditMetadata: [
        "local", "plex", "jellyfin", "subsonic", "navidrome",
        "gonic", "airsonic", "astiga"
    ].indexOf(String(item.source || "").toLowerCase()) >= 0

    // Zebra is painted HERE, not forwarded to the shared row. TrackRow tints
    // its own background rectangle, which is `parent.width` wide — and this
    // wrapper hands it `root.width - 26 - 46`, holding back a 26px gutter for
    // the source glyph and 46px for the context menu it draws itself. Tinting
    // from in there therefore stopped 72px short and left those two controls
    // sitting outside the stripe. Declared before the shared row so it paints
    // underneath, and with no pointer area of its own.
    Rectangle {
        anchors.fill: parent
        radius: 8
        // Same literal and same parity rule as rows/TrackRow.qml, so a local
        // list and a Qobuz list stripe identically.
        color: (root.zebra && root.number % 2 === 0) ? "#07ffffff" : "transparent"
    }

    // The trailing gutter this wrapper draws OUTSIDE the shared row (source
    // glyph + ⋯ cell) is row body too: without it a double click there — or a
    // select-mode click — landed on nothing (#790). Covers ONLY the gutter,
    // never the shared row, so it cannot receive the clicks the shared row's
    // body area propagates. Declared before the ⋯ button so that stays on top.
    MouseArea {
        x: sharedRow.width
        width: root.width - sharedRow.width
        height: root.height
        onClicked: function (mouse) {
            if (root.selectMode) root.toggleSelect(mouse.modifiers)
            else sharedRow.bodyClicked()
        }
        onDoubleClicked: sharedRow.bodyDoubleClicked()
    }

    TrackRow { kioskHost: root.kioskHost;
        id: sharedRow
        // 26px source-glyph gutter + the 46px (32 cell + 14 gap) the shared
        // row would have spent on its own ⋯ — so every other column lands
        // exactly where it did before the menu moved out.
        width: root.width - 26 - (root.kioskHost ? 58 : 46)
        item: root.rowItem
        number: root.number
        showArtwork: root.showArtwork
        showAlbum: root.showAlbum
        showFavorite: false
        showDownload: false
        showMenu: false
        // Local rows drag through the LOCAL arm: item.id is a local DB row
        // id, so the shared drag carries it typed (`dragStartLocal`) and the
        // drop handlers resolve it source-aware — the same guardrails as the
        // picker. `draggable` stays false so `catalogRow` keeps meaning
        // "item.id is a Qobuz catalog id".
        draggable: false
        // NOT for native-catalog rows: their `item.id` is the catalog seam's
        // identity, not a legacy row id the drop resolver can look up — a
        // ghost that lands as a silent no-op is worse than no gesture.
        localDragSource: !root.nativeActions
        qualityStyle: "icon"
        // A failed physical row stays clickable so the user can retry after a
        // drive returns. The preflight prevents any queue/NPB mutation until
        // that retry succeeds; the marker is cleared by the success signal.
        leadingMarkerIcon: root.runtimeError !== "" ? "circle-alert" : ""
        // Multi-select is the SHARED row's arm now (rows/TrackRow.qml:116,
        // the port of TrackRow.slint:58 `multi-select-mode`). This file used
        // to overlay its own SelectCheck on the number cell because the
        // shared row had no such arm — it does now, and the overlay was
        // strictly worse: it drew a checkbox but left `sharedRow.selectMode`
        // false, so the row BODY kept its normal gestures and a single click
        // anywhere off the 14px disc PLAYED the track instead of selecting
        // it. TrackRow.slint:174 is explicit that in select mode "the WHOLE
        // row body is a selection target", and LocalAlbumRow.qml:55 already
        // does that for the albums tab. Forwarding the three members gets the
        // reference behaviour for free: body-click toggles, double-click no
        // longer plays, the number/play cell stands down, and the checkbox is
        // the 14px accent disc the .slint actually specifies.
        selectMode: root.selectMode
        checked: root.checked
        onToggleSelect: function (mods) { root.toggleSelect(mods) }
        onPlayRequested: root.playRequested()
        onEnqueueRequested: function (m) { root.enqueueRequested(m) }
    }

    // ⋯ menu button — the shared row's own gutter, re-drawn with the local
    // action set behind it.
    Rectangle {
        id: moreBtn
        x: root.width - (root.kioskHost ? 82 : 70)
        width: root.kioskHost ? 44 : 32
        height: root.kioskHost ? 44 : 32
        radius: theme.radiusSm
        anchors.verticalCenter: parent.verticalCenter
        color: moreArea.containsMouse ? theme.surfaceElevated : "transparent"
        QbzIcon {
            name: "ellipsis"
            width: 16
            height: 16
            anchors.centerIn: parent
            tintName: moreArea.containsMouse ? "textPrimary" : "muted"
        }
        MouseArea {
            id: moreArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: function (mouse) { root.openRowMenu(moreArea, mouse.x, mouse.y) }
        }
    }

    // Source glyph (local / offline cache / Plex) — TrackRow.slint:709-727.
    // Through controls/SourceIcon.qml, never QbzIcon: the Plex and Qobuz marks
    // are MULTI-COLOUR and a tint flattens them to a silhouette. This row used
    // to draw an accent-tinted `hard-drive` for Plex (a blue hard drive) and
    // `cloud-download` for an offline copy, where the .slint draws the two
    // brand marks.
    SourceIcon {
        visible: (root.item.source || "") !== ""
        // `sourceRaw` FIRST (contract §D.1): it carries `qobuz_purchase`, which
        // `source` folds into "offline" so the source chips still filter it.
        // A purchased track's row is gold; a plain download's is not.
        kind: root.item.sourceRaw || root.item.source || ""
        // A dense ROW: the media marks draw monochrome and tinted, like the
        // hard-drive beside them. Colour logos are for cards — a list of
        // them fights the text it labels.
        mono: true
        // .slint:722 — local 15, the marks 16. `localTint` muted is the rule
        // SourceGlyph.slint:6-8 calls load-bearing (never accent).
        glyphSize: 15
        plexSize: 16
        qobuzSize: 16
        localTint: "muted"
        // The glyph's CENTRE stays where the 14px icon's was (x = width-20,
        // centre = width-13) now that the marks are 15-16px wide.
        x: Math.round(root.width - 13 - width / 2)
        anchors.verticalCenter: parent.verticalCenter
    }

    // Right-click anywhere on the row opens the SAME menu at the pointer
    // (TrackRow.slint's `open-track-menu(..., at-pointer: true)`). Declared
    // last so it sits on top, and RIGHT-only so every left click still falls
    // through to the row body / checkbox / ⋯ underneath.
    //
    // Suppressed in select mode — TrackRow.slint:201 gates the same gesture on
    // `!root.multi-select-mode` ("a right-click toggles nothing there"), and
    // the shared row's own trArea already does it (rows/TrackRow.qml:926).
    // Disabled rather than hidden so the press falls through to the row body
    // instead of landing on nothing.
    MouseArea {
        id: rcArea
        anchors.fill: parent
        enabled: !root.selectMode
        acceptedButtons: Qt.RightButton
        onClicked: function (mouse) { root.openRowMenu(rcArea, mouse.x, mouse.y) }
    }

    // LAZY. This is a QtQuick.Controls Popup with a Repeater over its
    // entries, and it was built EAGERLY for every row — so a long list
    // constructed one whole popup per visible row, and rebuilt them all
    // on every scroll step, to show a menu only ever opened on click.
    // Same lazy-Loader idiom this file already uses for the clipboard
    // helper and the info modal.
    Loader {
        id: rowMenuLoader
        active: false
        sourceComponent: CardMenu { kioskHost: root.kioskHost;
            menuWidth: 220
            entries: {
                var m = [
                    { "label": QbzSession.tr("Play now", QbzSession.trRev), "icon": "play-fill", "action": "play" },
                    { "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" },
                    { "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" },
                    { "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" },
                    { "label": QbzSession.tr("Add to mixtape", QbzSession.trRev), "icon": "cassette-tape", "action": "add-to-mixtape" },
                    { "label": QbzSession.tr("Add to playlist", QbzSession.trRev), "icon": "list-music", "action": "add-to-playlist" },
                ]
                if (root.canGoAlbum) m.push({ "label": QbzSession.tr("Go to album", QbzSession.trRev), "icon": "disc-3", "action": "go-album" })
                if (root.canGoArtist) m.push({ "label": QbzSession.tr("Go to artist", QbzSession.trRev), "icon": "user", "action": "go-artist" })
                m.push({ "label": QbzSession.tr("Track info", QbzSession.trRev), "icon": "info", "action": "track-info" })
                if (root.canEditMetadata) m.push({ "label": QbzSession.tr("Edit metadata", QbzSession.trRev), "icon": "pen-line", "action": "edit-metadata" })
                return m
            }
            onPicked: function (a) {
                if (a === "play") root.playRequested()
                // LOCAL-mode adds: one-element selection through the bulk entry
                // point, which owns the only source-aware row -> ref resolver
                // (see the file header).
                else if (a === "add-to-playlist" || a === "add-to-mixtape"
                        || a === "edit-metadata" || a === "track-info") {
                    if (root.nativeActions)
                        QbzLocal.tracksNativeRowAction(root.nativeIndex, a)
                    else
                        QbzLocal.bulkAction("track", JSON.stringify([String(root.item.id)]), a)
                }
                else if (a === "go-album") {
                    QbzLocal.openAlbum(root.item.albumId)
                    QbzShell.navigateTo("localalbum")
                } else if (a === "go-artist") {
                    // Local/Plex artists have no catalog id — a NAME route into
                    // the Local Library Artists tab (local_album_actions.rs).
                    QbzLocal.openArtistByName(root.item.artist)
                    QbzShell.navigateTo("local")
                } else {
                    root.enqueueRequested(a)
                }
            }
        }
    }
    /// Build the popup on first use, then open it.
    function openRowMenu(anchor, x, y) {
        rowMenuLoader.active = true
        rowMenuLoader.item.openAtCursor(anchor, x, y)
    }
}
