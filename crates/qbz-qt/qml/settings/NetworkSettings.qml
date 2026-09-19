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
    property bool insecureTlsInput: net.proxyInsecureTls === true
    // A toggle/select has no empty-string sentinel for "untouched" the way
    // hostInput/userInput do, so each field gets its own flag instead —
    // set the moment the user interacts with it, checked below so a
    // republish arriving after that (even from this same panel's own
    // "Use a proxy" toggle, whose settingsBool round trip is NOT
    // instantaneous: it persists, applies the proxy, and re-enumerates
    // audio devices before publish_snapshot() answers) can never silently
    // revert a choice the user already made. A real, reproduced bug before
    // this: enabling the proxy, then quickly setting Type/host/"skip
    // certificate verification" and hitting Test & Save, saved with
    // insecure_tls back at its old persisted value — the late "proxy
    // enabled" republish had clobbered it in between.
    property bool authTouched: false
    property bool kindTouched: false
    property bool insecureTouched: false
    onNetChanged: {
        if (!authTouched) authEnabledInput = net.proxyAuthEnabled === true
        if (!kindTouched) kindIndexInput = net.proxyKindIndex || 0
        if (!insecureTouched) insecureTlsInput = net.proxyInsecureTls === true
    }

    // Index into proxyKindOptions ("HTTP", "HTTPS", "SOCKS5") — only an
    // https-kind proxy has a TLS handshake of its own to skip.
    readonly property bool isHttpsKind: root.kindIndexInput === 1

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
    // Dynamic, not just the row description below: appears the moment a
    // change is actually made (toggle, Test & Save, clear password) and
    // stays until QBZ restarts (the flag is process-lifetime only, never
    // persisted) — the row description alone was too easy to miss.
    WarningBanner {
        visible: root.net.restartRecommended === true
        variant: "warning"
        title: QbzSession.tr("Restart QBZ to finish applying this proxy change", QbzSession.trRev)
        body: QbzSession.tr("New streaming requests already use it. Your account session and Qobuz Connect only pick it up after a restart.", QbzSession.trRev)
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Use a proxy", QbzSession.trRev)
        description: QbzSession.tr("Route Qobuz traffic — streaming, the API and Qobuz Connect — through a proxy. Restart QBZ after enabling, disabling or changing it so the account session and Qobuz Connect pick it up too.", QbzSession.trRev)
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
            onSelected: function (i) { root.kindIndexInput = i; root.kindTouched = true }
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
        visible: root.net.proxyEnabled === true && root.isHttpsKind
        label: QbzSession.tr("Skip certificate verification for this proxy", QbzSession.trRev)
        description: QbzSession.tr("Only affects the connection to the proxy itself — Qobuz and everything else reached through it are still verified normally. Use this only for a proxy whose certificate you know and trust, such as a self-signed one you control.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.insecureTlsInput
            onToggled: function (v) { root.insecureTlsInput = v; root.insecureTouched = true }
        }
    }
    WarningBanner {
        visible: root.net.proxyEnabled === true && root.isHttpsKind && root.insecureTlsInput
        variant: "warning"
        title: QbzSession.tr("Certificate verification is off for this proxy", QbzSession.trRev)
        body: QbzSession.tr("QBZ will accept any certificate this proxy presents. Only leave this on if you trust the network path to it.", QbzSession.trRev)
    }

    SettingRow { kioskHost: root.kioskHost;
        visible: root.net.proxyEnabled === true
        label: QbzSession.tr("Authentication", QbzSession.trRev)
        description: QbzSession.tr("The proxy requires a username and password.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.authEnabledInput
            onToggled: function (v) { root.authEnabledInput = v; root.authTouched = true }
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
                    root.passInput,
                    root.insecureTlsInput)
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
    // Turning the block ON takes effect immediately (the LAN receiver is torn
    // down right away). Turning it back OFF does not: unlike enabling it,
    // there is no live restart — only the next connect() re-arms the LAN
    // receiver. Its own flag/banner, deliberately not sharing the PROXY
    // section's restartRecommended: different cause, different fix, and
    // showing both at once (proxy + this) must not be read as one issue.
    WarningBanner {
        visible: root.net.qconnectLanReconnectRecommended === true
        variant: "warning"
        title: QbzSession.tr("Reconnect or restart to fully turn this back on", QbzSession.trRev)
        body: QbzSession.tr("Qobuz Connect is already running on this device. Disconnect and reconnect it, or restart QBZ, so local-network discovery starts again.", QbzSession.trRev)
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
