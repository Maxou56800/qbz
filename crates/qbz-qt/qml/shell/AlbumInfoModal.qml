// Album Info (Credits / Review) modal — Qt port of AlbumCreditsModal.slint
// (itself 1:1 with Tauri's AlbumCreditsModal.svelte: left fixed meta column,
// right scrollable tabs). Opened from the AlbumView header info button.
//
// DATA: QbzAlbum.openAlbumInfo(albumId) publishes QbzAlbum.albumInfoJson
// while albumInfoLoading is true (album_info_qt.rs). Document shape:
//   { "error": "", "albumId": "", "title": "", "artist": "",
//     "artUrl": "", "customCoverPath": "", "label": "", "labelId": "",
//     "releaseDate": "September 2, 2021",
//     "metaLine": "Hard Rock · 10 tracks · 1h 21m",
//     "quality": "24-Bit / 96 kHz", "review": "", "hasReview": true,
//     "tracks": [ { "id", "number", "title", "artist", "hasCredits",
//                   "copyright", "performers":
//                     [ { "name", "roles", "primaryRole" } ] } ] }
// Tab switching and close are QML-local; the bridge carries data only.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Popup {
    id: root

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    padding: 0
    z: 3000
    modal: true
    // Our own scrim (the reference's #000000bf) — the default modal dimmer
    // would darken it twice.
    dim: false
    closePolicy: Popup.CloseOnEscape

    QbzTheme { id: theme }

    // --- Data ------------------------------------------------------------
    readonly property var doc: parseDoc()
    function parseDoc() {
        try {
            return JSON.parse(QbzAlbum.albumInfoJson || "{}")
        } catch (e) {
            return ({})
        }
    }
    readonly property bool loading: QbzAlbum.albumInfoLoading === true
    readonly property string errorText: QbzAlbum.albumInfoError || ""
    property string activeTab: "credits"
    // The album the user ASKED for. The data overlay holds until the
    // published document answers for this id — belt-and-braces next to the
    // Rust generation guard (album_info_qt.rs): a late doc for a previous
    // album must never render inside this one's page.
    //
    // This is an EXACT string comparison, so it can only ever be safe because
    // the publisher stamps `albumId` with the id it was asked for rather than
    // the one /album/get echoed back (album_info_qt.rs::map). Do not "fix"
    // that to the API's id: any divergence blanks this card with no log.
    property string requestedId: ""

    // --- Copy (2026-09-13) --------------------------------------------------
    // Basic = "{Album} - {Artist}"; full = the header lines plus the ACTIVE
    // tab as plain text: every track with its performers (Credits) or the
    // review. The glyph turns into a check for a moment as the receipt.
    QbzClipboard { id: clipboard }
    property bool justCopied: false
    Timer {
        id: copiedTimer
        interval: 1400
        onTriggered: root.justCopied = false
    }
    function nonEmpty(s) { return (s || "") !== "" }
    function basicText() {
        return [root.doc.title, root.doc.artist].filter(root.nonEmpty).join(" - ")
    }
    function fullText() {
        var d = root.doc
        var lines = [d.title, d.artist].filter(root.nonEmpty)
        var meta = [d.label, d.releaseDate, d.metaLine, d.quality].filter(root.nonEmpty)
        if (meta.length > 0) lines.push(meta.join(" · "))
        if (root.activeTab === "review") {
            if (root.nonEmpty(d.review)) { lines.push(""); lines.push(d.review) }
            return lines.join("\n")
        }
        var tracks = d.tracks || []
        if (tracks.length > 0) lines.push("")
        for (var i = 0; i < tracks.length; i++) {
            var tr = tracks[i]
            var num = (tr.number === undefined || tr.number === null) ? "" : String(tr.number)
            var head = (num !== "" ? num + ". " : "") + (tr.title || "")
            if (root.nonEmpty(tr.artist)) head += " — " + tr.artist
            lines.push(head)
            var perf = tr.performers || []
            for (var j = 0; j < perf.length; j++) {
                var p = perf[j]
                lines.push("    " + (p.name || "") + (root.nonEmpty(p.roles) ? ": " + p.roles : ""))
            }
            if (root.nonEmpty(tr.copyright)) lines.push("    " + tr.copyright)
        }
        return lines.join("\n")
    }
    function copy(which) {
        var text = which === "full" ? root.fullText() : root.basicText()
        if (clipboard.copy(text)) {
            root.justCopied = true
            copiedTimer.restart()
        }
    }
    CardMenu {
        id: copyMenu
        menuWidth: 200
        entries: [
            { "label": QbzSession.tr("Copy basic data", QbzSession.trRev), "icon": "copy", "action": "basic" },
            { "label": QbzSession.tr("Copy full", QbzSession.trRev), "icon": "clipboard", "action": "full" }
        ]
        onPicked: function (a) { root.copy(a) }
    }

    // --- Actions ---------------------------------------------------------

    /// Header info button. The view gates local/Plex albums (no button);
    /// the Rust side re-checks catalog ids.
    function openFor(albumId) {
        if (!albumId || albumId === "")
            return
        root.activeTab = "credits"
        root.requestedId = albumId
        QbzAlbum.openAlbumInfo(albumId)
        open()
    }

    function playTrack(trackId) {
        if (!trackId || trackId === "")
            return
        // Album context: the queue keeps the album, the cursor starts at the
        // tapped row (same seam as the view's own track rows).
        QbzPlayer.playAlbumFrom(doc.albumId || "", trackId)
    }

    function openLabel(labelId) {
        if (!labelId || labelId === "")
            return
        close()
        QbzHome.openLabel(labelId)
    }

    // Musician click — consumer #4 of six (contract §2.2).
    //
    // ORDER: dispatch, THEN close — the reverse of what stood here, and it
    // matches the reference (AlbumCreditsModal.svelte:20-28). It matters now
    // that the click has a destination: a `weak`/`none` result opens the
    // global MusicianModal, and closing this one first hands focus back to the
    // shell a moment before that modal takes it, which is precisely the
    // unstable-focus window the contract flags (§5.2). The neighbouring
    // openArtist/openLabel keep close-first because they navigate the shell
    // underneath rather than summoning another overlay.
    function openMusician(name, primaryRole) {
        if (!name || name === "")
            return
        QbzArtist.resolveMusician(name, primaryRole || "")
        close()
    }

    // Cover source: the custom override beats the remote best() URL, same
    // rule as the AlbumView header.
    readonly property string coverSource: (doc.customCoverPath || "") !== ""
        ? "file://" + doc.customCoverPath
        : (doc.artUrl || "")

    background: Rectangle { color: "#bf000000" }

    // One Credits row: number/play-on-hover, title + artist, expand chevron;
    // the expanded panel lists performers (name links) + copyright.
    component CreditTrackRow: Column {
        id: row
        property var track: ({})
        property bool isLast: false
        property bool expanded: false

        width: parent ? parent.width : 0

        Rectangle {
            width: parent.width
            height: 46
            color: "transparent"
            opacity: (rowArea.containsMouse && row.track.hasCredits) ? 0.8 : 1.0

            MouseArea {
                id: rowArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: row.track.hasCredits ? Qt.PointingHandCursor : Qt.ArrowCursor
                onClicked: if (row.track.hasCredits) row.expanded = !row.expanded
            }
            Row {
                anchors.fill: parent
                spacing: 12
                // Number / play-on-hover cell.
                Item {
                    width: 28
                    height: parent.height
                    Text {
                        anchors.centerIn: parent
                        visible: !rowArea.containsMouse && !playCell.containsMouse
                        text: row.track.number || ""
                        color: theme.textMuted
                        font.pixelSize: 14
                    }
                    Rectangle {
                        anchors.centerIn: parent
                        visible: rowArea.containsMouse || playCell.containsMouse
                        width: 28
                        height: 28
                        radius: 14
                        color: theme.accent
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "play-fill"
                            width: 14
                            height: 14
                            tintName: theme.accentGlyphTint
                        }
                    }
                    MouseArea {
                        id: playCell
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.playTrack(row.track.id)
                    }
                }
                Column {
                    width: parent.width - 28 - 18 - (credIcon.visible ? 18 : 0) - 36
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Text {
                        width: parent.width
                        text: row.track.title || ""
                        color: theme.textPrimary
                        font.pixelSize: 14
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                    }
                    Text {
                        width: parent.width
                        text: row.track.artist || ""
                        color: theme.textMuted
                        font.pixelSize: 12
                        elide: Text.ElideRight
                    }
                }
                QbzIcon {
                    id: credIcon
                    visible: row.track.hasCredits === true
                    anchors.verticalCenter: parent.verticalCenter
                    name: row.expanded ? "chevron-up" : "chevron-down"
                    width: 18
                    height: 18
                    tintName: row.expanded ? "primary" : "muted"
                }
            }
        }

        // Expanded credits.
        Column {
            visible: row.expanded && row.track.hasCredits
            width: parent.width
            leftPadding: 40
            bottomPadding: 12
            spacing: 0
            Repeater {
                model: row.track.performers || []
                delegate: Row {
                    required property var modelData
                    width: parent ? parent.width : 0
                    spacing: 0
                    topPadding: 4
                    bottomPadding: 4
                    QbzSelectableText {
                        id: perfName
                        text: modelData.name
                        color: theme.textPrimary
                        linkHref: "musician"
                        pixelSize: 13
                        weight: theme.weightMedium
                        onLinkActivated: root.openMusician(modelData.name, modelData.primaryRole)
                    }
                    QbzSelectableText {
                        width: Math.max(0, parent.width - perfName.width)
                        text: String(modelData.roles || "")
                        color: theme.textMuted
                        pixelSize: 13
                    }
                }
            }
            Item { visible: (row.track.copyright || "") !== ""; width: 1; height: 8 }
            Rectangle {
                visible: (row.track.copyright || "") !== ""
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }
            Item { visible: (row.track.copyright || "") !== ""; width: 1; height: 8 }
            Text {
                visible: (row.track.copyright || "") !== ""
                width: parent.width
                text: row.track.copyright || ""
                color: theme.textMuted
                font.pixelSize: 12
                wrapMode: Text.WordWrap
            }
        }

        Rectangle {
            visible: !row.isLast
            width: parent.width
            height: 1
            color: theme.borderSubtle
        }
    }

    contentItem: Item {
        // Scrim — click outside dismisses.
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 40, 850)
            height: Math.min(root.height - 80, bodyCol.implicitHeight + 24)
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            // Swallow clicks so they don't reach the scrim.
            MouseArea { anchors.fill: parent }

            Column {
                id: bodyCol
                width: parent.width
                spacing: 0

                // ---- Header ----------------------------------------------
                Row {
                    width: parent.width
                    height: 62
                    spacing: 16
                    Item { width: 8; height: 1 }
                    Column {
                        width: parent.width - 130
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 4
                        // Selectable (2026-09-13), like every field below.
                        QbzSelectableText {
                            width: parent.width
                            text: root.doc.title || ""
                            color: theme.textPrimary
                            pixelSize: 16
                            weight: theme.weightSemibold
                        }
                        QbzSelectableText {
                            width: parent.width
                            text: root.doc.artist || ""
                            color: theme.textMuted
                            pixelSize: theme.fontLegal
                        }
                    }
                    // Copy: basic line or the active tab, from the flyout.
                    Rectangle {
                        id: copyBtn
                        width: 32
                        height: 32
                        anchors.verticalCenter: parent.verticalCenter
                        color: "transparent"
                        QbzIcon {
                            anchors.centerIn: parent
                            name: root.justCopied ? "check" : "copy"
                            width: 17
                            height: 17
                            tintName: root.justCopied ? "accent"
                                : (copyArea.containsMouse ? "primary" : "muted")
                        }
                        MouseArea {
                            id: copyArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: copyMenu.openBelowLeft(copyBtn)
                        }
                    }
                    Rectangle {
                        width: 32
                        height: 32
                        anchors.verticalCenter: parent.verticalCenter
                        color: "transparent"
                        QbzIcon {
                            anchors.centerIn: parent
                            name: "x"
                            width: 18
                            height: 18
                            tintName: closeArea.containsMouse ? "primary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.close()
                        }
                    }
                    Item { width: 8; height: 1 }
                }

                // ---- Body (two columns) ----------------------------------
                Row {
                    width: parent.width - 48
                    x: 24
                    spacing: 24

                    // LEFT — album meta (fixed 260px).
                    Column {
                        width: 260
                        spacing: 0
                        Rectangle {
                            width: 200
                            height: 200
                            radius: 8
                            color: theme.surfaceElevated
                            clip: true
                            RoundedImage {
                                anchors.fill: parent
                                source: root.coverSource
                                radius: 8
                            }
                        }
                        Item { width: 1; height: 16 }
                        Column {
                            visible: (root.doc.label || "") !== ""
                            spacing: 0
                            Text {
                                text: QbzSession.tr("Released by", QbzSession.trRev)
                                color: theme.textMuted
                                font.pixelSize: 13
                            }
                            QbzSelectableText {
                                width: parent.width
                                text: root.doc.label || ""
                                color: theme.textPrimary
                                linkHref: (root.doc.labelId || "") !== "" ? "label" : ""
                                pixelSize: 13
                                weight: theme.weightSemibold
                                onLinkActivated: root.openLabel(root.doc.labelId || "")
                            }
                            QbzSelectableText {
                                visible: (root.doc.releaseDate || "") !== ""
                                width: parent.width
                                text: QbzSession.tr("on", QbzSession.trRev) + " " + (root.doc.releaseDate || "")
                                color: theme.textMuted
                                pixelSize: 13
                            }
                        }
                        Item { visible: (root.doc.label || "") !== ""; width: 1; height: 8 }
                        QbzSelectableText {
                            visible: (root.doc.metaLine || "") !== ""
                            width: parent.width
                            text: root.doc.metaLine || ""
                            color: theme.textMuted
                            pixelSize: 13
                        }
                        Item { visible: (root.doc.quality || "") !== ""; width: 1; height: 12 }
                        QbzSelectableText {
                            visible: (root.doc.quality || "") !== ""
                            width: parent.width
                            text: root.doc.quality || ""
                            color: theme.textSecondary
                            pixelSize: 13
                        }
                    }

                    // RIGHT — tabs + scrollable content.
                    Column {
                        width: parent.width - 260 - 24
                        spacing: 0

                        // Tab switcher (only when a review exists).
                        Column {
                            visible: root.doc.hasReview === true
                            width: parent.width
                            spacing: 0
                            Row {
                                spacing: 16
                                Text {
                                    text: QbzSession.tr("Credits", QbzSession.trRev)
                                    color: root.activeTab === "credits" ? theme.accent : theme.textMuted
                                    font.pixelSize: 13
                                    font.weight: theme.weightMedium
                                    MouseArea {
                                        anchors.fill: parent
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: root.activeTab = "credits"
                                    }
                                }
                                Text {
                                    text: QbzSession.tr("Review", QbzSession.trRev)
                                    color: root.activeTab === "review" ? theme.accent : theme.textMuted
                                    font.pixelSize: 13
                                    font.weight: theme.weightMedium
                                    MouseArea {
                                        anchors.fill: parent
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: root.activeTab = "review"
                                    }
                                }
                            }
                            Item { width: 1; height: 12 }
                            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
                            Item { width: 1; height: 12 }
                        }

                        // Content. Fixed height so the card's height binding
                        // stays acyclic: the Flickable scrolls what overflows.
                        Rectangle {
                            width: parent.width
                            height: 320
                            color: "transparent"
                            clip: true

                            Flickable {
                                id: flick
                                anchors.fill: parent
                                contentWidth: width
                                contentHeight: contentCol.implicitHeight
                                boundsBehavior: Flickable.StopAtBounds

                                Column {
                                    id: contentCol
                                    width: flick.width - 18
                                    spacing: 0

                                    // Loader-gated, not `visible:`-gated: a
                                    // hidden Column is still BUILT, so opening
                                    // this modal on the Description tab was
                                    // constructing one CreditTrackRow per
                                    // track of the album regardless — the
                                    // whole point of a credits tab being that
                                    // a box set has a lot of them.
                                    Loader {
                                        width: parent.width
                                        active: root.activeTab === "credits"
                                        visible: active
                                        sourceComponent: Column {
                                            width: parent.width
                                            spacing: 0
                                            Repeater {
                                                model: root.doc.tracks || []
                                                delegate: CreditTrackRow {
                                                    required property var modelData
                                                    required property int index
                                                    track: modelData
                                                    isLast: index === (root.doc.tracks || []).length - 1
                                                }
                                            }
                                        }
                                    }

                                    QbzSelectableText {
                                        visible: root.activeTab === "review"
                                        width: parent.width
                                        text: root.doc.review || ""
                                        color: theme.textSecondary
                                        pixelSize: 14
                                    }
                                }
                            }

                            QbzScrollBar {
                                target: flick
                                anchors.right: parent.right
                                anchors.top: parent.top
                                anchors.bottom: parent.bottom
                            }
                        }
                    }
                }

                Item { width: 1; height: 24 }
            }

            // ---- Loading / error / stale-doc overlay ----------------------
            Rectangle {
                anchors.fill: parent
                visible: root.loading || root.errorText !== ""
                    || (root.doc.albumId || "") !== root.requestedId
                color: theme.surfaceCard
                radius: theme.radiusMd
                MouseArea { anchors.fill: parent }
                Column {
                    anchors.centerIn: parent
                    spacing: 8
                    QbzSpinner {
                        anchors.horizontalCenter: parent.horizontalCenter
                        visible: root.loading && root.errorText === ""
                    }
                    Text {
                        visible: root.errorText !== ""
                        text: QbzSession.tr("Failed to load album credits", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontBody
                    }
                    Text {
                        visible: root.errorText !== ""
                        width: Math.min(card.width - 96, implicitWidth)
                        text: root.errorText
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
            }
        }
    }
}
