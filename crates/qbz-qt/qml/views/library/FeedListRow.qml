// FeedListRow — the Library "All" feed's LIST row (FavoritesView.slint:1212+,
// the windowed mixed row: cover · title/subtitle · TYPE · source glyph ·
// quality · favourite indicator · ⋯).
//
// EXTRACTED from views/LibraryView.qml, where it was an inline `component`
// ~300 lines long inside a 1,880-line file (track rule 2). Behaviour is
// unchanged; the only edit is that the six things it used to read off `root`
// now come through `view` — the LibraryView instance — which is the same seam
// views/local/*.qml uses for LocalLibraryView.

import QtQuick
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../theme"

Rectangle {
    id: feedRow

    /// The LibraryView root: artMap, skelPhase, showLocal and the three
    /// shared play/menu helpers live there (never duplicated per row).
    property var view: null
    property var item: ({})
    /// Viewport-relative position, for the skeleton's animated cap.
    property int rowIndex: 0
    /// Absolute model position, for the stable zebra independent of pooling.
    property int displayIndex: 0

    QbzTheme { id: theme }

    height: 50
    radius: 6
    color: rowArea.containsMouse && !feedRow.pulledDead
        ? theme.surfaceHover
        : (feedRow.displayIndex % 2 === 1 ? theme.alphaTier(4) : "transparent")

    // Catalog-withdrawal contract shared with TrackRow/TrackCard, plus a
    // persisted local favorite whose source album has disappeared. A complete
    // offline copy only revives the Qobuz arm; a missing local source remains
    // a non-navigable tombstone.
    readonly property bool sourceMissing:
        feedRow.item.sourceUnavailable === true
    readonly property bool pulled: (feedRow.item.kind === "track"
        || feedRow.item.kind === "album")
        && (feedRow.item.qobuzUnavailable === true || feedRow.sourceMissing)
    property int cacheStatus: feedRow.item.cacheStatus !== undefined
        ? feedRow.item.cacheStatus : 0
    readonly property bool pulledDead: feedRow.sourceMissing
        || (feedRow.pulled && feedRow.cacheStatus !== 3)

    // Live heart, as a real property — `item.isFavorite = !…` notified
    // nothing (plain JS object; see rows/TrackRow.qml for the measurement)
    // so this row's glyph and its menu label never moved on click. The
    // binding is re-established on every new row object, so a republished
    // feed still wins.
    property bool favorite: feedRow.item.isFavorite === true
    onItemChanged: {
        feedRow.favorite = Qt.binding(function () {
            return feedRow.item.isFavorite === true
        })
        feedRow.cacheStatus = Qt.binding(function () {
            return feedRow.item.cacheStatus !== undefined
                ? feedRow.item.cacheStatus : 0
        })
        feedRow.releaseForReuse()
    }
    function toggleFavorite() {
        feedRow.favorite = !feedRow.favorite
        QbzLibrary.libraryToggleFavorite(feedRow.item.kind, feedRow.item.id)
    }
    function albumMenuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var m = []
        if (!feedRow.pulledDead) {
            m.push({ "label": t("Open album", r), "icon": "library-big", "action": "open" })
            m.push({ "label": t("Play", r), "icon": "play-fill", "action": "play" })
        }
        // The old id may be dead, but the user must still be able to remove
        // its favorite entry instead of being left with a permanent tombstone.
        m.push({ "label": feedRow.favorite ? t("Remove from Library", r) : t("Add to Library", r),
                 "icon": feedRow.favorite ? "heart-filled" : "heart", "action": "favorite" })
        if (feedRow.item.source === "qobuz"
                && feedRow.item.qobuzUnavailable === true) {
            m.push({ "label": t("Look for better replacement", r),
                     "icon": "search", "action": "find-release" })
        }
        return m
    }
    function albumAction(action) {
        if ((action === "open" || action === "play") && feedRow.pulledDead)
            return
        if (action === "open") QbzAlbum.openAlbum(feedRow.item.id)
        else if (action === "play") QbzPlayer.playAlbum(feedRow.item.id)
        else if (action === "favorite") feedRow.toggleFavorite()
        else if (action === "find-release") QbzTrackReplace.openRelease(JSON.stringify({
            "targetKind": "album",
            "albumId": feedRow.item.id || "",
            "albumTitle": feedRow.item.title || "",
            "artist": feedRow.item.artist || "",
            "albumArtist": feedRow.item.artist || ""
        }))
    }
    // Settle + rollback + cross-surface walk. `artKey` IS
    // `library_qt::feed_key(kind, id)`, the very key the signal carries —
    // which is also why LibraryView's own handler can patch the backing row by
    // comparing against it.
    Connections {
        target: QbzLibrary
        function onLibraryFavoriteChanged(key, value) {
            if ((feedRow.item.artKey || "") !== "" && key === feedRow.item.artKey)
                feedRow.favorite = value
        }
    }
    Connections {
        target: QbzShell
        function onTrackCacheStatusChanged(trackId, status, progress) {
            // Numeric Qobuz ids overlap across entity types. This signal is
            // track-scoped and must not revive an album with the same number.
            if (feedRow.item.kind === "track"
                    && trackId === (feedRow.item.id || ""))
                feedRow.cacheStatus = status
        }
    }

    readonly property bool isTrack: feedRow.item.kind === "track"
    function playTrack() {
        if (!feedRow.pulledDead)
            feedRow.view.playTrackInContext(feedRow.item.id)
    }

    // Row body — click opens by kind. Declared BEFORE the cells so the ⋯
    // button and art-play win their clicks. A TRACK row follows the track-row
    // policy (#790, rows/TrackRow.qml `clickPlays`): the art-play disc plays,
    // a single click on the body does nothing, a double click plays.
    MouseArea {
        id: rowArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: (feedRow.pulledDead || feedRow.isTrack) ? Qt.ArrowCursor : Qt.PointingHandCursor
        onDoubleClicked: if (feedRow.isTrack) feedRow.playTrack()
        onClicked: {
            if (feedRow.isTrack)
                return
            if (feedRow.item.kind === "album") {
                if (!feedRow.pulledDead)
                    QbzAlbum.openAlbum(feedRow.item.id)
            }
            else if (feedRow.item.kind === "artist") QbzArtist.openArtist(feedRow.item.id)
            else if (feedRow.item.kind === "playlist") QbzBridge.openPlaylist(feedRow.item.id)
            else if (feedRow.item.kind === "label") QbzHome.openLabel(feedRow.item.id)
        }
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: 12
        opacity: feedRow.pulledDead ? 0.5 : 1.0
        // Col — artwork (round for artists) + hover play.
        Rectangle {
            width: 36
            height: 36
            anchors.verticalCenter: parent.verticalCenter
            radius: feedRow.item.kind === "artist" ? 18 : 5
            color: theme.surfaceElevated
            clip: true
            RoundedImage {
                anchors.fill: parent
                visible: !lrCollage.visible
                source: feedRow.view.artMap[feedRow.item.artKey] || ""
                radius: feedRow.item.kind === "artist" ? 18 : 5
                // A label logo is contain-fitted (cropping cuts the
                // wordmark) on a FLAT surface — those are transparent
                // PNGs whose edges carry no colour to derive from.
                // A playlist takes "auto": its own graphic is the 800x380
                // `image_rectangle` and pads with an image-derived
                // gradient, while a square member cover still crops. The
                // flag is not required for that any more, but it stays
                // authoritative where it IS published (this feed).
                fit: feedRow.item.kind === "label" ? "contain"
                    : feedRow.item.kind === "playlist"
                        ? (feedRow.item.playlistOwnImage === true ? "pad" : "auto")
                        : "crop"
            }
            // User playlists have no graphic of their own, so they show the
            // member-cover mosaic instead of a blank tile.
            PlaylistCollage {
                id: lrCollage
                anchors.fill: parent
                visible: feedRow.item.kind === "playlist"
                    && feedRow.item.playlistOwnImage !== true
                    && (feedRow.view.artMap[feedRow.item.artKey] || "") === ""
                urls: feedRow.item.covers || []
                radius: 6
            }
            // Per-row cover placeholder — same progressive rule as the
            // grid: it clears when THIS row's cover lands. Skipped for
            // the collage arm (a playlist mosaic is real content) and
            // for artists/labels (round designed placeholders).
            QbzSkeleton {
                variant: "art"
                anchors.fill: parent
                blockRadius: 6
                visible: (feedRow.item.kind === "track" || feedRow.item.kind === "album")
                    && feedRow.item.imageUrl !== ""
                    && (feedRow.view.artMap[feedRow.item.artKey] || "") === ""
                    && !feedRow.pulledDead
                phase: feedRow.view.skelPhase
                cellIndex: feedRow.rowIndex
            }
            Rectangle {
                visible: (feedRow.item.kind === "track" || feedRow.item.kind === "album"
                          || feedRow.item.kind === "playlist")
                    && !feedRow.pulledDead
                anchors.fill: parent
                radius: feedRow.item.kind === "artist" ? 18 : 5
                color: "#a6000000"
                opacity: lrPlayArea.containsMouse ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                // On the #a6000000 artwork scrim — dark under every theme.
                QbzIcon { name: "play-fill"; width: 16; height: 16; anchors.centerIn: parent; tintName: "white" }
                MouseArea {
                    id: lrPlayArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        if (feedRow.item.kind === "track") {
                            if (!feedRow.pulledDead)
                                feedRow.view.playTrackInContext(feedRow.item.id)
                        }
                        else if (feedRow.item.kind === "album") {
                            if (!feedRow.pulledDead)
                                QbzPlayer.playAlbum(feedRow.item.id)
                        }
                        // `playPlaylistById`, NOT `playPlaylist` — the latter
                        // plays the OPEN playlist page and takes no id
                        // (player_bridge.rs:189 is the by-id entry).
                        else if (feedRow.item.kind === "playlist") QbzPlayer.playPlaylistById(feedRow.item.id)
                    }
                }
            }
            // On a 36px thumbnail a text badge would be illegible. The alert
            // glyph is the same semantic mark TrackRow uses in its number
            // cell; the explicit translated label lives in the quality slot.
            QbzIcon {
                visible: feedRow.pulledDead
                name: "circle-alert"
                width: 16
                height: 16
                anchors.centerIn: parent
                tintName: "favorite"
            }
        }
        // Col — title + subtitle (artist link for track/album).
        Column {
            width: parent.width - 36 - 116 - (srcCol.visible ? 44 : 0)
                - 150 - 18 - 30 - 6 * 12
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Text {
                width: parent.width
                height: 18
                text: feedRow.item.title
                color: lrTitleArea.containsMouse && !feedRow.pulledDead
                    ? theme.accent : theme.textPrimary
                font.pixelSize: 14
                font.weight: theme.weightMedium
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
                MouseArea {
                    id: lrTitleArea
                    anchors.fill: parent
                    // A track's title is not a link: disabled, its clicks
                    // reach the row body (single = nothing, double = play).
                    enabled: !feedRow.isTrack
                    hoverEnabled: true
                    cursorShape: feedRow.pulledDead ? Qt.ArrowCursor : Qt.PointingHandCursor
                    onClicked: {
                        if (feedRow.item.kind === "album") {
                            if (!feedRow.pulledDead)
                                QbzAlbum.openAlbum(feedRow.item.id)
                        }
                        else if (feedRow.item.kind === "artist") QbzArtist.openArtist(feedRow.item.id)
                        else if (feedRow.item.kind === "playlist") QbzBridge.openPlaylist(feedRow.item.id)
                        else if (feedRow.item.kind === "label") QbzHome.openLabel(feedRow.item.id)
                    }
                }
            }
            Text {
                visible: feedRow.item.subtitle !== ""
                width: parent.width
                height: 16
                text: feedRow.item.subtitle
                color: (feedRow.item.artistId !== ""
                        && (feedRow.item.kind === "track" || feedRow.item.kind === "album")
                        && lrSubArea.hotName) ? theme.accent : theme.textMuted
                font.pixelSize: 12
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
                MouseArea {
                    id: lrSubArea
                    anchors.fill: parent
                    hoverEnabled: true
                    // On a track row only the drawn name is the link; the
                    // whitespace past it is row body (double click plays).
                    readonly property bool isLink: feedRow.item.artistId !== ""
                        && (feedRow.item.kind === "track" || feedRow.item.kind === "album")
                    function overName(x) { return !feedRow.isTrack || x <= parent.contentWidth }
                    property bool hotName: false
                    onPositionChanged: function (mouse) { hotName = overName(mouse.x) }
                    onExited: hotName = false
                    cursorShape: (isLink && hotName) ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: function (mouse) {
                        if (feedRow.item.artistId !== "" && overName(mouse.x))
                            QbzArtist.openArtist(feedRow.item.artistId)
                    }
                    onDoubleClicked: function (mouse) {
                        if (feedRow.isTrack && !(isLink && overName(mouse.x)))
                            feedRow.playTrack()
                    }
                }
            }
        }
        // Col — type (icon + caps label).
        Rectangle {
            width: 116
            height: parent.height
            color: "transparent"
            Row {
                spacing: 6
                anchors.verticalCenter: parent.verticalCenter
                QbzIcon {
                    name: feedRow.item.kind === "track" ? "music"
                        : feedRow.item.kind === "playlist" ? "list-music"
                        : feedRow.item.kind === "artist" ? "user"
                        : feedRow.item.kind === "label" ? "disc-3" : "disc"
                    width: 13
                    height: 13
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "muted"
                }
                Text {
                    text: feedRow.item.kind === "track" ? QbzSession.tr("Track", QbzSession.trRev)
                        : feedRow.item.kind === "album" ? QbzSession.tr("Album", QbzSession.trRev)
                        : feedRow.item.kind === "artist" ? QbzSession.tr("Artist", QbzSession.trRev)
                        : feedRow.item.kind === "playlist" ? QbzSession.tr("Playlist", QbzSession.trRev)
                        : QbzSession.tr("Label", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.2
                    verticalAlignment: Text.AlignVCenter
                }
            }
        }
        // Col — source glyph (only when local/Plex can appear).
        Rectangle {
            id: srcCol
            visible: feedRow.view.showSourceBadges
            width: 44
            height: parent.height
            color: "transparent"
            // Through controls/SourceIcon.qml, never QbzIcon: the Plex and
            // Qobuz marks are MULTI-COLOUR and a tint flattens them to a
            // silhouette. This column used to draw an accent-tinted
            // `hard-drive` for Plex — a blue hard drive.
            //
            // DEVIATION, stated on purpose: the .slint has NO source column in
            // this list at all (FavoritesView.slint:1767/2154 pass
            // `show-source: false` to AlbumCollectionView). The numbers below
            // are therefore its siblings' — AlbumListRow.slint:319, the row
            // glyph, not the card badge.
            SourceIcon {
                visible: (feedRow.item.source || "") !== ""
                    && feedRow.item.source !== "qobuz"
                kind: feedRow.item.source || ""
                // A dense ROW: the media marks draw monochrome and tinted, like the
                // hard-drive beside them. Colour logos are for cards — a list of
                // them fights the text it labels.
                mono: true
                glyphSize: 15
                plexSize: 16
                qobuzSize: 16
                // Host is a transparent column over the theme row (NOT an
                // artwork scrim), so this matches its siblings
                // local/LocalAlbumRow.qml and local/LocalTrackRow.qml.
                localTint: "muted"
                anchors.verticalCenter: parent.verticalCenter
            }
        }
        // Col — quality (albums + tracks).
        Rectangle {
            width: 150
            height: parent.height
            color: "transparent"
            QualityMini {
                visible: (feedRow.item.kind === "album" || feedRow.item.kind === "track")
                    && feedRow.item.qualityTier !== "" && !feedRow.pulledDead
                tier: feedRow.item.qualityTier
                // The CD/MP3 chip is 30px wide while the Hi-Res mark is 42px.
                // Left-aligning both shifts Hi-Res' visual centre 6px right.
                // Keep one 30px mark axis without centring the whole 150px
                // quality column (whose content is intentionally leading).
                x: Math.round((30 - width) / 2)
                anchors.verticalCenter: parent.verticalCenter
            }
            Text {
                visible: feedRow.pulledDead
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("Unavailable", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                font.weight: theme.weightSemibold
            }
        }
        // Col — favorite / follow indicator.
        QbzIcon {
            name: feedRow.item.kind === "artist" ? "user-plus"
                : (feedRow.favorite ? "heart-filled" : "heart")
            width: 18
            height: 18
            anchors.verticalCenter: parent.verticalCenter
            tintName: "accent"
        }
        // Col — context-menu button.
        Rectangle {
            width: 30
            height: 30
            radius: 6
            anchors.verticalCenter: parent.verticalCenter
            color: lrMenuArea.containsMouse ? theme.surfaceElevated : "transparent"
            QbzIcon { name: "ellipsis"; width: 16; height: 16; anchors.centerIn: parent; tintName: "secondary" }
            MouseArea {
                id: lrMenuArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: function (mouse) { feedRow.openLrMenu(lrMenuArea, mouse.x, mouse.y) }
            }
        }
    }
    // LAZY. This is a QtQuick.Controls Popup with a Repeater over its
    // entries, and it was built EAGERLY for every row — so a long list
    // constructed one whole popup per visible row, and rebuilt them all
    // on every scroll step, to show a menu only ever opened on click.
    // Same lazy-Loader idiom this file already uses for the clipboard
    // helper and the info modal.
    Loader {
        id: lrMenuLoader
        active: false
        sourceComponent: CardMenu {
            menuWidth: 196
            entries: feedRow.item.kind === "track"
                ? feedRow.view.trackMenuModel(feedRow.item, feedRow.favorite,
                                              feedRow.pulledDead)
                : feedRow.item.kind === "album" ? feedRow.albumMenuModel()
                : feedRow.item.kind === "artist" ? [
                    { "label": QbzSession.tr("Go to artist", QbzSession.trRev), "icon": "user", "action": "go-artist" },
                ]
                : [
                    { "label": QbzSession.tr("Open", QbzSession.trRev), "icon": "list-music", "action": "open" },
                    { "label": feedRow.favorite ? QbzSession.tr("Remove from Library", QbzSession.trRev) : QbzSession.tr("Add to Library", QbzSession.trRev),
                      "icon": feedRow.favorite ? "heart-filled" : "heart", "action": "favorite" },
                ]
            onPicked: function (a) {
                if (feedRow.item.kind === "track") { feedRow.view.trackAction(feedRow, a); return }
                if (feedRow.item.kind === "album") { feedRow.albumAction(a); return }
                if (a === "open") {
                    if (feedRow.item.kind === "playlist") QbzBridge.openPlaylist(feedRow.item.id)
                    else if (feedRow.item.kind === "label") QbzHome.openLabel(feedRow.item.id)
                } else if (a === "go-artist") {
                    QbzArtist.openArtist(feedRow.item.id)
                } else if (a === "favorite") {
                    feedRow.toggleFavorite()
                }
            }
        }
    }
    /// Build the popup on first use, then open it.
    function openLrMenu(anchor, x, y) {
        var entries = feedRow.item.kind === "track"
            ? feedRow.view.trackMenuModel(feedRow.item, feedRow.favorite,
                                          feedRow.pulledDead)
            : feedRow.item.kind === "album" ? feedRow.albumMenuModel() : [1]
        if (entries.length === 0)
            return
        lrMenuLoader.active = true
        lrMenuLoader.item.openAtCursor(anchor, x, y)
    }
    function releaseForReuse() {
        if (lrMenuLoader.item)
            lrMenuLoader.item.close()
        lrMenuLoader.active = false
    }
}
