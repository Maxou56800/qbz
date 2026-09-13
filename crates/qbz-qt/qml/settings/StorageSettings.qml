// Settings > Storage — the caches that have no section of their own: the
// shared artwork cache (new here, 2026-09-13), the lyrics cache (from
// Offline) and the Plex mirror (from Plex's danger zone). Each row still
// rides the bridge verb it always did. The offline downloads stay in
// Settings > Offline and the playback cache in Settings > Playback: both are
// part of how those features work, not just disk.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})
    readonly property var off: doc.offline || ({})
    readonly property var plex: (doc.library || ({})).plex || ({})
    /// The view-level SettingsConfirmHost (SettingsView.qml). Null in previews
    /// — every destructive row guards, so a preview degrades to the
    /// unconfirmed call rather than swallowing the click.
    property var confirmHost: null

    QbzTheme { id: theme }

    spacing: 4

    // ========================== ARTWORK CACHE ============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("ARTWORK CACHE", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Maximum size", QbzSession.trRev)
        description: QbzSession.tr("Album and artist images are kept on disk up to this budget; the least recently shown are dropped first, during use and at startup.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 160
            options: root.off.imageCacheSizes || []
            currentIndex: root.off.imageCacheSizeIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("image-cache-max", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Clear artwork cache", QbzSession.trRev)
        // "{} images using {}" — the shared image cache's own stats.
        description: root.off.imageCacheLoaded === true
            ? QbzSession.tr("{} images using {}", QbzSession.trRev)
                .replace("{}", root.off.imageCacheFiles)
                .replace("{}", root.off.imageCacheSize)
            : ""
        SettingsButton { kioskHost: root.kioskHost;
            danger: true
            text: QbzSession.tr("Clear", QbzSession.trRev)
            onClicked: QbzBridge.settingsString("image-cache-clear", "")
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================= LYRICS ================================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("LYRICS", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Clear lyrics cache", QbzSession.trRev)
        // "{} entries using {}" — the real per-user lyrics.db stats.
        description: root.off.lyricsLoaded === true
            ? QbzSession.tr("{} entries using {}", QbzSession.trRev)
                .replace("{}", root.off.lyricsEntries)
                .replace("{}", root.off.lyricsSize)
            : ""
        SettingsButton { kioskHost: root.kioskHost;
            danger: true
            text: QbzSession.tr("Clear", QbzSession.trRev)
            onClicked: QbzBridge.settingsString("lyrics-cache-clear", "")
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================ PLEX CACHE =============================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PLEX CACHE", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Clear cache", QbzSession.trRev)
        description: QbzSession.tr("Remove cached Plex libraries and tracks. Your sign-in is kept.", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            danger: true
            text: QbzSession.tr("Clear cache", QbzSession.trRev)
            enabled: root.plex.hasToken === true
            // plex_auth.rs:997-1001 — one prompt, sign-in kept.
            onClicked: {
                if (!root.confirmHost) {
                    QbzBridge.settingsString("plex-clear-cache", "")
                    return
                }
                root.confirmHost.ask(
                    QbzSession.tr("Clear Plex cache?", QbzSession.trRev),
                    QbzSession.tr("This removes cached Plex libraries and tracks. Your sign-in is kept.", QbzSession.trRev),
                    QbzSession.tr("Clear cache", QbzSession.trRev),
                    function () { QbzBridge.settingsString("plex-clear-cache", "") })
            }
        }
    }
}
