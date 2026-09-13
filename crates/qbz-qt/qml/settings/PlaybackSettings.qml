// Settings > Playback — the QML port of crates/qbz-ui/ui/settings/
// PlaybackSettings.slint. 1:1 group order, labels, descriptions and gating;
// every control rides the settingsJson document and the settings invokables.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    property var confirmHost: null

    QbzTheme { id: theme }

    spacing: 4

    // ============================ PLAYBACK ===============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PLAYBACK", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Continue playback after track ends", QbzSession.trRev)
        description: QbzSession.tr("Keep playing the rest of the album or playlist instead of stopping.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.continuePlayback === true
            onToggled: function (v) { QbzBridge.settingsBool("continue-playback", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show track playing context", QbzSession.trRev)
        description: QbzSession.tr("Display the context-stack icon in the player.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.showContextIcon === true
            onToggled: function (v) { QbzBridge.settingsBool("show-context-icon", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Gapless playback", QbzSession.trRev)
        description: QbzSession.tr("Play consecutive same-format tracks without a gap.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.gapless === true
            onToggled: function (v) { QbzBridge.settingsBool("gapless", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================= SESSION ===============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("SESSION", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Restore session on startup", QbzSession.trRev)
        description: QbzSession.tr("Restore the queue and current track on the next launch.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.persistSession === true
            onToggled: function (v) { QbzBridge.settingsBool("persist-session", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Resume playback position", QbzSession.trRev)
        description: QbzSession.tr("Also seek back to where the saved track left off.", QbzSession.trRev)
        rowEnabled: root.doc.persistSession === true
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.resumePosition === true
            enabled: root.doc.persistSession === true
            onToggled: function (v) { QbzBridge.settingsBool("resume-position", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Auto-connect Qobuz Connect on startup", QbzSession.trRev)
        description: QbzSession.tr("Choose whether Qobuz Connect activates automatically when QBZ launches.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 240
            options: root.doc.qconnectStartupModes || []
            currentIndex: root.doc.qconnectStartupIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("qconnect-startup", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("When playback conflicts", QbzSession.trRev)
        description: QbzSession.tr("Ask each time or automatically choose which queue and device should continue.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 300
            popupWidth: 420
            options: root.doc.qconnectConflictPolicies || []
            currentIndex: root.doc.qconnectConflictPolicyIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("qconnect-conflict-policy", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Qobuz Connect device name", QbzSession.trRev)
        description: QbzSession.tr("The name other Qobuz Connect apps see for this device. Applies on the next connection.", QbzSession.trRev)
        QbzLineEdit { kioskHost: root.kioskHost;
            width: 240
            text: root.doc.qconnectDeviceName || ""
            placeholder: root.doc.qconnectDeviceNameDefault || ""
            onCommitted: function (s) { QbzBridge.settingsString("qconnect-device-name", s) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================ STREAMING ==============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("STREAMING", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Stream uncached tracks", QbzSession.trRev)
        description: QbzSession.tr("Start uncached tracks via streaming instead of waiting for the full download.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.streamUncached === true
            onToggled: function (v) { QbzBridge.settingsBool("stream-uncached", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.streamUncached === true
        label: QbzSession.tr("Initial buffer size", QbzSession.trRev)
        description: QbzSession.tr("Seconds of audio buffered before streaming playback starts.", QbzSession.trRev)
        Row {
            spacing: 12
            QbzSlider { kioskHost: root.kioskHost;
                width: 160
                anchors.verticalCenter: parent.verticalCenter
                minimum: 1
                maximum: 10
                value: root.doc.bufferSeconds || 1
                onChanged: function (v) { QbzBridge.settingsSlider("buffer-seconds", v) }
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: (root.doc.bufferSeconds || 1) + QbzSession.tr("s", QbzSession.trRev)
                color: theme.textSecondary
                font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                font.weight: theme.weightMedium
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Streaming only", QbzSession.trRev)
        fitDescription: true
        description: QbzSession.tr("Mimics official clients’ streaming behavior with lower memory usage. Skips the playback cache and its protection against network interruptions: QBZ’s cache keeps upcoming audio ready for more reliable playback and instant replay.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.streamingOnly === true
            onToggled: function (v) { QbzBridge.settingsBool("streaming-only", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("When quality retries fail", QbzSession.trRev)
        description: QbzSession.tr("What to do when every quality tier for a track is unavailable.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 240
            options: root.doc.retryBehaviors || []
            currentIndex: root.doc.retryBehaviorIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("retry-behavior", i) }
        }
    }
}
