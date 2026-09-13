// FeedGridCell — one 200x246 slot of the Library GRID, dispatched by `kind`
// to the SAME card family the Slint mounts (FavoritesView.slint:1084-1187):
// track -> discover/TrackCard, album -> discover/AlbumCard, artist ->
// discover/ArtistGridCard, playlist -> discover/PlaylistCard, label ->
// discover/LabelCard.
//
// EXTRACTED from views/LibraryView.qml's GridView delegate (track rule 2).
// The per-cell cover skeleton rides along, because it is a property of the
// SLOT (it must sit above the card and below nothing) rather than of the view.

import QtQuick
import com.blitzfc.qbz
import "../../cards"
import "../../controls"
import "../../theme"

Item {
    id: cell

    QbzTheme { id: theme }

    /// The LibraryView root (artMap, skelPhase, showLocal, activeTab).
    property var view: null
    /// The feed row this slot renders.
    property var item: ({})
    /// Index INSIDE the reported artwork window — the skeleton's 48-instance
    /// animation cap counts from the viewport, never from a 10k-row model.
    property int cellIndex: 0

    readonly property string identity: (cell.item.kind || "") + ":" + (cell.item.id || "")
    function releaseLoadedCard() {
        if (cardLoader.item
            && typeof cardLoader.item.releaseForReuse === "function")
            cardLoader.item.releaseForReuse()
    }
    function restoreMutableBindings() {
        var loaded = cardLoader.item
        if (!loaded)
            return
        if (cell.item.kind === "album") {
            loaded.isFavorite = Qt.binding(function () {
                return cell.item.isFavorite === true
            })
            loaded.isPinned = Qt.binding(function () {
                return cell.item.isPinned === true
            })
        } else if (cell.item.kind === "artist"
                   || cell.item.kind === "playlist") {
            loaded.isPinned = Qt.binding(function () {
                return cell.item.isPinned === true
            })
        }
    }
    function scheduleMutableRestore() {
        mutableRestore.expectedIdentity = cell.identity
        mutableRestore.restart()
    }
    onItemChanged: {
        // A popup is a window-overlay child; close it while the old loaded
        // card still exists, then restore optimistic scalar bindings after
        // Loader has selected the new kind.
        cell.releaseLoadedCard()
        cell.scheduleMutableRestore()
    }
    GridView.onPooled: cell.releaseLoadedCard()
    GridView.onReused: cell.scheduleMutableRestore()

    width: 200
    height: 246

    // Lifetime-bound counterpart to Qt.callLater: recycling may destroy this
    // cell before a global deferred closure runs.
    Timer {
        id: mutableRestore
        interval: 0
        repeat: false
        property string expectedIdentity: ""
        onTriggered: {
            if (cell.identity === expectedIdentity)
                cell.restoreMutableBindings()
        }
    }

    Component {
        id: albumCardComp
        AlbumCard {
            albumId: cell.item.id
            title: cell.item.title
            artist: cell.item.artist
            artistId: cell.item.artistId
            genre: cell.item.genre
            year: cell.item.year
            qualityTier: cell.item.qualityTier
            qualityDetail: cell.item.qualityDetail || cell.item.qualityLabel || ""
            artSource: cell.view.artMap[cell.item.artKey] || ""
            isFavorite: cell.item.isFavorite
            isPinned: cell.item.isPinned
            qobuzUnavailable: cell.item.qobuzUnavailable === true
                || cell.item.sourceUnavailable === true
            replacementAffordance: cell.item.source === "qobuz"
                && cell.item.qobuzUnavailable === true
            cacheStatus: cell.item.cacheStatus !== undefined
                ? cell.item.cacheStatus : 0
            // The pin payload's display snapshot: the REMOTE url, never
            // `artMap` (a local file:// cache path that means nothing after a
            // cache wipe). Without it every album pinned from the Library grid
            // landed in the Home Pinned rail as a permanent grey placeholder,
            // across restarts.
            artworkUrl: cell.item.imageUrl || ""
            // Local/server rows have no portable catalog URL; retain their
            // resolved artwork snapshot for the per-machine Pinned rail.
            pinArtworkUrl: artworkUrl !== "" ? artworkUrl : artSource
            // Local source badges remain visible on every album. The toolbar
            // may additionally show the catalog mark in the mixed feed.
            source: cell.item.source
            sources: cell.item.sources || []
            catalogAffordances: !cell.view.isLocalFeedItem(cell.item)
            localMode: cell.view.isLocalFeedItem(cell.item)
            localPlaylistAffordance: localMode
            hostFavorite: localMode && badgeSources.some(function (source) {
                return ["local", "plex", "jellyfin", "subsonic", "navidrome",
                    "gonic", "airsonic", "astiga"].indexOf(source) >= 0
            })
            onFavoriteRequested: QbzLocal.albumToggleFavorite(cell.item.id,
                cell.item.title || "", cell.item.artist || "", artworkUrl,
                JSON.stringify(badgeSources))
            onOpenRequested: QbzAlbum.openAlbum(cell.item.id)
            onPlayRequested: QbzPlayer.playAlbum(cell.item.id)
            onEnqueueRequested: function (mode) { QbzPlayer.enqueueAlbum(cell.item.id, mode) }
            showSourceBadge: cell.view.showSourceBadges
        }
    }
    // Group separator. These are pseudo-rows injected into the SAME flat model
    // by `visibleItems`, so the list stays virtualized and the windowed
    // artwork report is unaffected. Only the tracks LIST produces them today;
    // the arm stays so a grouped grid cannot render a card for a header row.
    Component {
        id: groupHeaderComp
        Item {
            Text {
                anchors.left: parent.left
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 6
                text: cell.item.title
                color: theme.textSecondary
                font.pixelSize: 13
                font.weight: theme.weightSemibold
                elide: Text.ElideRight
                width: parent.width
            }
        }
    }
    Component {
        id: trackCardComp
        TrackCard {
            item: cell.item
            artSource: cell.view.artMap[cell.item.artKey] || ""
            showSourceBadge: cell.view.showSourceBadges
            confirmReleaseRemoval: function (item) {
                cell.view.askRemoveReleaseFavorites(item)
            }
        }
    }
    Component {
        id: artistCardComp
        ArtistCard {
            item: cell.item
            artSource: cell.view.artMap[cell.item.artKey] || ""
            isPinned: cell.item.isPinned === true
            // Snapshot url for the pin payload (see the album card above).
            artworkUrl: cell.item.imageUrl || ""
            // The Slint reference SPLITS here, and this one component serves
            // both of its call sites: the ARTISTS tab grids pass follow-mode
            // "none" (FavoritesView.slint:1831/1865 — every artist in that
            // grid is followed by definition, so the chip would be a permanent
            // tick), while the mixed ALL feed keeps the default and seeds
            // `following` from `is-favorite` (:1143-1147), which is exactly
            // the second spelling ArtistCard reads.
            followMode: cell.view.activeTab === "artists" ? "none" : "toggle"
        }
    }
    Component {
        id: playlistCardComp
        PlaylistCard {
            // artworkUrl is not passed on purpose: the card defaults it to
            // `item.imageUrl`, which IS this row's remote cover.
            item: cell.item
            artSource: cell.view.artMap[cell.item.artKey] || ""
            isPinned: cell.item.isPinned === true
        }
    }
    Component {
        id: labelCardComp
        LabelCard {
            item: cell.item
            artSource: cell.view.artMap[cell.item.artKey] || ""
        }
    }
    Loader {
        id: cardLoader
        anchors.fill: parent
        sourceComponent: cell.item.kind === "group-header" ? groupHeaderComp
            : cell.item.kind === "album" ? albumCardComp
            : cell.item.kind === "track" ? trackCardComp
            : cell.item.kind === "artist" ? artistCardComp
            : cell.item.kind === "playlist" ? playlistCardComp
            : labelCardComp
        onLoaded: cell.restoreMutableBindings()
    }
    // THE fix for "many grey squares that fill in unevenly": each pending
    // cover shimmers on its own and stops the moment ITS file:// path lands in
    // artMap, so the grid resolves progressively instead of looking like a
    // wall of dead tiles. Artists and labels are excluded — their cards
    // already draw a designed round gradient+glyph portrait placeholder
    // (ArtistGridCard/LabelCard). A bare Rectangle: it does not take pointer
    // events, so the card's hover/click areas keep working underneath.
    QbzSkeleton {
        artworkItem: cell.item.kind !== "group-header" && cardLoader.item
            && cardLoader.item.artworkImmediate ? cardLoader.item : null
        variant: "art"
        width: 200
        height: 200
        visible: (cell.item.kind === "album"
                  || cell.item.kind === "track"
                  || cell.item.kind === "playlist")
            && cell.item.imageUrl !== ""
            && (cardLoader.item && cardLoader.item.artworkImmediate
                ? opacity > 0.004 : (cell.view.artMap[cell.item.artKey] || "") === "")
            // TrackCard's unavailable scrim is semantic content. A pending
            // artwork skeleton must never paint over it and turn the honest
            // state back into an ambiguous grey tile.
            && !(cardLoader.item && cardLoader.item.pulledDead === true)
        phase: cell.view.skelPhase
        cellIndex: cell.cellIndex
    }
}
