// Settings > Audio — the QML port of crates/qbz-ui/ui/settings/
// AudioSettings.slint. Group order, labels, descriptions, gating and control
// types are 1:1 with the Slint; every control rides the single settingsJson
// document (root.doc) and the settingsBool/Select invokables — never local
// truth. The audio path itself is PROTECTED: this panel only calls the
// settings keys settings_qt.rs already dispatches.
//
// "Detected device limit" + its fallback disclosure (#638 fix 3) ARE shipped
// (2026-08-17): the probe moved out of the Slint binary crate into
// `qbz-app::device_cap`, and both rows read the cap cache off the settings
// document. They are read-only by design — see the comment at the rows.
//
// The HiFi Wizard IS shipped (2026-08-11): the last row of OUTPUT opens
// settings/DacWizardModal.qml, mounted at the SettingsView root. Its logic
// lives in `qbz-dac-wizard-core`, the frontend-agnostic crate extracted for
// this port. Placement and backend gating diverge from the reference — see
// the row itself.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    property string openedHardwareVolumeChoice: ""

    function maybeOpenHardwareVolumeChoice() {
        const status = root.doc.alsaHardwareVolumeStatus || ""
        const needsChoice = status === "ambiguous" || status === "stale"
        const token = root.doc.alsaHardwareVolumeChoiceToken || ""
        if (!needsChoice) {
            root.openedHardwareVolumeChoice = ""
            return
        }
        if (token !== "" && token !== root.openedHardwareVolumeChoice) {
            root.openedHardwareVolumeChoice = token
            Qt.callLater(function () { hardwareVolumeSelect.openPopup() })
        }
    }

    onDocChanged: maybeOpenHardwareVolumeChoice()
    Component.onCompleted: maybeOpenHardwareVolumeChoice()

    QbzTheme { id: theme }

    spacing: 4

    // ============================ STREAMING ==============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("STREAMING", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Streaming quality", QbzSession.trRev)
        description: QbzSession.tr("The quality tier QBZ requests for playback.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.streamingQualities || []
            currentIndex: root.doc.streamingQualityIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("streaming-quality", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Limit quality to device", QbzSession.trRev)
        description: QbzSession.tr("Cap the requested streaming quality at your output device's limit. Applies to local playback only, never to casting.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.limitQualityToDevice === true
            onToggled: function (v) { QbzBridge.settingsBool("limit-quality-to-device", v) }
        }
    }
    // Read-only detected line — deliberately NOT a control (#638 fix 3): a Hz
    // dropdown was the documented root-cause trap (it promised rates the app
    // cannot request; Qobuz sells four discrete tiers), and with real detection
    // there is nothing left to pick. The value is Rust-composed from the probe
    // ("192 kHz · Hi-Res+") and is empty while nothing is cached, which is what
    // hides the row rather than showing a blank value.
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.limitQualityToDevice === true
                 && (root.doc.deviceCapSummary || "") !== ""
        label: QbzSession.tr("Detected device limit", QbzSession.trRev)
        Text {
            text: root.doc.deviceCapSummary || ""
            color: theme.textPrimary
            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
            font.weight: theme.weightMedium
            verticalAlignment: Text.AlignVCenter
        }
    }
    // Fallback disclosure (#638 Decision B): plain informative copy — no
    // warning icon, no error styling. The cap still applies on the assumed
    // common set; it just is not measured truth for this hardware.
    Text {
        visible: root.doc.limitQualityToDevice === true
                 && (root.doc.deviceCapSummary || "") !== ""
                 && root.doc.deviceCapDetected === false
        width: parent.width
        text: QbzSession.tr("Your device's exact limits could not be read on this system, so a common set is assumed. The cap still applies, but may not match your hardware exactly.", QbzSession.trRev)
        color: theme.textMuted
        font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
        wrapMode: Text.WordWrap
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================= OUTPUT ================================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("OUTPUT", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Audio backend", QbzSession.trRev)
        description: QbzSession.tr("The audio stack QBZ routes playback through.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 220
            options: root.doc.backends || []
            currentIndex: root.doc.backendIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("backend", i) }
        }
    }
    // JACK routes through the JACK graph (which resamples to the graph rate),
    // so it can never be bit-perfect — say so instead of letting the
    // bit-perfect toggles below imply otherwise.
    WarningBanner {
        visible: root.doc.backendIsJack === true
        variant: "warning"
        title: QbzSession.tr("JACK is not bit-perfect", QbzSession.trRev)
        body: QbzSession.tr("The JACK backend routes audio through the JACK graph, which resamples to the graph's sample rate. For bit-perfect playback use ALSA (direct) or PipeWire with passthrough.", QbzSession.trRev)
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Output device", QbzSession.trRev)
        description: QbzSession.tr("The DAC or sound device that receives audio.", QbzSession.trRev)
        Row {
            spacing: 8
            // Refresh / release: frees a device QBZ holds exclusively (ALSA
            // direct) and re-enumerates, so a freed or hot-plugged DAC shows
            // up without an app restart.
            SettingsButton { kioskHost: root.kioskHost;
                iconName: "refresh-cw"
                onClicked: QbzBridge.refreshDevices()
            }
            QbzSelect { kioskHost: root.kioskHost;
                menuWidth: 300
                popupWidth: 480
                searchable: true
                options: root.doc.devices || []
                currentIndex: root.doc.deviceIndex || 0
                onSelected: function (i) { QbzBridge.settingsSelect("device", i) }
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.backendIsAlsa === true
        label: QbzSession.tr("ALSA plugin", QbzSession.trRev)
        description: QbzSession.tr("How ALSA opens the device — hw is bit-perfect, plughw converts.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 220
            options: root.doc.alsaPlugins || []
            currentIndex: root.doc.alsaPluginIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("alsa-plugin", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.backendIsAlsa === true && root.doc.alsaDirectSelected === true
        label: QbzSession.tr("Hardware volume control", QbzSession.trRev)
        description: QbzSession.tr("Control volume through a mixer exposed by the selected ALSA device. If unavailable, direct playback stays fixed at 100%.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.alsaHardwareVolume === true
            onToggled: function (v) { QbzBridge.settingsBool("alsa-hardware-volume", v) }
        }
    }
    Text {
        visible: root.doc.backendIsAlsa === true
                 && root.doc.alsaDirectSelected === true
                 && (root.doc.alsaHardwareVolumeMessage || "") !== ""
        width: parent.width
        text: root.doc.alsaHardwareVolumeMessage || ""
        color: (root.doc.alsaHardwareVolumeStatus || "") === "probing"
            ? theme.textMuted : theme.warning
        font.pixelSize: root.kioskHost ? (12) * 1.2 : (12)
        wrapMode: Text.WordWrap
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.backendIsAlsa === true
                 && root.doc.alsaDirectSelected === true
                 && (root.doc.alsaHardwareVolumeSelected || "") !== ""
        label: QbzSession.tr("Selected mixer control", QbzSession.trRev)
        Text {
            text: root.doc.alsaHardwareVolumeSelected || ""
            color: theme.textPrimary
            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
            font.weight: theme.weightMedium
            verticalAlignment: Text.AlignVCenter
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.backendIsAlsa === true
                 && root.doc.alsaDirectSelected === true
                 && ((root.doc.alsaHardwareVolumeStatus || "") === "ambiguous"
                     || (root.doc.alsaHardwareVolumeStatus || "") === "stale"
                     || (root.doc.alsaHardwareVolume === true
                         && root.doc.alsaHardwareVolumeCanChange === true))
                 && (root.doc.alsaHardwareVolumeOptions || []).length > 0
        label: ((root.doc.alsaHardwareVolumeStatus || "") === "ambiguous"
                || (root.doc.alsaHardwareVolumeStatus || "") === "stale")
            ? QbzSession.tr("Choose mixer control", QbzSession.trRev)
            : QbzSession.tr("Change mixer control", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            id: hardwareVolumeSelect
            menuWidth: 260
            popupWidth: 480
            searchable: (root.doc.alsaHardwareVolumeOptions || []).length > 8
            options: root.doc.alsaHardwareVolumeOptions || []
            currentIndex: root.doc.alsaHardwareVolumeIndex !== undefined
                ? root.doc.alsaHardwareVolumeIndex : -1
            onSelected: function (i) {
                QbzBridge.settingsSelect("alsa-hardware-volume-control", i)
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // ALSA only. fc0d35e29 opened this to WASAPI expecting plan B Task 6
        // (DoP over exclusive mode); the spike then measured this DAC refusing
        // 176400, which is the DSD64 DoP carrier, and Task 6 lost its target.
        // Both non-convert modes are `cfg(target_os = "linux")` in the audio
        // thread and answer "DoP playback is Linux-only" with a stream error,
        // so on Windows the row could only ever offer two ways to fail. DSD
        // still plays there -- converted to PCM, which is the default.
        visible: root.doc.backendIsAlsa === true
        label: QbzSession.tr("DSD playback", QbzSession.trRev)
        description: QbzSession.tr("How DSD tracks reach the DAC. WARNING: choose DoP or Native only if your DAC supports it — on any other DAC they play as loud noise. Volume is fixed in DoP/Native mode. Native additionally needs kernel support for the DAC.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 280
            options: root.doc.dsdModes || []
            currentIndex: root.doc.dsdModeIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("dsd-mode", i) }
        }
    }
    // HiFi Wizard — guided bit-perfect DAC setup. The LAST row of OUTPUT,
    // under "Output device", and shown for EVERY backend.
    //
    // TWO DIVERGENCES FROM THE REFERENCE, both owner rulings (2026-08-11):
    // it lives in OUTPUT, not at the end of BIT-PERFECT; and it is NOT gated
    // on `backend-is-pipewire` the way `AudioSettings.slint:282` gates it.
    // The reference hides it off PipeWire because the config it generates is
    // PipeWire/WirePlumber — but that is exactly who needs it: someone on ALSA
    // direct who wants to set their DAC up cannot reach the wizard that would
    // walk them through it. Do not "restore parity" by putting the gate back.
    //
    // Opening it resets the wizard and kicks the audio-stack probe off the UI
    // thread, so there is nothing to arm here.
    SettingRow { kioskHost: root.kioskHost;
        // The generated config is PipeWire/WirePlumber and qbz-dac-wizard-core
        // knows no other stack, so off Linux the wizard cannot finish. Hidden,
        // not disabled: a disabled row invites a bug report. This is NOT the
        // reference's `backend-is-pipewire` gate coming back - within Linux
        // every backend still sees it, which is the owner ruling above.
        visible: QbzShell.isLinux
        label: QbzSession.tr("HiFi Wizard", QbzSession.trRev)
        description: QbzSession.tr("Auto-detect your DACs and set up bit-perfect playback, step by step.", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            iconName: "gandalf"
            text: QbzSession.tr("Open Wizard", QbzSession.trRev)
            onClicked: QbzDacWizard.open()
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // =========================== BIT-PERFECT =============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("BIT-PERFECT", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        // Visible wherever a backend can honour it, enabled only on that
        // backend. Per platform:
        //
        // LINUX: unchanged since 2026-08-27 - always visible, enabled on ALSA.
        //
        // MACOS: System default IS the CoreAudio exclusive backend. With
        // `exclusive_mode` set, qbz-audio backend.rs
        // `create_output_stream_with_exclusive_guard` pins the device, takes
        // Hog Mode and switches its nominal rate to the track (PR #391,
        // shipped in 1.2.11 and never removed). fc0d35e29 hid this row on
        // macOS on the wrong premise that no backend there could honour it;
        // #748 restored it. `backendIsCoreAudio` is published by
        // settings_qt.rs only on macOS, so the `=== true` idiom keeps every
        // other platform exactly where it was.
        //
        // WINDOWS: hidden on purpose, even now that Phase B publishes
        // `backendIsWasapi`. The exclusive path is chosen by the BACKEND
        // dropdown ("WASAPI Exclusive"), not by this toggle -- the selection
        // arm in qbz-player keys on `backend_type` and never reads
        // `exclusive_mode`. Showing the toggle would put a control under the
        // BIT-PERFECT header that changes nothing, which is worse than no
        // control at all when the thing being judged IS bit-perfect. Whether
        // Windows should instead mirror the Linux split (backend = transport,
        // toggle = hw/plughw) is an open design question for the owner.
        visible: QbzShell.isLinux || root.doc.backendIsCoreAudio === true
        label: QbzSession.tr("Exclusive mode", QbzSession.trRev)
        description: QbzSession.tr("Lock the device so no other app can resample it.", QbzSession.trRev)
        rowEnabled: root.doc.backendIsAlsa === true || root.doc.backendIsWasapi === true || root.doc.backendIsCoreAudio === true
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.exclusiveMode === true
            enabled: root.doc.backendIsAlsa === true || root.doc.backendIsWasapi === true || root.doc.backendIsCoreAudio === true
            onToggled: function (v) { QbzBridge.settingsBool("exclusive-mode", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // D-Bus org.freedesktop.ReserveDevice1, which is Linux only. On
        // Windows exclusive mode IS the reservation (Phase B); on macOS it is
        // Hog Mode.
        visible: QbzShell.isLinux
        label: QbzSession.tr("Reserve DAC while running", QbzSession.trRev)
        description: QbzSession.tr("Hold the device reserved so other apps can't grab it.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.reserveDac === true
            onToggled: function (v) { QbzBridge.settingsBool("reserve-dac", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("DAC passthrough", QbzSession.trRev)
        description: QbzSession.tr("Send the bitstream untouched to the DAC.", QbzSession.trRev)
        rowEnabled: root.doc.backendIsPipewire === true
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.dacPassthrough === true
            enabled: root.doc.backendIsPipewire === true
            onToggled: function (v) { QbzBridge.settingsBool("dac-passthrough", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.dacPassthrough === true && root.doc.backendIsPipewire === true
        label: QbzSession.tr("Force bit-perfect", QbzSession.trRev)
        description: QbzSession.tr("Pin the PipeWire quantum and rate for the active track.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.pwForceBitperfect === true
            onToggled: function (v) { QbzBridge.settingsBool("pw-force-bitperfect", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Allow quality fallback", QbzSession.trRev)
        description: QbzSession.tr("Drop to a lower tier when the requested one is unavailable.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.allowQualityFallback === true
            onToggled: function (v) { QbzBridge.settingsBool("allow-quality-fallback", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================= LOUDNESS ==============================
    // The bars' audio flyout keeps its normalization switch; this is the
    // same key with its two companions. Target and clipping guard only show
    // while normalization is on.
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("LOUDNESS", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Volume normalization", QbzSession.trRev)
        description: QbzSession.tr("Level loudness across tracks", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.normalization === true
            onToggled: function (v) { QbzBridge.settingsBool("normalization", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.normalization === true
        label: QbzSession.tr("Target loudness", QbzSession.trRev)
        description: QbzSession.tr("The loudness every track is aligned to. -14 LUFS matches streaming services, -18 LUFS is the ReplayGain reference.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 260
            options: root.doc.normalizationTargets || []
            currentIndex: root.doc.normalizationTargetIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("normalization-target", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.normalization === true
        label: QbzSession.tr("Prevent clipping", QbzSession.trRev)
        description: QbzSession.tr("Cap the boost so the loudest peak stays under full scale. Off lets quiet tracks reach the target even if their peaks clip.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.normalizationPreventClipping === true
            onToggled: function (v) { QbzBridge.settingsBool("normalization-prevent-clipping", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================= STARTUP ===============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("STARTUP", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Sync audio settings on startup", QbzSession.trRev)
        description: QbzSession.tr("Reload saved audio settings into the player when QBZ launches.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.syncAudioOnStartup === true
            onToggled: function (v) { QbzBridge.settingsBool("sync-audio-on-startup", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: root.doc.backendIsPipewire === true
        label: QbzSession.tr("Lock output device", QbzSession.trRev)
        description: QbzSession.tr("Keep external routing intact — skip switching the default sink.", QbzSession.trRev)
        rowEnabled: root.doc.dacPassthrough !== true
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.skipSinkSwitch === true
            enabled: root.doc.dacPassthrough !== true
            onToggled: function (v) { QbzBridge.settingsBool("skip-sink-switch", v) }
        }
    }

    Item { width: 1; height: 20 }

    // Reset — restores Audio + Playback defaults.
    SettingsButton { kioskHost: root.kioskHost;
        text: QbzSession.tr("Reset to defaults", QbzSession.trRev)
        onClicked: QbzBridge.settingsReset()
    }
}
