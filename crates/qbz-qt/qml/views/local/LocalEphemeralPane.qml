// Ephemeral-folder pane (LocalLibraryView.slint:114 EphemeralPane) — shown
// in the Folders tab while an ad-hoc folder is open. Album-grouped (a folder
// can hold several albums): each block is a 56px cover + title/artist/meta,
// with a per-album play button ONLY for multi-album sessions, then the
// block's tracks.
//
// Metadata-bound actions (favorite / playlist / album+artist links) are off:
// ephemeral tracks have no DB row. The CUE badge marks a single-file rip.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../rows"
import "../../theme"

Item {
    id: root

    property var view: null

    QbzTheme { id: theme }

    readonly property var eph: view ? view.ephemeral : null
    readonly property var allAlbums: eph && eph.albums ? eph.albums : []

    /// A CLIENT-side filter over the session that is already on screen.
    ///
    /// It exists because a box set is not a browsable list: the owner's Saint
    /// Seiya Eternal CD-Box is 247 tracks, and reaching one of them meant
    /// scrolling past the other 246. Nothing here asks Rust for anything — the
    /// whole session is already in this document, so filtering it is a pass
    /// over an array the pane is holding anyway.
    property string query: ""
    readonly property var albums: {
        var q = root.query.trim().toLowerCase()
        if (q === "")
            return root.allAlbums
        var out = []
        for (var i = 0; i < root.allAlbums.length; i++) {
            var a = root.allAlbums[i]
            var src = a.tracks || []
            // A block whose ALBUM matches keeps ALL of its tracks: searching
            // for a record means wanting the record, not only the rows whose
            // titles happen to repeat its name.
            var albumHit = (a.title || "").toLowerCase().indexOf(q) !== -1
                || (a.artist || "").toLowerCase().indexOf(q) !== -1
            var hits = []
            if (albumHit) {
                hits = src
            } else {
                for (var j = 0; j < src.length; j++) {
                    var t = src[j]
                    if ((t.title || "").toLowerCase().indexOf(q) !== -1
                        || (t.artist || "").toLowerCase().indexOf(q) !== -1)
                        hits.push(t)
                }
            }
            if (hits.length === 0)
                continue
            out.push({ "groupKey": a.groupKey, "title": a.title, "artist": a.artist,
                       "meta": a.meta, "isCue": a.isCue, "artKey": a.artKey,
                       "tracks": hits })
        }
        return out
    }
    readonly property int matchCount: {
        var n = 0
        for (var i = 0; i < root.albums.length; i++)
            n += (root.albums[i].tracks || []).length
        return n
    }

    /// Art for the 224px header cell: the FIRST block's key. For the common
    /// one-album session that is simply "the cover"; for a multi-album folder
    /// it is a representative, and each block keeps drawing its own 56px art
    /// below. Empty when the session has no art at all, which collapses the
    /// cell instead of reserving an empty square.
    readonly property string headerArtKey:
        allAlbums.length > 0 && allAlbums[0].artKey ? allAlbums[0].artKey : ""

    // Same rule as LocalFolderDetail: the host reports this pane's covers as
    // one window when the document changes, so re-opening the pane on an
    // unchanged document (leaving and returning to the Folders tab) needs its
    // own trigger, or the evicted covers never come back.
    Component.onCompleted: if (view && visible) view.reportEphemeralWindow()
    Component.onDestruction: if (view) view.releaseWindow("ephemeral")
    onVisibleChanged: {
        if (!view) return
        if (visible) view.reportEphemeralWindow()
        else view.releaseWindow("ephemeral")
    }
    Connections {
        target: root.view
        function onArtworkRefresh() {
            if (root.visible) root.view.reportEphemeralWindow()
            else root.view.releaseWindow("ephemeral")
        }
    }

    Flickable {
        id: pane
        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: page.height + 100
        boundsBehavior: Flickable.StopAtBounds

        Column {
            id: page
            x: 32
            y: 16
            width: parent.width - 64
            spacing: 14

            // ---- Header: cover + name / count · duration, and the actions ----
            //
            // Shaped after LocalAlbumHeader, because for the common case this
            // pane IS an album page: one folder, one album, one cover. The art
            // is 224px there (`LocalAlbumHeader.qml:45`) and 224 here, so an
            // opened folder and an indexed album do not present the same record
            // at two different sizes.
            //
            // The cover cell collapses to zero width when the session has no
            // art at all, rather than reserving a 224px hole — a CUE rip with
            // no cover.jpg would otherwise open on a mostly empty header.
            Row {
                width: parent.width
                spacing: root.headerArtKey === "" ? 0 : 20

                readonly property int actionsW: 3 * 40 + 2 * 12

                Rectangle {
                    id: heroArt
                    visible: root.headerArtKey !== ""
                    width: visible ? 224 : 0
                    height: visible ? 224 : 0
                    radius: 8
                    color: theme.surfaceHover
                    clip: true
                    QbzIcon {
                        name: "disc-3"
                        width: 64
                        height: 64
                        anchors.centerIn: parent
                        tintName: "muted"
                    }
                    RoundedImage {
                        id: heroImg
                        anchors.fill: parent
                        source: root.view ? root.view.artPathOf(root.headerArtKey) : ""
                        radius: 8
                    }
                    QbzSkeleton {
                        variant: "art"
                        anchors.fill: parent
                        blockRadius: 8
                        pending: root.view ? root.view.artWanted(root.headerArtKey) : false
                        coverReady: heroImg.ready
                        phase: root.view ? root.view.skelPhase : false
                        settleMs: root.view ? root.view.artSettleMs : 0
                        settleHold: root.view ? root.view.artPulse : false
                    }
                }

                Column {
                    width: parent.width - heroArt.width
                        - (root.headerArtKey === "" ? 0 : 20)
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 8

                    Text {
                        width: parent.width
                        text: root.eph ? (root.eph.title || root.eph.name || "") : ""
                        color: theme.textPrimary
                        font.pixelSize: theme.fontSection
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }
                    // The artist, in LocalAlbumHeader's own shape (fontHeading,
                    // bold, text-secondary) — for a one-album session this pane
                    // IS an album page, and the two must not present the same
                    // record differently.
                    //
                    // NOT clickable, unlike the album header's: that one routes
                    // into the Artists tab, and the act on a CD the library has
                    // never seen is a route to an empty page. A disc names its
                    // artist; it does not promise the library knows them.
                    //
                    // Hidden rather than blank when there is none: an
                    // unidentified disc has no artist to state, and an empty
                    // bold line reads as a loading failure.
                    Text {
                        width: parent.width
                        visible: text !== ""
                        text: root.allAlbums.length > 0 ? (root.allAlbums[0].artist || "") : ""
                        color: theme.textSecondary
                        font.pixelSize: theme.fontHeading
                        font.weight: theme.weightBold
                        elide: Text.ElideRight
                    }
                    // "10 tracks · 42 min · Folder name". Folder context is
                    // appended only when Rust inferred one consistent tagged
                    // album/collection title; a mixed folder already uses its
                    // own name as the title and does not repeat it here.
                    Text {
                        width: parent.width
                        text: {
                            var n = root.eph ? (root.eph.trackCount || 0) : 0
                            var d = root.eph ? (root.eph.totalDuration || "") : ""
                            var folder = root.eph ? (root.eph.folderContext || "") : ""
                            var t = n + " " + QbzSession.tr("tracks", QbzSession.trRev)
                            if (d !== "") t += " · " + d
                            if (folder !== "") t += " · " + folder
                            return t
                        }
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        elide: Text.ElideRight
                    }
                    // Every control here is a bare glyph, and three of them do
                    // things that are not guessable from a circle with an icon
                    // in it — so each one says what it does on hover.
                    Row {
                        spacing: 12
                        topPadding: 4
                        QbzCircleAction {
                            id: playAllBtn
                            name: "play-fill"
                            primary: true
                            onClicked: QbzLocal.ephemeralPlayAll(false)
                            HoverHandler {
                                onHoveredChanged: tips.hover(hovered, playAllBtn, "eph-play",
                                    QbzSession.tr("Play the whole disc from the first track",
                                                  QbzSession.trRev))
                            }
                        }
                        QbzCircleAction {
                            id: shuffleBtn
                            name: "shuffle"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzLocal.ephemeralPlayAll(true)
                            HoverHandler {
                                onHoveredChanged: tips.hover(hovered, shuffleBtn, "eph-shuffle",
                                    QbzSession.tr("Play it in a shuffled order", QbzSession.trRev))
                            }
                        }
                        QbzCircleAction {
                            id: editFolderAlbumBtn
                            visible: !QbzLocal.localEphemeralIsCd
                                && root.allAlbums.length === 1
                                && !QbzLocal.localEphemeralLoading
                            name: "pen-line"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzLocal.ephemeralEditTags(
                                root.allAlbums[0].groupKey)
                            HoverHandler {
                                onHoveredChanged: tips.hover(hovered, editFolderAlbumBtn,
                                    "eph-edit-album",
                                    QbzSession.tr("Edit metadata", QbzSession.trRev))
                            }
                        }
                        // Fix the names. Offered for both DISC media — a
                        // CD-DA carries no titles at all, and a SACD's Master
                        // TOC can carry the wrong ones ("names itself" and
                        // "names itself correctly" are different claims) — and
                        // for NEITHER on an opened folder, which has no disc
                        // identity to write a correction under.
                        //
                        // `tags`, not a magnifier: the button edits this
                        // record's metadata. The search is only how it gets
                        // there, and a magnifier next to a search field that
                        // filters tracks reads as the same control twice.
                        QbzCircleAction {
                            id: metaBtn
                            visible: QbzLocal.localSessionIsDisc && !QbzLocal.localRipActive
                            name: "tags"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzDiscMeta.open()
                            HoverHandler {
                                onHoveredChanged: tips.hover(hovered, metaBtn, "eph-meta",
                                    QbzSession.tr("Look this disc up and correct its details",
                                                  QbzSession.trRev))
                            }
                        }
                        // Rip — a PHYSICAL disc only. An image is already a
                        // file, so offering it there would be offering to copy
                        // something the user already has.
                        QbzCircleAction {
                            id: ripBtn
                            visible: QbzLocal.localEphemeralIsCd && !QbzLocal.localRipActive
                            name: "disc-folder"
                            anchors.verticalCenter: parent.verticalCenter
                            onClicked: QbzLocal.ripWizardOpen()
                            HoverHandler {
                                onHoveredChanged: tips.hover(hovered, ripBtn, "eph-rip",
                                    QbzSession.tr("Copy this CD to your computer as FLAC files",
                                                  QbzSession.trRev))
                            }
                        }
                        // While it runs the button gives way to its progress —
                        // one control, one state, rather than a button that
                        // looks pressable during a job it cannot start twice.
                        //
                        // And the indicator is a DOOR, not a label: it is the
                        // only thing on screen while a rip runs, and the two
                        // questions it cannot answer — which track, and is it
                        // safe to eject — are the ones worth opening a panel
                        // for.
                        Rectangle {
                            id: ripChip
                            visible: QbzLocal.localRipActive
                            width: ripRow.width + 20
                            height: 32
                            radius: 6
                            anchors.verticalCenter: parent.verticalCenter
                            color: ripArea.containsMouse
                                ? theme.elevatedHoverFill
                                : theme.elevatedFill
                            Row {
                                id: ripRow
                                anchors.centerIn: parent
                                spacing: 8
                                QbzSpinner {
                                    anchors.verticalCenter: parent.verticalCenter
                                    size: 15
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: QbzSession.tr("Ripping", QbzSession.trRev)
                                        + " " + QbzLocal.localRipProgress
                                    color: theme.textSecondary
                                    font.pixelSize: theme.fontLegal
                                }
                            }
                            MouseArea {
                                id: ripArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: QbzLocal.ripPanel(true)
                                onContainsMouseChanged: tips.hover(containsMouse, ripChip,
                                    "rip-open",
                                    QbzSession.tr("See what the rip is doing, or stop it",
                                                  QbzSession.trRev))
                            }
                        }
                    }
                }
            }

            // Scanning — the shape of the album blocks that are coming
            // (56px cover + two text bars per block), not a spinner.
            QbzSkeleton {
                visible: QbzLocal.localEphemeralLoading
                variant: "rowList"
                width: parent.width
                height: visible ? 3 * 68 : 0
                rowH: 56
                rowGap: 12
                rowArtSize: 56
                phase: root.view ? root.view.skelPhase : false
            }

            // Empty (no playable tracks).
            Text {
                visible: !QbzLocal.localEphemeralLoading && root.allAlbums.length === 0
                width: parent.width
                topPadding: 24
                text: QbzSession.tr("No playable tracks in this folder.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontBody
                horizontalAlignment: Text.AlignHCenter
            }

            // ---- Filter ----
            //
            // Above the blocks rather than in the header: it acts on the LIST,
            // and a control that sits with the play buttons reads as another
            // thing you do to the album. Only drawn once there is enough to be
            // worth filtering — on a seven-track CD it would be furniture.
            Item {
                visible: !QbzLocal.localEphemeralLoading && root.eph
                         && (root.eph.trackCount || 0) > 12
                width: parent.width
                height: visible ? 40 : 0

                QbzLineEdit {
                    id: filterBox
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 260
                    sm: true
                    searchMode: true
                    placeholder: QbzSession.tr("Filter tracks", QbzSession.trRev)
                    text: root.query
                    onEdited: function (v) { root.query = v }
                }
                Text {
                    anchors.right: filterBox.left
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.query.trim() !== ""
                    text: root.matchCount + " / " + (root.eph ? (root.eph.trackCount || 0) : 0)
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                }
            }

            // Nothing matched. Distinct from "no playable tracks": the session
            // is fine, the filter is what is empty, and saying so is what
            // stops it reading as a folder that failed to scan.
            Text {
                visible: !QbzLocal.localEphemeralLoading
                         && root.allAlbums.length > 0 && root.albums.length === 0
                width: parent.width
                topPadding: 24
                text: QbzSession.tr("No track matches that.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontBody
                horizontalAlignment: Text.AlignHCenter
            }

            // ---- Album blocks ----
            Column {
                visible: !QbzLocal.localEphemeralLoading
                width: parent.width
                spacing: 22
                Repeater {
                    model: root.albums
                    delegate: Column {
                        id: block
                        required property var modelData
                        width: page.width
                        spacing: 6

                        // The per-block header is for MULTI-album sessions,
                        // where it is the only thing separating one record
                        // from the next. A single-album session already has
                        // all of it — cover, title, artist, count — in the
                        // 224px header above, and drawing it twice made the
                        // pane read like a page with a stutter.
                        Row {
                            visible: root.eph && root.eph.multiAlbum === true
                            height: visible ? implicitHeight : 0
                            width: parent.width
                            spacing: 12
                            Rectangle {
                                width: 56
                                height: 56
                                radius: 6
                                color: theme.surfaceHover
                                clip: true
                                QbzIcon {
                                    name: "disc-3"
                                    width: 24
                                    height: 24
                                    anchors.centerIn: parent
                                    tintName: "muted"
                                }
                                RoundedImage {
                                    id: blockArt
                                    anchors.fill: parent
                                    source: root.view
                                        ? root.view.artPathOf(block.modelData.artKey) : ""
                                    radius: 6
                                }
                                // Per-item: hands over when THIS album's
                                // cover is actually painted, settles out when
                                // it has none.
                                QbzSkeleton {
                                    variant: "art"
                                    anchors.fill: parent
                                    blockRadius: 6
                                    pending: root.view
                                        ? root.view.artWanted(block.modelData.artKey) : false
                                    coverReady: blockArt.ready
                                    phase: root.view ? root.view.skelPhase : false
                                    settleMs: root.view ? root.view.artSettleMs : 0
                                    settleHold: root.view ? root.view.artPulse : false
                                }
                            }
                            Column {
                                width: parent.width - 56 - blockActions.width - 24
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 2
                                Row {
                                    width: parent.width
                                    spacing: 6
                                    Text {
                                        width: parent.width - (block.modelData.isCue ? 44 : 0)
                                        text: block.modelData.title || ""
                                        color: theme.textPrimary
                                        font.pixelSize: theme.fontBody
                                        font.weight: theme.weightSemibold
                                        elide: Text.ElideRight
                                    }
                                    // CUE badge (single-file rip) — a 16px
                                    // rounded rect, not a pill (ADR-008).
                                    Rectangle {
                                        visible: block.modelData.isCue === true
                                        anchors.verticalCenter: parent.verticalCenter
                                        width: cueText.implicitWidth + 12
                                        height: 16
                                        radius: 3
                                        color: theme.surfaceElevated
                                        Text {
                                            id: cueText
                                            anchors.centerIn: parent
                                            text: "CUE"
                                            color: theme.textSecondary
                                            font.pixelSize: theme.fontLegal
                                            font.weight: theme.weightSemibold
                                        }
                                    }
                                }
                                Text {
                                    width: parent.width
                                    text: block.modelData.artist || ""
                                    color: theme.textSecondary
                                    font.pixelSize: theme.fontLegal
                                    elide: Text.ElideRight
                                }
                                Text {
                                    visible: (block.modelData.meta || "") !== ""
                                    width: parent.width
                                    text: block.modelData.meta || ""
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                    elide: Text.ElideRight
                                }
                            }
                            Row {
                                id: blockActions
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 8
                                QbzCircleAction {
                                    id: editBlockBtn
                                    visible: !QbzLocal.localEphemeralIsCd
                                    name: "pen-line"
                                    onClicked: QbzLocal.ephemeralEditTags(
                                        block.modelData.groupKey)
                                    HoverHandler {
                                        onHoveredChanged: tips.hover(hovered, editBlockBtn,
                                            "eph-edit-block",
                                            QbzSession.tr("Edit metadata", QbzSession.trRev))
                                    }
                                }
                                QbzCircleAction {
                                    name: "play-fill"
                                    onClicked: QbzLocal.ephemeralPlayAlbum(
                                        block.modelData.groupKey)
                                }
                            }
                        }

                        Repeater {
                            model: block.modelData.tracks || []
                            delegate: TrackRow {
                                required property var modelData
                                required property int index
                                width: page.width
                                item: modelData
                                number: index + 1
                                showArtwork: false
                                showAlbum: false
                                showFavorite: false
                                showDownload: false
                                // The menu is BACK, but only its queue block.
                                // A disc track can be queued, played next and
                                // played later; it cannot be favourited, put
                                // in a playlist or cached, because none of
                                // those references survive the eject.
                                showMenu: true
                                queueOnly: true
                                draggable: false
                                zebra: true
                                onPlayRequested: QbzLocal.ephemeralPlayTrack(modelData.id)
                            }
                        }
                    }
                }
            }
        }
    }
    // Close, pinned to the pane's top-right corner.
    //
    // It used to sit in the action row between Shuffle and the rest, which is
    // where a hand goes without looking — a destructive action one pixel from
    // "play". Up here it is deliberate, and it is the same place every other
    // dismissable surface in this app puts its close.
    //
    // OUTSIDE the Flickable on purpose: scrolling a long disc must not carry
    // the only way to close the session off the top of the screen.
    QbzCircleAction {
        id: closeSession
        name: "x"
        anchors.right: parent.right
        anchors.top: parent.top
        anchors.rightMargin: 32
        anchors.topMargin: 16
        onClicked: QbzLocal.ephemeralClear()
        HoverHandler {
            onHoveredChanged: tips.hover(hovered, closeSession, "eph-close",
                QbzSession.tr("Close this disc and go back to your library",
                              QbzSession.trRev))
        }
    }

    // The pane's own tooltip overlay. It takes no pointer and owns no
    // animator, so an idle one costs nothing (QbzTooltip's header).
    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 900
    }

    QbzScrollBar {
        anchors.right: parent.right
        anchors.rightMargin: 4
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        target: pane
        visible: pane.contentHeight > pane.height
    }
}
