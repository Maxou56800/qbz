import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

FocusScope {
    id: root
    property bool kioskHost: false
    anchors.fill: parent
    z: 3000
    readonly property var doc: JSON.parse(QbzAbout.updatesJson)
    readonly property bool installing: doc.phase === "installing"
    visible: doc.open === true
    enabled: visible
    QbzTheme { id: theme }
    onVisibleChanged: if (visible) root.forceActiveFocus()
    Keys.onEscapePressed: root.dismiss()
    function dismiss() { if (!installing) QbzAbout.updatesClose() }
    function statusText() {
        switch (doc.phase) {
        case "checking": return QbzSession.tr("Checking for updates...", QbzSession.trRev)
        case "current": return QbzSession.tr("You are using the latest version of QBZ.", QbzSession.trRev)
        case "available": return QbzSession.tr("A new version of QBZ is available.", QbzSession.trRev)
        case "downloading": return QbzSession.tr("Downloading update...", QbzSession.trRev)
        case "verifying": return QbzSession.tr("Verifying update signature...", QbzSession.trRev)
        case "installing": return QbzSession.tr("Installing update...", QbzSession.trRev)
        case "updating": return QbzSession.tr("Installing update...", QbzSession.trRev)
        case "installed": return QbzSession.tr("Update installed. Close and reopen QBZ to use the new version.", QbzSession.trRev)
        case "prepared": return QbzSession.tr("Update ready. Close QBZ to finish installation. It will reopen automatically.", QbzSession.trRev)
        case "cancelled": return QbzSession.tr("Update cancelled.", QbzSession.trRev)
        case "error": return QbzSession.tr("The update could not be completed. Please try again.", QbzSession.trRev)
        default: return ""
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.75)
        MouseArea {
            anchors.fill: parent
            onClicked: root.dismiss()
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }
    Rectangle {
        id: panel
        anchors.centerIn: parent
        width: Math.min(root.width - 40, 640)
        height: Math.min(root.height - 40, content.implicitHeight + 40)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        MouseArea { anchors.fill: parent; onWheel: function (wheel) { wheel.accepted = true } }
        Flickable {
            anchors.fill: parent
            anchors.margins: 20
            contentHeight: content.implicitHeight
            clip: true
            Column {
                id: content
                width: parent.width
                spacing: 16
                Text {
                    width: parent.width
                    text: QbzSession.tr("Updates", QbzSession.trRev)
                    font.pixelSize: theme.fontTitle
                    font.weight: theme.weightBold
                    color: theme.textPrimary
                }
                Text {
                    width: parent.width
                    text: root.statusText()
                    wrapMode: Text.WordWrap
                    font.pixelSize: theme.fontBody
                    color: theme.textPrimary
                }
                Text {
                    width: parent.width
                    visible: (root.doc.version || "") !== ""
                    text: (root.doc.currentVersion || "") + " → " + (root.doc.version || "")
                    font.pixelSize: theme.fontBody
                    color: theme.textSecondary
                }
                Text {
                    width: parent.width
                    visible: root.doc.phase === "available" && !root.doc.canInstall
                    text: QbzSession.tr("Update QBZ through your installation source. Packages may become available after the GitHub release.", QbzSession.trRev)
                    wrapMode: Text.WordWrap
                    font.pixelSize: theme.fontBody
                    color: theme.textMuted
                }
                Text {
                    width: parent.width
                    visible: root.doc.phase === "downloading"
                    text: Math.floor((root.doc.downloaded || 0) / 1048576) + " MB"
                        + (root.doc.total ? " / " + Math.ceil(root.doc.total / 1048576) + " MB" : "")
                    font.pixelSize: theme.fontBody
                    color: theme.textSecondary
                }
                Text {
                    width: parent.width
                    visible: root.doc.phase === "updating"
                    text: (root.doc.percent || 0) + "%"
                    font.pixelSize: theme.fontBody
                    color: theme.textSecondary
                }
                Text {
                    width: parent.width
                    visible: root.doc.phase === "error"
                    text: root.doc.error || ""
                    textFormat: Text.PlainText
                    wrapMode: Text.WrapAnywhere
                    font.pixelSize: theme.fontBody
                    color: theme.textMuted
                }
                Flow {
                    width: parent.width
                    layoutDirection: Qt.RightToLeft
                    spacing: 10
                    SettingsButton {
                        kioskHost: root.kioskHost
                        minWidth: 0
                        btnHeight: 36
                        objectName: "updateInstallButton"
                        visible: root.doc.canInstall === true && !root.doc.busy
                        text: QbzSession.tr("Download and install", QbzSession.trRev)
                        onClicked: QbzAbout.updatesInstall()
                    }
                    SettingsButton {
                        kioskHost: root.kioskHost
                        minWidth: 0
                        btnHeight: 36
                        objectName: "updateReleaseButton"
                        visible: !!root.doc.version && !root.doc.busy && root.doc.phase !== "installed"
                        text: QbzSession.tr("Visit release page", QbzSession.trRev)
                        onClicked: QbzShell.openExternalUrl(root.doc.releaseUrl || "")
                    }
                    SettingsButton {
                        kioskHost: root.kioskHost
                        minWidth: 0
                        btnHeight: 36
                        visible: root.doc.phase === "downloading" || root.doc.phase === "verifying" || root.doc.phase === "updating"
                        objectName: "updateCancelButton"
                        text: root.doc.phase === "updating" ? QbzSession.tr("Cancel", QbzSession.trRev) : QbzSession.tr("Cancel download", QbzSession.trRev)
                        onClicked: QbzAbout.updatesCancel()
                    }
                    SettingsButton {
                        kioskHost: root.kioskHost
                        minWidth: 0
                        btnHeight: 36
                        objectName: "updateCloseButton"
                        enabled: !root.installing
                        text: QbzSession.tr("Close", QbzSession.trRev)
                        onClicked: root.dismiss()
                    }
                    SettingsButton {
                        kioskHost: root.kioskHost
                        minWidth: 0
                        btnHeight: 36
                        visible: root.doc.phase === "available"
                        text: QbzSession.tr("Skip this version", QbzSession.trRev)
                        onClicked: QbzAbout.updatesIgnore()
                    }
                }
            }
        }
    }
}
