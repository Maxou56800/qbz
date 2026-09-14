// "This is the wrong record" — looking a disc up by hand, for a CD or a SACD.
//
// The automatic naming is one guess from one provider, and a DiscID names the
// GEOMETRY rather than the pressing: the owner's Fear Inoculum answers with
// four releases that share a table of contents. So the list shows the fields
// that TELL THEM APART — year, country, label, catalogue number, format and
// the track count next to the disc's own — rather than a row of titles that
// all read the same.
//
// Selecting is deliberately not applying: picking a row fetches it in full and
// shows its track list, because that list is the only thing that proves a
// plausible title is the right pressing.
//
// Mounted ONCE in AppShell, like TrackReplacementModal: applying RENAMES the
// session underneath, so a modal parented into the pane would be torn down by
// its own success.
//
// Data: QbzDiscMeta.metaJson (disc_meta_qt.rs). The only local state is the
// query being typed.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    visible: doc.open === true

    readonly property var doc: {
        try { return JSON.parse(QbzDiscMeta.metaJson) } catch (e) { return ({}) }
    }
    readonly property var rows: doc.results || []
    readonly property var preview: doc.preview || null
    readonly property bool busy: doc.searching === true || doc.applying === true

    property string queryText: ""
    onVisibleChanged: if (visible) queryText = root.doc.query || ""

    function tr(s) { return QbzSession.tr(s, QbzSession.trRev) }

    function submit() {
        var q = root.queryText.trim()
        if (q !== "" && !root.busy)
            QbzDiscMeta.search(q)
    }

    QbzTheme { id: theme }

    Rectangle {
        anchors.fill: parent
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: if (doc.applying !== true) QbzDiscMeta.close()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 720)
        height: Math.min(parent.height * 0.9, 640)
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

        // ---- Header ---------------------------------------------------
        Item {
            id: header
            width: parent.width
            height: 72
            Column {
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.right: closeX.left
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                spacing: 3
                Text {
                    width: parent.width
                    text: root.tr("Correct disc details")
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    // Says WHICH medium, because a disc image and the disc in
                    // the drive reach this modal from the same button.
                    text: (root.doc.kind === "sacd"
                            ? root.tr("SACD image") : root.tr("Audio CD"))
                          + " · " + (root.doc.discTrackCount || 0) + " "
                          + root.tr("tracks")
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }
            Item {
                id: closeX
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                width: 28
                height: 28
                QbzIcon {
                    anchors.centerIn: parent
                    name: "x"
                    width: 17
                    height: 17
                    tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: closeArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    enabled: root.doc.applying !== true
                    onClicked: QbzDiscMeta.close()
                    onContainsMouseChanged: tips.hover(containsMouse, closeX,
                        "meta-close", root.tr("Close without changing anything"))
                }
            }
        }
        Rectangle { y: header.height; width: parent.width; height: 1; color: theme.borderSubtle }

        // ---- Provider + query ------------------------------------------
        Item {
            id: bar
            y: header.height + 1
            width: parent.width
            height: 67

            Row {
                id: providers
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                spacing: 8
                Repeater {
                    model: [
                        { "id": "musicbrainz", "label": "MusicBrainz",
                          "tip": root.tr("Community database. Knows discs by their table of contents.") },
                        { "id": "discogs", "label": "Discogs",
                          "tip": root.tr("Collector database. Stronger on pressings, editions and catalogue numbers.") }
                    ]
                    Rectangle {
                        required property var modelData
                        readonly property bool active: root.doc.provider === modelData.id
                        width: pLabel.width + 24
                        height: 30
                        radius: 6
                        color: active ? theme.accent
                             : (pArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated)
                        Text {
                            id: pLabel
                            anchors.centerIn: parent
                            text: parent.modelData.label
                            color: parent.active ? theme.onAccent : theme.textSecondary
                            font.pixelSize: 13
                        }
                        MouseArea {
                            id: pArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            enabled: !root.busy
                            onClicked: QbzDiscMeta.setProvider(parent.modelData.id)
                            onContainsMouseChanged: tips.hover(containsMouse, parent,
                                "prov-" + parent.modelData.id, parent.modelData.tip)
                        }
                    }
                }
            }

            Rectangle {
                anchors.left: providers.right
                anchors.leftMargin: 12
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                height: 34
                radius: theme.radiusSm
                color: theme.surfaceCard
                border.width: 1
                border.color: queryInput.activeFocus ? theme.accent : theme.borderSubtle

                TextInput {

                    QbzTextEditMenu { }
                    id: queryInput
                    anchors.fill: parent
                    anchors.leftMargin: 12
                    anchors.rightMargin: 40
                    color: theme.textPrimary
                    font.pixelSize: theme.fontLink
                    verticalAlignment: Text.AlignVCenter
                    clip: true
                    selectByMouse: true
                    enabled: !root.busy
                    text: root.queryText
                    onTextEdited: root.queryText = text
                    onAccepted: root.submit()
                    Binding {
                        target: queryInput
                        property: "text"
                        value: root.queryText
                        when: !queryInput.activeFocus
                    }
                }
                Item {
                    id: goBtn
                    width: 28
                    height: 28
                    anchors.right: parent.right
                    anchors.rightMargin: 4
                    anchors.verticalCenter: parent.verticalCenter
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "search"
                        width: 15
                        height: 15
                        visible: root.doc.searching !== true
                        tintName: goArea.containsMouse ? "textPrimary" : "muted"
                    }
                    QbzSpinner {
                        anchors.centerIn: parent
                        size: 15
                        visible: root.doc.searching === true
                    }
                    MouseArea {
                        id: goArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        enabled: !root.busy
                        onClicked: root.submit()
                        onContainsMouseChanged: tips.hover(containsMouse, goBtn,
                            "meta-go", root.tr("Search this provider"))
                    }
                }
            }
        }
        Rectangle { y: header.height + bar.height + 1; width: parent.width; height: 1; color: theme.borderSubtle }

        // ---- Results (left) + preview (right) --------------------------
        Item {
            y: header.height + bar.height + 2
            width: parent.width
            height: card.height - header.height - bar.height - 2 - 66

            // Empty states. Three of them, because "nothing yet", "nothing
            // found" and "the provider refused" are three different things and
            // only one of them means the disc is unknown.
            Text {
                anchors.centerIn: parent
                width: parent.width - 80
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
                visible: root.rows.length === 0 && root.doc.searching !== true
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                text: root.doc.rateLimited === true
                    ? root.tr("The provider is rate-limiting right now. Try again in a moment.")
                    : (root.doc.searched === true
                        ? root.tr("No releases matched. Try fewer words, or the other provider.")
                        : root.tr("Search for the release this disc actually is."))
            }

            ListView {
                id: list
                width: root.preview ? parent.width * 0.52 : parent.width
                height: parent.height
                clip: true
                model: root.rows
                boundsBehavior: Flickable.StopAtBounds
                delegate: Rectangle {
                    id: row
                    required property var modelData
                    readonly property bool chosen: root.doc.selectedId === modelData.id
                    width: list.width
                    height: 62
                    color: chosen ? theme.surfaceHover
                         : (rowArea.containsMouse ? theme.surfaceElevatedA50 : "transparent")

                    Column {
                        anchors.left: parent.left
                        anchors.leftMargin: 24
                        anchors.right: rowSpin.left
                        anchors.rightMargin: 8
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 2
                        Text {
                            width: parent.width
                            text: row.modelData.title
                            color: theme.textPrimary
                            font.pixelSize: theme.fontLink
                            elide: Text.ElideRight
                        }
                        Text {
                            width: parent.width
                            text: row.modelData.artist
                            color: theme.textSecondary
                            font.pixelSize: theme.fontLegal
                            elide: Text.ElideRight
                        }
                        // The line that actually distinguishes four pressings
                        // of one album. A track count that disagrees with the
                        // disc is called out rather than merely shown.
                        Text {
                            width: parent.width
                            text: {
                                var bits = []
                                if (row.modelData.year) bits.push(row.modelData.year)
                                if (row.modelData.country) bits.push(row.modelData.country)
                                if (row.modelData.format) bits.push(row.modelData.format)
                                if (row.modelData.label) bits.push(row.modelData.label)
                                if (row.modelData.catalogNumber) bits.push(row.modelData.catalogNumber)
                                if (row.modelData.trackCount > 0) {
                                    var n = row.modelData.trackCount
                                    bits.push(n === (root.doc.discTrackCount || 0)
                                        ? n + " " + root.tr("tracks")
                                        : n + " " + root.tr("tracks") + " ⚠")
                                }
                                return bits.join(" · ")
                            }
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                            elide: Text.ElideRight
                        }
                    }
                    QbzSpinner {
                        id: rowSpin
                        anchors.right: parent.right
                        anchors.rightMargin: 24
                        anchors.verticalCenter: parent.verticalCenter
                        size: 15
                        visible: root.doc.loadingId === row.modelData.id
                    }
                    MouseArea {
                        id: rowArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        enabled: root.doc.applying !== true
                        onClicked: QbzDiscMeta.select(row.modelData.id)
                    }
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width
                        height: 1
                        color: theme.borderSubtle
                    }
                }
            }

            Rectangle {
                visible: root.preview !== null
                x: list.width
                width: 1
                height: parent.height
                color: theme.borderSubtle
            }

            // The track list of the chosen candidate — the evidence.
            Flickable {
                visible: root.preview !== null
                x: list.width + 1
                width: parent.width - list.width - 1
                height: parent.height
                contentWidth: width
                contentHeight: prevCol.height + 24
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                Column {
                    id: prevCol
                    x: 16
                    y: 12
                    width: parent.width - 32
                    spacing: 6
                    Text {
                        width: parent.width
                        text: root.preview ? root.preview.title : ""
                        color: theme.textPrimary
                        font.pixelSize: theme.fontLink
                        font.weight: theme.weightSemibold
                        elide: Text.ElideRight
                    }
                    Text {
                        width: parent.width
                        text: root.preview
                            ? (root.preview.artist
                               + (root.preview.year ? " · " + root.preview.year : ""))
                            : ""
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        elide: Text.ElideRight
                    }
                    Item { width: 1; height: 4 }
                    Repeater {
                        model: root.preview ? root.preview.tracks : []
                        Row {
                            required property var modelData
                            width: prevCol.width
                            spacing: 8
                            Text {
                                width: 22
                                text: parent.modelData.number
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                horizontalAlignment: Text.AlignRight
                            }
                            Text {
                                width: parent.width - 30
                                text: parent.modelData.title
                                color: theme.textSecondary
                                font.pixelSize: theme.fontLegal
                                elide: Text.ElideRight
                            }
                        }
                    }
                }
            }
        }

        // ---- Footer ----------------------------------------------------
        Rectangle {
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: footerRow.top
            anchors.bottomMargin: 16
            height: 1
            color: theme.borderSubtle
        }
        // The escape hatch, only where it can do something. Without it a bad
        // correction is permanent, which would make people afraid to use the
        // button at all.
        Item {
            id: forgetBtn
            visible: root.doc.hasCorrection === true
            anchors.left: parent.left
            anchors.leftMargin: 24
            anchors.verticalCenter: footerRow.verticalCenter
            width: forgetText.width
            height: 28
            Text {
                id: forgetText
                anchors.verticalCenter: parent.verticalCenter
                text: root.tr("Remove my correction")
                color: forgetArea.containsMouse ? theme.textPrimary : theme.textMuted
                font.pixelSize: theme.fontLegal
            }
            MouseArea {
                id: forgetArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: QbzDiscMeta.forget()
                onContainsMouseChanged: tips.hover(containsMouse, forgetBtn, "meta-forget",
                    root.tr("Forget the saved details for this disc and look it up fresh next time"))
            }
        }
        Row {
            id: footerRow
            anchors.right: parent.right
            anchors.rightMargin: 24
            anchors.bottom: parent.bottom
            anchors.bottomMargin: 16
            spacing: 12

            SettingsButton {
                id: cancelBtn
                anchors.verticalCenter: parent.verticalCenter
                text: root.tr("Cancel")
                btnHeight: 34
                minWidth: 0
                enabled: root.doc.applying !== true
                onClicked: QbzDiscMeta.close()
                HoverHandler {
                    onHoveredChanged: tips.hover(hovered, cancelBtn, "meta-cancel",
                        root.tr("Close without changing anything"))
                }
            }
            QbzPrimaryButton {
                id: applyBtn
                anchors.verticalCenter: parent.verticalCenter
                btnHeight: 34
                label: root.doc.applying === true
                    ? root.tr("Applying...") : root.tr("Use these details")
                btnEnabled: root.doc.applying !== true && root.preview !== null
                onClicked: QbzDiscMeta.apply()
                // A HoverHandler rather than a MouseArea: this control already
                // has one, and stacking a second would eat the click.
                HoverHandler {
                    onHoveredChanged: tips.hover(hovered, applyBtn, "meta-apply",
                        root.tr("Rename this disc and remember it for next time"))
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
