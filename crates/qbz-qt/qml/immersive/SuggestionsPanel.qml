// SuggestionsPanel — the SPLIT-right Suggestions panel (split-panel == 2;
// §6.4 of the 2026-08-02 immersive-port contract), port of
// crates/qbz-ui/ui/immersive/ImmersiveSuggestionsPanel.slint:35-477.
//
// A cards row (curated artist-playlist BOOK-collage cards + the seed
// Song-Radio DIAMOND-collage card, each with play/queue/play-next hover
// overlays + a corner badge; the radio card shows a spinner while building)
// and below, a "Recommended tracks" list (play on click). Split-only.
//
// Data: QbzSuggestions.suggestionsJson (§4.5 shape — {loading, error,
// cards, tracks}; status derivations live HERE: loading / error / empty).
// Covers are REMOTE URLs in the doc (the §4.4 coverflowJson precedent):
// resolution rides the shared artwork pipeline
// (QbzShell.sidebarArtworkWindow + QbzLibrary.libraryArtworkReady ->
// coverMap), the CoverflowPanel.qml:81-104 idiom. The Slint artwork-job
// half + the cover0..3 decoded-image slots have no Qt twin.
//
// COLLAGES ARE HAND-ROLLED, cards/PlaylistCollage.qml is NOT reused: that
// component implements the discover MOSAIC (grid cell splits 0.62/0.38,
// 0.34/0.66 with a 2px seam), a different layout FUNCTION from the
// suggestions BOOK collage (three overlapping, vertically-centered tiles at
// 0.62/0.62/0.66 of the well, :116-134) and the radio DIAMOND (four
// overlapping tiles, :143-168) — reuse would fork its layout, which its own
// header forbids.
//
// Slint RRGGBBAA -> Qt #AARRGGBB throughout (e.g. #000000a6 -> #a6000000).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    // --- Data (§4.5 doc; guarded parse, trap 15) ----------------------------
    readonly property var doc: {
        try {
            var d = JSON.parse(QbzSuggestions.suggestionsJson)
            if (d && d.cards)
                return d
        } catch (e) {
        }
        return { "loading": false, "error": false, "cards": [], "tracks": [] }
    }
    // Doc FIELD reads go through plain functions, NEVER chained readonly
    // vars (`readonly property var x: doc.field` stays UNDEFINED in the app
    // context, measured 2026-08-03) — the TrackInfoModal.qml parseDoc()
    // idiom.
    function cardsList() {
        return root.doc.cards || []
    }
    function tracksList() {
        return root.doc.tracks || []
    }

    // The four states (:394-441). Slint's empty branch reads artist-id == ""
    // — the §4.5 doc keeps that field Rust-side, so the empty state is the
    // derived !loading && !error && nothing-loaded (the same observable
    // state machine: empty only when a load produced nothing or no load ran).
    readonly property bool stateEmpty: !doc.loading && !doc.error
        && root.cardsList().length === 0 && root.tracksList().length === 0
    readonly property bool stateLoading: doc.loading
        && root.cardsList().length === 0 && root.tracksList().length === 0
    readonly property bool stateError: doc.error
    readonly property bool stateLoaded: !doc.error
        && (root.cardsList().length > 0 || root.tracksList().length > 0)

    // --- Cover resolution (shared artwork pipeline) ------------------------
    property var coverMap: ({})
    Connections {
        target: QbzLibrary
        function onLibraryArtworkReady(key, path) {
            var m = root.coverMap
            m[key] = path
            root.coverMap = Object.assign({}, m)
        }
    }
    Component.onCompleted: root.dispatchCovers()
    onDocChanged: root.dispatchCovers()
    function dispatchCovers() {
        var urls = []
        var cards = root.cardsList()
        for (var i = 0; i < cards.length; i++) {
            var cu = cards[i].coverUrls || []
            for (var j = 0; j < cu.length; j++)
                if (cu[j])
                    urls.push(cu[j])
        }
        var tracks = root.tracksList()
        for (var k = 0; k < tracks.length; k++)
            if (tracks[k].artUrl)
                urls.push(tracks[k].artUrl)
        if (urls.length > 0)
            QbzShell.sidebarArtworkWindow(JSON.stringify(urls))
    }
    function coverFor(url) {
        return url ? (root.coverMap[url] || "") : ""
    }

    // --- Inline components -------------------------------------------------

    // Circular overlay action button (:29-61): `primary` = 44px accent disc,
    // else a 34px translucent disc with a hairline rim.
    component OverlayButton: Rectangle {
        property string icon: ""
        property bool primary: false
        property int iconSize: 16
        signal clicked()

        width: primary ? 44 : 34
        height: primary ? 44 : 34
        radius: width / 2
        color: primary
            ? (btnArea.containsMouse ? theme.accentHover : theme.accent)
            : (btnArea.containsMouse ? "#33ffffff" : "#1fffffff")
        border.width: primary ? 0 : 1
        border.color: "#4dffffff"
        Behavior on color { ColorAnimation { duration: 120 } }

        QbzTheme { id: theme }

        QbzIcon {
            name: parent.icon
            width: parent.iconSize
            height: parent.iconSize
            anchors.centerIn: parent
            tintName: "white"
        }
        MouseArea {
            id: btnArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: parent.clicked()
        }
    }

    // One collage cover tile (:64-76): absolutely placed, radius 4, crop-fit.
    // (The Slint tile carries drop-shadow 8/#00000066; per-tile MultiEffect
    // shadows inside a scroll list are not worth the FBOs — dropped, the
    // TrackInfoModal shadow-drop precedent.)
    component CollageTile: Rectangle {
        property string artSource: ""
        visible: artSource !== ""
        radius: 4
        clip: true
        color: "#40000000"
        Image {
            anchors.fill: parent
            source: parent.artSource
            fillMode: Image.PreserveAspectCrop
            asynchronous: true
        }
    }

    // One suggestion card (:81-289): 168x224, collage well 144px on top,
    // title/subtitle below; hover overlay with the actions.
    component SuggestionCardView: Rectangle {
        id: sugCard
        property var card: null
        readonly property bool isRadio: card && card.kind === "radio"
        // Collage slot URLs, guarded (the doc array can be short).
        function coverUrl(i) {
            var cu = card ? (card.coverUrls || []) : []
            return i < cu.length ? cu[i] : ""
        }

        width: 168
        height: 224
        radius: 12
        color: cardArea.containsMouse ? "#2effffff" : "#1fffffff"
        border.width: 1
        border.color: cardArea.containsMouse ? "#40ffffff" : "#26ffffff"
        Behavior on color { ColorAnimation { duration: 150 } }
        Behavior on border.color { ColorAnimation { duration: 150 } }

        MouseArea {
            id: cardArea
            anchors.fill: parent
            hoverEnabled: true
        }

        Column {
            anchors.fill: parent
            anchors.margins: 12
            spacing: 10

            // ---- Collage / image well (:104-272) -------------------------
            Rectangle {
                id: well
                width: parent.width
                height: 144
                radius: 8
                color: "#40000000"
                clip: true

                // Book collage (playlist): 3 covers, left/center/right —
                // declared in the Slint paint order (cover0, cover2, then
                // cover1 ON TOP, :114-134).
                Item {
                    visible: !isRadio
                    anchors.fill: parent
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(0))
                        width: parent.width * 0.62
                        height: parent.height * 0.82
                        x: parent.width * 0.06
                        y: (parent.height - height) / 2
                    }
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(2))
                        width: parent.width * 0.62
                        height: parent.height * 0.82
                        x: parent.width * 0.32
                        y: (parent.height - height) / 2
                    }
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(1))
                        width: parent.width * 0.66
                        height: parent.height * 0.88
                        x: (parent.width - width) / 2
                        y: (parent.height - height) / 2
                    }
                }

                // Diamond collage (radio): up to 4 covers (:138-169).
                Item {
                    visible: isRadio
                    anchors.fill: parent
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(1))
                        width: parent.width * 0.62
                        height: parent.height * 0.62
                        x: parent.width * 0.04
                        y: (parent.height - height) / 2
                    }
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(2))
                        width: parent.width * 0.62
                        height: parent.height * 0.62
                        x: parent.width * 0.34
                        y: (parent.height - height) / 2
                    }
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(0))
                        width: parent.width * 0.64
                        height: parent.height * 0.64
                        x: (parent.width - width) / 2
                        y: parent.height * 0.06
                    }
                    CollageTile {
                        artSource: root.coverFor(sugCard.coverUrl(3))
                        width: parent.width * 0.5
                        height: parent.height * 0.5
                        x: (parent.width - width) / 2
                        y: parent.height * 0.42
                    }
                }

                // Corner badge (:174-197): 30px dark disc; the full-colour
                // Qobuz logo (playlist cards — UNTINTED, the SourceIcon.qml
                // load-bearing rule) or a white radio glyph ("qbz").
                Rectangle {
                    x: parent.width - width - 6
                    y: 6
                    width: 30
                    height: 30
                    radius: 15
                    color: "#a6000000"
                    Image {
                        visible: card && card.badge === "qobuz"
                        width: 20
                        height: 20
                        anchors.centerIn: parent
                        source: "qrc:/qt/qml/com/blitzfc/qbz/qml/assets/brand/qobuz-logo-filled.svg"
                        fillMode: Image.PreserveAspectFit
                        sourceSize.width: 20
                        sourceSize.height: 20
                    }
                    QbzIcon {
                        visible: card && card.badge !== "qobuz"
                        name: "radio"
                        width: 18
                        height: 18
                        anchors.centerIn: parent
                        tintName: "white"
                    }
                }

                // Radio building spinner — dim veil + spinner (:201-211).
                // QbzSpinner is accent-tinted; the Slint spinner is
                // #ffffffcc, so this one is hand-rolled (a white
                // loader-circle on the same 360deg/1000ms rotation).
                Rectangle {
                    visible: isRadio && card && card.loading === true
                    anchors.fill: parent
                    color: "#99000000"
                    QbzIcon {
                        name: "loader-circle"
                        width: 28
                        height: 28
                        anchors.centerIn: parent
                        tintName: "white"
                        opacity: 0.8
                        RotationAnimation on rotation {
                            running: parent.visible
                            from: 0
                            to: 360
                            duration: 1000
                            loops: Animation.Infinite
                        }
                    }
                }

                // Hover overlay with actions (:214-271). The hover sensor is
                // a HoverHandler on the WELL, NOT a MouseArea inside the
                // overlay: a MouseArea under the buttons loses containsMouse
                // the moment the cursor crosses onto a button, so the
                // overlay faded out under the cursor's target (measured over
                // RFB 2026-08-03). A HoverHandler does not compete with the
                // buttons' MouseAreas, so the veil stays up across the whole
                // well — the observable Slint behavior (its ov-ta covers the
                // full well, :220). Hovering the title/subtitle shows the
                // card fill but NOT the overlay — only the well drives it.
                HoverHandler {
                    id: wellHover
                }
                Rectangle {
                    anchors.fill: parent
                    color: "#99000000"
                    opacity: wellHover.hovered ? 1.0 : 0.0
                    Behavior on opacity { NumberAnimation { duration: 150 } }

                    Row {
                        visible: !isRadio
                        anchors.centerIn: parent
                        spacing: 10
                        OverlayButton {
                            icon: "list-end"
                            onClicked: QbzSuggestions.queuePlaylist(card.playlistId)
                        }
                        OverlayButton {
                            primary: true
                            icon: "play-fill"
                            iconSize: 18
                            onClicked: QbzSuggestions.playPlaylist(card.playlistId)
                        }
                        OverlayButton {
                            icon: "list-end"
                            onClicked: QbzSuggestions.playNextPlaylist(card.playlistId)
                        }
                    }
                    Row {
                        visible: isRadio
                        anchors.centerIn: parent
                        OverlayButton {
                            primary: true
                            icon: "radio"
                            iconSize: 18
                            // Gated !loading (:261) — a second tap while the
                            // session builds is inert.
                            onClicked: {
                                if (!card.loading)
                                    QbzSuggestions.startRadio(
                                        card.seedTrackId,
                                        card.seedTrackName,
                                        card.seedArtistId)
                            }
                        }
                    }
                }
            }

            // ---- Title + subtitle (:275-287) ------------------------------
            Text {
                width: parent.width
                text: card ? (card.title || "") : ""
                color: "#ffffff"
                font.pixelSize: 15
                font.weight: Font.DemiBold // 600
                elide: Text.ElideRight
            }
            Text {
                width: parent.width
                text: card ? (card.subtitle || "") : ""
                color: "#b3ffffff"
                font.pixelSize: 13
                elide: Text.ElideRight
            }
        }
    }

    // One recommended-track row (:292-375): 40px cover + title/artist +
    // duration, play on click.
    component RecTrackRow: Rectangle {
        id: recRow
        property var track: null
        property string artSource: ""

        width: parent ? parent.width : 0
        height: 56
        radius: 8
        readonly property bool hovered: rowArea.containsMouse || artPlayArea.containsMouse
        color: recRow.hovered ? "#21ffffff" : "transparent"
        Behavior on color { ColorAnimation { duration: 150 } }

        Row {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 10

            // Artwork + hover play overlay (:307-337).
            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: 40
                height: 40
                radius: 4
                clip: true
                color: "#14ffffff"
                Image {
                    anchors.fill: parent
                    source: recRow.artSource
                    visible: source !== "" && status === Image.Ready
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                }
                Rectangle {
                    anchors.fill: parent
                    color: "#80000000"
                    opacity: recRow.hovered ? 1.0 : 0.0
                    Behavior on opacity { NumberAnimation { duration: 150 } }
                    QbzIcon {
                        name: "play-fill"
                        width: 14
                        height: 14
                        anchors.centerIn: parent
                        tintName: "white"
                    }
                }
                // The hover disc is the row's play button: first click plays.
                MouseArea {
                    id: artPlayArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzSuggestions.playTrack(recRow.track.id)
                }
            }

            // Title + artist (:340-357).
            Column {
                anchors.verticalCenter: parent.verticalCenter
                width: parent.width - 40 - 10 - durText.implicitWidth - 10
                spacing: 2
                Text {
                    width: parent.width
                    text: track ? (track.title || "") : ""
                    color: "#ffffff"
                    font.pixelSize: 15
                    font.weight: Font.Medium // 500
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: track ? (track.artist || "") : ""
                    color: "#b3ffffff"
                    font.pixelSize: 13
                    elide: Text.ElideRight
                }
            }

            // Duration (:359-368).
            Text {
                id: durText
                anchors.verticalCenter: parent.verticalCenter
                text: track ? (track.duration || "") : ""
                color: "#99ffffff"
                font.pixelSize: 13
                font.weight: Font.Medium
            }
        }

        // The track-row policy (#790, rows/TrackRow.qml `clickPlays`): a
        // single click on the body does nothing, a double click plays. Behind
        // the cells (z -1) so the artwork's play button answers its own click.
        MouseArea {
            id: rowArea
            z: -1
            anchors.fill: parent
            hoverEnabled: true
            onDoubleClicked: QbzSuggestions.playTrack(recRow.track.id)
        }
    }

    // --- Centered, height-capped content (:386-392) -------------------------
    // height min(vh, clamp(340, vh*0.62, 760)) — vh is the plate's height.
    Item {
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.verticalCenter: parent.verticalCenter
        width: parent.width
        height: Math.min(root.height, Math.max(340, Math.min(root.height * 0.62, 760)))

        // ---- Empty (no track selected, :395-413) -------------------------
        Column {
            visible: root.stateEmpty
            anchors.centerIn: parent
            spacing: 12
            QbzIcon {
                name: "radio"
                width: 32
                height: 32
                anchors.horizontalCenter: parent.horizontalCenter
                tintName: "white"
                opacity: 0.7 // #ffffffb3
            }
            Text {
                text: QbzSession.tr("No track selected", QbzSession.trRev)
                color: "#b3ffffff"
                font.pixelSize: 18
            }
        }

        // ---- Loading (no content yet, :416-425) ---------------------------
        Text {
            visible: root.stateLoading
            anchors.centerIn: parent
            text: QbzSession.tr("Loading suggestions...", QbzSession.trRev)
            color: "#ccffffff"
            font.pixelSize: 18
        }

        // ---- Error (:428-437) ---------------------------------------------
        Text {
            visible: root.stateError
            anchors.centerIn: parent
            text: QbzSession.tr("Failed to load suggestions", QbzSession.trRev)
            color: "#e6ffffff"
            font.pixelSize: 18
            font.weight: Font.DemiBold // 600
        }

        // ---- Loaded (:440-489) ---------------------------------------------
        Item {
            visible: root.stateLoaded
            anchors.fill: parent
            clip: true

            Flickable {
                id: flick
                anchors.fill: parent
                contentWidth: width - 12 // padding-right 12 (:450)
                contentHeight: body.implicitHeight
                boundsBehavior: Flickable.StopAtBounds
                clip: true

                Column {
                    id: body
                    width: flick.contentWidth
                    spacing: 24

                    // Cards row (:454-461).
                    Row {
                        visible: root.cardsList().length > 0
                        spacing: 12
                        Repeater {
                            model: root.cardsList()
                            delegate: SuggestionCardView {
                                required property var modelData
                                card: modelData
                            }
                        }
                    }

                    // Recommended tracks (:464-477).
                    Text {
                        visible: root.tracksList().length > 0
                        text: QbzSession.tr("Recommended tracks", QbzSession.trRev)
                        color: "#e6ffffff"
                        font.pixelSize: 14
                        font.weight: Font.DemiBold // 600
                        font.letterSpacing: 0.4
                    }
                    Column {
                        visible: root.tracksList().length > 0
                        width: parent.width
                        spacing: 4
                        Repeater {
                            model: root.tracksList()
                            delegate: RecTrackRow {
                                required property var modelData
                                track: modelData
                                artSource: root.coverFor(modelData.artUrl)
                            }
                        }
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
