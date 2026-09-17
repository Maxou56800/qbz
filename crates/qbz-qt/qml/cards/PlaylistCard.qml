// PlaylistCard — THE playlist card (discover/PlaylistCard.slint), shared
// by every surface since phase 21: Home/Editor/ForYou playlist rails, the
// Pinned rail, the Library playlist grid + All feed, the Search all-tab
// carousel + playlists grid.
//
// 200x246: 200px cover (Radius.sm) + hover scrim 0.6, overlay row at
// y=120 (fav tri-state / play / more), pin badge top-right, then the
// left-aligned meta: semibold title + (category accent eyebrow | muted
// subtitle). Body click OPENS the playlist (the Slint body-opens arm —
// every Slint mount sets it).
//
// item contract: { id, title, subtitle, category, playlistOwned,
// playlistFollowing, isFavorite } plus the scalar artSource / artworkUrl
// / isPinned props (the AlbumCard pattern).
//
// Artwork, two arms (1:1 with Tauri, which has TWO playlist cards):
//   - `item.playlistOwnImage` — the playlist carries its OWN Qobuz graphic
//     (`image_rectangle`). Those are landscape and cropping butchers them,
//     so they render CONTAIN (QobuzPlaylistCard: `object-fit: contain`).
//   - otherwise — the mosaic of member-track covers from `item.covers`
//     (FavoritePlaylistCard -> PlaylistCollage). Rows without either get
//     the collage's list-music placeholder.
// Surfaces that publish neither field (Home rails, Search) keep the single
// crop-fitted cover they already passed through `artSource`.
//
// Ownership tri-state (Slint): owned -> heart (library favorite);
// foreign followed -> check; foreign -> user-plus (Qobuz follow).
//
// SETTLE ASYMMETRY, deliberate and not a hole this file can close: the
// OWNED arm settles on `QbzLibrary.libraryFavoriteChanged` (rollback on a
// failed write included). The FOREIGN follow arm has no signal at all —
// `playlist_qt::set_follow_by_id` logs the error and otherwise settles only
// the caches (`follow_settled`: the ownership snapshot, the sidebar tree and
// the Library feed), so a FAILED subscribe leaves the optimistic tick up
// until the card's document is republished. Closing it needs a Rust-side
// settle emit, not a QML change.
//
// Offline the foreign arm is a WRITE that cannot land (`playlist/subscribe`
// is gate-refused), and this card offers it anyway — the detail header now
// disables its twin (`views/PlaylistView.qml`, `online`). Not fixed here
// because the same control also carries the OWNED heart, which is a local
// library.db write and MUST stay live offline; splitting the enablement is a
// change to this card's contract, not a one-liner.
//
// --- Menu inventory vs discover/PlaylistCard.slint ---------------------
//   Play · Play next · Play later · Add to queue ·
//   Add to Library (is-owned) | Follow/Unfollow on Qobuz (!is-owned) ·
//   Add to mixtape · Make available offline (QoL round additions over the
//     .slint inventory) · Copy to your library (!is-owned && !is-copied)
// All live except the last — see `hasCopyByIdSeam`. The ⋯ button and a
// right press on the cover or the title open the same menu.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    readonly property bool artworkReady: root.artSource !== "" && artworkLoader.item
        ? artworkLoader.item.ready : false
    readonly property bool artworkImmediate: root.artSource !== "" && artworkLoader.item
        ? artworkLoader.item.immediateGridArtwork : false

    property var item: ({})
    // Host-resolved artwork path (the AlbumCard artSource pattern).
    property string artSource: ""
    // Remote cover URL for the pin payload. Defaults to whatever the row
    // carries — the playlist's own graphic, else its first member cover —
    // so a pinned playlist keeps art in the Pinned rail; hosts that resolve
    // it themselves may still override.
    // `artUrl` is the Home / Search / Browse row field (home_qt::HomeCard),
    // `imageUrl` the Library one — without both arms a playlist pinned from
    // a Discover rail stored an EMPTY snapshot url and landed in the Pinned
    // rail as a permanent placeholder.
    property string artworkUrl: (root.item && root.item.imageUrl)
        ? root.item.imageUrl
        : ((root.item && root.item.artUrl)
            ? root.item.artUrl
            : (root.collageUrls.length > 0 ? root.collageUrls[0] : ""))

    // Member-track covers for the mosaic arm ([] on surfaces that publish a
    // single pre-resolved cover instead).
    readonly property var collageUrls: (root.item && root.item.covers)
        ? root.item.covers : []
    // The row's image is the playlist's own Qobuz graphic -> contain.
    readonly property bool ownImage: root.item
        ? root.item.playlistOwnImage === true : false
    // Pinned state (AlbumCard pattern: scalar prop, optimistic flip on
    // click — the model re-publish re-creates the delegate).
    property bool isPinned: false

    // "Copy to your library": QbzBridge.playlistCopy() copies the playlist
    // that is currently OPEN — it takes no id, so a card cannot call it. The
    // row stays out of the menu until a by-id copy invokable exists (and
    // `item.playlistCopied` is published for the .slint's is-copied gate).
    readonly property bool hasCopyByIdSeam: false

    color: "transparent"

    QbzTheme { id: theme }

    readonly property bool overlayOn: plArtArea.containsMouse || pinArea.containsMouse
        || plFav.hovered || plPlay.hovered || plMore.hovered

    implicitWidth: 200
    implicitHeight: 246

    // --- The tri-state's two mutable halves, as REAL QML properties ------
    // They used to be `root.item.isFavorite` / `root.item.playlistFollowing`,
    // mutated in place on click. That is a plain JS object: no notifier, so
    // the glyph binding never re-evaluated, and `item: modelData` is a COPY,
    // so the write reached neither the delegate's model row nor the host's
    // array (both measured under qml6 6.11.1 — see rows/TrackRow.qml). The
    // card's overlay therefore never changed on click, on ANY surface.
    // `owned` stays a plain read: nothing in the UI can change who owns a
    // playlist.
    readonly property bool owned: root.item.playlistOwned === true
    property bool favorite: root.item.isFavorite === true
    property bool following: root.item.playlistFollowing === true
    onItemChanged: {
        root.favorite = Qt.binding(function () { return root.item.isFavorite === true })
        root.following = Qt.binding(function () { return root.item.playlistFollowing === true })
    }

    /// The overlay button and the menu row — ONE implementation. Owned
    /// playlists take the library heart (a qbz-LOCAL flag, library.db); a
    /// foreign one takes the Qobuz follow (playlist/subscribe). Different
    /// writes, different settle signals — see the Connections below.
    function toggleFavorite() {
        if (root.owned) {
            root.favorite = !root.favorite
            QbzLibrary.libraryToggleFavorite("playlist", root.item.id)
        } else {
            root.following = !root.following
            QbzBridge.playlistSetFollowById(root.item.id, root.following)
        }
    }

    function openMenu(anchor, x, y) {
        plMenuLoader.active = true
        plMenuLoader.item.openAtCursor(anchor, x, y)
    }
    function releaseForReuse() {
        if (plMenuLoader.item)
            plMenuLoader.item.close()
        plMenuLoader.active = false
    }

    Connections {
        target: QbzLibrary
        // Pin fan-out — the AlbumCard contract, playlist key (see AlbumCard.qml).
        function onPinChanged(key, value) {
            var pid = (root.item && root.item.id !== undefined) ? root.item.id : ""
            if (pid !== "" && key === "playlist:" + pid)
                root.isPinned = value
        }
        // Heart fan-out + rollback. Only the OWNED arm settles from here:
        // `library_qt::toggle_favorite` routes kind "playlist" to
        // `toggle_playlist_favorite` (the library.db flag) and reports the
        // result through the shared settle point, so the key is
        // `playlist:{id}` exactly like every other kind.
        function onLibraryFavoriteChanged(key, value) {
            var pid = (root.item && root.item.id !== undefined) ? root.item.id : ""
            if (pid !== "" && key === "playlist:" + pid)
                root.favorite = value
        }
    }

    Column {
        spacing: 0
        Rectangle {
            width: 200
            height: 200
            radius: theme.radiusSm
            color: theme.surfaceElevated
            // No clip: the child RoundedImage confines itself on both arms, and
            // a rectangular scissor never produced this radius. A clip is an
            // unconditional batch root, so this one cost a draw call per item.
            // Arm 1 — a resolved single cover.
            //
            // The playlist's own Qobuz graphic (`image_rectangle`) is a
            // 2.11:1 banner — 800x380 on every such file in the local cache.
            // Cropping it to this 200px square keeps the middle 380 of 800
            // px, i.e. throws away 53% of the width; that is what the owner
            // reported, and it is what Slint does too
            // (discover/PlaylistCollage.slint's single tile and
            // playlist/PlaylistView.slint:481 are both `image-fit: cover`).
            // `pad` keeps the ratio and fills the two uncovered bands with a
            // gradient sampled from the image's own edges — see
            // theme/RoundedImage.qml.
            //
            // `auto` on the other arm because the flag is not universal:
            // only `library_qt.rs` publishes `playlistOwnImage`, while the
            // Home / For You / Search / Browse rails map their playlist art
            // from `image.rectangle` all the same (`home_qt.rs::map_playlist`)
            // with no flag at all. `auto` measures the ratio instead, so
            // those surfaces pad too, and a genuinely square cover
            // (`image.covers[0]`, the fallback that map picks) still crops.
            Loader {
                id: artworkLoader
                anchors.fill: parent
                sourceComponent: root.artSource !== "" ? singleArtwork : collageArtwork
            }
            Component {
                id: singleArtwork
                RoundedImage {
                    gridArtwork: true
                    source: root.artSource
                    radius: theme.radiusSm
                    fit: root.ownImage ? "pad" : "auto"
                }
            }
            // Arm 2 — the member-cover mosaic. A row that HAS its own
            // graphic passes no urls, so it shows the placeholder while
            // that graphic downloads instead of flashing a mosaic.
            Component {
                id: collageArtwork
                PlaylistCollage {
                    gridArtwork: true
                    urls: root.ownImage ? [] : root.collageUrls
                    radius: theme.radiusSm
                }
            }
            Rectangle {
                anchors.fill: parent
                radius: theme.radiusSm
                color: "#000000"
                opacity: root.overlayOn ? 0.6 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
            }
            MouseArea {
                id: plArtArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                acceptedButtons: Qt.LeftButton | Qt.RightButton
                // body-opens (every Slint mount); right press = the ⋯ menu.
                onClicked: function (mouse) {
                    if (mouse.button === Qt.RightButton)
                        root.openMenu(plArtArea, mouse.x, mouse.y)
                    else
                        QbzBridge.openPlaylist(root.item.id)
                }
            }
            // Hover overlay — fav / play / more (y=120, h=44, centered).
            CardOverlayRow {
                y: 120
                width: parent.width
                shown: root.overlayOn
                CardOverlayButton {
                    id: plFav
                    name: root.owned ? "heart"
                        : root.following ? "check" : "user-plus"
                    active: root.owned && root.favorite
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: root.toggleFavorite()
                }
                CardOverlayButton {
                    id: plPlay
                    name: "play-fill"
                    primary: true
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzPlayer.playPlaylistById(root.item.id)
                }
                CardOverlayButton {
                    id: plMore
                    name: "ellipsis"
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: function (mouse) { root.openMenu(plMore, mouse.x, mouse.y) }
                }
            }
            // Pin badge — top-right (opacity follows overlay-on).
            Rectangle {
                x: parent.width - width - 8
                y: 8
                width: 26
                height: 26
                radius: 13
                color: pinArea.containsMouse ? "#cc000000" : "#99000000"
                opacity: root.overlayOn ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                QbzIcon {
                    name: root.isPinned ? "pin-filled" : "pin"
                    width: 14
                    height: 14
                    anchors.centerIn: parent
                    // On a #99000000/#cc000000 scrim — dark under every theme.
                    tintName: root.isPinned ? "accent" : "white"
                }
                MouseArea {
                    id: pinArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.isPinned = !root.isPinned
                        QbzLibrary.togglePin("playlist", root.item.id, root.item.title,
                            root.item.subtitle || "", root.artworkUrl)
                    }
                }
            }
            Loader {
                id: plMenuLoader
                active: false
                sourceComponent: CardMenu {
                    menuWidth: 196
                    entries: root.menuModel()
                    onPicked: function (a) { root.menuAction(a) }
                }
            }
        }
        Item { width: 1; height: 6 }
        // Meta: left-aligned, semibold title + (category eyebrow | subtitle).
        Column {
            width: 200
            height: 40
            spacing: 2
            leftPadding: theme.spacingXs
            Text {
                width: parent.width
                height: 20
                text: root.item.title || ""
                color: theme.textPrimary
                font.pixelSize: theme.fontBody - 2
                font.weight: theme.weightSemibold
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
            Text {
                visible: !!root.item.category
                width: parent.width
                height: 16
                text: (root.item.category || "").toUpperCase()
                color: theme.accent
                font.pixelSize: 11
                font.weight: theme.weightSemibold
                font.letterSpacing: 0.5
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
            Text {
                visible: !root.item.category
                width: parent.width
                height: 16
                text: root.item.subtitle || ""
                color: theme.textMuted
                font.pixelSize: theme.fontLink - 1
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
            }
        }
    }

    // Right-press catcher over the meta block (the cover has its own in
    // plArtArea). Declared OUTSIDE the Column so it takes no layout space,
    // and RightButton-only so left clicks still fall through to the text.
    MouseArea {
        id: plMetaArea
        x: 0
        y: 206
        width: 200
        height: 40
        acceptedButtons: Qt.RightButton
        onClicked: function (mouse) { root.openMenu(plMetaArea, mouse.x, mouse.y) }
    }

    // PlaylistCard.slint's menu, in its order.
    function menuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var owned = root.owned
        var following = root.following
        var m = [
            { "label": t("Play", r), "icon": "play-fill", "action": "play" },
            { "label": t("Play next", r), "icon": "list-start", "action": "play-next" },
            // #442 "Play later" — end of the manual block.
            { "label": t("Play later", r), "icon": "list-plus", "action": "play-later" },
            { "label": t("Add to queue", r), "icon": "list-end", "action": "queue" },
        ]
        if (owned)
            m.push({ "label": root.favorite ? t("Remove from Library", r) : t("Add to Library", r),
                     "icon": root.favorite ? "heart-filled" : "heart", "action": "favorite" })
        else
            m.push({ "label": following ? t("Unfollow on Qobuz", r) : t("Follow on Qobuz", r),
                     "icon": following ? "check" : "user-plus", "action": "favorite" })
        // QoL round additions over the .slint inventory: the container and
        // offline actions every card mount can honour — this card is only
        // ever mounted on Qobuz playlist rows (its play/enqueue actions all
        // go through the by-id catalog invokables), so neither needs a gate.
        m.push({ "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" })
        m.push({ "label": t("Make available offline", r), "icon": "cloud-download", "action": "cache" })
        if (root.hasCopyByIdSeam && !owned && root.item.playlistCopied !== true)
            m.push({ "label": t("Copy to your library", r), "icon": "copy", "action": "copy" })
        // Delete (2026-09-13): the fast path from search / library / browse
        // cards, behind the shell-level confirmation. Owned playlists only —
        // a followed one has Unfollow above.
        if (owned)
            m.push({ "label": t("Delete playlist", r), "icon": "trash-2", "action": "delete", "danger": true })
        return m
    }

    function menuAction(a) {
        if (a === "play") QbzPlayer.playPlaylistById(root.item.id)
        else if (a === "play-next") QbzPlayer.enqueuePlaylistById(root.item.id, "next")
        else if (a === "play-later") QbzPlayer.enqueuePlaylistById(root.item.id, "later")
        else if (a === "queue") QbzPlayer.enqueuePlaylistById(root.item.id, "queue")
        else if (a === "favorite") root.toggleFavorite()
        else if (a === "mixtape") {
            // The payload is assembled at the call site (the SidebarRowMenu /
            // PmGridCard idiom). `trackCount` is omitted on purpose — the card
            // contract carries no member count and myqbz_add_qt PERSISTS the
            // value, so absent (NULL) is the truthful "unknown".
            QbzMyQbzAdd.open(JSON.stringify([{
                "itemType": "playlist", "source": "qobuz",
                "sourceItemId": String(root.item.id),
                "title": root.item.title || "",
                "subtitle": root.item.subtitle || "",
                "artworkUrl": root.artworkUrl || ""
            }]))
        }
        else if (a === "cache") QbzOffline.cachePlaylist(String(root.item.id))
        else if (a === "delete") QbzPlaylistEdit.askDelete(String(root.item.id), String(root.item.title || ""))
    }
}
