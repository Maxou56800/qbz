// Physical media facts for Local Library. This intentionally does not reuse
// the Qobuz Track/Album Info document: those modals describe a catalog record;
// this one describes the playable file/server copy and exposes copyable paths.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
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
    dim: false
    closePolicy: Popup.CloseOnEscape

    QbzTheme { id: theme }

    readonly property var doc: {
        try { return JSON.parse(QbzLocal.localMediaInfoJson || "{}") }
        catch (e) { return ({}) }
    }
    readonly property bool loading: QbzLocal.localMediaInfoLoading

    function sourceName(kind) {
        if (kind === "local") return QbzSession.tr("Local file", QbzSession.trRev)
        if (kind === "network") return QbzSession.tr("Network file", QbzSession.trRev)
        if (kind === "offline") return QbzSession.tr("Offline download", QbzSession.trRev)
        if (kind === "plex") return "Plex"
        if (kind === "jellyfin") return "Jellyfin"
        if (kind === "subsonic") return "Subsonic / Navidrome"
        return kind || ""
    }
    function sourceText() {
        var values = root.doc.sourceKinds || []
        var out = []
        for (var i = 0; i < values.length; i++) out.push(root.sourceName(values[i]))
        return out.join(", ")
    }
    function locationName(kind) {
        if (kind === "file") return QbzSession.tr("File path", QbzSession.trRev)
        if (kind === "folder") return QbzSession.tr("Containing folder", QbzSession.trRev)
        if (kind === "item") return QbzSession.tr("Media item ID", QbzSession.trRev)
        return QbzSession.tr("Album ID", QbzSession.trRev)
    }
    function factRows() {
        var rows = []
        function add(label, value) {
            if (String(value || "") !== "") rows.push({"label": label, "value": value})
        }
        add(QbzSession.tr("Source", QbzSession.trRev), root.sourceText())
        add(QbzSession.tr("Server", QbzSession.trRev), root.doc.server)
        if (root.doc.kind === "album")
            add(QbzSession.tr("Tracks", QbzSession.trRev), root.doc.trackCount)
        add(QbzSession.tr("Duration", QbzSession.trRev), root.doc.duration)
        add(QbzSession.tr("Format", QbzSession.trRev), root.doc.formats)
        add(QbzSession.tr("Quality", QbzSession.trRev), root.doc.quality)
        add(QbzSession.tr("Channels", QbzSession.trRev), root.doc.channels)
        add(QbzSession.tr("File size", QbzSession.trRev), root.doc.fileSize)
        return rows
    }

    Connections {
        target: QbzLocal
        function onLocalMediaInfoOpenChanged() {
            if (QbzLocal.localMediaInfoOpen) root.open()
            else root.close()
        }
    }
    Component.onCompleted: if (QbzLocal.localMediaInfoOpen) open()
    onClosed: if (QbzLocal.localMediaInfoOpen) QbzLocal.closeMediaInfo()

    background: Rectangle { color: "#bf000000" }

    contentItem: Item {
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 40, 760)
            height: Math.min(root.height - 40, 620)
            anchors.centerIn: parent
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            MouseArea { anchors.fill: parent }

            Column {
                id: header
                anchors.left: parent.left
                anchors.right: closeButton.left
                anchors.leftMargin: 24
                anchors.rightMargin: 16
                anchors.top: parent.top
                anchors.topMargin: 20
                spacing: 4
                Text {
                    width: parent.width
                    text: root.doc.kind === "album"
                        ? QbzSession.tr("Album info", QbzSession.trRev)
                        : QbzSession.tr("Track info", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontSection
                    font.weight: theme.weightBold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: root.doc.title || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: root.doc.subtitle || ""
                    visible: text !== ""
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }

            Rectangle {
                id: closeButton
                width: 32
                height: 32
                radius: theme.radiusSm
                anchors.right: parent.right
                anchors.rightMargin: 16
                anchors.top: parent.top
                anchors.topMargin: 16
                color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon { anchors.centerIn: parent; name: "x"; width: 16; height: 16; tintName: "muted" }
                MouseArea {
                    id: closeArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.close()
                }
            }

            QbzSpinner {
                visible: root.loading
                width: 32
                height: 32
                anchors.centerIn: parent
            }

            Text {
                visible: !root.loading && (root.doc.error || "") !== ""
                anchors.centerIn: parent
                width: parent.width - 80
                text: root.doc.error || ""
                color: theme.textMuted
                font.pixelSize: theme.fontBody
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
            }

            Flickable {
                id: flick
                visible: !root.loading && (root.doc.error || "") === ""
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: header.bottom
                anchors.bottom: parent.bottom
                anchors.leftMargin: 24
                anchors.rightMargin: 24
                anchors.topMargin: 20
                anchors.bottomMargin: 20
                clip: true
                contentWidth: width
                contentHeight: body.implicitHeight
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: body
                    width: flick.width - 18
                    spacing: 10

                    Repeater {
                        model: root.factRows()
                        delegate: Item {
                            required property var modelData
                            width: body.width
                            height: Math.max(34, factValue.implicitHeight + 8)
                            Text {
                                width: 142
                                anchors.verticalCenter: parent.verticalCenter
                                text: modelData.label
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                            }
                            TextEdit {
                                QbzTextEditMenu { }
                                id: factValue
                                x: 154
                                width: parent.width - x
                                anchors.verticalCenter: parent.verticalCenter
                                text: String(modelData.value)
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLegal
                                readOnly: true
                                selectByMouse: true
                                wrapMode: TextEdit.Wrap
                            }
                        }
                    }

                    Rectangle {
                        visible: (root.doc.locations || []).length > 0
                        width: parent.width
                        height: 1
                        color: theme.borderSubtle
                    }

                    Text {
                        visible: (root.doc.locations || []).length > 0
                        text: QbzSession.tr("Location", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        font.weight: theme.weightSemibold
                    }

                    Repeater {
                        model: root.doc.locations || []
                        delegate: Rectangle {
                            required property var modelData
                            width: body.width
                            height: 68
                            radius: theme.radiusSm
                            color: theme.surfaceElevated
                            border.width: 1
                            border.color: theme.borderSubtle
                            Text {
                                x: 12
                                y: 8
                                text: root.locationName(modelData.kind)
                                color: theme.textMuted
                                font.pixelSize: theme.fontLegal
                                font.weight: theme.weightSemibold
                            }
                            TextEdit {
                                QbzTextEditMenu { }
                                x: 12
                                y: 28
                                width: parent.width - 92
                                height: 30
                                text: modelData.value || ""
                                color: theme.textPrimary
                                font.pixelSize: theme.fontLegal
                                readOnly: true
                                selectByMouse: true
                                wrapMode: TextEdit.NoWrap
                                clip: true
                            }
                            SettingsButton {
                                anchors.right: parent.right
                                anchors.rightMargin: 10
                                anchors.verticalCenter: parent.verticalCenter
                                text: QbzSession.tr("Copy", QbzSession.trRev)
                                onClicked: QbzLocal.copyMediaInfo(modelData.value || "")
                            }
                        }
                    }
                }
            }

            QbzScrollBar {
                visible: flick.visible && flick.contentHeight > flick.height
                target: flick
                anchors.right: parent.right
                anchors.rightMargin: 4
                anchors.top: flick.top
                anchors.bottom: flick.bottom
            }
        }
    }
}
