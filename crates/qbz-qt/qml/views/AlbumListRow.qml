// AlbumListRow — primitives/AlbumListRow.slint:129+, the LIST arm of a
// CATALOG album collection (Discover Browse, Label Releases). 64px
// (`list-row-h`, DiscoverBrowseView.slint:29), radius 8, odd-row zebra,
// hover surface-hover — coherent with TrackRow.
//
// The local twin is views/local/LocalAlbumRow.qml (56px, local actions, a
// host-owned skeleton). This one is Qobuz-wired: the body opens the catalog
// album, and the ⋯ menu carries the five entries the Slint always shows
// (Open album / Play / Play next / Play later / Add to queue). Right-clicking
// the row opens the same menu at the pointer, like every other ⋯ site.
//
// "Block this album" is the sixth entry, live since QbzBlacklist landed
// (primitives/AlbumListRow.slint:434-441). It is pushed conditionally rather
// than sitting in the literal because the .slint gates it on a non-local /
// non-plex source — see the `entries` block.
//
// item contract: home_qt::HomeCard — { id, title, artist, artistId, year,
// qualityTier, qualityDetail, artUrl, artPath }. `artUrl` is the REMOTE cover
// url and `artPath` the local file:// cache path; only the former may be
// persisted (src/home_qt.rs:117-121).

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../kiosk"
import "../theme"

Rectangle {
    id: root

    /// Kiosk host opt-in (contract §2.6 / §5.2). DEFAULT FALSE IS DESKTOP and
    /// every branch is `kioskHost ? … : <the old literal>`, so an unset host
    /// is the row that shipped. Threaded from AlbumCollection, which is
    /// itself only flagged by ContentRouter's kiosk `setSource` map.
    ///
    /// Three things change, and only these:
    ///   * the 44px thumb is drawn by kiosk/KioskArtwork instead of
    ///     theme/RoundedImage — the desktop path decodes the FULL original
    ///     before it asks Rust for a derivative, which on a 7" panel is a
    ///     600px decode for a 44px cell;
    ///   * the ⋯ hit box grows 32 -> 44 (secondary-control floor) and a
    ///     press-and-hold opens the same menu the desktop right-click does,
    ///     because there is no right button on a panel;
    ///   * the menu carries the album actions AlbumCard would have offered —
    ///     see `menuEntries()`. Kiosk renders this row INSTEAD of that card,
    ///     so dropping them would be losing functionality silently.
    property bool kioskHost: false
    property bool pinned: item.isPinned === true
    Connections { target: QbzLibrary; function onPinChanged(key, value) { if (key === "album:" + (root.item.id || "")) root.pinned = value } }

    property var item: ({})
    /// Row index — drives the even/odd zebra (coherent with TrackRow).
    /// NOT named `index`: a Repeater injects one into the delegate context
    /// and a same-named property here would shadow it.
    property int rowIndex: 0
    /// Cover override for hosts whose rows carry no `artPath` of their own.
    /// The Library feed is id-keyed (`artKey` -> a decoded file:// path in
    /// LibraryView's artMap), so its rows never gain the key AlbumCollection's
    /// producers bake in; everything else keeps working unchanged because ""
    /// falls straight back to `item.artPath`.
    property string artSource: ""
    /// Multi-select arm (FavoritesView.slint:514 — Library > Albums in LIST
    /// mode only). Default off, so the two catalog call sites are untouched.
    property bool selectMode: false
    property bool checked: false
    readonly property bool pulled: root.item.qobuzUnavailable === true
    readonly property int cacheStatus: root.item.cacheStatus !== undefined
        ? root.item.cacheStatus : 0
    readonly property bool pulledDead: (root.pulled && root.cacheStatus !== 3)
        || root.item.sourceUnavailable === true
    /// `modifiers` rides straight off the mouse event: Shift is what turns
    /// a click into a range (controls/SelectionModel.qml).
    signal toggleSelect(int modifiers)

    QbzTheme { id: theme }

    // Same columns (and widths) as AlbumListHeader.qml.
    readonly property int colArt: 52
    readonly property int colQuality: root.kioskHost && width < 500 ? 0 : 150
    readonly property int colYear: root.kioskHost && width < 500 ? 0 : 64
    readonly property int colOverflow: root.kioskHost ? 44 : 36
    readonly property int colGap: 12

    readonly property bool rowHovered: rowArea.containsMouse || moreArea.containsMouse
        || artistArea.containsMouse

    width: parent ? parent.width : 0
    height: 64
    radius: theme.radiusSm
    color: rowHovered && !root.pulledDead ? theme.surfaceHover
         : (rowIndex % 2 === 1 ? theme.alphaTier(4) : "transparent")

    MouseArea {
        id: rowArea
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: root.pulledDead && !root.selectMode
            ? Qt.ArrowCursor : Qt.PointingHandCursor
        onClicked: function (mouse) {
            if (mouse.button === Qt.RightButton) {
                // In select mode the row is a selection target, not a menu
                // host (Tauri/Slint parity: a right-click toggles nothing
                // there either).
                if (!root.selectMode)
                    root.openMenu(rowArea, mouse.x, mouse.y)
                return
            }
            if (root.selectMode) root.toggleSelect(mouse.modifiers)
            else if (!root.pulledDead) QbzAlbum.openAlbum(root.item.id || "")
        }
        // Touch has no right button. Desktop-inert: `pressAndHold` never
        // fires for a mouse press that is released normally, and the guard
        // keeps even a stalled desktop press from opening a second menu.
        onPressAndHold: function (mouse) {
            if (root.kioskHost && !root.selectMode)
                root.openMenu(rowArea, mouse.x, mouse.y)
        }
    }

    /// The same "this id IS a Qobuz catalog album" rule the existing "Block
    /// this album" gate uses (and AlbumCard's `catalogAffordances`): every
    /// server source is in the same class as Plex, because the invokables
    /// below all resolve a catalog id.
    readonly property bool catalogRow: {
        var src = root.item.source || ""
        return src === "" || src === "qobuz"
    }

    function menuEntries() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var m = []
        if (!root.pulledDead) {
            m.push({ "label": t("Open album", r), "icon": "library-big", "action": "open" })
            m.push({ "label": t("Play", r), "icon": "play-fill", "action": "play" })
            m.push({ "label": t("Play next", r), "icon": "list-start", "action": "next" })
            m.push({ "label": t("Play later", r), "icon": "list-plus", "action": "later" })
            m.push({ "label": t("Add to queue", r), "icon": "list-end", "action": "queue" })
            // KIOSK ONLY. On desktop these four live on AlbumCard's menu and a
            // user who wants them flips the grid/list toggle back. Kiosk has no
            // grid arm, so without this block forcing list mode would delete
            // four working actions from six routes — the "sin perder acciones"
            // half of the ask. Every msgid, icon and bridge call is lifted
            // verbatim from cards/AlbumCard.qml::menuModel/menuAction: no new
            // strings (the eight catalogues are untouched) and no new seam.
            if (root.kioskHost && root.catalogRow) {
                m.push({label: t("Quick view", r), icon: "eye", action: "quick"})
                m.push({label: t(root.pinned ? "Unpin" : "Pin", r), icon: "pin", action: "pin"})
                var fav = root.item.isFavorite === true
                m.push({ "label": fav ? t("Remove from Library", r) : t("Add to Library", r),
                         "icon": fav ? "heart-filled" : "heart", "action": "favorite" })
                m.push({ "label": t("Add to playlist", r), "icon": "list-music", "action": "add-playlist" })
                m.push({ "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" })
                m.push({ "label": root.cacheStatus === 3
                            ? t("Refresh offline copy", r) : t("Make available offline", r),
                         "icon": root.cacheStatus === 3 ? "refresh-cw" : "cloud-download",
                         "action": "cache-album" })
            }
        }
        if (!root.pulledDead && root.catalogRow)
            m.push({ "label": t("Block this album", r), "icon": "blind-eye", "action": "block" })
        return m
    }
    function menuAction(a) {
        var id = root.item.id || ""
        if (id === "") return
        if (root.pulledDead) return
        if (a === "open") QbzAlbum.openAlbum(id)
        else if (a === "play") QbzPlayer.playAlbum(id)
        // `artUrl`, never `artPath`: the store keeps a denormalized cover url
        // and a file:// cache path is dead on any other machine.
        else if (a === "block") QbzBlacklist.blockAlbum(id, root.item.title || "",
            root.item.artist || "", root.item.artUrl || "")
        // Kiosk tail (see menuEntries). Unreachable on desktop: no entry with
        // one of these actions is ever built there.
        else if (a === "quick") QbzAlbum.openQuickView(id)
        else if (a === "pin") QbzLibrary.togglePin("album", id, root.item.title || "", root.item.artist || "", root.item.artUrl || "")
        else if (a === "favorite") QbzLibrary.libraryToggleFavorite("album", id)
        else if (a === "add-playlist") QbzPlaylistPicker.openForAlbum(id)
        else if (a === "mixtape") QbzAlbum.addToMixtape(id)
        else if (a === "cache-album") QbzAlbum.albumCacheOffline(id)
        else QbzPlayer.enqueueAlbum(id, a)
    }
    Loader {
        id: rowMenuLoader
        active: false
        sourceComponent: CardMenu {
            kioskHost: root.kioskHost
            // The kiosk arm carries four more entries at 1.2x type.
            menuWidth: root.kioskHost ? 252 : 196
            entries: root.menuEntries()
            onPicked: function (a) { root.menuAction(a) }
        }
    }
    function openMenu(anchor, x, y) {
        if (root.menuEntries().length === 0)
            return
        rowMenuLoader.active = true
        rowMenuLoader.item.openAtCursor(anchor, x, y)
    }
    function releaseForReuse() {
        if (rowMenuLoader.item)
            rowMenuLoader.item.close()
        rowMenuLoader.active = false
    }
    ListView.onPooled: root.releaseForReuse()

    Row {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: root.colGap
        opacity: root.pulledDead ? 0.5 : 1.0

        // Selection checkbox — takes the leading slot in select mode
        // (MultiSelectBar's companion; the art cell keeps its own width so
        // the columns below the header never shift).
        Item {
            visible: root.selectMode
            width: visible ? 18 : 0
            height: parent.height
            QbzCheckbox {
                anchors.centerIn: parent
                checked: root.checked
                onToggled: function (mods) { root.toggleSelect(mods) }
            }
        }

        // Art cell (52px column, 44px thumb centred).
        Item {
            width: root.colArt
            height: parent.height
            Rectangle {
                width: 44
                height: 44
                anchors.centerIn: parent
                radius: 4
                color: theme.surfaceElevated
                // No clip: RoundedImage confines its own crop on both arms.
                // One batch root per list row, for a scissor that never
                // rounded anything.
                RoundedImage {
                    visible: !root.kioskHost
                    anchors.fill: parent
                    source: root.kioskHost ? "" : (root.artSource !== "" ? root.artSource : (root.item.artPath || ""))
                    radius: 4
                }
                KioskArtwork {
                    anchors.fill: parent
                    visible: root.kioskHost
                    source: root.kioskHost ? (root.artSource || root.item.artPath || "") : ""
                }
                Rectangle {
                    visible: root.pulledDead
                    anchors.fill: parent
                    radius: 4
                    color: theme.alphaTier(60)
                    QbzIcon {
                        name: "circle-alert"
                        width: 18
                        height: 18
                        anchors.centerIn: parent
                        tintName: "favorite"
                    }
                }
            }
        }

        // ITEM — title over artist (the artist line links to the artist).
        Column {
            // The select checkbox adds a leading 18px cell AND a gap; without
            // subtracting both, the row overflows its width by 30px the moment
            // multi-select is switched on.
            width: parent.width - root.colArt - root.colQuality - root.colYear
                - root.colOverflow - 4 * root.colGap
                - (root.selectMode ? 18 + root.colGap : 0)
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Text {
                width: parent.width
                text: root.item.title || ""
                color: theme.textPrimary
                font.pixelSize: theme.fontLink
                font.weight: theme.weightMedium
                elide: Text.ElideRight
            }
            Text {
                id: artistText
                width: parent.width
                text: (root.item.artist || "") + (root.kioskHost && (root.item.plays || 0) > 0 ? " · " + QbzSession.tr("{} plays", QbzSession.trRev).replace("{}", root.item.plays) : "")
                color: artistArea.containsMouse ? theme.textPrimary : theme.textMuted
                font.pixelSize: 12
                elide: Text.ElideRight
                MouseArea {
                    id: artistArea
                    anchors.fill: parent
                    // Only a real artist id is clickable — otherwise the
                    // pointer promises a page that cannot open.
                    enabled: (root.item.artistId || "") !== ""
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzArtist.openArtist(root.item.artistId)
                }
            }
        }

        // QUALITY — the SAME badge the album page shows (tier label over the
        // exact bit-depth / sample-rate line).
        Item {
            width: root.colQuality
            height: parent.height
            QualityBadgeFull {
                visible: !root.pulledDead
                anchors.verticalCenter: parent.verticalCenter
                tier: root.item.qualityTier || ""
                detail: root.item.qualityDetail || ""
            }
            Text {
                visible: root.pulledDead
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("Unavailable", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                font.weight: theme.weightSemibold
            }
        }

        Text {
            width: root.colYear
            height: parent.height
            text: root.item.year || ""
            color: theme.textMuted
            font.pixelSize: 12
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
        }

        Item {
            width: root.colOverflow
            height: parent.height
            visible: !root.pulledDead
            Rectangle {
                width: root.kioskHost ? 44 : 32
                height: root.kioskHost ? 44 : 32
                radius: 6
                anchors.centerIn: parent
                color: moreArea.containsMouse ? theme.surfaceElevated : "transparent"
                QbzIcon {
                    name: "ellipsis"
                    width: 18
                    height: 18
                    anchors.centerIn: parent
                    tintName: moreArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: moreArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: function (mouse) { root.openMenu(moreArea, mouse.x, mouse.y) }
                }
            }
        }
    }
}
