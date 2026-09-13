// Settings > Appearance — the QML port of crates/qbz-ui/ui/settings/
// AppearanceSettings.slint. Every control rides the settingsJson document
// (root.doc) and the settingsBool/Select/String invokables — never local
// truth.
//
// ── THE GROUPING IS THE OWNER'S, NOT THE SLINT'S (2026-08-21) ─────────────
// Row wiring is still 1:1 with the reference — nothing here changed what a
// control DOES — but the groups were re-cut, so do not "restore parity" by
// reading the group order out of AppearanceSettings.slint:
//
//   THEME                  the picker and the rows that exist only for it
//                          (auto's Source/Image/Regenerate, custom's editor)
//   TYPOGRAPHY & LANGUAGE  Language · Interface size · Font (NEW)
//   NAVIGATION             was LIBRARY & VISUALS, minus the visual rows;
//                          gained the three Purchases/menu rows from
//                          PLAYER VIEWS
//   SEARCH                 NEW — Intelligent Search came out of THEME,
//                          Immersive search out of TYPOGRAPHY & LANGUAGE
//   PLAYER & VISUALS       was PLAYER VIEWS; gained the visual rows
//   WINDOW                 WINDOW TITLE + TITLE BAR + WINDOW CONTROLS, merged
//   SYSTEM INTEGRATION     NOTIFICATIONS + SYSTEM TRAY, merged
//   RENDERER               unchanged
//
// FOUR rows are now ABSENT rather than rendered-and-disabled, because a
// control that cannot do anything should not be on screen: Purchases in
// title bar (Show Purchases off), Window controls position (system title bar
// on), Close to tray and Tray icon variant (tray icon off). Each keeps its
// stored value, so turning the parent back on restores the user's answer.
//
// The THEME row is live: QbzShell.themeSlug / themeListJson / themeFilter
// (theme_qt.rs) and themeSet() repaints the whole app through QbzTheme.qml
// (the Slint theme::push_colors equivalent).
//
// The "Auto (dynamic)" block is complete: Source · Select Image... (native
// rfd picker through `settingsString("auto-theme-select-image")`) · the
// "Detected: <desktop>" hint · Regenerate.
//
// Not shipped, 1:1 the owner's cut: the commented-out Slint blocks (immersive
// background / panels / FPS, window-title template). If the owner reopens the
// immersive block in Slint, it gets ported on both sides at once.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    property bool kioskHost: false

    id: root

    property var doc: ({})

    QbzTheme { id: theme }

    spacing: 4

    // --- Theme row state (theme_qt.rs via the bridge) --------------------
    SettingRow { kioskHost: root.kioskHost;
        fitDescription: true
        label: QbzSession.tr("Visual resource profile", QbzSession.trRev)
        description: QbzSession.tr("Low uses the Small player, compact headers, no dynamic background, header gradient, spectrum, animated play indicator, sidebar collages or optional track artwork. Default restores these visual defaults. Editing a managed option selects Custom. Audio quality and playback memory are independent.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 240
            options: [QbzSession.tr("Low", QbzSession.trRev), QbzSession.tr("Default", QbzSession.trRev), QbzSession.tr("Custom", QbzSession.trRev)]
            currentIndex: Math.max(0, ["low", "default", "custom"].indexOf(root.doc.appearanceProfile || "default"))
            onSelected: function(i) { QbzBridge.settingsString("appearance-profile", ["low", "default", "custom"][i]) }
        }
    }
    readonly property var themeEntries: {
        try {
            return JSON.parse(QbzShell.themeListJson)
        } catch (e) {
            return []
        }
    }
    // 0 All / 1 Dark / 2 Light; the Auto/Custom synthetics only list under
    // All (theme.rs:217-227).
    readonly property var filteredThemes: {
        const f = QbzShell.themeFilter
        const out = []
        for (let i = 0; i < themeEntries.length; i++) {
            const e = themeEntries[i]
            const synthetic = e.slug === "auto" || e.slug === "custom"
            if (f === 1 && (e.isLight || synthetic)) continue
            if (f === 2 && (!e.isLight || synthetic)) continue
            out.push(e)
        }
        return out
    }
    readonly property var filteredLabels: filteredThemes.map(function (e) { return e.label })
    readonly property int currentThemeIndex: {
        for (let i = 0; i < filteredThemes.length; i++) {
            if (filteredThemes[i].slug === QbzShell.themeSlug) return i
        }
        return -1
    }
    readonly property bool tbLocked: doc.hideTitleBar === true || doc.useSystemTitleBar === true

    // ============================ THEME ==================================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("THEME", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Theme", QbzSession.trRev)
        Row {
            spacing: 8
            // Theme list filter cycle (All / Dark / Light).
            SettingsButton { kioskHost: root.kioskHost;
                iconName: QbzShell.themeFilter === 1 ? "moon"
                    : QbzShell.themeFilter === 2 ? "sun" : "sun-moon"
                onClicked: QbzShell.themeSetFilter((QbzShell.themeFilter + 1) % 3)
            }
            QbzSelect { kioskHost: root.kioskHost;
                menuWidth: 220
                // 36 registered themes — a name filter (Slint parity).
                searchable: true
                options: root.filteredLabels
                currentIndex: root.currentThemeIndex
                onSelected: function (i) {
                    if (i >= 0 && i < root.filteredThemes.length)
                        QbzShell.themeSet(root.filteredThemes[i].slug)
                }
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Album header gradient", QbzSession.trRev)
        description: QbzSession.tr("Use artwork-derived blur as a backdrop in album and artist detail views.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.albumHeaderGradient === true
            onToggled: function (v) { QbzBridge.settingsBool("album-header-gradient", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Compact header", QbzSession.trRev)
        description: QbzSession.tr("Show less data in album headers.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.compactAlbumHeader === true
            onToggled: function (v) { QbzBridge.settingsBool("compact-album-header", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Dynamic background", QbzSession.trRev)
        description: QbzSession.tr("Animated album-art background behind the whole app. High resource use — GPU accelerated.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.appBackgroundModes || []
            currentIndex: root.doc.appBackgroundIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("app-background", i) }
        }
    }
    // Auto-theme rows (the "auto" theme only).
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "auto"
        label: QbzSession.tr("Source", QbzSession.trRev)
        description: QbzSession.tr("Generate a color theme from your system wallpaper or a custom image", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.autoThemeSources || []
            currentIndex: root.doc.autoThemeSourceIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("auto-theme-source", i) }
        }
    }
    // "Custom Image" only (source index 2) — AppearanceSettings.slint:283-292.
    // The row's LABEL is the picked path, which is how the reference shows
    // what is currently in use; with nothing picked it repeats the button's
    // text. Both strings already exist in the catalogs.
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "auto"
            && (root.doc.autoThemeSourceIndex || 0) === 2
        label: (root.doc.autoThemeImagePath || "") !== ""
            ? root.doc.autoThemeImagePath
            : QbzSession.tr("Select Image...", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            text: QbzSession.tr("Select Image...", QbzSession.trRev)
            // The action seam for button rows is settingsString with an empty
            // payload — the `library-pick-folder` precedent
            // (LibraryFolderTable.qml:111).
            onClicked: QbzBridge.settingsString("auto-theme-select-image", "")
        }
    }
    // The detected desktop, with the experimental caveat under it
    // (`:294-297`). Hint only — no control, and it hides when the shared
    // detector cannot name a desktop.
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "auto"
            && (root.doc.autoThemeDetectedDe || "") !== ""
        label: QbzSession.tr("Detected: ", QbzSession.trRev)
            + (root.doc.autoThemeDetectedDe || "")
        description: QbzSession.tr("Experimental: theme may not match your system exactly.", QbzSession.trRev)
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "auto"
        label: QbzSession.tr("Regenerate", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            text: QbzSession.tr("Regenerate", QbzSession.trRev)
            onClicked: QbzShell.themeSet(QbzShell.themeSlug)
        }
    }

    // Custom-theme editor rows (the appended "Custom" theme only), 1:1 with
    // AppearanceSettings.slint:313-425. "Start from current theme" re-seeds
    // the base from the applied palette; the "Dark theme" toggle flips
    // polarity; the grid below edits each base token and the rest of the
    // palette is derived live in Rust on every change.
    //
    // There is deliberately NO save / save-as, NO delete, NO named-theme list
    // and NO import/export: the model is ONE implicit custom theme that
    // autosaves, and the reference has none of those affordances either.
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "custom"
        label: QbzSession.tr("Start from current theme", QbzSession.trRev)
        description: QbzSession.tr("Copy the colors of the currently applied theme into the editor as a starting point.", QbzSession.trRev)
        SettingsButton { kioskHost: root.kioskHost;
            text: QbzSession.tr("Use current colors", QbzSession.trRev)
            onClicked: {
                QbzShell.customSeedFromCurrent()
                customThemeEditor.closePicker()
            }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.themeSlug === "custom"
        label: QbzSession.tr("Dark theme", QbzSession.trRev)
        description: QbzSession.tr("Set the overall light or dark polarity. Affects derived shades, borders and overlays.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: customThemeEditor.isDark
            onToggled: function (v) { QbzShell.customToggleDark(v) }
        }
    }
    // NOT inside a SettingRow: that control hardcodes 52/64px and centres one
    // child, which would clip the ~250px picker. The reference keeps the grid
    // outside its own rows for the same reason.
    CustomThemeEditor { kioskHost: root.kioskHost;
        id: customThemeEditor
        visible: QbzShell.themeSlug === "custom"
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ===================== TYPOGRAPHY & LANGUAGE =========================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("TYPOGRAPHY & LANGUAGE", QbzSession.trRev) }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Language", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.languages || []
            currentIndex: root.doc.languageIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("language", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Interface size", QbzSession.trRev)
        description: QbzSession.tr("Scales the whole interface, like browser zoom. Small fits more content on screen, Large and Extra large improve readability (requires restart)", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.uiScales || []
            currentIndex: root.doc.uiScaleIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("ui-scale", i) }
        }
    }
    // The app-wide typeface. The four named families are bundled — the same
    // set the lyrics panel offers (shell/LyricsControlsFlyout.qml) — while
    // "System" deliberately leaves Qt's operating-system default untouched.
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Font", QbzSession.trRev)
        description: QbzSession.tr("The typeface used across the app. System follows your operating system; the other choices are bundled with QBZ (requires restart)", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.appFonts || []
            currentIndex: root.doc.appFontIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("app-font", i) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ========================= PLAYER & VISUALS =========================
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("PLAYER & VISUALS", QbzSession.trRev) }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show volume +/- buttons", QbzSession.trRev)
        description: QbzSession.tr("Add discrete plus and minus buttons next to the volume slider in the player bar.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.showVolumeSteppers === true
            onToggled: function (v) { QbzBridge.settingsBool("show-volume-steppers", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show 10-second skip buttons", QbzSession.trRev)
        description: QbzSession.tr("Add ±10 s buttons beside Previous and Next in the player bar.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.showSkipTen === true
            onToggled: function (v) { QbzBridge.settingsBool("show-skip-ten", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Startup page", QbzSession.trRev)
        description: QbzSession.tr("Choose which page to show when the app starts", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.startupPages || []
            currentIndex: root.doc.startupPageIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("startup-page", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Immersive default view", QbzSession.trRev)
        description: QbzSession.tr("Which immersive view opens by default. 'Remember last' restores your last view.", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.immersiveDefaultViews || []
            currentIndex: root.doc.immersiveDefaultViewIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("immersive-default-view", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Mini player default view", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.miniDefaultViews || []
            currentIndex: root.doc.miniDefaultViewIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("mini-default-view", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Play indicator animation", QbzSession.trRev)
        description: QbzSession.tr("Animate the now-playing row with equalizer bars. Off (default) shows a static pause icon with an accent edge mark — lighter on CPU.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.playIndicatorAnimation === true
            onToggled: function (v) { QbzBridge.settingsBool("play-indicator-animation", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show featured artists in track rows", QbzSession.trRev)
        description: QbzSession.tr("Append \"feat. …\" with the track's featured performers to the artist line of Qobuz track rows. Off by default.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.showFeaturedArtists === true
            onToggled: function (v) { QbzBridge.settingsBool("show-featured-artists", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // Parked for a later visual pass. Keep the preference wiring intact so
        // restoring the experiment does not require a settings migration.
        visible: false
        label: QbzSession.tr("Track waveform", QbzSession.trRev)
        description: QbzSession.tr("Show the full-track waveform in the player seek bar.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.seekbarWaveform === true
            onToggled: function (v) { QbzBridge.settingsBool("seekbar-waveform", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show playlist cover collages in sidebar", QbzSession.trRev)
        description: QbzSession.tr("Render a 2×2 thumbnail of track covers next to each playlist. Disable on low-end machines to skip the extra images.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.sidebarPlaylistCollage === true
            onToggled: function (v) { QbzBridge.settingsBool("sidebar-playlist-collage", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show album artwork in Queue track list", QbzSession.trRev)
        description: QbzSession.tr("Replace queue track numbers with album cover thumbnails. Off by default.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.queueTrackArtwork === true
            onToggled: function (v) { QbzBridge.settingsBool("queue-track-artwork", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show album artwork in Library track list", QbzSession.trRev)
        description: QbzSession.tr("Display the album cover thumbnail between the track number and title. Off by default.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.libraryTrackArtwork === true
            onToggled: function (v) { QbzBridge.settingsBool("library-track-artwork", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show album artwork in Local Library track list", QbzSession.trRev)
        description: QbzSession.tr("Display the album cover thumbnail next to each track in the Tracks and Folders views. Off by default — large libraries pay a per-row image-decode cost.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.localLibraryTrackArtwork === true
            onToggled: function (v) { QbzBridge.settingsBool("local-library-track-artwork", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ============================== WINDOW ==============================
    // OWNER RULING 2026-08-05, and a DELIBERATE divergence from the
    // reference: Slint hides the whole TITLE BAR and WINDOW CONTROLS groups on
    // macOS (AppearanceSettings.slint:723-800). The owner wants the SWITCH
    // there — system title bar vs the QBZ header with the native traffic
    // lights overlaid on it — so only the two rows that genuinely cannot work
    // on macOS are hidden:
    //
    //   Hide title bar          — frameless with no controls at all; it would
    //                             take the traffic lights with it, and it is a
    //                             tiling-WM affordance by its own description.
    //   Window controls position — places the DRAWN cluster, which macOS never
    //                             draws; AppKit owns where the lights sit.
    //
    // "Show window controls" stays: it gates whether the cluster is drawn at
    // all, which is a meaningful answer on every platform.
    //
    // The three groups this used to be (WINDOW TITLE / TITLE BAR / WINDOW
    // CONTROLS) are ONE group as of 2026-08-21: they are all answers to
    // "what does the window frame look like", and split three ways each held
    // one or two rows.
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("WINDOW", QbzSession.trRev) }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Use system title bar", QbzSession.trRev)
        description: QbzSession.tr("Keep your system's native window decorations. Turn off to use the QBZ header as the title bar, with its own window controls and drag support. Takes effect after restarting QBZ.", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.useSystemTitleBar === true
            onToggled: function (v) { QbzBridge.settingsBool("use-system-title-bar", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // ABSENT, not disabled, while the system title bar is in charge:
        // there is no QBZ-drawn cluster to place then, so the row cannot do
        // anything (owner 2026-08-21). `rowEnabled` still covers the OTHER
        // way it goes inert — "Hide title bar", which removes the cluster
        // rather than handing it to the system.
        visible: !QbzShell.isMacos && root.doc.useSystemTitleBar !== true
        label: QbzSession.tr("Window controls position", QbzSession.trRev)
        description: QbzSession.tr("Place the window control buttons on the left or right side of the title bar", QbzSession.trRev)
        rowEnabled: !root.tbLocked
        QbzSelect { kioskHost: root.kioskHost;
            enabled: !root.tbLocked
            menuWidth: 160
            options: root.doc.wcPositions || []
            currentIndex: root.doc.wcPositionIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("wc-position", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // Also hidden on macOS (owner, 2026-08-05, after seeing it there):
        // it gates the DRAWN cluster, which macOS never draws, and its own
        // description offers a window-manager rationale that has no meaning
        // on a Mac. Three rows hidden there now, not two.
        visible: !QbzShell.isMacos
        label: QbzSession.tr("Show window controls", QbzSession.trRev)
        description: QbzSession.tr("Show minimize, maximize, and close buttons in the title bar. Disable if your window manager handles these.", QbzSession.trRev)
        rowEnabled: !root.tbLocked
        QbzToggle { kioskHost: root.kioskHost;
            enabled: !root.tbLocked
            checked: root.doc.showWindowControls === true
            onToggled: function (v) { QbzBridge.settingsBool("show-window-controls", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: !QbzShell.isMacos
        label: QbzSession.tr("Hide title bar", QbzSession.trRev)
        description: QbzSession.tr("Frameless window without window controls or header drag (for tiling window manager users)", QbzSession.trRev)
        rowEnabled: root.doc.useSystemTitleBar !== true
        QbzToggle { kioskHost: root.kioskHost;
            enabled: root.doc.useSystemTitleBar !== true
            checked: root.doc.hideTitleBar === true
            onToggled: function (v) { QbzBridge.settingsBool("hide-title-bar", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Show track in window title", QbzSession.trRev)
        visible: !QbzShell.isMacos || QbzShell.systemTitleBar
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.windowTitleShow === true
            onToggled: function (v) { QbzBridge.settingsBool("window-title-show", v) }
        }
    }

    SettingsSpacer { }
    SettingsDivider { }
    SettingsSpacer { }

    // ======================== SYSTEM INTEGRATION ========================
    // NOTIFICATIONS and SYSTEM TRAY merged 2026-08-21: both are "how QBZ
    // shows up in the rest of the desktop". The tray rows keep their own
    // platform wording (macOS calls it the menu bar).
    GroupHeader { kioskHost: root.kioskHost; text: QbzSession.tr("SYSTEM INTEGRATION", QbzSession.trRev) }

    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("In-app toasts notifications", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.inAppToasts === true
            onToggled: function (v) { QbzBridge.settingsBool("in-app-toasts", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("System Notifications", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.systemNotifications === true
            onToggled: function (v) { QbzBridge.settingsBool("system-notifications", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Enable tray icon", QbzSession.trRev)
        description: QbzSession.tr("Show icon in system tray (requires restart)", QbzSession.trRev)
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.trayEnable === true
            onToggled: function (v) { QbzBridge.settingsBool("tray-enable", v) }
        }
    }
    // A separate "Minimize to tray" toggle is deliberately absent, 1:1 with
    // AppearanceSettings.slint:897-903: redirecting the window-manager minimize
    // button to the tray means owning that button, and Wayland forbids a client
    // from intercepting the compositor's minimize. The reference HIDES the
    // option rather than showing it disabled, and this row reads "Close to
    // tray". The setting still round-trips through the store (the
    // "tray-minimize-to-tray" write arm, src/settings_qt.rs) so a Qt session
    // cannot drop a value the Slint build wrote.
    SettingRow { kioskHost: root.kioskHost;
        label: QbzSession.tr("Close to tray", QbzSession.trRev)
        // macOS says "menu bar"; the LABEL stays "Close to tray" on both
        // platforms (AppearanceSettings.slint:905-908).
        description: root.doc.isMacos === true
                     ? QbzSession.tr("Keep playing in the menu bar instead of quitting when you close the window", QbzSession.trRev)
                     : QbzSession.tr("Keep playing in the tray instead of quitting when you close the window", QbzSession.trRev)
        // Absent while there is no tray icon to close to (owner
        // 2026-08-21). The value still round-trips through the store, so
        // turning the tray back on restores the answer the user gave.
        visible: root.doc.trayEnable === true && root.doc.isMacos !== true
        QbzToggle { kioskHost: root.kioskHost;
            checked: root.doc.trayCloseToTray === true
            onToggled: function (v) { QbzBridge.settingsBool("tray-close-to-tray", v) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // Absent while the tray is off — there is no icon to give a
        // variant to (owner 2026-08-21).
        //
        // And absent on Windows, where the notification area draws the
        // executable's own icon: Windows owns light/dark there and the tray
        // has no theme arm, so every choice here would render, persist and
        // change nothing.
        visible: root.doc.trayEnable === true && QbzShell.isWindows !== true
        label: QbzSession.tr("Tray icon variant", QbzSession.trRev)
        description: QbzSession.tr("Pick a mono glyph to match your panel (Plasma, GNOME's permanently dark top bar) or the full colour vinyl logo", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 160
            options: root.doc.trayIconThemes || []
            currentIndex: root.doc.trayIconThemeIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("tray-icon-theme", i) }
        }
    }

    // The group's own separator is gated too, or the panel ends on a divider
    // with nothing under it (the reference gates its three spacer/divider
    // elements the same way, AppearanceSettings.slint:1015-1017).
    SettingsSpacer { visible: QbzShell.isLinux }
    SettingsDivider { visible: QbzShell.isLinux }
    SettingsSpacer { visible: QbzShell.isLinux }

    // =========================== RENDERER ================================
    // Linux only, 1:1 with the reference: every element of this group —
    // including the Preferred-GPU row below — is gated on
    // `renderer-setting-visible`, which main.rs:318 seeds from
    // `cfg!(target_os = "linux")`. macOS is always Skia/Metal and Windows
    // negotiates its own backend, so off Linux the selector offered choices
    // that changed nothing (AppearanceSettings.slint:1011-1043).
    GroupHeader { kioskHost: root.kioskHost;
        visible: QbzShell.isLinux
        text: QbzSession.tr("RENDERER", QbzSession.trRev)
    }
    SettingRow { kioskHost: root.kioskHost;
        visible: QbzShell.isLinux
        label: QbzSession.tr("Rendering backend", QbzSession.trRev)
        description: QbzSession.tr("Auto picks the best renderer for your graphics hardware. Only change this if the app feels slow or renders incorrectly (requires restart)", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 200
            options: root.doc.renderers || []
            currentIndex: root.doc.rendererIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("renderer", i) }
        }
    }
    SettingRow { kioskHost: root.kioskHost;
        // `gpuSelectable`, not `isLinux`: same answer today, but it comes from
        // the SAME place as the code that applies the choice
        // (`renderer_qt::gpu_selectable`), so the row cannot outlive the
        // capability. macOS is false because QRhi hardcodes
        // `MTLCreateSystemDefaultDevice` — there is no env and no API, so the
        // row hides rather than lying (PARITY-DEBT #83 scoping).
        visible: root.doc.gpuSelectable === true
        label: QbzSession.tr("Preferred GPU", QbzSession.trRev)
        // The description states the Vulkan coupling out loud: picking a
        // non-default GPU changes the RENDERER too, and a setting that quietly
        // moves another setting is exactly the kind of surprise this row must
        // not spring. Vulkan is the only Qt backend that can select a device —
        // measured, see src/renderer_qt.rs.
        description: QbzSession.tr("Which GPU renders the app. Only the GPUs actually present are listed. Choosing one other than the default also switches the renderer to Vulkan — the only backend Qt can select a GPU on (requires restart)", QbzSession.trRev)
        QbzSelect { kioskHost: root.kioskHost;
            menuWidth: 260
            options: root.doc.gpuPowers || []
            currentIndex: root.doc.gpuPowerIndex || 0
            onSelected: function (i) { QbzBridge.settingsSelect("gpu-power", i) }
        }
    }

    Item { width: 1; height: 40 }
}
