// Settings > Local Library > a MEDIA SERVER (Jellyfin or Subsonic).
//
// ONE component, instantiated twice, because it is one form twice: address,
// account, password, connect, library sweep, disconnect. Everything the two
// protocols disagree about is a property set by the caller, so the differences
// are visible in ONE place (`LocalLibrarySettings.qml`) instead of being spread
// across two near-identical files that drift.
//
// Wiring mirrors PlexSettings.qml: the persisted state rides the settings
// document (`settings_qt/library.rs`), the actions go through the QbzLocal
// bridge, and THE CREDENTIAL NEVER LEAVES RUST — the panel gets
// `hasCredential`, never the token or the password.
//
// ── WHAT DIFFERS BETWEEN THE TWO, AND WHY THE USER SEES IT ────────────────
//
// **"Test" means different things.** Jellyfin has an unauthenticated probe
// (`/System/Info/Public`), so its test genuinely checks the ADDRESS before the
// user types a password. Subsonic has no such endpoint — `ping.view` needs
// credentials — so its test can only confirm that something Subsonic-shaped
// answered. `testHint` says which, because a button that claims more than it
// checked is worse than no button.
//
// **The password is kept, or it is not.** Jellyfin issues a token and forgets
// the password. Subsonic has no session and re-derives `md5(password + salt)`
// on every request, so the password is stored. That is the protocol's choice,
// not ours, and `credentialNote` tells the user rather than leaving them to
// wonder what is on disk.
//
// **A sweep costs very different amounts.** 45.8 s for 4924 Jellyfin tracks
// (server-side media-info hydration, the only way to get bit depth) against
// 0.81 s for 6678 Subsonic ones. `syncCost` sets expectations before the click,
// which is the difference between "it is working" and "it is frozen".

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    /// "jellyfin" | "subsonic" — the word every bridge call is keyed on.
    required property string server
    /// Display name in the header ("Jellyfin", "Subsonic").
    required property string title
    /// One line under the title.
    required property string subtitle
    /// Placeholder for the address field.
    required property string urlPlaceholder
    /// What this server's "Test" actually proves. See the header.
    required property string testHint
    /// What is written to disk for this protocol. See the header.
    required property string credentialNote
    /// What a first sweep will cost, in the user's terms.
    required property string syncCost
    /// The slice of the settings document for this server.
    required property var state

    /// The view-level SettingsConfirmHost, handed down by
    /// LocalLibrarySettings. Null in previews; the disconnect falls back to
    /// acting directly.
    property var confirmHost: null

    // In-progress field values. The fields commit on Enter / focus loss; the
    // buttons submit whatever is typed RIGHT NOW.
    property string urlInput: ""
    property string userInput: ""
    property string passInput: ""

    /// What an action would actually send: freshly typed text if there is any,
    /// else the persisted value. Single-sourced so the gate and the click judge
    /// the SAME string — judging the field while sending the fallback is how a
    /// gate ends up decorative (PlexSettings.qml learned this one).
    readonly property string effectiveUrl:
        root.urlInput !== "" ? root.urlInput : (root.state.serverUrl || "")
    readonly property string effectiveUser:
        root.userInput !== "" ? root.userInput : (root.state.username || "")
    readonly property bool canConnect:
        root.effectiveUrl.trim() !== "" && root.effectiveUser.trim() !== "" && root.passInput !== ""

    readonly property var operation: (JSON.parse(QbzLocal.mediaStatus || "{}"))[root.server] || ({})
    readonly property bool busy: operation.busy === true || operation.syncing === true
    readonly property bool pairing: root.server === "jellyfin" && root.busy
        && (operation.phase || "").indexOf("pairing") === 0
    function cancelPairing() {
        if (root.server === "jellyfin") QbzLocal.mediaCancelQuickConnect()
    }
    onVisibleChanged: { if (!visible) cancelPairing() }
    Component.onDestruction: cancelPairing()
    readonly property string statusText: {
        // Each panel observes its own provider, including when both sync at once.
        if (operation.syncing === true)
            return QbzSession.tr("Connected. Syncing library…", QbzSession.trRev)
        switch (operation.phase || "") {
        case "pairing-start": return QbzSession.tr("Requesting Quick Connect code…", QbzSession.trRev)
        case "pairing-waiting": return QbzSession.tr("Waiting for authorization…", QbzSession.trRev)
        case "pairing-verifying": return QbzSession.tr("Checking library access…", QbzSession.trRev)
        case "testing": return QbzSession.tr("Checking server…", QbzSession.trRev)
        case "authenticating": return QbzSession.tr("Signing in…", QbzSession.trRev)
        case "verifying": return QbzSession.tr("Checking library access…", QbzSession.trRev)
        // A saved Jellyfin session stays valid after a Test: "Sign in to
        // connect" read as if the server had been disconnected.
        case "reachable": return root.server === "jellyfin"
            ? (root.state.hasCredential === true
                ? QbzSession.tr("Saved connection. Sync to check access.", QbzSession.trRev)
                : QbzSession.tr("Server reachable. Sign in to connect.", QbzSession.trRev))
            : QbzSession.tr("Server reachable. Credentials have not been checked.", QbzSession.trRev)
        case "syncing": return QbzSession.tr("Connected. Syncing library…", QbzSession.trRev)
        case "ready": return QbzSession.tr("Connected. Library is up to date.", QbzSession.trRev)
        case "connected": return QbzSession.tr("Connected", QbzSession.trRev)
        case "refreshing": return QbzSession.tr("Connected. The server is refreshing its library.", QbzSession.trRev)
        case "failed": case "sync-failed": return operation.error || ""
        default: return root.state.hasCredential === true
            ? QbzSession.tr("Saved connection. Sync to check access.", QbzSession.trRev)
            : QbzSession.tr("Sign in to sync this server's library.", QbzSession.trRev)
        }
    }

    QbzTheme { id: theme }

    width: parent ? parent.width : 0
    spacing: 4

    // ============================== header ================================
    Item {
        width: parent.width
        height: 44

        Column {
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: parent.width - 120
            spacing: 2
            Row {
                spacing: 8
                SourceIcon {
                    anchors.verticalCenter: parent.verticalCenter
                    kind: root.server
                    glyphSize: 18
                    plexSize: 18
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.title
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? (14) * 1.2 : (14)
                    font.weight: Font.DemiBold
                }
                // What the server called itself, once we have talked to it.
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: (root.state.serverName || "") !== ""
                    text: "· " + (root.state.serverName || "")
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
                }
            }
            Text {
                width: parent.width
                text: root.subtitle
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
                wrapMode: Text.WordWrap
            }
        }

        Row {
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            Rectangle {
                id: collapseButton
                visible: root.state.enabled === true
                width: root.kioskHost ? 44 : 28
                height: root.kioskHost ? 44 : 28
                radius: theme.radiusSm
                anchors.verticalCenter: parent.verticalCenter
                color: chevArea.containsMouse ? theme.surfaceHover : "transparent"
                activeFocusOnTab: visible && enabled
                border.width: activeFocus ? 2 : 0
                border.color: theme.accent
                Accessible.role: Accessible.Button
                Accessible.name: root.title
                Accessible.onPressAction: collapseButton.activate()
                function activate() {
                    QbzBridge.settingsBool(
                        root.server + "-collapse", root.state.collapsed !== true)
                }
                Keys.onPressed: function (event) {
                    if (!event.isAutoRepeat
                            && (event.key === Qt.Key_Space
                                || event.key === Qt.Key_Return
                                || event.key === Qt.Key_Enter)) {
                        collapseButton.activate()
                        event.accepted = true
                    }
                }
                QbzIcon {
                    anchors.centerIn: parent
                    name: root.state.collapsed === true ? "chevron-right" : "chevron-down"
                    width: 18
                    height: 18
                    tintName: "secondary"
                }
                MouseArea {
                    id: chevArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onPressed: collapseButton.forceActiveFocus()
                    onClicked: collapseButton.activate()
                }
            }
            QbzToggle { kioskHost: root.kioskHost;
                anchors.verticalCenter: parent.verticalCenter
                checked: root.state.enabled === true
                onToggled: function (v) { QbzLocal.mediaSetEnabled(root.server, v) }
            }
        }
    }

    // =============================== body =================================
    Column {
        visible: root.state.enabled === true && root.state.collapsed !== true
        onVisibleChanged: { if (!visible) root.cancelPairing() }
        width: parent.width
        spacing: 4

        Item { width: 1; height: 10 }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Server address", QbzSession.trRev)
            description: root.testHint
            Row {
                spacing: 8
                QbzLineEdit { kioskHost: root.kioskHost;
                    enabled: !root.busy
                    width: 240
                    text: root.state.serverUrl || ""
                    placeholder: root.urlPlaceholder
                    onEdited: function (v) { root.urlInput = v }
                    onCommitted: function (v) { root.urlInput = v }
                }
                IconTextButton {
                    anchors.verticalCenter: parent.verticalCenter
                    label: QbzSession.tr("Test", QbzSession.trRev)
                    hasIcon: false
                    btnEnabled: root.effectiveUrl.trim() !== "" && !root.busy
                    onClicked: QbzLocal.mediaTest(root.server, root.effectiveUrl)
                }
            }
        }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Username", QbzSession.trRev)
            QbzLineEdit { kioskHost: root.kioskHost;
                    enabled: !root.busy
                width: 240
                text: root.state.username || ""
                placeholder: QbzSession.tr("Account name", QbzSession.trRev)
                onEdited: function (v) { root.userInput = v }
                onCommitted: function (v) { root.userInput = v }
            }
        }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Password", QbzSession.trRev)
            description: root.credentialNote
            QbzLineEdit { kioskHost: root.kioskHost;
                    enabled: !root.busy
                width: 240
                // NEVER prefilled: the document does not carry it, and asking
                // for it again is the honest cost of not shipping it to QML.
                text: ""
                isPassword: true
                placeholder: root.state.hasCredential === true
                    ? QbzSession.tr("Stored — type to replace", QbzSession.trRev)
                    : QbzSession.tr("Password", QbzSession.trRev)
                onEdited: function (v) { root.passInput = v }
                onCommitted: function (v) { root.passInput = v }
            }
        }

        // --- Connect ------------------------------------------------------
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Connection", QbzSession.trRev)
            description: root.statusText
            QbzPrimaryButton {
                label: QbzSession.tr("Connect", QbzSession.trRev)
                btnEnabled: root.canConnect && !root.busy
                btnHeight: 34
                onClicked: QbzLocal.mediaConnect(
                    root.server, root.effectiveUrl, root.effectiveUser, root.passInput)
            }
        }

        SettingRow { kioskHost: root.kioskHost;
            id: quickConnectRow
            objectName: "quickConnectRow"
            visible: root.server === "jellyfin"
            label: QbzSession.tr("Quick Connect", QbzSession.trRev)
            description: QbzSession.tr("Enter this code in Settings > Quick Connect in a Jellyfin client that is already signed in. QBZ waits up to 5 minutes.", QbzSession.trRev)
            Row {
                spacing: 12
                Text {
                    objectName: "quickConnectCode"
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.pairing && (root.operation.pairing_code || "") !== ""
                    text: root.operation.pairing_code || ""
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? 28 : 24
                    font.weight: Font.DemiBold
                    Accessible.name: text
                }
                IconTextButton {
                    objectName: "quickConnectButton"
                    anchors.verticalCenter: parent.verticalCenter
                    label: root.pairing ? QbzSession.tr("Cancel", QbzSession.trRev)
                        : QbzSession.tr("Quick Connect", QbzSession.trRev)
                    hasIcon: false
                    btnEnabled: root.pairing || (!root.busy && root.effectiveUrl.trim() !== "")
                    onClicked: {
                        if (root.pairing) root.cancelPairing()
                        else QbzLocal.mediaQuickConnect(root.effectiveUrl)
                    }
                }
            }
        }

        // --- Library sweep ------------------------------------------------
        SettingRow { kioskHost: root.kioskHost;
            visible: root.state.hasCredential === true
            label: QbzSession.tr("Library", QbzSession.trRev)
            // The count is what tells the user the sweep did anything; the cost
            // line is what stops a 45-second Jellyfin sweep reading as a hang.
            description: (root.state.cachedTracks > 0
                ? QbzSession.tr("%1 tracks cached.", QbzSession.trRev)
                    .replace("%1", root.state.cachedTracks)
                : QbzSession.tr("Not synced yet.", QbzSession.trRev)) + " " + root.syncCost
            Row {
                spacing: 8
                // The progress text replaces the buttons while a sweep runs —
                // one control, one state, and no button to click twice.
                Text {
                    visible: root.operation.syncing === true
                    anchors.verticalCenter: parent.verticalCenter
                    text: (root.operation.progress || "") !== ""
                        ? QbzSession.tr("Syncing…", QbzSession.trRev) + " " + (root.operation.progress || "")
                        : QbzSession.tr("Syncing…", QbzSession.trRev)
                    color: theme.textSecondary
                    font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
                }
                IconTextButton {
                    visible: root.operation.syncing !== true
                    anchors.verticalCenter: parent.verticalCenter
                    label: QbzSession.tr("Sync now", QbzSession.trRev)
                    hasIcon: false
                    btnEnabled: !root.busy
                    onClicked: QbzLocal.mediaSync(root.server, false)
                }
                // FULL is separate because it is the expensive one and the
                // answer to "the delta missed something", not the default.
                IconTextButton {
                    visible: root.operation.syncing !== true && root.state.lastSyncAt > 0
                    anchors.verticalCenter: parent.verticalCenter
                    label: QbzSession.tr("Full re-sync", QbzSession.trRev)
                    hasIcon: false
                    btnEnabled: !root.busy
                    onClicked: QbzLocal.mediaSync(root.server, true)
                }
            }
        }

        // --- Danger zone --------------------------------------------------
        SettingRow { kioskHost: root.kioskHost;
            visible: root.state.hasCredential === true
            label: QbzSession.tr("Disconnect", QbzSession.trRev)
            description: QbzSession.tr(
                "Signs out and removes this server's tracks from your library.",
                QbzSession.trRev)
            IconTextButton {
                label: QbzSession.tr("Disconnect", QbzSession.trRev)
                hasIcon: false
                danger: true
                onClicked: {
                    // CONFIRMED, unlike the master toggle: the toggle only
                    // hides the rows and is one click to undo, while this
                    // purges the cache and costs a full re-sweep to get back.
                    if (root.confirmHost) {
                        root.confirmHost.ask(
                            QbzSession.tr("Disconnect this server?", QbzSession.trRev),
                            QbzSession.tr(
                                "Its tracks are removed from your library. Your files are not touched.",
                                QbzSession.trRev),
                            QbzSession.tr("Disconnect", QbzSession.trRev),
                            function () { QbzLocal.mediaDisconnect(root.server) })
                    } else {
                        QbzLocal.mediaDisconnect(root.server)
                    }
                }
            }
        }

        Item { width: 1; height: 6 }
    }
}
