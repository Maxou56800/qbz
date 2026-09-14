// THE shared album card — QML replica of discover/AlbumCard.slint, used
// by BOTH the Home rails and the Library All grid (the Slint mounts the
// same component in both places).
//
// 200x246: 200px artwork (Radius.sm) + placeholder, hover scrim with
// genre/year meta, Quick View + pin group (top-right), favorite / play / more
// overlay buttons, award ribbon, source badge (opt-in), then the title/artist
// lines with the icon-only quality badge.
//
// Live wiring: play (art click + overlay play), favorite heart (optimistic
// + signal), pin badge (pinned store), ⋯ context menu — and the SAME menu
// on a right press anywhere on the artwork or the title.
//
// --- Menu inventory vs discover/AlbumCard.slint ------------------------
//   Open album · Play · Play next · Play later · Add to queue ·
//   Add to/Remove from Library (show-favorite) ·
//   Add to playlist · Add to mixtape · Make available offline (catalog, not
//     pulled — QoL round + 2026-08-31 additions over the .slint inventory) ·
//   Block this album (source != local && source != plex)
// (the first entry's NOUN is overridable per host — see `openLabel`; the last
//  two are gated on `catalogAffordances`)
// …plus whatever the host appends through `extraMenuEntries` (My QBZ's
// "Remove from collection" — see that property).
// All but the last are live. "Block this album" is gated OFF by
// `hasBlacklistSeam` below: the Qt bridge exposes the blacklist COUNTERS
// (settings_qt/devtools.rs) but no block-album invokable, and the row used
// to render and do nothing at all.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root

    // --- card data (the ONE contract both hosts fill) --------------------
    property string albumId: ""
    property string title: ""
    property string artist: ""
    property string artistId: ""
    /// The release's label id when the host knows it (library feed rows do):
    /// the "View label" menu entry (2026-09-13). "" = no entry.
    property string labelId: ""
    // Hosts with only a display snapshot (Home's persisted Pinned row) can
    // still make the artist line a link and resolve the destination lazily.
    // Normal catalog cards keep the direct artistId path and pay no lookup.
    property bool hostArtistLink: false
    signal artistRequested()
    property string genre: ""
    property string year: ""
    property string qualityTier: ""
    /// Exact stored quality ("24-bit / 96 kHz"), shown by the badge's hover
    /// tooltip. Hosts whose model has no detail (un-hydrated Plex/remote
    /// rows) may pass the bare filetype ("FLAC") — known limitation, and
    /// still truthful; empty shows no tooltip at all.
    property string qualityDetail: ""
    property string ribbon: ""
    property string ribbonKind: ""
    // Artwork image source (file://… or "") — the host's cache lookup.
    property string artSource: ""
    property int artFadeMs: coverImage.immediateGridArtwork ? 0 : 200
    readonly property bool artworkReady: coverImage.ready
    readonly property bool artworkImmediate: coverImage.immediateGridArtwork
    readonly property bool artworkRevealed: coverImage.revealed
    // REMOTE cover url, for the pin payload only (the AlbumCard.slint pin
    // TouchArea passes `album.artwork-url`). The pinned store keeps a
    // denormalized display snapshot taken at pin time, so a host that
    // leaves this empty pins a row the Pinned rail can only draw as a
    // placeholder — `artSource` is NOT a substitute, it is a local
    // file:// path that means nothing on another machine or after a
    // cache wipe.
    property string artworkUrl: ""
    property bool isFavorite: false
    property bool isPinned: false
    // Explicit catalog withdrawal. Absence/false means the producer made no
    // unavailability claim. A complete offline copy keeps the album live.
    property bool qobuzUnavailable: false
    // Library tombstones opt in to release replacement. Kept separate from
    // qobuzUnavailable because a missing local/server source also uses the
    // card's pulled presentation but has no Qobuz release to search for.
    property bool replacementAffordance: false
    property int cacheStatus: 0
    readonly property bool pulled: root.qobuzUnavailable
    readonly property bool pulledDead: root.pulled && root.cacheStatus !== 3
    // The row's SOURCE word — "local" | "offline" | "plex" | "jellyfin" |
    // a Subsonic brand spelling (the Local Library badge set,
    // src/local_rows.rs `badge_source`) or "qobuz" (the Library
    // ALL feed, library_qt.rs `FeedItem.source`). "" = a catalog surface that
    // publishes no source at all.
    //
    // Read by TWO consumers, which is why it is NOT the badge's on/off switch:
    // the badge below, and the "Block this album" menu gate
    // (AlbumCard.slint:434 keeps the entry off local/plex rows). Hosts that
    // want the badge hidden set `showSourceBadge`, never `source: ""`.
    property string source: ""
    // Local Library logical albums can have one physical copy on several
    // sources. Other hosts leave this empty and retain the scalar badge.
    property var sources: []
    readonly property var badgeSources: root.sources && root.sources.length > 0
        ? root.sources : (root.source !== "" ? [root.source] : [])
    /// Source words that mean "this album does not live in the Qobuz catalog".
    /// Same fold as `SourceIcon.isSubsonic`: a row stamped with a Subsonic
    /// BRAND is still a server row, so it must not be offered a catalog-only
    /// action.
    readonly property bool serverAlbum: root.source === "local"
        || root.source === "plex" || root.source === "jellyfin"
        || root.source === "subsonic" || root.source === "navidrome"
        || root.source === "gonic" || root.source === "airsonic"
        || root.source === "astiga"
    // Hosts can also show catalog badges. Local Library provenance is always
    // visible, even when a host's optional catalog-badge toggle is off.
    property bool showSourceBadge: false
    readonly property bool hasLocalSourceBadge: root.badgeSources.some(function (source) {
        return source !== "" && source !== "qobuz"
    })
    // Local play count — rendered as the muted "{} plays" line under the
    // artist, and ONLY there (discover/AlbumCard.slint:508-513). Every
    // surface but Most Played Albums publishes 0, which is why that page's
    // card is 286px tall and every other one is 266px: this line IS the
    // 20px difference.
    property int plays: 0

    // --- Local Library mode (additive; default = the Qobuz behaviour) ----
    // The Local Library mounts THIS card, but its `albumId` is a group key
    // (folder or metadata identity), not a Qobuz catalog id — routing it
    // through QbzAlbum.openAlbum / QbzPlayer.playAlbum would fire a
    // catalog fetch for a folder path. In localMode every action is emitted
    // to the host instead. Nothing else about the card changes, so the two
    // surfaces stay pixel-identical.
    //
    // ROUTING ONLY, since the My QBZ round (2026-08-01). It used to ALSO hide
    // the card affordances — see the independent gates below for why routing,
    // catalog actions, Quick View and pinning are different questions.
    property bool localMode: false

    // --- Catalog-only affordances: heart + "Block this album" --------------
    //
    // Split out of `localMode` because a host can need one answer and not the
    // other, and My QBZ is that host. Its grid routes EVERY action through
    // `QbzMyQbz` (only it knows how to open a Plex album, a track or a
    // playlist), so it needs `localMode: true` — but a Qobuz ALBUM cell's
    // `albumId` IS a catalog album id, and its heart / block are the same
    // entity every other AlbumCard in the app talks about. The blanket flag
    // hid the heart on those cells, which is the owner's report ("al overlay le
    // falta el botón de favorite"): a container that is multi-source PER ITEM
    // cannot be described by one grid-wide boolean.
    //
    // Defaults to `!localMode`, so the two pre-existing hosts (Local Library
    // grid, Local album view) keep byte-identical behaviour and no caller has
    // to be updated. A host that wants the split sets it explicitly.
    //
    // The heart still goes through `toggleFavorite()` -> the shared
    // `QbzLibrary.libraryToggleFavorite` seam and still settles on
    // `libraryFavoriteChanged`, so a heart flipped here and one flipped on the
    // album page agree. Do NOT let a host paint its own heart on top of the
    // card — see the `selectMode` note below for the same mistake's other half.
    property bool catalogAffordances: !root.localMode
    // Full-album ownership comes from the account-wide entitlement index, not
    // from whichever producer happened to instantiate this shared card. The
    // revision argument turns the otherwise read-only invokable into a live
    // QML binding after refresh/account switch.
    readonly property int purchaseEntitlementRev: QbzPurchases.entitlementRev
    readonly property bool purchasedAlbum: root.catalogAffordances
        && root.albumId !== ""
        && QbzPurchases.isAlbumPurchased(root.albumId, root.purchaseEntitlementRev)
    readonly property string purchasedQuality: root.purchasedAlbum
        ? QbzPurchases.albumPurchasedQuality(root.albumId, root.purchaseEntitlementRev)
        : ""
    /// Local-mode mounts whose `albumId` is a Local Library group key opt in
    /// to the album-level "Add to playlist" row (LocalAlbumCollection sets
    /// it). My QBZ's heterogeneous cells stay out: their local ids are
    /// container items the local bulk seam cannot resolve.
    property bool localPlaylistAffordance: false
    // Quick View is broader than the catalog-only heart/block family.
    // Catalog cards keep it by default; local and heterogeneous hosts opt in
    // only for rows whose id is genuinely an album key.
    property bool quickViewAffordance: root.catalogAffordances
    // Pin storage accepts string album keys too. Kept independent from the
    // catalog family so a local album can be pinned without accidentally
    // gaining the Qobuz heart or blacklist actions.
    property bool pinAffordance: root.catalogAffordances
    // Catalog hosts persist the remote snapshot URL. Local hosts override this
    // with their resolved file/server artwork so Pinned does not draw blank.
    property string pinArtworkUrl: root.artworkUrl
    // A Local Library album uses the separate LocalFavoritesService rather
    // than the Qobuz catalog seam. This additive flag exposes the same heart
    // geometry/menu row while handing the write back to that host.
    property bool hostFavorite: false
    readonly property bool favoriteAffordance: root.catalogAffordances || root.hostFavorite
    signal favoriteRequested()

    // --- Multi-select mode — discover/AlbumCard.slint:83, :179-197, :207,
    // :239, :465. NOT an invention of this port and NOT a host concern: the
    // reference card owns the whole presentation of select mode, and it owns
    // it because three things have to move together —
    //   * the hover action buttons are HIDDEN (:239) — in select mode the
    //     gesture is "pick this card", and a live play button there plays an
    //     album the user was trying to tick;
    //   * the pin badge is HIDDEN (:207), because the checkbox takes its
    //     corner;
    //   * the checkbox is drawn in THAT corner (top-right, :179-197) and the
    //     card click toggles instead of opening (:169, :465).
    // A host that paints a tick on top of the card can only do the third.
    // `views/local/LocalAlbumCollection.qml` used to do exactly that — a third
    // tick geometry, top-left, with the card's play button still live
    // underneath it — and it was moved onto these members on 2026-08-01. Both
    // grid hosts (Local Library, My QBZ) are on the card's select mode now;
    // there is no host-painted tick left in the tree, and a new one is a
    // regression, not a shortcut.
    property bool selectMode: false
    /// Park the select indicator bottom-right of the art instead of top-right.
    property bool selectMarkBottom: false
    property bool selected: false
    /// `modifiers` rides straight off the click: a grid host turns Shift into
    /// a range (controls/SelectionModel.qml). Every select-mode target on the
    /// card — artwork, title, artist line — emits it.
    signal selectToggled(int modifiers)

    // Album blacklist ("Block this album") — LIVE since QbzBlacklist landed
    // (`blockAlbum(id, title, artist, coverUrl)`). Kept as a property, not
    // inlined, because a host whose `albumId` is not a Qobuz catalog id must
    // be able to turn it off; `localMode` already covers the two shipped
    // cases. One-way, exactly like the .slint: the card drops on the grid's
    // next reload, there is no live removal.
    property bool hasBlacklistSeam: true

    // --- Host-supplied extras (additive; every default is the old behaviour)
    //
    // 1. `placeholderIcon` — the glyph drawn in the empty artwork well when
    //    `artSource` is "". The Qobuz/Local hosts leave it "" and get the bare
    //    `surface-elevated` square they always had. My QBZ needs it because its
    //    grid is HETEROGENEOUS (album / track / playlist) and the reference
    //    card for that grid draws a type glyph there
    //    (MixtapeDetailView.slint:522-533, 28px, text-muted) — dropping it when
    //    that grid moved onto this card would have been a visual regression.
    property string placeholderIcon: ""
    // 2. `extraMenuEntries` / `extraMenuAction` — a host-owned TAIL on the ONE
    //    menu this card already owns (the ⋯ overlay button AND the right press
    //    open the same `albumMenu`). My QBZ has to offer "Remove from
    //    collection", which is a CONTAINER action no album card can know about,
    //    and the alternative in the tree — `views/local/LocalAlbumCollection`
    //    mounting a SECOND CardMenu for the right press — leaves the ⋯ button
    //    showing a different menu than the right click. Entries use CardMenu's
    //    shape ({ label, icon, action, danger } / { sep: true }); an action that
    //    matches one of them is emitted here instead of being interpreted.
    property var extraMenuEntries: []
    signal extraMenuAction(string action)
    // 3. `openLabel` — the NOUN on the first menu entry. "" (default) keeps the
    //    card's own `tr("Open album")`, which is what every catalog surface
    //    wants. My QBZ's grid is heterogeneous: a PLAYLIST cell routes to the
    //    playlist page (`myqbz_detail_qt::open_item`), so "Open album" on it was
    //    simply the wrong word — the action was never wrong, only its label.
    //    A TRACK cell keeps "Open album" on purpose: `open_item` sends
    //    "album" | "track" to the same album page (1:1 with the reference,
    //    `qbz/src/main.rs:6275-6277`), so the noun is accurate there.
    //    It is a LABEL, not a second action: the entry's `action` stays "open"
    //    and still reaches `openRequested()` / `QbzAlbum.openAlbum`.
    property string openLabel: ""

    signal openRequested()
    signal playRequested()
    signal enqueueRequested(string mode)

    QbzTheme { id: theme }

    /// Build the context popup on first use, then open it.
    function openAlbumMenu(anchor, x, y) {
        if (root.menuModel().length === 0)
            return
        albumMenuLoader.active = true
        albumMenuLoader.item.openAtCursor(anchor, x, y)
    }
    /// A GridView recycling host must close window-overlay children before
    /// this card is rebound to another album.
    function releaseForReuse() {
        if (albumMenuLoader.item)
            albumMenuLoader.item.close()
        albumMenuLoader.active = false
    }

    width: 200
    // 246 normally; +20 for the "{} plays" line (see `plays`) — the same
    // 266 -> 286 step MostPlayedAlbumsView.slint:23 takes for its grid.
    height: 246 + (root.plays > 0 ? 20 : 0)
    color: "transparent"

    readonly property bool overlayOn: artArea.containsMouse
        || quickViewArea.containsMouse || pinArea.containsMouse
        || purchaseArea.containsMouse
        || favBtn.hovered || playBtn.hovered || moreBtn.hovered

    function toggleFavorite() {
        root.isFavorite = !root.isFavorite
        if (root.hostFavorite)
            root.favoriteRequested()
        else
            QbzLibrary.libraryToggleFavorite("album", root.albumId)
    }
    function togglePin() {
        root.isPinned = !root.isPinned
        QbzLibrary.togglePin("album", root.albumId, root.title, root.artist,
            root.pinArtworkUrl)
    }

    // Pin fan-out. The pinned store has no change-notify, so `pinChanged`
    // (key `{kind}:{id}`, emitted by main.rs::toggle_pin after the write) is
    // the ONLY signal that this album's state moved — and it moves from
    // anywhere: another rail, another tab, the album page header. This is the
    // port's equivalent of the Slint `set_album_row_pinned` walk over every
    // live model, and the reason no surface has to republish its document to
    // keep a glyph honest. Assigning breaks the host's `isPinned` binding on
    // purpose (the optimistic flip above already does).
    Connections {
        target: QbzLibrary
        function onPinChanged(key, value) {
            if (root.albumId !== "" && key === "album:" + root.albumId)
                root.isPinned = value
        }
        // Favourite fan-out — the SAME shape, one signal later. `isFavorite`
        // was the odd one out: the optimistic flip above breaks the host's
        // binding (by design), and nothing settled it afterwards, so a write
        // that FAILED stayed visibly wrong until the user navigated away, and
        // a heart flipped on another surface (the album page header, a search
        // result, the queue) never reached this card. `libraryFavoriteChanged`
        // carries the value the write actually produced — the flipped one on
        // success, the UNCHANGED one on failure — so this is both the
        // cross-surface walk and the rollback.
        function onLibraryFavoriteChanged(key, value) {
            if (!root.hostFavorite && root.albumId !== ""
                    && key === "album:" + root.albumId)
                root.isFavorite = value
        }
    }
    Connections {
        target: root.hostFavorite ? QbzLocal : null
        function onLocalAlbumFavoriteChanged(id, value) {
            if (root.hostFavorite && root.albumId !== "" && id === root.albumId)
                root.isFavorite = value
        }
    }

    // AlbumCard.slint's album-menu, in its order. `catalogAffordances: false`
    // drops the catalog-only rows (heart + block); `localMode` routes the five
    // navigation/playback rows to the host's signals instead. The two are
    // independent — see the property notes.
    function menuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var m = []
        if (!root.pulledDead) {
            m.push({ "label": root.openLabel !== "" ? root.openLabel : t("Open album", r),
                     "icon": "library-big", "action": "open" })
            m.push({ "label": t("Play", r), "icon": "play-fill", "action": "play" })
            m.push({ "label": t("Play next", r), "icon": "list-start", "action": "next" })
            // #442 "Play later" — end of the manual block.
            m.push({ "label": t("Play later", r), "icon": "list-plus", "action": "later" })
            m.push({ "label": t("Add to queue", r), "icon": "list-end", "action": "queue" })
            // Local albums get the row too (owner, 2026-08-31: playlists can
            // be 100% local or mixed) — via the existing local bulk seam.
            if (root.localMode && root.localPlaylistAffordance)
                m.push({ "label": t("Add to playlist", r), "icon": "list-music", "action": "add-playlist" })
        }
        if (root.favoriteAffordance) {
            // Keep removal reachable even after Qobuz withdraws the old id.
            m.push({ "label": root.isFavorite ? t("Remove from Library", r) : t("Add to Library", r),
                     "icon": root.isFavorite ? "heart-filled" : "heart", "action": "favorite" })
        }
        if (root.replacementAffordance && root.pulled) {
            m.push({ "label": t("Look for better replacement", r),
                     "icon": "search", "action": "find-release" })
        }
        if (root.catalogAffordances && !root.pulled) {
            // QoL round: the album page's container + offline actions, on the
            // card. `catalogAffordances` is the catalog-id guarantee (the
            // add/cache invokables both run get_album), and a PULLED album is
            // refused both — the container stores an id that resolves nowhere
            // and the download arm has no stream url left (the same two
            // refusals ArtistView's PopularTrackRow menu documents).
            // Album-level "Add to playlist" (owner, 2026-08-31): the card
            // only knows the album id — the Rust side fetches the track list
            // and seeds the picker (playlist_picker_qt::open_for_album).
            // Catalog-gated like mixtape: the picker carries catalog ids.
            m.push({ "label": t("Add to playlist", r), "icon": "list-music", "action": "add-playlist" })
            m.push({ "label": t("Add to mixtape", r), "icon": "cassette-tape", "action": "mixtape" })
            m.push({ "label": root.cacheStatus === 3 ? t("Refresh offline copy", r) : t("Make available offline", r),
                     "icon": root.cacheStatus === 3 ? "refresh-cw" : "cloud-download", "action": "cache-album" })
        }
        if (root.catalogAffordances) {
            // .slint gates this on a non-local/plex source as well — and the
            // rule is "not a Qobuz catalog album", so every server source is
            // in the same class as Plex. Blocking a Jellyfin album would write
            // a blacklist entry against an id the Qobuz catalog never had.
            if (root.hasBlacklistSeam && !root.serverAlbum)
                m.push({ "label": t("Block this album", r), "icon": "blind-eye", "action": "block" })
        }
        // Host tail (see `extraMenuEntries`). `concat` so the host's array is
        // never mutated — it is usually a binding's return value.
        if (root.labelId !== "")
            m.push({ "label": t("View label", r), "icon": "tags", "action": "view-label" })
        var extra = root.extraMenuEntries || []
        return extra.length > 0 ? m.concat(extra) : m
    }

    function menuAction(a) {
        // A host entry is HANDED BACK, never interpreted: the card has no idea
        // what "remove from this collection" means.
        var extra = root.extraMenuEntries || []
        for (var i = 0; i < extra.length; i++) {
            if (extra[i].sep !== true && extra[i].action === a) {
                root.extraMenuAction(a)
                return
            }
        }
        // CATALOG-only rows first, because they are orthogonal to routing: they
        // are in the menu at all only when `catalogAffordances` is on, which
        // means `albumId` IS a catalog album id — so they target the catalog
        // whatever `localMode` says about open/play. Handling them after the
        // `localMode` early-return (the old shape) made them unreachable for
        // My QBZ, i.e. a menu row that rendered and did nothing.
        // `artworkUrl` (the REMOTE url), never `artSource` — the store keeps a
        // denormalized snapshot and a file:// cache path is dead on any other
        // machine, the same reason the pin payload uses it.
        if (a === "favorite") { root.toggleFavorite(); return }
        if (a === "view-label") { QbzHome.openLabel(root.labelId); return }
        if (a === "find-release") {
            QbzTrackReplace.openRelease(JSON.stringify({
                "targetKind": "album",
                "albumId": root.albumId || "",
                "albumTitle": root.title || "",
                "artist": root.artist || "",
                "albumArtist": root.artist || ""
            }))
            return
        }
        if (a === "add-playlist") {
            if (root.localMode)
                // The bulk "album" scope resolves a local group key to its
                // rows and opens the picker in LOCAL MODE (local_bulk.rs).
                QbzLocal.bulkAction("album",
                    JSON.stringify([String(root.albumId)]), "add-to-playlist")
            else
                QbzPlaylistPicker.openForAlbum(root.albumId)
            return
        }
        if (a === "mixtape") { QbzAlbum.addToMixtape(root.albumId); return }
        if (a === "cache-album") { QbzAlbum.albumCacheOffline(root.albumId); return }
        if (a === "block") {
            QbzBlacklist.blockAlbum(root.albumId, root.title, root.artist,
                root.artworkUrl)
            return
        }
        if (root.pulledDead)
            return
        if (root.localMode) {
            if (a === "open") root.openRequested()
            else if (a === "play") root.playRequested()
            else if (a === "next") root.enqueueRequested("next")
            else if (a === "later") root.enqueueRequested("later")
            else if (a === "queue") root.enqueueRequested("queue")
            return
        }
        if (a === "open") QbzAlbum.openAlbum(root.albumId)
        else if (a === "play") QbzPlayer.playAlbum(root.albumId)
        else if (a === "next") QbzPlayer.enqueueAlbum(root.albumId, "next")
        else if (a === "later") QbzPlayer.enqueueAlbum(root.albumId, "later")
        else if (a === "queue") QbzPlayer.enqueueAlbum(root.albumId, "queue")
    }

    Column {
        spacing: 0

        // --- Artwork + hover overlay -----------------------------------
        Rectangle {
            width: 200
            height: 200
            radius: theme.radiusSm
            color: theme.surfaceElevated
            // No clip: every child is geometrically contained and the overlay
            // Texts above are now bounded. One batch root per grid card.

            RoundedImage {
                gridArtwork: true
                id: coverImage
                anchors.fill: parent
                source: root.artSource
                radius: theme.radiusSm
                fadeMs: root.artFadeMs
            }

            // Empty-well glyph — opt-in, see `placeholderIcon`. Declared
            // before every hit area so it never takes the pointer (it is an
            // Image with no MouseArea, but z-order is declaration order and
            // the scrim must still paint over it).
            QbzIcon {
                visible: root.placeholderIcon !== "" && root.artSource === ""
                anchors.centerIn: parent
                name: root.placeholderIcon
                width: 28
                height: 28
                tintName: "muted"
            }

            // Hover scrim.
            Rectangle {
                anchors.fill: parent
                radius: theme.radiusSm
                color: "#000000"
                opacity: root.overlayOn && !root.pulledDead ? 0.6 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
            }

            // Hover meta — genre + year, top-left.
            Column {
                x: 12
                y: 12
                spacing: 2
                opacity: root.overlayOn ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                Text {
                    visible: root.genre !== ""
                    text: root.genre
                    // Bounded because the tile's clip is gone. When the joined
                    // top-right group is present, stop 8px before it instead
                    // of letting a long genre run under both buttons.
                    width: (root.quickViewAffordance || root.purchasedAlbum) && !root.selectMode
                        && !root.pulledDead ? 118 : 176
                    elide: Text.ElideRight
                    height: 20
                    color: "#ebffffff"
                    font.pixelSize: 13
                    font.weight: theme.weightBold
                    verticalAlignment: Text.AlignVCenter
                }
                Text {
                    visible: root.year !== ""
                    text: root.year
                    width: (root.quickViewAffordance || root.purchasedAlbum) && !root.selectMode
                        && !root.pulledDead ? 118 : 176
                    elide: Text.ElideRight
                    height: 17
                    color: "#ccffffff"
                    font.pixelSize: 12
                    verticalAlignment: Text.AlignVCenter
                }
            }

            // Card-open + hover detector (declared before the action
            // buttons so those win the pointer).
            MouseArea {
                id: artArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: root.pulledDead ? Qt.ArrowCursor : Qt.PointingHandCursor
                // Right press opens the SAME menu as the ⋯ button.
                acceptedButtons: Qt.LeftButton | Qt.RightButton
                // Phase 8: the card opens the album view (the overlay play
                // button carries the play affordance).
                onClicked: function (mouse) {
                    if (mouse.button === Qt.RightButton) {
                        root.openAlbumMenu(artArea, mouse.x, mouse.y)
                        return
                    }
                    // .slint:169 — in select mode the card click TOGGLES.
                    if (root.selectMode) {
                        root.selectToggled(mouse.modifiers)
                        return
                    }
                    if (root.pulledDead)
                        return
                    if (root.localMode)
                        root.openRequested()
                    else
                        QbzAlbum.openAlbum(root.albumId)
                }
            }

            // Multi-select checkbox — top-right of the art, select-mode only
            // (.slint:179-197, number for number): 24px disc at
            // `width - 24 - 8, 8`, accent when selected else `#00000099`
            // (Slint #RRGGBBAA -> Qt #AARRGGBB = #99000000), 1.5px border,
            // accent when selected else `#ffffffcc` -> `#ccffffff`, 120ms
            // colour animation, and a 14px white check that only shows when
            // selected. It is the INDICATOR: the card click above is what
            // toggles, so it carries no MouseArea of its own — exactly like
            // the .slint, which gives it no TouchArea either.
            Rectangle {
                visible: root.selectMode
                x: parent.width - width - 8
                // Top-right by default (the Library bulk mode); the artist
                // page's album-section "Play selected" parks it bottom-right
                // inside the art (`selectMarkBottom`).
                y: root.selectMarkBottom ? parent.height - height - 8 : 8
                width: 24
                height: 24
                radius: 12
                color: root.selected ? theme.accent : "#99000000"
                border.width: 1.5
                border.color: root.selected ? theme.accent : "#ccffffff"
                Behavior on color { ColorAnimation { duration: 120 } }
                QbzIcon {
                    visible: root.selected
                    name: "check"
                    width: 14
                    height: 14
                    anchors.centerIn: parent
                    // .slint:196 hardcodes #ffffff here, and unlike
                    // SelectCheck's 8px glyph this one sits on a 24px disc —
                    // the contrast argument that made SelectCheck diverge does
                    // not reach this size, so the reference stands.
                    tintName: "white"
                }
            }

            // Quick View + optional pin + purchased ticket — one joined group in the old pin
            // badge's top-right slot. Local album grids opt into BOTH without
            // opting into unrelated catalog heart/block actions; a host that
            // cannot pin may still expose Quick View alone. Always mounted
            // (opacity), so either half's hover joins overlayOn and reveals it.
            Rectangle {
                id: cardTopGroup
                readonly property bool expanded: root.overlayOn
                readonly property bool actionsAvailable: !root.pulledDead
                readonly property int expandedSegments:
                    (root.quickViewAffordance && cardTopGroup.actionsAvailable ? 1 : 0)
                    + (root.pinAffordance && cardTopGroup.actionsAvailable ? 1 : 0)
                    + (root.purchasedAlbum ? 1 : 0)
                // Hidden in select mode too: the checkbox owns this corner.
                // The Quick View controller uses the id router to load
                // local/server keys from their physical source instead of the
                // Qobuz endpoint.
                visible: !root.selectMode
                    && (root.purchasedAlbum
                        || (cardTopGroup.actionsAvailable
                            && (root.quickViewAffordance || root.pinAffordance)))
                x: parent.width - width - 8
                y: 8
                width: root.purchasedAlbum && !cardTopGroup.expanded
                    ? 27 : cardTopGroup.expandedSegments * 27
                height: 26
                radius: 13
                color: (quickViewArea.containsMouse || pinArea.containsMouse
                        || purchaseArea.containsMouse)
                    ? "#cc000000" : "#99000000"
                opacity: root.purchasedAlbum || root.overlayOn ? 1.0 : 0.0
                Behavior on opacity { NumberAnimation { duration: 150 } }

                QbzIcon {
                    visible: cardTopGroup.expanded && cardTopGroup.actionsAvailable
                        && root.quickViewAffordance
                    name: "picture-in-picture-2"
                    width: 14
                    height: 14
                    x: Math.round((27 - width) / 2)
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: quickViewArea.containsMouse ? "accent" : "white"
                }
                Rectangle {
                    visible: cardTopGroup.expanded && cardTopGroup.actionsAvailable
                        && root.pinAffordance
                        && root.quickViewAffordance
                    x: 27
                    y: 5
                    width: 1
                    height: 16
                    color: "#45ffffff"
                }
                QbzIcon {
                    visible: cardTopGroup.expanded && cardTopGroup.actionsAvailable
                        && root.pinAffordance
                    name: root.isPinned ? "pin-filled" : "pin"
                    width: 14
                    height: 14
                    x: (root.quickViewAffordance && cardTopGroup.actionsAvailable ? 27 : 0)
                        + Math.round((27 - width) / 2)
                    anchors.verticalCenter: parent.verticalCenter
                    // On a #99000000/#cc000000 scrim — dark under every theme.
                    tintName: root.isPinned ? "accent" : "white"
                }
                MouseArea {
                    id: quickViewArea
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 27
                    visible: cardTopGroup.expanded && cardTopGroup.actionsAvailable
                        && root.quickViewAffordance
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzAlbum.openQuickView(root.albumId)
                    ToolTip.visible: containsMouse
                    ToolTip.text: QbzSession.tr("Quick view", QbzSession.trRev)
                    ToolTip.delay: 350
                }
                MouseArea {
                    id: pinArea
                    visible: cardTopGroup.expanded && cardTopGroup.actionsAvailable
                        && root.pinAffordance
                    x: root.quickViewAffordance && cardTopGroup.actionsAvailable ? 27 : 0
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 27
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.togglePin()
                    ToolTip.visible: containsMouse
                    ToolTip.text: QbzSession.tr(root.isPinned ? "Unpin" : "Pin",
                                                QbzSession.trRev)
                    ToolTip.delay: 350
                }
                Rectangle {
                    visible: cardTopGroup.expanded && root.purchasedAlbum
                        && cardTopGroup.actionsAvailable
                        && (root.quickViewAffordance || root.pinAffordance)
                    x: (root.quickViewAffordance && cardTopGroup.actionsAvailable ? 27 : 0)
                        + (root.pinAffordance && cardTopGroup.actionsAvailable ? 27 : 0)
                    y: 5
                    width: 1
                    height: 16
                    color: "#45ffffff"
                }
                QbzIcon {
                    visible: root.purchasedAlbum
                    name: "ticket-check"
                    width: 14
                    height: 14
                    x: cardTopGroup.expanded
                        ? (root.quickViewAffordance && cardTopGroup.actionsAvailable ? 27 : 0)
                          + (root.pinAffordance && cardTopGroup.actionsAvailable ? 27 : 0)
                          + Math.round((27 - width) / 2)
                        : Math.round((27 - width) / 2)
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "warning"
                }
                MouseArea {
                    id: purchaseArea
                    visible: root.purchasedAlbum
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 27
                    hoverEnabled: true
                    cursorShape: Qt.ArrowCursor
                    acceptedButtons: Qt.LeftButton | Qt.RightButton
                    onClicked: function (mouse) { mouse.accepted = true }
                    ToolTip.visible: containsMouse
                    ToolTip.text: QbzSession.tr("Purchased", QbzSession.trRev)
                        + (root.purchasedQuality !== ""
                           ? " · " + root.purchasedQuality : "")
                    ToolTip.delay: 350
                }
            }

            // Hover action buttons — favorite / play / more (y=120, h=44,
            // centered, spacing 12).
            CardOverlayRow {
                // .slint:239 `visible: !root.select-mode` — in select mode the
                // click toggles selection, so a live play/⋯ row here would act
                // on a card the user is only trying to tick.
                visible: !root.selectMode
                y: 120
                width: parent.width
                shown: root.overlayOn && !root.pulledDead

                CardOverlayButton {
                    id: favBtn
                    // ABSENT, not present-and-dead, when `albumId` is not a
                    // catalog album id — the same gate as the menu's heart row.
                    visible: root.favoriteAffordance
                    name: root.isFavorite ? "heart-filled" : "heart"
                    active: root.isFavorite
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: root.toggleFavorite()
                }
                CardOverlayButton {
                    id: playBtn
                    name: "play-fill"
                    primary: true
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: root.localMode ? root.playRequested()
                                              : QbzPlayer.playAlbum(root.albumId)
                }
                CardOverlayButton {
                    id: moreBtn
                    name: "ellipsis"
                    anchors.verticalCenter: parent.verticalCenter
                    // CardOverlayButton.clicked() carries no mouse payload,
                    // so fall back to the disc's centre — the menu still
                    // opens under the ⋯ (worst case 18px off the pointer).
                    // Stays correct if the signal ever forwards the event.
                    onClicked: function (mouse) {
                        root.openAlbumMenu(moreBtn,
                            mouse ? mouse.x : moreBtn.width / 2,
                            mouse ? mouse.y : moreBtn.height / 2)
                    }
                }
            }

            // Context menu (AlbumCard.slint's album-menu) — the shared
            // CardMenu surface, not a second copy of its delegate.
            // LAZY. A CardMenu is a QtQuick.Controls Popup with a Repeater
            // over its entries, and this card is the unit the catalog surfaces
            // are built from: Discover Home mounts ~423 of them, and the Local
            // Albums grid recycles a screenful of them on every scroll step.
            // Constructing a popup per card — for a menu that only ever opens
            // on a click — is paid on every one of those.
            Loader {
                id: albumMenuLoader
                active: false
                sourceComponent: CardMenu {
                    menuWidth: 196
                    entries: root.menuModel()
                    onPicked: function (a) { root.menuAction(a) }
                }
            }

            // Award ribbon — content-width, capped at the card width.
            Rectangle {
                visible: root.ribbon !== ""
                x: 0
                y: parent.height - height - 8
                height: 20
                width: Math.min(ribbonRow.width, 200)
                color: root.ribbonKind === "press" ? "#d49511" : "#e0000000"
                topRightRadius: 3
                bottomRightRadius: 3
                clip: true
                Rectangle {
                    width: root.ribbonKind === "press" ? 0 : 3
                    height: parent.height
                    color: root.ribbonKind === "qobuzissime" ? "#8b5cf6" : "#eab308"
                }
                Row {
                    id: ribbonRow
                    height: parent.height
                    leftPadding: 10
                    rightPadding: 10
                    width: ribbonText.implicitWidth + 20
                    Text {
                        id: ribbonText
                        height: parent.height
                        text: root.ribbon
                        color: root.ribbonKind === "press" ? "#1f1407" : "#ffffff"
                        font.pixelSize: 9
                        font.weight: theme.weightSemibold
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }
                }
            }

            // Always-visible source badge — bottom-right of the art, ON TOP of
            // the scrim and the hover buttons (declaration order = z-order).
            // 24x24 rounded SQUARE (ADR-008: not a pill), 6px inset.
            // discover/AlbumCard.slint:326-354, number for number.
            //
            // The glyph goes through controls/SourceIcon.qml, never QbzIcon:
            // the Qobuz and Plex marks are MULTI-COLOUR and a tint flattens
            // them to a silhouette. This card used to draw an accent-tinted
            // `hard-drive` for Plex — a blue hard drive where the design calls
            // for the Plex mark.
            Rectangle {
                visible: (root.showSourceBadge || root.hasLocalSourceBadge)
                    && root.badgeSources.length > 0
                x: parent.width - width - 6
                y: parent.height - height - 6
                width: sourceBadgeRow.implicitWidth
                height: 24
                color: "transparent"
                Row {
                    id: sourceBadgeRow
                    height: 24
                    spacing: 3
                    Repeater {
                        model: root.badgeSources
                        delegate: Rectangle {
                            id: sourceBadge
                            required property string modelData
                            width: 24
                            height: 24
                            radius: 4
                            color: modelData === "qobuz_purchase" ? "#d91e1400"
                                 : (modelData === "offline" ? "transparent" : "#b3000000")
                            border.width: modelData === "qobuz_purchase" ? 1 : 0
                            border.color: "#80eab308"
                            SourceIcon {
                                kind: sourceBadge.modelData
                                glyphSize: 14
                                plexSize: 16
                                qobuzSize: sourceBadge.modelData === "offline" ? 22 : 18
                                localTint: "white"
                                x: Math.round((parent.width - width) / 2)
                                y: Math.round((parent.height - height) / 2)
                            }
                        }
                    }
                }
            }

            // A catalog-withdrawn album is explicit content, not a failed
            // image load. Keep it above every hover affordance and label it.
            Rectangle {
                visible: root.pulledDead
                anchors.fill: parent
                radius: theme.radiusSm
                color: theme.alphaTier(60)
                Text {
                    anchors.centerIn: parent
                    width: parent.width - 20
                    text: QbzSession.tr("Unavailable", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontLegal
                    font.weight: theme.weightSemibold
                    horizontalAlignment: Text.AlignHCenter
                    elide: Text.ElideRight
                }
            }
        }
        Item { width: 1; height: 6 }

        // --- Title / artist + quality badge ------------------------------
        Row {
            width: 200
            height: 40 + (root.plays > 0 ? 20 : 0)
            spacing: theme.spacingSm
            opacity: root.pulledDead ? 0.5 : 1.0
            Column {
                width: parent.width - (qBadge.visible ? qBadge.width + theme.spacingSm : 0)
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                Text {
                    id: titleText
                    width: parent.width
                    height: 20
                    text: root.title
                    color: titleArea.containsMouse && !root.pulledDead
                        ? theme.accent : theme.textPrimary
                    font.pixelSize: theme.fontBody - 2
                    font.weight: theme.weightMedium
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                    // The title is ALWAYS the full one (version included) and
                    // elides at the card's width; when it is cut, hovering shows
                    // the whole text in a small bubble above the line. `truncated`
                    // is Qt's own verdict on the elision, so a title that fits
                    // never raises a tooltip — no character counting. Qt's own
                    // ToolTip, like QualityBadge.qml: a recycled delegate deep
                    // inside a view cannot name the shell's tooltip overlay.
                    ToolTip {
                        id: titleTip
                        visible: titleArea.containsMouse && titleText.truncated
                            && root.title !== ""
                        text: root.title
                        delay: 350
                        timeout: -1
                        padding: 0
                        x: 0
                        y: -height - 4
                        contentWidth: titleTipText.implicitWidth
                        contentHeight: titleTipText.implicitHeight
                        contentItem: Text {
                            id: titleTipText
                            text: titleTip.text
                            color: theme.textPrimary
                            font.pixelSize: 11
                            font.weight: theme.weightMedium
                            wrapMode: Text.Wrap
                            width: Math.min(implicitWidth, 300)
                            maximumLineCount: 3
                            elide: Text.ElideRight
                            verticalAlignment: Text.AlignVCenter
                            leftPadding: 9
                            rightPadding: 9
                            topPadding: 5
                            bottomPadding: 5
                        }
                        background: Rectangle {
                            color: theme.surfaceElevated
                            radius: theme.radiusSm
                            border.width: 1
                            border.color: theme.borderSubtle
                        }
                    }
                    MouseArea {
                        id: titleArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: root.pulledDead ? Qt.ArrowCursor : Qt.PointingHandCursor
                        acceptedButtons: Qt.LeftButton | Qt.RightButton
                        onClicked: function (mouse) {
                            if (mouse.button === Qt.RightButton) {
                                root.openAlbumMenu(titleArea, mouse.x, mouse.y)
                                return
                            }
                            // .slint:465 — the title carries the SAME target
                            // as the artwork, select mode included.
                            if (root.selectMode) {
                                root.selectToggled(mouse.modifiers)
                                return
                            }
                            if (root.pulledDead)
                                return
                            if (root.localMode)
                                root.openRequested()
                            else
                                QbzAlbum.openAlbum(root.albumId)
                        }
                    }
                }
                Text {
                    width: parent.width
                    height: 18
                    text: root.artist
                    color: !root.selectMode
                        && (root.artistId !== "" || root.hostArtistLink)
                        && artistArea.containsMouse
                        ? theme.textPrimary : theme.textMuted
                    font.pixelSize: theme.fontLink - 1
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                    MouseArea {
                        id: artistArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: root.selectMode || root.artistId !== "" || root.hostArtistLink
                            ? Qt.PointingHandCursor : Qt.ArrowCursor
                        onClicked: function (mouse) {
                            // In select mode the artist line is the card too:
                            // opening the artist mid-selection would drop it.
                            if (root.selectMode) {
                                root.selectToggled(mouse.modifiers)
                                return
                            }
                            if (root.artistId !== "")
                                QbzArtist.openArtist(root.artistId)
                            else if (root.hostArtistLink)
                                root.artistRequested()
                        }
                    }
                }
                // Play count — Most Played Albums only (see `plays`).
                Text {
                    visible: root.plays > 0
                    width: parent.width
                    height: visible ? 18 : 0
                    text: QbzSession.tr("{} plays", QbzSession.trRev).replace("{}", root.plays)
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
            }
            // Icon-only quality badge (QualityBadge.slint). `label` feeds the
            // badge's own hover tooltip with the exact stored quality; the
            // badge shows nothing when the host has no detail to give.
            QualityMini {
                id: qBadge
                tier: root.qualityTier
                label: root.qualityDetail
                anchors.verticalCenter: parent.verticalCenter
            }
        }
    }
}
