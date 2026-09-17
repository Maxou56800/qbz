// Experimental host inspection, constructed only when --orbit is present.
pragma ComponentBehavior: Bound
import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    id: root
    property bool kioskHost: false
    property var doc: ({})
    property string listenAddress: "127.0.0.1:17290"
    property string peerAddress: "http://127.0.0.1:17290"
    property string accessKey: ""
    property string query: ""
    spacing: 4
    QbzTheme { id: theme }

    function reload() {
        try { doc = JSON.parse(QbzOrbit.stateJson) } catch (e) { doc = ({}) }
    }
    Component.onCompleted: reload()
    Connections {
        target: QbzOrbit
        function onStateJsonChanged() { root.reload() }
    }
    function statusText() {
        switch (doc.status || "") {
        case "verified": return QbzSession.tr("Host verified", QbzSession.trRev)
        case "not-supported": return QbzSession.tr("This host does not support library inspection.", QbzSession.trRev)
        case "invalid-address": return QbzSession.tr("Enter a valid host address.", QbzSession.trRev)
        case "listen-failed": return QbzSession.tr("Could not start the Orbit listener.", QbzSession.trRev)
        case "host-changed": return QbzSession.tr("The host changed. Verify it again.", QbzSession.trRev)
        case "host-unavailable": return QbzSession.tr("Library unavailable", QbzSession.trRev)
        case "verify-failed": return QbzSession.tr("Could not verify the host.", QbzSession.trRev)
        default: return ""
        }
    }

    GroupHeader { kioskHost: root.kioskHost; text: "ORBIT" }
    Text {
        width: parent.width
        text: QbzSession.tr("Orbit is experimental. Verifying a host does not switch playback or settings.", QbzSession.trRev)
        wrapMode: Text.WordWrap
        color: theme.textMuted
        font.pixelSize: theme.fontBody
    }
    SettingsSpacer {}
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Listening address", QbzSession.trRev)
        QbzLineEdit {
            kioskHost: root.kioskHost
            enabled: !root.doc.listening && !root.doc.starting
            text: root.listenAddress
            onEdited: function(value) { root.listenAddress = value }
            onCommitted: function(value) { root.listenAddress = value }
        }
    }
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Share this library", QbzSession.trRev)
        description: root.doc.address || ""
        QbzToggle {
            kioskHost: root.kioskHost
            checked: root.doc.listening === true
            enabled: !root.doc.starting
            onToggled: function(value) {
                if (value) QbzOrbit.startHost(root.listenAddress)
                else QbzOrbit.stopHost()
            }
        }
    }
    SettingRow {
        kioskHost: root.kioskHost
        visible: root.doc.listening === true
        label: QbzSession.tr("Access key", QbzSession.trRev)
        SettingsButton {
            kioskHost: root.kioskHost
            text: QbzSession.tr("Copy access key", QbzSession.trRev)
            onClicked: QbzOrbit.copyAccessKey()
        }
    }
    SettingsDivider {}
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Host address", QbzSession.trRev)
        QbzLineEdit {
            kioskHost: root.kioskHost
            text: root.peerAddress
            onEdited: function(value) { root.peerAddress = value }
            onCommitted: function(value) { root.peerAddress = value }
        }
    }
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Access key", QbzSession.trRev)
        QbzLineEdit {
            kioskHost: root.kioskHost
            text: root.accessKey
            isPassword: true
            onEdited: function(value) { root.accessKey = value }
            onCommitted: function(value) { root.accessKey = value }
        }
    }
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Verify host", QbzSession.trRev)
        SettingsButton {
            kioskHost: root.kioskHost
            text: root.doc.probing ? QbzSession.tr("Checking server…", QbzSession.trRev)
                : QbzSession.tr("Verify host", QbzSession.trRev)
            enabled: !root.doc.probing
            onClicked: QbzOrbit.verifyHost(root.peerAddress, root.accessKey)
        }
    }
    Text {
        width: parent.width
        visible: text !== ""
        text: root.statusText()
        wrapMode: Text.WordWrap
        color: root.doc.status === "verified" ? theme.success : theme.danger
        font.pixelSize: theme.fontLegal
    }
    Text {
        width: parent.width
        visible: !!root.doc.peer
        text: root.doc.peer ? root.doc.peer.name + " · " + root.doc.peer.tracks + " " + QbzSession.tr("Tracks", QbzSession.trRev) : ""
        textFormat: Text.PlainText
        wrapMode: Text.WordWrap
        color: theme.textPrimary
        font.pixelSize: theme.fontBody
    }
    SettingRow {
        kioskHost: root.kioskHost
        visible: !!root.doc.peer
        label: QbzSession.tr("Local files", QbzSession.trRev)
        Row {
            spacing: 8
            QbzLineEdit {
                kioskHost: root.kioskHost
                text: root.query
                onEdited: function(value) { root.query = value }
                onCommitted: function(value) { root.query = value }
                onAccepted: QbzOrbit.searchLibrary(root.query)
            }
            SettingsButton {
                kioskHost: root.kioskHost
                text: root.doc.searching ? QbzSession.tr("Searching…", QbzSession.trRev)
                    : QbzSession.tr("Search", QbzSession.trRev)
                enabled: !root.doc.searching && root.query.trim().length >= 2
                onClicked: QbzOrbit.searchLibrary(root.query)
            }
        }
    }
    Repeater {
        model: root.doc.page ? root.doc.page.tracks : []
        delegate: Text {
            required property var modelData
            width: root.width
            text: modelData.title + " · " + modelData.artist + " · " + modelData.album
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: theme.textSecondary
            font.pixelSize: theme.fontBody
            height: 32
            verticalAlignment: Text.AlignVCenter
        }
    }
}
