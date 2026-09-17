import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    id: root
    property bool kioskHost: false
    readonly property var doc: JSON.parse(QbzAbout.updatesJson)
    spacing: 4
    QbzTheme { id: theme }

    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Check for new releases on launch", QbzSession.trRev)
        QbzToggle {
            kioskHost: root.kioskHost
            checked: root.doc.checkOnLaunch !== false
            onToggled: function (enabled) { QbzAbout.updatesSetLaunch(enabled) }
        }
    }
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Check for updates now", QbzSession.trRev)
        SettingsButton {
            objectName: "updateCheckButton"
            kioskHost: root.kioskHost
            width: 160
            text: root.doc.busy ? QbzSession.tr("Checking...", QbzSession.trRev) : QbzSession.tr("Check", QbzSession.trRev)
            enabled: root.doc.busy !== true
            onClicked: QbzAbout.updatesCheck()
        }
    }
    Text {
        width: parent.width
        text: QbzSession.tr("Current version", QbzSession.trRev) + ": " + (root.doc.currentVersion || "")
        color: theme.textMuted
        font.pixelSize: theme.fontBody
    }
    Text {
        width: parent.width
        text: QbzSession.tr("Installation", QbzSession.trRev) + ": " + (root.doc.installMethod === "System / manual" ? QbzSession.tr("System / manual", QbzSession.trRev) : (root.doc.installMethod || ""))
        color: theme.textMuted
        font.pixelSize: theme.fontBody
    }
    SettingsSpacer { }
    SettingRow {
        kioskHost: root.kioskHost
        label: QbzSession.tr("Show changelog for current version", QbzSession.trRev)
        SettingsButton {
            kioskHost: root.kioskHost
            width: 160
            text: QbzSession.tr("Show", QbzSession.trRev)
            onClicked: QbzAbout.whatsNewOpen()
        }
    }
}
