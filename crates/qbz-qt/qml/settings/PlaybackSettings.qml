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

    function applyMemory() {
        if (root.doc.playbackMemoryApplyBusy === true) return
        const track = QbzPlayer.npHasTrack ? QbzPlayer.npTrackId : ""
        if (QbzPlayer.npPlaying || QbzPlayer.npLoading) {
            if (!root.confirmHost) return
            root.confirmHost.ask(
                QbzSession.tr("Apply playback memory profile?", QbzSession.trRev),
                QbzSession.tr("Playback buffers will be released. If playing, the current track will stop and restart from the beginning. If paused, it will stay stopped at 0:00. Your queue and disk cache will be kept.", QbzSession.trRev),
                QbzSession.tr("Apply & free memory", QbzSession.trRev),
                function () { QbzBridge.settingsString("playback-cache-apply", "restart:" + track) })
        } else {
            QbzBridge.settingsString("playback-cache-apply", "idle:" + track)
        }
    }
    readonly property var cachePolicy: doc.playbackCache || ({})
    readonly property var cacheUsage: doc.playbackCacheUsage || ({})
    readonly property var storage: doc.playbackStorage || ({})
    readonly property var memoryProfileIds: ["auto", "high", "desktop", "low", "custom"]
    readonly property string memoryProfile: doc.playbackMemoryProfile ||
        (cachePolicy.profile || ((cachePolicy.dynamic || cachePolicy.min_mib != null || cachePolicy.max_mib != null) ? "custom" : "auto"))
    readonly property var memoryProfileDescriptions: [
        QbzSession.tr("Auto selects Desktop or Low from the computer's RAM. Fixed cache budget; dynamic growth is off. Recommended default.", QbzSession.trRev),
        QbzSession.tr("High starts with a 400 MiB cache budget and grows up to 1600 MiB when memory is available. It shrinks after inactivity or memory pressure. For long Hi-Res tracks and computers with spare RAM.", QbzSession.trRev),
        QbzSession.tr("Desktop uses a fixed 400 MiB cache budget and normal prefetch. Larger tracks use disk-backed buffers.", QbzSession.trRev),
        QbzSession.tr("Low uses a 50 MiB cache budget, smaller startup buffers and reduced prefetch. Large tracks use disk; speculative Hi-Res prefetch is skipped. Useful on Raspberry Pi or when saving RAM.", QbzSession.trRev),
        QbzSession.tr("Custom uses your cache limits and growth preference below. Prefetch follows the computer's detected memory class. Editing a preset switches to Custom.", QbzSession.trRev)
    ]

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

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PLAYBACK CACHE", QbzSession.trRev) }
    GroupBox {
        id: memoryProfiles
        objectName: "playbackMemoryFieldset"
        width: root.width
        padding: 16
        topPadding: label.implicitHeight + 28
        title: QbzSession.tr("Playback memory profile", QbzSession.trRev)
        Accessible.role: Accessible.Grouping
        Accessible.name: title
        label: Text {
            x: memoryProfiles.leftPadding
            y: 12
            text: memoryProfiles.title
            color: theme.textPrimary
            font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
            font.weight: theme.weightMedium
        }
        background: Rectangle {
            color: "transparent"
            border.color: theme.borderSubtle
            radius: 6
        }
        contentItem: Column {
            spacing: 16
            Text {
                objectName: "playbackMemoryStreamingNotice"
                width: parent.width
                visible: root.doc.streamingOnly === true
                text: QbzSession.tr("These options do not apply while Streaming only is enabled. Turn it off to choose a playback memory profile and apply changes.", QbzSession.trRev)
                color: theme.textSecondary
                font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                wrapMode: Text.WordWrap
            }
            Column {
                width: parent.width
                spacing: 16
                Repeater {
                    id: profileOptions
                    model: root.memoryProfileIds
                    delegate: QbzRadioOption {
                        required property int index
                        required property string modelData
                        objectName: "memoryProfile-" + modelData
                        width: parent.width
                        kioskHost: root.kioskHost
                        label: [QbzSession.tr("Auto — recommended", QbzSession.trRev),
                            QbzSession.tr("High — dynamic", QbzSession.trRev),
                            QbzSession.tr("Desktop", QbzSession.trRev),
                            QbzSession.tr("Low — Pi style", QbzSession.trRev),
                            QbzSession.tr("Custom", QbzSession.trRev)][index]
                        description: root.memoryProfileDescriptions[index]
                        selected: root.memoryProfile === modelData
                        enabled: root.doc.streamingOnly !== true
                        opacity: enabled ? 1 : 0.45
                        onClicked: QbzBridge.settingsString("playback-memory-profile", modelData)
                        function step(delta) {
                            const next = profileOptions.itemAt((index + delta + 5) % 5)
                            next.forceActiveFocus()
                            next.clicked()
                        }
                        Keys.onUpPressed: step(-1)
                        Keys.onDownPressed: step(1)
                        Keys.onLeftPressed: step(-1)
                        Keys.onRightPressed: step(1)
                    }
                }
            }
            Column {
                objectName: "playbackMemoryCustomOptions"
                width: parent.width
                visible: root.memoryProfile === "custom"
                spacing: 4
                SettingRow { kioskHost: root.kioskHost;
                    fitDescription: true
                    label: QbzSession.tr("Grow cache when memory is available", QbzSession.trRev)
                    description: QbzSession.tr("Allow the cache to grow within your maximum when RAM is available. High enables this automatically. Editing this option switches to Custom.", QbzSession.trRev)
                    rowEnabled: root.doc.streamingOnly !== true
                    QbzToggle { kioskHost: root.kioskHost;
                        checked: root.cachePolicy.dynamic === true
                        enabled: root.doc.streamingOnly !== true
                        onToggled: function (v) { QbzBridge.settingsBool("playback-cache-dynamic", v) }
                    }
                }
                SettingRow { kioskHost: root.kioskHost;
                    fitDescription: true
                    label: QbzSession.tr("Minimum cache budget (MiB)", QbzSession.trRev)
                    description: QbzSession.tr("Base budget, not reserved RAM. Memory pressure can reduce it. Leave blank for the recommended value.", QbzSession.trRev)
                    rowEnabled: root.doc.streamingOnly !== true
                    QbzLineEdit { kioskHost: root.kioskHost;
                        width: 240
                        enabled: root.doc.streamingOnly !== true
                        text: root.cachePolicy.min_mib == null ? "" : String(root.cachePolicy.min_mib)
                        placeholder: String(Math.round((root.cacheUsage.base_size_bytes || root.cacheUsage.recommended_size_bytes || 419430400) / 1048576))
                        onCommitted: function (s) { QbzBridge.settingsString("playback-cache-min", s) }
                    }
                }
                SettingRow { kioskHost: root.kioskHost;
                    fitDescription: true
                    label: QbzSession.tr("Maximum cache budget (MiB)", QbzSession.trRev)
                    description: QbzSession.tr("Growth ceiling; available memory may impose a lower limit. Leave blank for automatic.", QbzSession.trRev)
                    rowEnabled: root.doc.streamingOnly !== true && root.cachePolicy.dynamic === true
                    QbzLineEdit { kioskHost: root.kioskHost;
                        width: 240
                        enabled: root.doc.streamingOnly !== true && root.cachePolicy.dynamic === true
                        text: root.cachePolicy.max_mib == null ? "" : String(root.cachePolicy.max_mib)
                        placeholder: String(Math.round((root.cacheUsage.ceiling_size_bytes || 419430400) / 1048576))
                        onCommitted: function (s) { QbzBridge.settingsString("playback-cache-max", s) }
                    }
                }
            }
            Column {
                objectName: "playbackMemoryTradeoff"
                width: parent.width
                spacing: 4
                Text {
                    width: parent.width
                    text: QbzSession.tr("RAM and storage trade-off", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
                    font.weight: theme.weightMedium
                    wrapMode: Text.WordWrap
                }
                Text {
                    width: parent.width
                    text: QbzSession.tr("All profiles preserve audio quality and can use disk cache. Lower memory budgets rely more on storage speed and writes; more RAM can reduce that work. Cached disk tracks are read from disk even in High. Choose storage appropriate for your listening workload.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                    wrapMode: Text.WordWrap
                }
            }
            Text {
                width: parent.width
                text: QbzSession.tr("The profile already applies to new buffers. Apply now to release existing playback buffers and the memory cache. Memory use can grow again as playback continues.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
                wrapMode: Text.WordWrap
            }
            Row {
                objectName: "playbackMemoryActions"
                anchors.right: parent.right
                spacing: 8
                SettingsButton {
                    objectName: "playbackMemoryReset"
                    kioskHost: root.kioskHost
                    text: QbzSession.tr("Restore defaults", QbzSession.trRev)
                    enabled: root.doc.streamingOnly !== true && root.doc.playbackMemoryApplyBusy !== true
                    onClicked: QbzBridge.settingsString("playback-cache-reset", "")
                }
                SettingsButton {
                    objectName: "playbackMemoryApply"
                    kioskHost: root.kioskHost
                    text: QbzSession.tr("Apply & free memory", QbzSession.trRev)
                    busy: root.doc.playbackMemoryApplyBusy === true
                    enabled: root.doc.streamingOnly !== true && !busy
                    onClicked: root.applyMemory()
                }
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Memory cache usage", QbzSession.trRev)
        description: QbzSession.tr("Used / current budget: %1 / %2 MiB. This excludes decoder memory and offline downloads.", QbzSession.trRev)
            .arg(Math.round((root.cacheUsage.current_size_bytes || 0) / 1048576))
            .arg(Math.round((root.cacheUsage.max_size_bytes || 0) / 1048576))
        SettingsButton { kioskHost: root.kioskHost;
            text: QbzSession.tr("Refresh", QbzSession.trRev)
            onClicked: QbzBridge.settingsString("playback-cache-refresh", "")
        }
    }
    Column {
        objectName: "playbackStorageSetting"
        width: root.width
        spacing: 8
        Text {
            width: parent.width
            text: QbzSession.tr("Playback storage folder", QbzSession.trRev)
            color: theme.textPrimary
            font.pixelSize: root.kioskHost ? theme.fontBody * 1.2 : theme.fontBody
            font.weight: theme.weightMedium
        }
        Text {
            width: parent.width
            text: QbzSession.tr("Choose an existing folder on the drive you want to use. QBZ stores its playback cache and temporary track buffers in a qbz-playback subfolder. Offline downloads are separate. Leave blank for the default location.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
            wrapMode: Text.WordWrap
        }
        Row {
            width: parent.width
            spacing: 8
            QbzLineEdit {
                objectName: "playbackStoragePath"
                kioskHost: root.kioskHost
                width: Math.max(0, Math.min(storageBrowse.width * 2, parent.width - storageBrowse.width - parent.spacing))
                text: root.storage.candidate || ""
                onCommitted: function(path) { QbzBridge.settingsString("playback-storage-folder", path) }
            }
            SettingsButton {
                id: storageBrowse
                objectName: "playbackStorageBrowse"
                kioskHost: root.kioskHost
                text: QbzSession.tr("Browse...", QbzSession.trRev)
                onClicked: QbzBridge.settingsString("playback-storage-browse", "")
            }
        }
        Text {
            objectName: "playbackStorageActiveHint"
            width: parent.width
            text: QbzSession.tr("Current playback storage", QbzSession.trRev) + ": " + (root.storage.active || "—")
                + "\n" + QbzSession.tr("Changing the folder takes effect after restarting QBZ. Existing cache files stay in the old location; they are not moved or deleted automatically.", QbzSession.trRev)
            color: theme.textMuted
            font.pixelSize: root.kioskHost ? theme.fontLegal * 1.2 : theme.fontLegal
            wrapMode: Text.Wrap
        }
    }
    Text {
        width: root.width
        visible: (root.storage.error || "") !== ""
        text: QbzSession.tr("Could not use this playback storage folder:", QbzSession.trRev) + " " + (root.storage.error || "")
        wrapMode: Text.Wrap
        color: theme.danger
        font.pixelSize: theme.fontLegal
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: (root.storage.sandbox || "") !== ""
        fitDescription: true
        label: (root.storage.sandbox || "") + " — " + QbzSession.tr("Folder access", QbzSession.trRev)
        description: root.storage.sandbox === "Flatpak"
            ? QbzSession.tr("If the sandbox blocks this folder, run the command below in a host terminal, restart QBZ, then select the folder again. Only grant access to a folder you intend to use.", QbzSession.trRev)
            : QbzSession.tr("Snap can access supported home folders and removable storage under /mnt, /media or /run/media. For removable storage, connect the interface below and retry. Snap cannot grant arbitrary folder access with a Flatpak-style override.", QbzSession.trRev)
    }
    CommandBlock {
        visible: (root.storage.command || "") !== ""
        width: root.width
        command: root.storage.command || ""
    }
}
