// Settings > Offline — the QML port of crates/qbz-ui/ui/settings/
// OfflineSettings.slint.
//
// OFFLINE MODE (the app running without Qobuz) is the shared engine: the
// status line reads the LIVE QbzSession properties the engine forwarder
// publishes (tri-state online / no connection / induced + captive portal),
// and the toggle rides settingsBool("offline-mode-enabled") into
// `OfflineModeEngine::set_induced` (which also takes the #279 stream-first
// snapshot).
//
// "Check now" IS shipped (`offline-recheck`); so is the live tri-state status
// line (QbzSession.offlineMode / captivePortal).
//
// The on-disk half (offline downloads, artwork, lyrics) lives in Settings >
// Storage (StorageSettings.qml) since 2026-09-13, next to every other cache.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    readonly property var off: doc.offline || ({})
    /// The view-level SettingsConfirmHost (SettingsView.qml). Null in previews
    /// — the Clear-all row guards, so a preview degrades to the unconfirmed
    /// call rather than swallowing the click.
    property var confirmHost: null

    QbzTheme { id: theme }

    spacing: 4

    // ========================== OFFLINE MODE =============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("OFFLINE MODE", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Status", QbzSession.trRev)
        description: QbzSession.captivePortal
            ? QbzSession.tr("Captive portal detected — sign in to the network to get online.", QbzSession.trRev)
            : ""
        Text {
            // Tri-state: online green / real-offline amber / induced accent.
            text: QbzSession.offlineMode === 2 ? QbzSession.tr("Offline mode enabled", QbzSession.trRev)
                : QbzSession.offlineMode === 1 ? QbzSession.tr("No connection", QbzSession.trRev)
                : QbzSession.tr("Online", QbzSession.trRev)
            color: QbzSession.offlineMode === 2 ? theme.accent
                : QbzSession.offlineMode === 1 ? theme.warning : theme.success
            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
            font.weight: theme.weightMedium
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Enable Offline Mode", QbzSession.trRev)
        description: QbzSession.tr("Manually switch to offline mode even with internet.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.off.modeEnabled === true
            onToggled: function (v) { QbzBridge.settingsBool("offline-mode-enabled", v) }
        }
    }
    // Restored from the Tauri offline settings. Immediate and accumulated
    // modes are mutually exclusive in SQLite, so one settings republish keeps
    // both toggles in lock-step even if the write originates elsewhere.
    SettingRow { kioskHost: root.kioskHost;
        visible: root.off.modeEnabled === true
        label: QbzSession.tr("Immediate scrobbling", QbzSession.trRev)
        description: QbzSession.tr("Send scrobbles immediately while manual offline mode is enabled.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.off.allowImmediateScrobbling === true
            onToggled: function (v) {
                QbzBridge.settingsBool("offline-scrobble-immediate", v)
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.off.modeEnabled === true
        label: QbzSession.tr("Accumulated scrobbling", QbzSession.trRev)
        description: QbzSession.tr("Queue scrobbles while the network is unavailable and send them when it returns. Last.fm accepts scrobbles up to 2 weeks old.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.off.allowAccumulatedScrobbling === true
            onToggled: function (v) {
                QbzBridge.settingsBool("offline-scrobble-accumulated", v)
            }
        }
    }
    // Ask the connectivity actor for an immediate probe rather than waiting
    // for its next scheduled one. Disabled under MANUAL offline (mode 2),
    // where the answer is a user decision and not a network fact.
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Check connection", QbzSession.trRev)
        description: QbzSession.tr("Test the connection now instead of waiting for the next automatic check.", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            text: QbzSession.tr("Check now", QbzSession.trRev)
            enabled: QbzSession.offlineMode !== 2
            onClicked: QbzBridge.settingsString("offline-recheck", "")
        }
    }
}
