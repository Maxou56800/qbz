// Responsive workspace for the local metadata editor. Album fields and the
// track spreadsheet scroll independently so the primary editing surface does
// not fall below a long form. Wide windows use an 8/12 + 4/12 split; compact
// windows switch between the two panes instead of stacking them.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: workspace
    required property var editor
    readonly property bool kioskHost: editor.kioskHost === true

    property string compactPane: "tracks"
    property bool advancedOpen: false
    property bool lookupOpen: false
    readonly property bool wide: width >= 1120
    readonly property int gap: 12

    QbzTheme { id: theme }

    Row {
        id: compactSwitch
        visible: !workspace.wide
        height: visible ? (workspace.kioskHost ? 44 : 34) : 0
        spacing: 6
        anchors.horizontalCenter: parent.horizontalCenter

        Repeater {
            model: [
                { "id": "tracks", "label": workspace.editor.tr("Tracks") },
                { "id": "tags", "label": workspace.editor.tr("Album tags") }
            ]
            delegate: Rectangle {
                id: paneChoice
                required property var modelData
                width: 128
                height: workspace.kioskHost ? 44 : 32
                radius: theme.radiusSm
                color: workspace.compactPane === modelData.id
                    ? theme.surfaceElevated
                    : choiceMouse.containsMouse ? theme.surfaceHover : "transparent"
                border.width: workspace.compactPane === modelData.id ? 1 : 0
                border.color: theme.borderMuted
                Text {
                    anchors.centerIn: parent
                    text: paneChoice.modelData.label
                    color: workspace.compactPane === paneChoice.modelData.id
                        ? theme.textPrimary : theme.textSecondary
                    font.pixelSize: theme.fontBody
                    font.weight: workspace.compactPane === paneChoice.modelData.id
                        ? theme.weightSemibold : theme.weightRegular
                }
                MouseArea {
                    id: choiceMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: workspace.compactPane = paneChoice.modelData.id
                }
            }
        }
    }

    Item {
        id: panes
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.top: workspace.wide ? parent.top : compactSwitch.bottom
        anchors.topMargin: workspace.wide ? 0 : 8

        Rectangle {
            id: trackPane
            visible: workspace.wide || workspace.compactPane === "tracks"
            x: 0
            width: workspace.wide
                ? Math.floor((panes.width - workspace.gap) * 8 / 12)
                : panes.width
            height: panes.height
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            property bool detailsOpen: false
            readonly property int tableW: Math.max(0, width - 16)
            readonly property int discW: 56
            readonly property int trackW: 64
            readonly property int artistW: Math.max(150, tableW * 0.23)
            readonly property int fileW: Math.max(150, tableW * 0.22)
            readonly property int titleW: Math.max(180,
                tableW - discW - trackW - artistW - fileW)

            Item {
                id: trackTitle
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: workspace.kioskHost ? 0 : 44
                visible: !workspace.kioskHost
                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    text: workspace.editor.tr("Tracks")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                }
                Text {
                    anchors.right: parent.right
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    width: Math.max(0, parent.width - 120)
                    horizontalAlignment: Text.AlignRight
                    text: workspace.editor.tr("File names are shown for reference and cannot be changed here.")
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }

            Rectangle {
                id: tableHeader
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: trackTitle.bottom
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                height: 32
                color: theme.alphaTier(8)
                border.width: 1
                border.color: theme.borderMuted

                Row {
                    anchors.fill: parent
                    Repeater {
                        model: [
                            { "label": workspace.editor.tr("Disc"), "width": trackPane.discW },
                            { "label": workspace.editor.tr("Track"), "width": trackPane.trackW },
                            { "label": workspace.editor.tr("Title"), "width": trackPane.titleW },
                            { "label": workspace.editor.tr("Artist credit"), "width": trackPane.artistW },
                            { "label": workspace.editor.tr("File"), "width": trackPane.fileW }
                        ]
                        delegate: Item {
                            required property var modelData
                            width: modelData.width
                            height: tableHeader.height
                            Text {
                                anchors.fill: parent
                                leftPadding: 9
                                rightPadding: 7
                                text: parent.modelData.label
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightSemibold
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                            Rectangle {
                                anchors.right: parent.right
                                width: 1
                                height: parent.height
                                color: theme.borderSubtle
                            }
                        }
                    }
                }
            }

            ListView {
                id: trackList
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: tableHeader.bottom
                anchors.bottom: detailsBar.top
                anchors.leftMargin: 8
                anchors.rightMargin: 8
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                model: workspace.editor.tracks

                delegate: Rectangle {
                    id: trackDelegate
                    required property var modelData
                    required property int index
                    width: trackList.width
                    height: workspace.kioskHost ? 44 : 32
                    readonly property bool selected:
                        workspace.editor.selectedTrackIndex === trackDelegate.index
                    color: selected ? theme.alphaTier(12)
                        : rowHover.hovered ? theme.surfaceHover
                        : trackDelegate.index % 2 ? theme.alphaTier(4) : "transparent"

                    HoverHandler { id: rowHover }
                    TapHandler {
                        acceptedButtons: Qt.LeftButton
                        onTapped: workspace.editor.selectedTrackIndex = trackDelegate.index
                    }
                    Rectangle {
                        anchors.left: parent.left
                        width: trackDelegate.selected ? 2 : 0
                        height: parent.height
                        color: theme.accent
                    }
                    Row {
                        anchors.fill: parent

                        component EditCell: Rectangle {
                            id: cell
                            property alias text: input.text
                            property bool numeric: false
                            signal edited(string value)
                            signal focused()
                            height: trackDelegate.height
                            color: input.activeFocus ? theme.alphaTier(8) : "transparent"
                            border.width: input.activeFocus ? 1 : 0
                            border.color: theme.focusRing
                            TextInput {
                                QbzTextEditMenu { }
                                id: input
                                anchors.fill: parent
                                leftPadding: 9
                                rightPadding: 7
                                color: theme.textPrimary
                                selectionColor: theme.accent
                                selectedTextColor: theme.accentText
                                font.pixelSize: theme.fontBody
                                verticalAlignment: TextInput.AlignVCenter
                                selectByMouse: true
                                clip: true
                                inputMethodHints: cell.numeric ? Qt.ImhDigitsOnly : Qt.ImhNone
                                onTextEdited: cell.edited(text)
                                onActiveFocusChanged: if (activeFocus) cell.focused()
                            }
                            Rectangle {
                                anchors.right: parent.right
                                width: 1
                                height: parent.height
                                color: theme.borderSubtle
                            }
                        }

                        EditCell {
                            width: trackPane.discW
                            numeric: true
                            text: trackDelegate.modelData.discNumber
                            onEdited: function(value) { trackDelegate.modelData.discNumber = value }
                            onFocused: workspace.editor.selectedTrackIndex = trackDelegate.index
                        }
                        EditCell {
                            width: trackPane.trackW
                            enabled: !workspace.editor.fixedTrackNumbers
                            numeric: true
                            text: trackDelegate.modelData.trackNumber
                            onEdited: function(value) { trackDelegate.modelData.trackNumber = value }
                            onFocused: workspace.editor.selectedTrackIndex = trackDelegate.index
                        }
                        EditCell {
                            width: trackPane.titleW
                            text: trackDelegate.modelData.title
                            onEdited: function(value) { trackDelegate.modelData.title = value }
                            onFocused: workspace.editor.selectedTrackIndex = trackDelegate.index
                        }
                        EditCell {
                            width: trackPane.artistW
                            text: trackDelegate.modelData.artistCredit
                            onEdited: function(value) { trackDelegate.modelData.artistCredit = value }
                            onFocused: workspace.editor.selectedTrackIndex = trackDelegate.index
                        }
                        Item {
                            width: trackPane.fileW
                            height: trackDelegate.height
                            Text {
                                anchors.fill: parent
                                leftPadding: 9
                                rightPadding: 7
                                text: trackDelegate.modelData.fileName
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideMiddle
                            }
                        }
                    }
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width
                        height: 1
                        color: theme.borderSubtle
                    }
                }
                QbzScrollBar {
                    target: trackList
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                }
            }

            Rectangle {
                id: detailsBar
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: detailsPane.top
                height: 38
                color: detailsMouse.containsMouse ? theme.surfaceHover : theme.alphaTier(6)
                border.width: 1
                border.color: theme.borderSubtle
                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    width: parent.width - 48
                    text: workspace.editor.selectedTrackIndex >= 0
                        && workspace.editor.selectedTrackIndex < workspace.editor.tracks.length
                        ? workspace.editor.tr("Selected track details") + " · "
                            + workspace.editor.tracks[workspace.editor.selectedTrackIndex].fileName
                        : workspace.editor.tr("Select a track to edit its extended credits")
                    color: theme.textSecondary
                    font.pixelSize: theme.fontBody
                    elide: Text.ElideMiddle
                }
                QbzIcon {
                    anchors.right: parent.right
                    anchors.rightMargin: 12
                    anchors.verticalCenter: parent.verticalCenter
                    width: 14
                    height: 14
                    name: trackPane.detailsOpen ? "chevron-down" : "chevron-up"
                    tintName: "muted"
                }
                MouseArea {
                    id: detailsMouse
                    anchors.fill: parent
                    hoverEnabled: true
                    enabled: workspace.editor.selectedTrackIndex >= 0
                    cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                    onClicked: trackPane.detailsOpen = !trackPane.detailsOpen
                }
            }

            Rectangle {
                id: detailsPane
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                height: trackPane.detailsOpen ? 218 : 0
                visible: height > 0
                color: theme.alphaTier(4)
                clip: true
                property var rowData: workspace.editor.selectedTrackIndex >= 0
                    && workspace.editor.selectedTrackIndex < workspace.editor.tracks.length
                    ? workspace.editor.tracks[workspace.editor.selectedTrackIndex] : null

                Grid {
                    anchors.fill: parent
                    anchors.margins: 10
                    columns: 2
                    columnSpacing: 10
                    rowSpacing: 6

                    component DetailField: Column {
                        property string label: ""
                        property string value: ""
                        signal edited(string value)
                        width: (detailsPane.width - 30) / 2
                        spacing: 3
                        Text {
                            width: parent.width
                            text: parent.label
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                            elide: Text.ElideRight
                        }
                        QbzLineEdit {
                            width: parent.width
                            text: parent.value
                            onEdited: function(value) { parent.edited(value) }
                        }
                    }

                    DetailField {
                        label: workspace.editor.tr("Artists (ordered; semicolon separated)")
                        value: detailsPane.rowData ? workspace.editor.listText(detailsPane.rowData.artists) : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.artists = workspace.editor.splitList(value) }
                    }
                    DetailField {
                        label: workspace.editor.tr("Composers")
                        value: detailsPane.rowData ? workspace.editor.listText(detailsPane.rowData.composers) : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.composers = workspace.editor.splitList(value) }
                    }
                    DetailField {
                        label: workspace.editor.tr("Performers")
                        value: detailsPane.rowData ? workspace.editor.listText(detailsPane.rowData.performers) : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.performers = workspace.editor.splitList(value) }
                    }
                    DetailField {
                        label: workspace.editor.tr("MusicBrainz artist IDs")
                        value: detailsPane.rowData ? workspace.editor.listText(detailsPane.rowData.musicbrainzArtistIds) : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.musicbrainzArtistIds = workspace.editor.splitList(value) }
                    }
                    DetailField {
                        label: workspace.editor.tr("MusicBrainz recording ID")
                        value: detailsPane.rowData ? detailsPane.rowData.musicbrainzRecordingId : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.musicbrainzRecordingId = value }
                    }
                    DetailField {
                        label: workspace.editor.tr("MusicBrainz track ID")
                        value: detailsPane.rowData ? detailsPane.rowData.musicbrainzTrackId : ""
                        onEdited: function(value) { if (detailsPane.rowData) detailsPane.rowData.musicbrainzTrackId = value }
                    }
                }
            }
        }

        Rectangle {
            id: tagPane
            visible: workspace.wide || workspace.compactPane === "tags"
            x: workspace.wide ? trackPane.width + workspace.gap : 0
            width: workspace.wide ? panes.width - x : panes.width
            height: panes.height
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            Flickable {
                id: tagScroll
                anchors.fill: parent
                anchors.margins: 12
                contentWidth: width
                contentHeight: tagContent.implicitHeight
                clip: true
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: tagContent
                    width: tagScroll.width - 12
                    spacing: 10

                    Row {
                        width: parent.width
                        height: 96
                        spacing: 10
                        Rectangle {
                            width: 96
                            height: 96
                            radius: theme.radiusSm
                            color: theme.surfaceElevated
                            border.width: 1
                            border.color: theme.borderSubtle
                            clip: true
                            Image {
                                id: cover
                                anchors.fill: parent
                                source: workspace.editor.artwork.previewPath || ""
                                fillMode: Image.PreserveAspectCrop
                                asynchronous: true
                                cache: false
                                visible: source !== ""
                            }
                            QbzIcon {
                                anchors.centerIn: parent
                                width: 32
                                height: 32
                                name: "disc-3"
                                tintName: "muted"
                                visible: !cover.visible
                            }
                            Rectangle {
                                anchors.fill: parent
                                color: theme.surfaceMainA50
                                visible: QbzTagEditor.artworkSearching || QbzTagEditor.artworkLoading
                                QbzSpinner {
                                    anchors.centerIn: parent
                                    size: 24
                                }
                            }
                        }
                        Column {
                            width: parent.width - 106
                            spacing: 7
                            Text {
                                width: parent.width
                                text: workspace.editor.tr("Album artwork")
                                color: theme.textPrimary
                                font.pixelSize: theme.fontBody
                                font.weight: theme.weightSemibold
                                elide: Text.ElideRight
                            }
                            Row {
                                spacing: 6
                                SettingsButton {
                                    text: workspace.editor.artwork.previewPath
                                        ? workspace.editor.tr("Replace file")
                                        : workspace.editor.tr("Choose file")
                                    iconName: "image-plus"
                                    minWidth: 0
                                    enabled: !QbzTagEditor.artworkLoading
                                    onClicked: QbzTagEditor.chooseArtwork()
                                }
                                QbzIconButton {
                                    visible: (workspace.editor.artwork.token || "") !== ""
                                    name: "rotate-ccw"
                                    tooltipText: workspace.editor.tr("Revert selection")
                                    btnEnabled: !QbzTagEditor.artworkLoading
                                    onClicked: QbzTagEditor.clearArtwork()
                                }
                            }
                            SettingsButton {
                                text: workspace.editor.tr("Find metadata and artwork")
                                iconName: "search"
                                minWidth: 0
                                onClicked: workspace.lookupOpen = true
                            }
                        }
                    }

                    component Field: Column {
                        property string label: ""
                        property string value: ""
                        signal edited(string value)
                        width: tagContent.width
                        spacing: 3
                        Text {
                            width: parent.width
                            text: parent.label
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            elide: Text.ElideRight
                        }
                        QbzLineEdit {
                            width: parent.width
                            text: parent.value
                            onEdited: function(value) { parent.edited(value) }
                            onCommitted: function(value) { parent.edited(value) }
                        }
                    }

                    Field {
                        label: workspace.editor.tr("Album title")
                        value: workspace.editor.albumTitle
                        onEdited: function(value) { workspace.editor.albumTitle = value }
                    }
                    Field {
                        label: workspace.editor.tr("Album artists (ordered; separate with semicolons)")
                        value: workspace.editor.albumArtistsText
                        onEdited: function(value) { workspace.editor.albumArtistsText = value }
                    }
                    Field {
                        label: workspace.editor.tr("Album artist")
                        value: workspace.editor.albumArtist
                        onEdited: function(value) { workspace.editor.albumArtist = value }
                    }
                    Row {
                        width: parent.width
                        spacing: 7
                        QbzToggle {
                            anchors.verticalCenter: parent.verticalCenter
                            checked: workspace.editor.compilation
                            onToggled: function(value) { workspace.editor.compilation = value }
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            width: parent.width - 40
                            text: workspace.editor.tr("Compilation / Various Artists")
                            color: theme.textSecondary
                            font.pixelSize: theme.fontBody
                            elide: Text.ElideRight
                        }
                    }
                    Field {
                        label: workspace.editor.tr("Genre")
                        value: workspace.editor.genre
                        onEdited: function(value) { workspace.editor.genre = value }
                    }
                    Row {
                        width: parent.width
                        spacing: 8
                        Field {
                            width: (parent.width - 8) * 0.34
                            label: workspace.editor.tr("Year")
                            value: workspace.editor.year
                            onEdited: function(value) { workspace.editor.year = value }
                        }
                        Field {
                            width: (parent.width - 8) * 0.66
                            label: workspace.editor.tr("Catalog number")
                            value: workspace.editor.catalogNumber
                            onEdited: function(value) { workspace.editor.catalogNumber = value }
                        }
                    }

                    Rectangle {
                        width: parent.width
                        height: 34
                        radius: theme.radiusSm
                        color: advancedMouse.containsMouse ? theme.surfaceHover : theme.alphaTier(4)
                        Text {
                            anchors.left: parent.left
                            anchors.leftMargin: 9
                            anchors.verticalCenter: parent.verticalCenter
                            text: workspace.editor.tr("Provider identifiers")
                            color: theme.textSecondary
                            font.pixelSize: theme.fontBody
                        }
                        QbzIcon {
                            anchors.right: parent.right
                            anchors.rightMargin: 9
                            anchors.verticalCenter: parent.verticalCenter
                            width: 13
                            height: 13
                            name: workspace.advancedOpen ? "chevron-up" : "chevron-down"
                            tintName: "muted"
                        }
                        MouseArea {
                            id: advancedMouse
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: workspace.advancedOpen = !workspace.advancedOpen
                        }
                    }
                    Column {
                        visible: workspace.advancedOpen
                        width: parent.width
                        height: visible ? implicitHeight : 0
                        spacing: 8
                        Field {
                            label: workspace.editor.tr("MusicBrainz release ID")
                            value: workspace.editor.musicbrainzReleaseId
                            onEdited: function(value) { workspace.editor.musicbrainzReleaseId = value }
                        }
                        Field {
                            label: workspace.editor.tr("MusicBrainz release-group ID")
                            value: workspace.editor.musicbrainzReleaseGroupId
                            onEdited: function(value) { workspace.editor.musicbrainzReleaseGroupId = value }
                        }
                        Field {
                            label: workspace.editor.tr("MusicBrainz album artist IDs")
                            value: workspace.editor.musicbrainzAlbumArtistIdsText
                            onEdited: function(value) { workspace.editor.musicbrainzAlbumArtistIdsText = value }
                        }
                        Field {
                            label: workspace.editor.tr("Discogs release ID")
                            value: workspace.editor.discogsReleaseId
                            onEdited: function(value) { workspace.editor.discogsReleaseId = value }
                        }
                    }

                    Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
                    Text {
                        text: workspace.editor.tr("Tag layers")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }
                    Text {
                        width: parent.width
                        text: workspace.editor.inspection.error
                            ? workspace.editor.inspection.error
                            : workspace.editor.tr("Canonical") + ": "
                                + workspace.editor.layerSummary(workspace.editor.inspection.canonicalLayers || [])
                        color: workspace.editor.inspection.error ? theme.danger : theme.textSecondary
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    Text {
                        width: parent.width
                        text: workspace.editor.tr("Detected") + ": "
                            + workspace.editor.layerSummary(workspace.editor.inspection.presentLayers || [])
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                    Rectangle {
                        visible: (workspace.editor.inspection.conflictingFiles || 0) > 0
                        width: parent.width
                        height: visible ? conflictText.implicitHeight + 16 : 0
                        radius: theme.radiusSm
                        color: theme.warningBg
                        Text {
                            id: conflictText
                            anchors.fill: parent
                            anchors.margins: 8
                            text: workspace.editor.tr("Some files contain conflicting tag layers. The canonical layer wins unless synchronization is enabled.")
                            color: theme.textPrimary
                            font.pixelSize: theme.fontLegal
                            wrapMode: Text.WordWrap
                        }
                    }
                }
                QbzScrollBar {
                    target: tagScroll
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                }
            }
        }
    }

    Rectangle {
        id: lookupShield
        anchors.fill: parent
        visible: workspace.lookupOpen
        z: 100
        color: theme.cardShadow
        MouseArea {
            anchors.fill: parent
            onClicked: workspace.lookupOpen = false
        }

        Rectangle {
            id: lookupCard
            anchors.centerIn: parent
            width: Math.min(960, parent.width - 40)
            height: Math.min(640, parent.height - 32)
            radius: theme.radiusLg
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderStrong
            clip: true
            MouseArea { anchors.fill: parent }

            Item {
                id: lookupHeader
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                height: 52
                Text {
                    anchors.left: parent.left
                    anchors.leftMargin: 16
                    anchors.verticalCenter: parent.verticalCenter
                    text: workspace.editor.tr("Find metadata and artwork")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                }
                QbzIconButton {
                    anchors.right: parent.right
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    name: "x"
                    tooltipText: workspace.editor.tr("Close")
                    onClicked: workspace.lookupOpen = false
                }
            }
            Rectangle {
                anchors.top: lookupHeader.bottom
                width: parent.width
                height: 1
                color: theme.borderSubtle
            }

            Row {
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: lookupHeader.bottom
                anchors.bottom: parent.bottom
                anchors.margins: 14
                anchors.topMargin: 15
                spacing: 14

                Column {
                    id: metadataLookup
                    width: (parent.width - 14) / 2
                    height: parent.height
                    spacing: 9
                    Text {
                        text: workspace.editor.tr("Release metadata")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }
                    Row {
                        width: parent.width
                        spacing: 7
                        QbzSelect {
                            width: 150
                            menuWidth: 150
                            options: ["MusicBrainz", "Discogs"]
                            currentIndex: workspace.editor.remoteProvider === "discogs" ? 1 : 0
                            enabled: !QbzTagEditor.remoteSearching && !QbzTagEditor.remoteLoading
                            onSelected: function(index) {
                                workspace.editor.remoteProvider = index === 1 ? "discogs" : "musicbrainz"
                            }
                        }
                        SettingsButton {
                            text: workspace.editor.tr("Search")
                            iconName: "search"
                            minWidth: 0
                            enabled: !QbzTagEditor.remoteSearching && !QbzTagEditor.remoteLoading
                            onClicked: QbzTagEditor.searchRemote(
                                workspace.editor.remoteProvider,
                                workspace.editor.albumTitle,
                                workspace.editor.albumArtist)
                        }
                        QbzSpinner {
                            anchors.verticalCenter: parent.verticalCenter
                            size: 20
                            visible: QbzTagEditor.remoteSearching || QbzTagEditor.remoteLoading
                        }
                    }
                    Rectangle {
                        id: metadataHeader
                        width: parent.width
                        height: 28
                        color: theme.alphaTier(8)
                        border.width: 1
                        border.color: theme.borderMuted
                        Row {
                            anchors.fill: parent
                            component HeaderCell: Text {
                                height: metadataHeader.height
                                leftPadding: 7
                                rightPadding: 5
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightSemibold
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                            HeaderCell { width: parent.width * 0.43; text: workspace.editor.tr("Title") }
                            HeaderCell { width: parent.width * 0.34; text: workspace.editor.tr("Artist") }
                            HeaderCell { width: parent.width * 0.12; text: workspace.editor.tr("Year") }
                            HeaderCell { width: parent.width * 0.11; text: workspace.editor.tr("Tracks") }
                        }
                    }
                    ListView {
                        id: metadataResults
                        width: parent.width
                        height: parent.height - 160
                        clip: true
                        spacing: 0
                        model: workspace.editor.remoteResults
                        delegate: Rectangle {
                            id: metadataResult
                            required property var modelData
                            required property int index
                            width: metadataResults.width
                            height: 32
                            radius: 0
                            color: metadataMouse.containsMouse ? theme.surfaceHover
                                : metadataResult.index % 2 ? theme.alphaTier(4) : "transparent"
                            Row {
                                anchors.fill: parent
                                Text {
                                    width: parent.width * 0.43
                                    height: parent.height
                                    leftPadding: 7
                                    rightPadding: 5
                                    text: metadataResult.modelData.title || ""
                                    color: theme.textPrimary
                                    font.pixelSize: theme.fontBody
                                    verticalAlignment: Text.AlignVCenter
                                    elide: Text.ElideRight
                                }
                                Text {
                                    width: parent.width * 0.34
                                    height: parent.height
                                    leftPadding: 7
                                    rightPadding: 5
                                    text: metadataResult.modelData.artist || ""
                                    color: theme.textSecondary
                                    font.pixelSize: theme.fontLegal
                                    verticalAlignment: Text.AlignVCenter
                                    elide: Text.ElideRight
                                }
                                Text {
                                    width: parent.width * 0.12
                                    height: parent.height
                                    leftPadding: 7
                                    text: metadataResult.modelData.year || ""
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                    verticalAlignment: Text.AlignVCenter
                                }
                                Text {
                                    width: parent.width * 0.11
                                    height: parent.height
                                    leftPadding: 7
                                    text: metadataResult.modelData.track_count || ""
                                    color: theme.textMuted
                                    font.pixelSize: theme.fontLegal
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }
                            Rectangle {
                                anchors.bottom: parent.bottom
                                width: parent.width
                                height: 1
                                color: theme.borderSubtle
                            }
                            MouseArea {
                                id: metadataMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                enabled: !QbzTagEditor.remoteLoading
                                onClicked: QbzTagEditor.loadRemote(
                                    workspace.editor.remoteProvider,
                                    metadataResult.modelData.provider_id)
                            }
                        }
                    }
                    Row {
                        visible: workspace.editor.remoteMetadata !== null
                        spacing: 7
                        SettingsButton {
                            text: workspace.editor.tr("Apply to form")
                            iconName: "cloud-download"
                            minWidth: 0
                            onClicked: workspace.editor.applyRemote()
                        }
                        SettingsButton {
                            text: workspace.editor.tr("Open source")
                            iconName: "external-link"
                            minWidth: 0
                            onClicked: QbzTagEditor.openRemote(
                                String(workspace.editor.remoteMetadata.provider
                                    || workspace.editor.remoteProvider),
                                workspace.editor.remoteMetadata.provider_id || "")
                        }
                    }
                }

                Rectangle { width: 1; height: parent.height; color: theme.borderSubtle }

                Column {
                    id: artworkLookup
                    width: (parent.width - 15) / 2
                    height: parent.height
                    spacing: 9
                    Text {
                        text: workspace.editor.tr("Album artwork")
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }
                    Row {
                        width: parent.width
                        spacing: 7
                        QbzSelect {
                            width: 150
                            menuWidth: 150
                            options: ["MusicBrainz", "Discogs", "Last.fm"]
                            currentIndex: workspace.editor.artworkProvider === "discogs" ? 1
                                : workspace.editor.artworkProvider === "lastfm" ? 2 : 0
                            enabled: !QbzTagEditor.artworkSearching && !QbzTagEditor.artworkLoading
                            onSelected: function(index) {
                                workspace.editor.artworkProvider = index === 1 ? "discogs"
                                    : index === 2 ? "lastfm" : "musicbrainz"
                            }
                        }
                        SettingsButton {
                            text: workspace.editor.tr("Search")
                            iconName: "search"
                            minWidth: 0
                            enabled: !QbzTagEditor.artworkSearching && !QbzTagEditor.artworkLoading
                            onClicked: QbzTagEditor.searchArtwork(
                                workspace.editor.artworkProvider,
                                workspace.editor.albumTitle,
                                workspace.editor.albumArtist,
                                workspace.editor.catalogNumber)
                        }
                        QbzSpinner {
                            anchors.verticalCenter: parent.verticalCenter
                            size: 20
                            visible: QbzTagEditor.artworkSearching || QbzTagEditor.artworkLoading
                        }
                    }
                    GridView {
                        id: artworkResults
                        width: parent.width
                        height: parent.height - 90
                        clip: true
                        cellWidth: 108
                        cellHeight: 126
                        model: workspace.editor.artworkResults
                        delegate: Rectangle {
                            id: artworkResult
                            required property var modelData
                            width: 102
                            height: 120
                            radius: 0
                            color: artworkMouse.containsMouse ? theme.surfaceHover : "transparent"
                            border.width: 1
                            border.color: theme.borderSubtle
                            Image {
                                anchors.top: parent.top
                                anchors.horizontalCenter: parent.horizontalCenter
                                anchors.topMargin: 3
                                width: 94
                                height: 94
                                source: artworkResult.modelData.previewUrl || ""
                                fillMode: Image.PreserveAspectCrop
                                asynchronous: true
                            }
                            Text {
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.bottom: parent.bottom
                                anchors.margins: 4
                                height: 18
                                text: artworkResult.modelData.source || artworkResult.modelData.title || ""
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                                elide: Text.ElideRight
                            }
                            MouseArea {
                                id: artworkMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                enabled: !QbzTagEditor.artworkLoading
                                onClicked: QbzTagEditor.selectArtwork(
                                    artworkResult.modelData.id || "")
                            }
                        }
                        QbzSpinner {
                            anchors.centerIn: parent
                            size: 26
                            visible: QbzTagEditor.artworkSearching || QbzTagEditor.artworkLoading
                        }
                    }
                }
            }
        }
    }
}
