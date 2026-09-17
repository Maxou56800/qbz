// Settings > Network — the outgoing HTTP(S)/SOCKS proxy and the QConnect
// LAN / Cast safety switches. Backing store: settings_qt/network.rs.
//
// The proxy fields are staged like MediaServerSettings' address/user/pass:
// typed text is not sent anywhere until "Test & Save" is pressed, which
// persists every field, applies it process-wide, and probes a real Qobuz
// endpoint through it. The two safety toggles and the master switch apply
// instantly, like every other Settings toggle — there is nothing to stage.
//
// The password field is NEVER prefilled (root.net.proxyHasPassword only
// says whether one is stored); leaving it blank on Test & Save keeps the
// saved password, "Clear saved password" is the only way to remove one.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    readonly property var net: doc.network || ({})

    // Staged input. Empty string means "nothing typed yet, use the persisted
    // value" for host/username; the port and kind selector default to the
    // persisted value directly since there is always a concrete choice.
    property string hostInput: ""
    property string userInput: ""
    property string passInput: ""
    property bool authEnabledInput: net.proxyAuthEnabled === true
    property int kindIndexInput: net.proxyKindIndex || 0
    // Re-seed the staged auth/kind choice whenever the document changes
    // underneath an untouched field (e.g. another window changed it), but
    // never clobber text the user is mid-typing.
    onNetChanged: {
        authEnabledInput = net.proxyAuthEnabled === true
        kindIndexInput = net.proxyKindIndex || 0
    }

    readonly property string effectiveHost:
        root.hostInput !== "" ? root.hostInput : (root.net.proxyHost || "")
    readonly property string effectiveUser:
        root.userInput !== "" ? root.userInput : (root.net.proxyUsername || "")
    readonly property bool canSave: root.effectiveHost.trim() !== "" && portField.acceptableValue

    readonly property string resultText: {
        switch (root.net.testResultKind || "") {
        case "reachable": return QbzSession.tr("Qobuz is reachable through this proxy.", QbzSession.trRev)
        case "proxy-unreachable": return QbzSession.tr("Could not reach the proxy at that address.", QbzSession.trRev)
        case "failed": return (root.net.testResultDetail || "") !== ""
            ? (root.net.testResultDetail || "")
            : QbzSession.tr("The request through the proxy failed.", QbzSession.trRev)
        default: return ""
        }
    }
    readonly property color resultColor: (root.net.testResultKind || "") === "reachable" ? theme.success
        : (root.net.testResultKind || "") === "" ? theme.textMuted : theme.warning

    QbzTheme { id: theme }

    spacing: 4

    // ============================== PROXY =================================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PROXY", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Use a proxy", QbzSession.trRev)
        description: QbzSession.tr("Route Qobuz traffic — streaming, the API and Qobuz Connect — through a proxy.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.net.proxyEnabled === true
            onToggled: function (v) { QbzBridge.settingsBool("network-proxy-enabled", v) }
        }
    }

    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true
        label: QbzSession.tr("Type", QbzSession.trRev)
        description: QbzSession.tr("SOCKS resolves hostnames through the proxy, never on this machine.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 160
            options: root.net.proxyKindOptions || []
            currentIndex: root.kindIndexInput
            onSelected: function (i) { root.kindIndexInput = i }
        }
    }

    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true
        label: QbzSession.tr("Address", QbzSession.trRev)
        Row {
            spacing: 8
            QbzLineEdit { kioskHost: root.kioskHost;
                width: 200
                text: root.net.proxyHost || ""
                placeholder: QbzSession.tr("Host or IP", QbzSession.trRev)
                onEdited: function (v) { root.hostInput = v }
                onCommitted: function (v) { root.hostInput = v }
            }
            QbzLineEdit { kioskHost: root.kioskHost;
                id: portField
                width: 90
                readonly property bool acceptableValue: {
                    const n = parseInt(text, 10)
                    return !isNaN(n) && n >= 1 && n <= 65535
                }
                text: String(root.net.proxyPort || 1080)
                placeholder: QbzSession.tr("Port", QbzSession.trRev)
                onEdited: function (v) { text = v }
                onCommitted: function (v) { text = v }
            }
        }
    }

    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true
        label: QbzSession.tr("Authentication", QbzSession.trRev)
        description: QbzSession.tr("The proxy requires a username and password.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.authEnabledInput
            onToggled: function (v) { root.authEnabledInput = v }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true && root.authEnabledInput
        label: QbzSession.tr("Username", QbzSession.trRev)
        QbzLineEdit { kioskHost: root.kioskHost;
            width: 240
            text: root.net.proxyUsername || ""
            placeholder: QbzSession.tr("Proxy username", QbzSession.trRev)
            onEdited: function (v) { root.userInput = v }
            onCommitted: function (v) { root.userInput = v }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true && root.authEnabledInput
        label: QbzSession.tr("Password", QbzSession.trRev)
        description: root.net.proxyHasPassword === true
            ? QbzSession.tr("A password is saved. Leave blank to keep it.", QbzSession.trRev)
            : ""
        Row {
            spacing: 8
            QbzLineEdit { kioskHost: root.kioskHost;
                width: 200
                // NEVER prefilled — see the header comment.
                text: ""
                isPassword: true
                placeholder: root.net.proxyHasPassword === true
                    ? QbzSession.tr("Stored — type to replace", QbzSession.trRev)
                    : QbzSession.tr("Password", QbzSession.trRev)
                onEdited: function (v) { root.passInput = v }
                onCommitted: function (v) { root.passInput = v }
            }
            IconTextButton {
                anchors.verticalCenter: parent.verticalCenter
                visible: root.net.proxyHasPassword === true
                label: QbzSession.tr("Clear saved password", QbzSession.trRev)
                hasIcon: false
                onClicked: QbzBridge.settingsString("network-clear-password", "")
            }
        }
    }

    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true
        label: QbzSession.tr("Connection", QbzSession.trRev)
        description: root.net.testBusy === true
            ? QbzSession.tr("Testing…", QbzSession.trRev)
            : root.resultText
        Row {
            spacing: 8
            Text {
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.net.testBusy && root.resultText !== ""
                text: (root.net.testResultKind || "") === "reachable" ? "✓" : "⚠"
                color: root.resultColor
                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
            }
            IconTextButton {
                anchors.verticalCenter: parent.verticalCenter
                label: QbzSession.tr("Test & Save", QbzSession.trRev)
                hasIcon: false
                btnEnabled: root.canSave && root.net.testBusy !== true
                onClicked: QbzBridge.networkTestAndSave(
                    root.kindIndexInput,
                    root.effectiveHost,
                    parseInt(portField.text, 10),
                    root.authEnabledInput,
                    root.effectiveUser,
                    root.passInput)
            }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================== SAFETY =================================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("SAFETY", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Block Qobuz Connect on the local network", QbzSession.trRev)
        description: QbzSession.tr("Stop mDNS advertisement and the local Connect receiver, so nothing reaches other devices on this network outside the proxy.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.net.blockQconnectLan === true
            onToggled: function (v) { QbzBridge.settingsBool("network-block-qconnect-lan", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Block casting", QbzSession.trRev)
        description: QbzSession.tr("Stop Chromecast/DLNA discovery and the local cast media server.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.net.blockCast === true
            onToggled: function (v) { QbzBridge.settingsBool("network-block-cast", v) }
        }
    }
}
