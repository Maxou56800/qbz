// Settings > Local Library > PLEX — the QML port of the PLEX half of
// crates/qbz-ui/ui/settings/LocalLibrarySettings.slint (lines 517-855):
// master toggle + collapse header, server address, token, connection
// actions, the music-library picker, the metadata-write toggle and the Plex
// danger zone.
//
// Wiring: the LIVE state (enabled / available / syncing / sections / error)
// is the QbzLocal bridge's own Plex properties — the SAME ones the Local
// Library browse view reads, so a change here reloads the browse union in
// one hop. The two fields the bridge does not publish (the persisted server
// url and whether a token is stored) ride the settings document
// (settings_qt/library.rs). The token itself NEVER leaves Rust.
//
// Both auth paths are shipped: the PIN flow (Authorize / Generate code /
// Link code / Copy code / Open Plex sign-in) and the manual X-Plex-Token
// field.
//
// Delta vs the Slint: they are shown TOGETHER rather than behind its "Enter
// token manually" switch. One fewer control, and the row that would hide the
// token field is the one a user reaches for precisely when the PIN flow did
// not work for them.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    /// The view-level SettingsConfirmHost, handed down by
    /// LocalLibrarySettings (which is itself handed it by SettingsView).
    /// Null in previews; every call site falls back to acting directly.
    property var confirmHost: null

    /// What Connect would actually send: the freshly typed text if there is
    /// any, else the persisted address. Single-sourced so the warning, the
    /// button gate and the click all judge the SAME string — judging the
    /// field while sending the fallback is how a gate ends up decorative.
    readonly property string effectiveUrl:
        root.urlInput !== "" ? root.urlInput : (root.plex.serverUrl || "")
    /// Empty counts as "not yet wrong": no warning before anything is typed.
    readonly property bool urlIsLocal:
        root.effectiveUrl === "" || QbzLocal.plexUrlIsLocal(root.effectiveUrl)
    readonly property var plex: (doc.library || ({})).plex || ({})

    // In-progress field values (the fields commit on Enter / focus loss; the
    // Connect button submits whatever is typed right now).
    property string urlInput: ""
    property string tokenInput: ""

    QbzTheme { id: theme }

    spacing: 4

    readonly property var sections: {
        try {
            return JSON.parse(QbzLocal.plexSectionsJson)
        } catch (e) {
            return []
        }
    }

    // Same lazy-load hook as the Slint panel's init: re-publish the Plex
    // state (sections + gates) whenever the section becomes visible.
    onVisibleChanged: {
        if (visible) {
            urlInput = plex.serverUrl || ""
            QbzLocal.refreshPlex()
        } else {
            // Drop any outstanding PIN and stop the 2.5 s poll against
            // plex.tv. The reference cannot do this — Slint has no unmount
            // hook, so its poll watches the settings section on every tick
            // and stops itself (plex_auth.rs:505-515). QML just tells us.
            QbzLocal.plexStopPin()
        }
    }

    // ===================== header: title + toggle + chevron ================
    // HOMOLOGATED with Jellyfin and Subsonic on 2026-08-21 (owner). The three
    // media servers sit one under the other in this panel and Plex was the odd
    // one out: a small-caps "PLEX" eyebrow with no brand mark, in a 64px band,
    // against the 44px `SourceIcon` + 14px DemiBold title the other two use.
    // Same header, same metrics, same glyph pipeline — `SourceIcon` already
    // draws the full-colour Plex mark untinted (controls/SourceIcon.qml).
    //
    // ONE affordance the other two have is NOT copied: the "· <server name>"
    // suffix. `PlexFields` (settings_qt/library.rs:51) carries no name — Plex
    // pairing stores a url and a token — so the row would be permanently
    // invisible. An empty binding is not parity, it is a dead branch.
    //
    // Deliberately NOT folded into MediaServerSettings.qml: that component is
    // one FORM twice (address / account / password / connect / sweep), and
    // Plex's body is a different shape — a PIN pairing flow, a server picker
    // and a per-library table. Only the header is the same.
    Item {
        width: parent.width
        height: 44
        Column {
            anchors.left: parent.left
            anchors.right: plexControls.left
            anchors.rightMargin: 24
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Row {
                spacing: 8
                SourceIcon {
                    anchors.verticalCenter: parent.verticalCenter
                    kind: "plex"
                    glyphSize: 18
                    plexSize: 18
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Plex"
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? (14) * 1.2 : (14)
                    font.weight: Font.DemiBold
                }
            }
            Text {
                width: parent.width
                text: QbzSession.tr("Connect a Plex Media Server on your local network.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
                wrapMode: Text.WordWrap
            }
        }
        Row {
            id: plexControls
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: 8
            Rectangle {
                id: collapseButton
                visible: QbzLocal.plexEnabled
                width: root.kioskHost ? 44 : 28
                height: root.kioskHost ? 44 : 28
                radius: theme.radiusSm
                anchors.verticalCenter: parent.verticalCenter
                color: chevArea.containsMouse ? theme.surfaceHover : "transparent"
                activeFocusOnTab: visible && enabled
                border.width: activeFocus ? 2 : 0
                border.color: theme.accent
                Accessible.role: Accessible.Button
                Accessible.name: "Plex"
                Accessible.onPressAction: collapseButton.activate()
                function activate() {
                    QbzBridge.settingsBool(
                        "plex-collapse", root.plex.collapsed !== true)
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
                    name: root.plex.collapsed === true ? "chevron-right" : "chevron-down"
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
                checked: QbzLocal.plexEnabled
                onToggled: function (v) { QbzLocal.plexSetEnabled(v) }
            }
        }
    }

    // ============================== body ==================================
    Column {
        visible: QbzLocal.plexEnabled && root.plex.collapsed !== true
        width: parent.width
        spacing: 4

        Item { width: 1; height: 10 }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Server address", QbzSession.trRev)
            description: QbzSession.tr("Only local network servers are supported.", QbzSession.trRev)
            QbzLineEdit { kioskHost: root.kioskHost;
                id: urlField
                width: 240
                text: root.plex.serverUrl || ""
                placeholder: "http://127.0.0.1:32400"
                onEdited: function (v) { root.urlInput = v }
                onCommitted: function (v) { root.urlInput = v }
            }
        }

        // LAN-only inline warning, 1:1 with LocalLibrarySettings.slint:611-616
        // (same string, same #e0564f, same legal size). Live as the user
        // types — the reference recomputes `is-local-address` on every gate
        // refresh, and this calls the same predicate through the bridge.
        Text {
            visible: root.effectiveUrl !== "" && !root.urlIsLocal
            width: parent.width
            text: QbzSession.tr("Only local network servers are supported.", QbzSession.trRev)
            color: "#e0564f"
            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
            wrapMode: Text.WordWrap
        }
        // --- Authorize (PIN) ---------------------------------------------
        // LocalLibrarySettings.slint:624-635. Gated on the same three things
        // as the reference: Plex enabled, a LAN address, and no request in
        // flight. Rust re-checks all three.
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Authorize", QbzSession.trRev)
            description: QbzSession.tr("Generate a code and sign in to Plex in your browser.", QbzSession.trRev)
            SettingsButton { kioskHost: root.kioskHost;
                text: QbzLocal.pinBusy
                    ? QbzSession.tr("Working...", QbzSession.trRev)
                    : QbzSession.tr("Generate code", QbzSession.trRev)
                enabled: QbzLocal.plexEnabled && root.urlIsLocal && !QbzLocal.pinBusy
                onClicked: QbzLocal.plexGenerateCode(root.effectiveUrl)
            }
        }

        // --- Code block, only while a code is outstanding -----------------
        // The reference mounts this with `if pin-code != ""`; `visible` is
        // the QML equivalent and the row collapses with it.
        SettingRow { kioskHost: root.kioskHost;
            visible: (QbzLocal.pinCode || "") !== ""
            label: QbzSession.tr("Link code", QbzSession.trRev)
            description: QbzSession.tr("Enter this code at the Plex sign-in page.", QbzSession.trRev)
            Row {
                spacing: 8
                // The code chip: 34px tall, surface-elevated, subtle border,
                // semibold with 1.5px letter-spacing (slint :645-661).
                Rectangle {
                    anchors.verticalCenter: parent.verticalCenter
                    width: codeText.implicitWidth + 20
                    height: 34
                    radius: theme.radiusSm
                    border.width: 1
                    border.color: theme.borderSubtle
                    color: theme.surfaceElevated
                    Text {
                        id: codeText
                        anchors.centerIn: parent
                        text: QbzLocal.pinCode
                        color: theme.textPrimary
                        font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                        font.weight: theme.weightSemibold
                        font.letterSpacing: 1.5
                    }
                }
                SettingsButton { kioskHost: root.kioskHost;
                    text: QbzSession.tr("Copy code", QbzSession.trRev)
                    onClicked: QbzLocal.plexCopyCode()
                }
                SettingsButton { kioskHost: root.kioskHost;
                    text: QbzSession.tr("Open Plex sign-in", QbzSession.trRev)
                    onClicked: QbzLocal.plexOpenAuthUrl()
                }
            }
        }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Token", QbzSession.trRev)
            description: root.plex.hasToken === true
                ? QbzSession.tr("A token is stored for this server.", QbzSession.trRev)
                : QbzSession.tr("Use an existing X-Plex-Token.", QbzSession.trRev)
            QbzLineEdit { kioskHost: root.kioskHost;
                id: tokenField
                width: 240
                isPassword: true
                placeholder: QbzSession.tr("X-Plex-Token", QbzSession.trRev)
                onEdited: function (v) { root.tokenInput = v }
                onCommitted: function (v) { root.tokenInput = v }
            }
        }
        // Connect = persist the credentials + sync. `plex_connect` refuses a
        // non-LAN address BEFORE persisting (local_bridge.rs) — until
        // 2026-08-04 it did not, and the comment here claimed it did.
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Connect", QbzSession.trRev)
            description: QbzSession.tr("Saves the address and token, then fetches your libraries.", QbzSession.trRev)
            SettingsButton { kioskHost: root.kioskHost;
                text: QbzLocal.plexSyncing
                    ? QbzSession.tr("Working...", QbzSession.trRev)
                    : QbzSession.tr("Connect", QbzSession.trRev)
                // Gated on the address too (LocalLibrarySettings.slint:631
                // gates its Authorize button the same way). Rust re-checks —
                // the UI gate is the affordance, not the enforcement.
                enabled: !QbzLocal.plexSyncing && root.urlIsLocal
                onClicked: {
                    QbzLocal.plexConnect(root.urlInput !== "" ? root.urlInput
                        : (root.plex.serverUrl || ""), root.tokenInput)
                    root.tokenInput = ""
                    tokenField.text = ""
                }
            }
        }

        // Ping the STORED server (not the typed field) and report what
        // answered. A successful ping is also the only thing that stamps the
        // machine id onto the cache — see plex_pin_qt::check_connection.
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Check connection", QbzSession.trRev)
            description: QbzSession.tr("Ping the saved server and report what answers.", QbzSession.trRev)
            SettingsButton { kioskHost: root.kioskHost;
                text: QbzLocal.plexSyncing
                    ? QbzSession.tr("Working...", QbzSession.trRev)
                    : QbzSession.tr("Check now", QbzSession.trRev)
                enabled: root.plex.hasToken === true && !QbzLocal.plexSyncing
                onClicked: QbzLocal.plexCheckConnection()
            }
        }

        Item { width: 1; height: 14 }

        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Libraries", QbzSession.trRev)
            description: QbzSession.tr("Fetch your Plex music libraries.", QbzSession.trRev)
            SettingsButton { kioskHost: root.kioskHost;
                text: QbzLocal.plexSyncing
                    ? QbzSession.tr("Working...", QbzSession.trRev)
                    : QbzSession.tr("Get libraries", QbzSession.trRev)
                enabled: QbzLocal.plexAvailable && !QbzLocal.plexSyncing
                onClicked: QbzLocal.syncPlex()
            }
        }

        // Status line: the last Plex error, or the last sync's track count.
        Text {
            visible: QbzLocal.plexError !== "" || QbzLocal.plexLastSyncTracks >= 0
            width: parent.width
            text: QbzLocal.plexError !== ""
                ? QbzLocal.plexError
                : QbzSession.tr("Synced {} tracks", QbzSession.trRev)
                    .replace("{}", QbzLocal.plexLastSyncTracks)
            color: QbzLocal.plexError !== "" ? theme.danger : theme.success
            font.pixelSize: root.kioskHost ? (theme.fontLegal) * 1.2 : (theme.fontLegal)
            wrapMode: Text.WordWrap
        }

        Item { width: 1; height: 6 }

        // ---------------------- music library picker ----------------------
        Column {
            visible: root.sections.length > 0
            width: parent.width
            spacing: 2
            GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("MUSIC LIBRARIES", QbzSession.trRev) }
            Item { width: 1; height: 4 }
            Repeater {
                model: root.sections
                delegate: Rectangle {
                    id: secRow
                    required property var modelData
                    width: parent ? parent.width : 0
                    height: 42
                    radius: theme.radiusSm
                    color: secArea.containsMouse ? theme.surfaceHover : "transparent"

                    function toggle() {
                        const keys = []
                        for (let i = 0; i < root.sections.length; i++) {
                            const s = root.sections[i]
                            const on = s.key === secRow.modelData.key ? !s.selected : s.selected
                            if (on) keys.push(s.key)
                        }
                        QbzLocal.setPlexSections(JSON.stringify(keys))
                    }

                    MouseArea {
                        id: secArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: secRow.toggle()
                    }
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 12
                        spacing: 12
                        QbzCheckbox { kioskHost: root.kioskHost;
                            anchors.verticalCenter: parent.verticalCenter
                            checked: secRow.modelData.selected === true
                            onToggled: secRow.toggle()
                        }
                        Text {
                            width: parent.width - 18 - 24
                            height: parent.height
                            text: secRow.modelData.title || ""
                            color: theme.textPrimary
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            verticalAlignment: Text.AlignVCenter
                            elide: Text.ElideRight
                        }
                    }
                }
            }
            Item { width: 1; height: 14 }
        }

        // -------------------------- metadata write -------------------------
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Write metadata to Plex (experimental)", QbzSession.trRev)
            description: QbzSession.tr("Allow QBZ to write track metadata back to your Plex server.", QbzSession.trRev)
            QbzToggle { kioskHost: root.kioskHost;
                checked: root.plex.metadataWrite === true
                onToggled: function (v) { QbzBridge.settingsBool("plex-metadata-write", v) }
            }
        }

        Item { width: 1; height: 18 }

        // ------------------------- plex danger zone ------------------------
        GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PLEX DANGER ZONE", QbzSession.trRev) }
        SettingRow { kioskHost: root.kioskHost;
            label: QbzSession.tr("Disconnect", QbzSession.trRev)
            description: QbzSession.tr("Sign out of Plex and clear the local cache.", QbzSession.trRev)
            SettingsButton { kioskHost: root.kioskHost;
                danger: true
                text: QbzSession.tr("Disconnect", QbzSession.trRev)
                enabled: root.plex.hasToken === true
                // Confirm first — 1:1 with plex_auth.rs:963-968, including the
                // strings. Disconnect wipes the credentials AND the cache, and
                // this port used to do both on the first click.
                onClicked: {
                    if (!root.confirmHost) {
                        QbzLocal.plexDisconnect()
                        return
                    }
                    root.confirmHost.ask(
                        QbzSession.tr("Disconnect from Plex?", QbzSession.trRev),
                        QbzSession.tr("This signs out of Plex and clears the locally cached libraries and tracks.", QbzSession.trRev),
                        QbzSession.tr("Disconnect", QbzSession.trRev),
                        function () { QbzLocal.plexDisconnect() })
                }
            }
        }
    }
}
