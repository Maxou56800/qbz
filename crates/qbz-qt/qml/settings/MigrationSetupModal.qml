// Account-migration preparation surface. Selection and copy options live here
// rather than in the Settings page so the user reviews one explicit
// source -> authenticated destination before the long-running operation.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    property var confirmHost: null
    readonly property var ie: doc.importExport || ({})
    readonly property var profiles: ie.snapshots || []
    readonly property var selectedProfile: {
        for (let i = 0; i < profiles.length; i++) {
            if (profiles[i].path === selectedPath)
                return profiles[i]
        }
        return null
    }

    property bool opened: false
    property string selectedPath: ""
    property bool copyLocalProfile: true
    property bool copyMediaServers: true
    property bool copyScrobblers: true
    property bool copyListeningHistory: true

    QbzTheme { id: theme }

    function open() {
        root.selectedPath = ""
        root.copyLocalProfile = true
        root.copyMediaServers = true
        root.copyScrobblers = true
        root.copyListeningHistory = true
        root.opened = true
        Qt.callLater(function () { closeButton.forceActiveFocus() })
    }
    function close() {
        root.opened = false
        root.restoreShellFocus()
    }
    function restoreShellFocus() {
        var item = root
        while (item.parent) {
            if (item.parent.isQbzShellRoot === true) {
                item.parent.forceActiveFocus()
                return
            }
            item = item.parent
        }
    }
    function askDelete(path) {
        if (!root.confirmHost)
            return
        root.confirmHost.ask(
            QbzSession.tr("Delete migration snapshot?", QbzSession.trRev),
            QbzSession.tr("This removes only the migration bundle. The old account's QBZ profile and user data are kept.", QbzSession.trRev),
            QbzSession.tr("Delete snapshot", QbzSession.trRev),
            function () {
                if (root.selectedPath === path)
                    root.selectedPath = ""
                QbzBridge.settingsString("account-delete-snapshot", path)
            })
    }
    function startMigration() {
        if (!root.selectedProfile || root.ie.migrationBusy)
            return
        QbzBridge.settingsString("account-migrate", JSON.stringify({
            path: root.selectedProfile.path,
            local_profile: root.copyLocalProfile,
            media_servers: root.copyMediaServers,
            scrobblers: root.copyScrobblers,
            listening_history: root.copyListeningHistory
        }))
        root.close()
    }

    visible: root.opened
    enabled: root.opened
    z: 3050

    Keys.onEscapePressed: function (event) {
        root.close()
        event.accepted = true
    }

    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: card
        anchors.centerIn: parent
        width: Math.min(parent.width - 80, 760)
        height: Math.min(parent.height - 48, 780)
        radius: theme.radiusLg
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        MouseArea {
            anchors.fill: parent
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Item {
            id: header
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            height: 68
            Text {
                anchors.left: parent.left
                anchors.leftMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("Migrate account data", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? (theme.fontHeading) * 1.2 : (theme.fontHeading)
                font.weight: theme.weightSemibold
            }
        }
        Rectangle {
            id: headerDivider
            anchors.top: header.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: theme.borderSubtle
        }

        Item {
            id: footer
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            height: 70
            Row {
                anchors.right: parent.right
                anchors.rightMargin: 24
                anchors.verticalCenter: parent.verticalCenter
                spacing: 10
                SettingsButton { kioskHost: root.kioskHost;
                    id: closeButton
                    minWidth: 100
                    text: QbzSession.tr("Cancel", QbzSession.trRev)
                    onClicked: root.close()
                }
                SettingsButton { kioskHost: root.kioskHost;
                    minWidth: 160
                    text: QbzSession.tr("Start migration", QbzSession.trRev)
                    enabled: root.selectedProfile !== null && !root.ie.migrationBusy
                    onClicked: root.startMigration()
                }
            }
        }
        Rectangle {
            id: footerDivider
            anchors.bottom: footer.top
            anchors.left: parent.left
            anchors.right: parent.right
            height: 1
            color: theme.borderSubtle
        }

        Flickable {
            id: flick
            anchors.top: headerDivider.bottom
            anchors.bottom: footerDivider.top
            anchors.left: parent.left
            anchors.right: parent.right
            clip: true
            contentWidth: width
            contentHeight: body.implicitHeight + 48
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: body
                x: 24
                y: 24
                width: parent.width - 48
                spacing: 12

                WarningBanner {
                    width: parent.width
                    variant: "info"
                    title: QbzSession.tr("What is a migration profile?", QbzSession.trRev)
                    body: QbzSession.tr("It is a one-time bundle created from the source account. It preserves that account's Qobuz favorites and playlists so they can be added to the account signed in now. Optional local data is copied directly from the old QBZ profile on this computer. Deleting the bundle does not delete the old profile or either account's data.", QbzSession.trRev)
                }

                GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("SAVED SOURCE PROFILES", QbzSession.trRev) }

                Text {
                    visible: root.profiles.length === 0
                    width: parent.width
                    text: QbzSession.tr("No saved migration profiles are available.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                    wrapMode: Text.WordWrap
                }

                Repeater {
                    model: root.profiles
                    delegate: Rectangle {
                        required property var modelData
                        readonly property bool selected:
                            root.selectedPath === modelData.path
                        width: body.width
                        height: 92
                        radius: theme.radiusMd
                        color: selected ? theme.surfaceElevatedHover : theme.surfaceElevated
                        border.width: selected ? 2 : 1
                        border.color: selected ? theme.accent : theme.borderSubtle

                        Column {
                            anchors.left: parent.left
                            anchors.leftMargin: 14
                            anchors.right: actions.left
                            anchors.rightMargin: 16
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 6
                            Text {
                                width: parent.width
                                text: modelData.label
                                color: theme.textPrimary
                                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                                font.weight: theme.weightMedium
                                elide: Text.ElideRight
                            }
                            Text {
                                width: parent.width
                                text: QbzSession.tr("{} favorites · {} playlists · {} followed", QbzSession.trRev)
                                    .replace("{}", modelData.favorites)
                                    .replace("{}", modelData.playlists)
                                    .replace("{}", modelData.subscriptions)
                                    + (modelData.isCurrentAccount
                                        ? " · " + QbzSession.tr("this account", QbzSession.trRev)
                                        : "")
                                color: theme.textMuted
                                font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
                                elide: Text.ElideRight
                            }
                        }
                        Row {
                            id: actions
                            anchors.right: parent.right
                            anchors.rightMargin: 12
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 8
                            SettingsButton { kioskHost: root.kioskHost;
                                minWidth: 92
                                text: parent.parent.selected
                                    ? QbzSession.tr("Selected", QbzSession.trRev)
                                    : QbzSession.tr("Select", QbzSession.trRev)
                                enabled: !modelData.isCurrentAccount
                                    && !root.ie.migrationBusy
                                onClicked: root.selectedPath = modelData.path
                            }
                            SettingsButton { kioskHost: root.kioskHost;
                                minWidth: 92
                                text: QbzSession.tr("Delete…", QbzSession.trRev)
                                danger: true
                                enabled: !root.ie.migrationBusy
                                onClicked: root.askDelete(modelData.path)
                            }
                        }
                    }
                }

                GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("MIGRATION DIRECTION", QbzSession.trRev) }
                Rectangle {
                    width: parent.width
                    height: direction.implicitHeight + 24
                    radius: theme.radiusMd
                    color: theme.surfaceElevated
                    border.width: 1
                    border.color: theme.borderSubtle
                    Column {
                        id: direction
                        x: 12
                        y: 12
                        width: parent.width - 24
                        spacing: 8
                        Text {
                            width: parent.width
                            text: QbzSession.tr("From", QbzSession.trRev) + ": "
                                + (root.selectedProfile
                                    ? root.selectedProfile.sourceIdentity
                                    : QbzSession.tr("Select a source profile above", QbzSession.trRev))
                            color: theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            wrapMode: Text.WordWrap
                        }
                        Text {
                            width: parent.width
                            text: QbzSession.tr("To", QbzSession.trRev) + ": "
                                + QbzSession.tr("Signed-in Qobuz account · ID {}", QbzSession.trRev)
                                    .replace("{}", root.ie.currentUserId || "—")
                            color: theme.textPrimary
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            font.weight: theme.weightSemibold
                            wrapMode: Text.WordWrap
                        }
                    }
                }

                GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("WHAT TO COPY", QbzSession.trRev) }
                SettingRow { kioskHost: root.kioskHost;
                    label: QbzSession.tr("Also copy the local profile", QbzSession.trRev)
                    description: QbzSession.tr("Library folders, playlist folders and order, pinned items, blacklist and preferences of the old account's QBZ profile on this computer.", QbzSession.trRev)
                    QbzToggle { kioskHost: root.kioskHost;
                        checked: root.copyLocalProfile
                        onToggled: function (value) { root.copyLocalProfile = value }
                    }
                }
                SettingRow { kioskHost: root.kioskHost;
                    rowEnabled: root.copyLocalProfile
                    label: QbzSession.tr("Media server connections", QbzSession.trRev)
                    description: QbzSession.tr("Plex, Jellyfin and Subsonic settings, including their credentials.", QbzSession.trRev)
                    QbzToggle { kioskHost: root.kioskHost;
                        checked: root.copyMediaServers
                        enabled: root.copyLocalProfile
                        onToggled: function (value) { root.copyMediaServers = value }
                    }
                }
                SettingRow { kioskHost: root.kioskHost;
                    rowEnabled: root.copyLocalProfile
                    label: QbzSession.tr("Scrobbler accounts", QbzSession.trRev)
                    description: QbzSession.tr("Last.fm and ListenBrainz settings, including their credentials.", QbzSession.trRev)
                    QbzToggle { kioskHost: root.kioskHost;
                        checked: root.copyScrobblers
                        enabled: root.copyLocalProfile
                        onToggled: function (value) { root.copyScrobblers = value }
                    }
                }
                SettingRow { kioskHost: root.kioskHost;
                    rowEnabled: root.copyLocalProfile
                    label: QbzSession.tr("Listening history", QbzSession.trRev)
                    description: QbzSession.tr("The listen log and the events behind offline recommendations.", QbzSession.trRev)
                    QbzToggle { kioskHost: root.kioskHost;
                        checked: root.copyListeningHistory
                        enabled: root.copyLocalProfile
                        onToggled: function (value) { root.copyListeningHistory = value }
                    }
                }

                WarningBanner {
                    width: parent.width
                    variant: "warning"
                    title: QbzSession.tr("This only happens once. Please wait for it to finish and do not close the window.", QbzSession.trRev)
                    body: QbzSession.tr("Migration is additive: it does not remove favorites or playlists from either account.", QbzSession.trRev)
                }
            }
        }
    }
}
