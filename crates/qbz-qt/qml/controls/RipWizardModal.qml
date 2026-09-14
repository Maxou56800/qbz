// The rip wizard — say what is about to happen, let it be corrected, do it.
//
// Three steps, and each one exists because of something the first version got
// wrong by not asking:
//
//   1. WHAT — which disc, how many tracks, and that the output is FLAC. A
//      ripper that silently picks a format is one you find out about later.
//   2. TRACKS — editable. A CD-DA carries no titles at all, so "named wrong"
//      is the ordinary case, and once files are on disk it is a rename job.
//   3. LIBRARY — a ripped album the library cannot see is a folder full of
//      files. The wizard already knows whether the destination sits inside a
//      registered folder, so it offers the right follow-up rather than a
//      generic one.
//
// THE FORM OWNS ITS OWN STATE. Rust publishes seeds on open and never
// republishes over them (the `TrackReplacementModal` convention) — otherwise a
// late publish would overwrite what the user is typing.
//
// Mounted ONCE in AppShell: starting the rip RENAMES the session underneath,
// so a modal parented into the pane would be torn down by its own success.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    visible: doc.open === true

    readonly property var doc: {
        try { return JSON.parse(QbzLocal.localRipPlan) } catch (e) { return ({}) }
    }

    property int step: 0
    property string album: ""
    property string albumArtist: ""
    property string year: ""
    /// [{number, title, artist, duration}] — a COPY of the seeds, because the
    /// document's array is replaced wholesale on every publish and editing a
    /// binding's source array is how half the edits vanish.
    property var tracks: []
    /// "none" | "rescan" | "add"
    property string libraryChoice: "none"

    /// Bumped on every tick, so the "N of M selected" line and the Start
    /// button re-evaluate. The track array is a plain `var` mutated in place
    /// (see the delegate) and mutating it notifies nothing by itself.
    property int selectionRev: 0
    readonly property int selectedCount: {
        var _ = root.selectionRev
        var n = 0
        for (var i = 0; i < root.tracks.length; i++)
            if (root.tracks[i].selected) n++
        return n
    }
    readonly property bool canStart: (doc.destination || "") !== ""
        && root.selectedCount > 0

    function setSelected(i, on) {
        root.tracks[i].selected = on
        root.selectionRev += 1
    }
    function selectAll(on) {
        for (var i = 0; i < root.tracks.length; i++)
            root.tracks[i].selected = on
        root.selectionRev += 1
    }

    function tr(s) { return QbzSession.tr(s, QbzSession.trRev) }

    onVisibleChanged: {
        if (!visible)
            return
        step = 0
        album = doc.album || ""
        albumArtist = doc.albumArtist || ""
        year = doc.year || ""
        // The boxes too (QbzLineEdit.reset()): a disc re-read with the same
        // tags changes no draft above.
        albumField.reset()
        yearField.reset()
        albumArtistField.reset()
        libraryChoice = "none"
        var copy = []
        var src = doc.tracks || []
        for (var i = 0; i < src.length; i++) {
            copy.push({ "number": src[i].number, "title": src[i].title,
                        "artist": src[i].artist, "duration": src[i].duration,
                        "selected": src[i].selected !== false })
        }
        tracks = copy
        selectionRev += 1
    }

    // The library question can only be answered once a destination exists, so
    // the default follows the verdict rather than sitting on "none" and
    // quietly doing nothing.
    Connections {
        target: QbzLocal
        function onLocalRipPlanChanged() {
            if (!root.visible)
                return
            var st = root.doc.libraryState || "unknown"
            if (st === "inside") root.libraryChoice = "rescan"
            else if (st === "outside") root.libraryChoice = "add"
        }
    }

    function start() {
        QbzLocal.ripStart(JSON.stringify({
            "album": root.album,
            "albumArtist": root.albumArtist,
            "year": root.year,
            "destination": root.doc.destination || "",
            "tracks": root.tracks,
            "library": root.libraryChoice
        }))
    }

    QbzTheme { id: theme }

    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: QbzLocal.ripWizardClose()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 660)
        height: Math.min(parent.height * 0.9, 620)
        radius: theme.radiusLg
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        clip: true

        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        // ---- Header + step dots ----------------------------------------
        Item {
            id: header
            width: parent.width
            height: 72
            Column {
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.right: stepRow.left
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                spacing: 3
                Text {
                    width: parent.width
                    text: root.tr("Rip this CD")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: [root.tr("What will happen"), root.tr("Track names"),
                           root.tr("Your library")][root.step]
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }
            Row {
                id: stepRow
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                spacing: 6
                Repeater {
                    model: 3
                    Rectangle {
                        required property int index
                        width: 7
                        height: 7
                        radius: 4
                        anchors.verticalCenter: parent.verticalCenter
                        color: index === root.step ? theme.accent : theme.borderSubtle
                    }
                }
            }
        }
        Rectangle { y: header.height; width: parent.width; height: 1; color: theme.borderSubtle }

        // ---- Body -------------------------------------------------------
        Item {
            id: body
            y: header.height + 1
            width: parent.width
            height: card.height - header.height - 1 - footer.height

            // ===== Step 1: what will happen =============================
            Flickable {
                visible: root.step === 0
                anchors.fill: parent
                contentWidth: width
                contentHeight: whatCol.height + 40
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                Column {
                    id: whatCol
                    x: 24
                    y: 20
                    width: parent.width - 48
                    spacing: 14

                    Text {
                        width: parent.width
                        wrapMode: Text.WordWrap
                        text: root.tr("Every track on the disc is read and written as a FLAC file — one folder per album, one file per track, with tags.")
                        color: theme.textSecondary
                        font.pixelSize: theme.fontLink
                    }
                    // FLAC is stated, not implied. It is the only format this
                    // app writes, and a ripper that picks one silently is one
                    // you find out about later.
                    Row {
                        spacing: 8
                        QbzIcon { name: "audio-lines"; width: 15; height: 15
                                  anchors.verticalCenter: parent.verticalCenter; tintName: "accent" }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.tr("Format: FLAC (lossless) — the only format QBZ writes")
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                        }
                    }

                    // Album / artist / year, editable here because they name
                    // the FOLDER as well as the tags.
                    //
                    // Two explicit ROWS, not a Grid. A Grid sizes each column
                    // to its widest cell, so the full-width "Album artist"
                    // field below made column 1 as wide as the card and shoved
                    // the Year field off the right edge — the layout only
                    // looked right until a cell disagreed with its column.
                    Row {
                        width: parent.width
                        spacing: 12
                        Column {
                            width: (parent.width - 12) * 0.66
                            spacing: 4
                            Text { text: root.tr("Album"); color: theme.textMuted
                                   font.pixelSize: theme.fontLegal }
                            QbzLineEdit {
                                width: parent.width
                                id: albumField
                                text: root.album
                                onEdited: function (v) { root.album = v }
                            }
                        }
                        Column {
                            width: (parent.width - 12) * 0.34
                            spacing: 4
                            Text { text: root.tr("Year"); color: theme.textMuted
                                   font.pixelSize: theme.fontLegal }
                            QbzLineEdit {
                                width: parent.width
                                id: yearField
                                text: root.year
                                onEdited: function (v) { root.year = v }
                            }
                        }
                    }
                    Column {
                        width: parent.width
                        spacing: 4
                        Text { text: root.tr("Album artist"); color: theme.textMuted
                               font.pixelSize: theme.fontLegal }
                        QbzLineEdit {
                            width: parent.width
                            id: albumArtistField
                            text: root.albumArtist
                            onEdited: function (v) { root.albumArtist = v }
                        }
                    }

                    // Destination — asked, never guessed.
                    Column {
                        width: parent.width
                        spacing: 6
                        Text { text: root.tr("Where the album goes"); color: theme.textMuted
                               font.pixelSize: theme.fontLegal }
                        Row {
                            width: parent.width
                            spacing: 10
                            SettingsButton {
                                id: pickBtn
                                anchors.verticalCenter: parent.verticalCenter
                                text: (root.doc.destination || "") === ""
                                    ? root.tr("Choose folder…") : root.tr("Change…")
                                btnHeight: 32
                                minWidth: 0
                                enabled: root.doc.picking !== true
                                onClicked: QbzLocal.ripPickDestination()
                                HoverHandler {
                                    onHoveredChanged: tips.hover(hovered, pickBtn, "rip-pick",
                                        root.tr("Pick the folder the album folder will be created in"))
                                }
                            }
                            QbzSpinner {
                                anchors.verticalCenter: parent.verticalCenter
                                size: 15
                                visible: root.doc.picking === true
                            }
                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                width: whatCol.width - pickBtn.width - 30
                                text: (root.doc.destination || "") === ""
                                    ? root.tr("Nothing chosen yet") : root.doc.destination
                                color: (root.doc.destination || "") === ""
                                    ? theme.textMuted : theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideMiddle
                            }
                        }
                    }
                }
            }

            // ===== Step 2: track names + what to rip ====================
            //
            // Everything is ticked on arrival and the user goes from all to
            // fewer. A wizard that starts empty makes the common case — rip
            // the whole disc — the one that costs the most clicks.
            Item {
                visible: root.step === 1
                anchors.fill: parent

                Item {
                    id: listHead
                    width: parent.width
                    height: 38
                    Row {
                        anchors.left: parent.left
                        anchors.leftMargin: 24
                        anchors.right: parent.right
                        anchors.rightMargin: 24
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 10
                        RipTick {
                            id: allTick
                            anchors.verticalCenter: parent.verticalCenter
                            checked: root.selectedCount === root.tracks.length
                                     && root.tracks.length > 0
                            partial: root.selectedCount > 0
                                     && root.selectedCount < root.tracks.length
                            onToggled: root.selectAll(root.selectedCount < root.tracks.length)
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.selectedCount + " / " + root.tracks.length + " "
                                  + root.tr("selected")
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                        }
                    }
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width
                        height: 1
                        color: theme.borderSubtle
                    }
                }

                ListView {
                    y: listHead.height
                    width: parent.width
                    height: parent.height - listHead.height
                    clip: true
                    model: root.tracks.length
                    boundsBehavior: Flickable.StopAtBounds
                    delegate: Item {
                        id: rowItem
                        required property int index
                        readonly property bool on: {
                            var _ = root.selectionRev
                            return root.tracks[rowItem.index].selected === true
                        }
                        width: ListView.view.width
                        height: 46
                        Row {
                            anchors.fill: parent
                            anchors.leftMargin: 24
                            anchors.rightMargin: 24
                            spacing: 10
                            RipTick {
                                anchors.verticalCenter: parent.verticalCenter
                                checked: rowItem.on
                                onToggled: root.setSelected(rowItem.index, !rowItem.on)
                            }
                            Text {
                                width: 22
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.tracks[rowItem.index].number
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                horizontalAlignment: Text.AlignRight
                            }
                            QbzLineEdit {
                                width: parent.width - 18 - 22 - 48 - 30
                                anchors.verticalCenter: parent.verticalCenter
                                // An unticked track is not going to be written,
                                // so its name is not worth typing.
                                enabled: rowItem.on
                                opacity: rowItem.on ? 1.0 : 0.45
                                text: root.tracks[rowItem.index].title
                                // Write back into the COPY. Mutating a `var`
                                // array in place does not notify, which is fine
                                // for the TITLE precisely because nothing binds
                                // to it — `start()` reads it once, on submit.
                                // The tick does bind, which is why selection
                                // goes through `setSelected` and its counter.
                                onEdited: function (v) {
                                    root.tracks[rowItem.index].title = v
                                }
                            }
                            Text {
                                width: 48
                                anchors.verticalCenter: parent.verticalCenter
                                text: root.tracks[rowItem.index].duration
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                horizontalAlignment: Text.AlignRight
                                opacity: rowItem.on ? 1.0 : 0.45
                            }
                        }
                        Rectangle {
                            anchors.bottom: parent.bottom
                            width: parent.width
                            height: 1
                            color: theme.borderSubtle
                        }
                    }
                }
            }

            // ===== Step 3: the library ==================================
            Column {
                visible: root.step === 2
                x: 24
                y: 20
                width: parent.width - 48
                spacing: 14

                Text {
                    width: parent.width
                    wrapMode: Text.WordWrap
                    text: root.doc.libraryState === "inside"
                        ? root.tr("This folder is already part of your Local Library.")
                        : (root.doc.libraryState === "outside"
                            ? root.tr("This folder is outside your Local Library, so the album will not appear there on its own.")
                            : root.tr("Choose a destination first and this step will know what to offer."))
                    color: theme.textSecondary
                    font.pixelSize: theme.fontLink
                }
                Text {
                    width: parent.width
                    visible: root.doc.libraryState === "inside"
                    wrapMode: Text.WordWrap
                    text: root.doc.libraryFolder || ""
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideMiddle
                }

                // Two radio rows, both spelled out. "Do nothing" is a real
                // choice and is offered as one rather than being the silent
                // default nobody chose.
                Repeater {
                    model: root.doc.libraryState === "inside"
                        ? [{ "id": "rescan", "label": root.tr("Re-scan that folder when the rip finishes"),
                             "hint": root.tr("Only that folder, not your whole library.") },
                           { "id": "none", "label": root.tr("Do nothing"),
                             "hint": root.tr("The album appears the next time you scan.") }]
                        : (root.doc.libraryState === "outside"
                            ? [{ "id": "add", "label": root.tr("Add this folder to Local Library and scan it"),
                                 "hint": root.tr("Future rips into the same folder will be picked up too.") },
                               { "id": "none", "label": root.tr("Do nothing"),
                                 "hint": root.tr("The files are written; the library will not know about them.") }]
                            : [])
                    Item {
                        id: optRow
                        required property var modelData
                        width: parent.width
                        height: 46
                        Row {
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 10
                            Rectangle {
                                width: 16
                                height: 16
                                radius: 8
                                anchors.verticalCenter: parent.verticalCenter
                                color: "transparent"
                                border.width: 1
                                border.color: root.libraryChoice === optRow.modelData.id
                                    ? theme.accent : theme.borderSubtle
                                Rectangle {
                                    anchors.centerIn: parent
                                    width: 8
                                    height: 8
                                    radius: 4
                                    visible: root.libraryChoice === optRow.modelData.id
                                    color: theme.accent
                                }
                            }
                            Column {
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: 2
                                Text {
                                    text: optRow.modelData.label
                                    color: theme.textPrimary
                                    font.pixelSize: theme.fontLink
                                }
                                Text {
                                    text: optRow.modelData.hint
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                }
                            }
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.libraryChoice = optRow.modelData.id
                        }
                    }
                }
            }
        }

        // ---- Footer -----------------------------------------------------
        Item {
            id: footer
            width: parent.width
            height: 92
            anchors.bottom: parent.bottom

            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }

            // The honest scope note. Small, muted, on every step: this is a
            // convenience ripper, and somebody who needs AccurateRip should
            // learn that here rather than after a 79-minute disc.
            Text {
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.top: parent.top
                anchors.topMargin: 12
                wrapMode: Text.WordWrap
                text: root.tr("A simple ripper, made to get your CDs into the library quickly. For AccurateRip checks and advanced options, use a dedicated tool.")
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
            }

            Row {
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.bottom: parent.bottom
                anchors.bottomMargin: 14
                spacing: 12

                SettingsButton {
                    id: backBtn
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.step === 0 ? root.tr("Cancel") : root.tr("Back")
                    btnHeight: 34
                    minWidth: 0
                    onClicked: {
                        if (root.step === 0) QbzLocal.ripWizardClose()
                        else root.step -= 1
                    }
                    HoverHandler {
                        onHoveredChanged: tips.hover(hovered, backBtn, "rip-back",
                            root.step === 0 ? root.tr("Close without ripping anything")
                                            : root.tr("Go back a step"))
                    }
                }
                QbzPrimaryButton {
                    id: nextBtn
                    anchors.verticalCenter: parent.verticalCenter
                    btnHeight: 34
                    label: root.step < 2 ? root.tr("Next") : root.tr("Start ripping")
                    // The destination gates the LAST step only: a user should
                    // be able to fix the titles before being sent to a folder
                    // chooser.
                    btnEnabled: root.step < 2 || root.canStart
                    onClicked: {
                        if (root.step < 2) root.step += 1
                        else root.start()
                    }
                    HoverHandler {
                        onHoveredChanged: tips.hover(hovered, nextBtn, "rip-next",
                            root.step < 2
                                ? root.tr("Go to the next step")
                                : (root.selectedCount === 0
                                    ? root.tr("Pick at least one track to rip.")
                                    : (root.canStart
                                        ? root.tr("Read the disc and write the FLAC files")
                                        : root.tr("Choose a destination folder first"))))
                    }
                }
            }
        }
    }

    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 900
    }
}
