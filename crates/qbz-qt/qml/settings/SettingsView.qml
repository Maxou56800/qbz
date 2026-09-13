// Settings — the QML port of crates/qbz-ui/ui/settings/SettingsView.slint:
// a 92px title header, a 232px left sub-navigation, and the active panel in
// a touch-draggable Flickable with a ListScrollbar replica.
//
// This file is the COMPOSITION ROOT only. Every section is its own file in
// this directory, 1:1 with the Slint's own per-section .slint files:
//
//   0 Audio          -> AudioSettings.qml
//   1 Playback       -> PlaybackSettings.qml
//   2 Appearance     -> AppearanceSettings.qml
//   3 Offline        -> OfflineSettings.qml
//  13 Storage        -> StorageSettings.qml (every on-disk cache; shown
//                       right after Offline)
//   4 Local Library  -> LocalLibrarySettings.qml (+ PlexSettings.qml)
//   5 Blacklist      -> BlacklistSettings.qml
//   6 Integrations   -> IntegrationsSettings.qml
//   7 Developer      -> DeveloperSettings.qml
//   8 Flatpak/Snap   -> SandboxSettings.qml (only on a sandboxed install)
//   9 Import/Export  -> ImportExportSettings.qml (shown between
//                       Integrations and Developer; index 9 keeps the
//                       persisted section numbers of 0-8 stable)
//
// All state is ONE JSON document (QbzBridge.settingsJson, settings_qt.rs
// SettingsDoc). Controls never keep local truth: they call the
// settingsBool/Select/Slider/String invokables, Rust persists + applies +
// republishes, and the rows re-render from the new document (the Slint
// "single source of truth in SettingsState" pattern).
//
// KEYBOARD FOCUS CONTRACT (for every new setting): Qt Quick has no HTML-like
// numeric tabindex; its tab index is the declaration order of enabled,
// visible items with `activeFocusOnTab`. Therefore settings and their controls
// MUST be declared in the same top-to-bottom order in which they are drawn and
// MUST use the shared QbzToggle/QbzSelect/QbzSlider/QbzLineEdit/SettingsButton/
// QbzCheckbox controls. Do not put a bare MouseArea on a new setting action:
// the shared controls provide the focus ring and Enter/Space activation, and
// accept Space so it cannot bubble into AppShell's global play/pause hotkey.
// If a custom interactive item is genuinely necessary, it inherits this same
// contract. This comment is intentionally at the composition root so both
// contributors and coding agents encounter it when adding a settings row.
//
// The NavButtons row inside the 92px header is a 0px placeholder — nav
// history lives in the global HeaderBar in this port, like every other view.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    property bool kioskHost: false

    id: root

    QbzTheme { id: theme }

    // The parsed settingsJson document ({} until the first publish lands).
    property var doc: ({})
    // Active sub-section (display order — see the header comment).
    //
    // A BINDING onto bridge state, never local truth. The content Loader
    // unmounts this whole view when the user navigates away, so a plain
    // `property int section: 0` was lost on every round trip — the Blacklist
    // panel's "Manage" chevron opens the manager VIEW, and coming Back landed
    // on Audio. See `bridge.rs`'s note on `settings_section`.
    //
    // Consequently every sub-nav row calls `QbzBridge.settingsSetSection(n)`
    // and NOTHING assigns `root.section` directly: in QML an imperative
    // assignment destroys the binding it lands on, so a single surviving
    // `root.section = n` would work once and then re-introduce the bug one
    // navigation later, in a way that looks fixed under casual testing.
    readonly property var kioskSections: [
        { label: QbzSession.tr("Audio", QbzSession.trRev), section: 0 },
        { label: QbzSession.tr("Playback", QbzSession.trRev), section: 1 },
        { label: QbzSession.tr("Appearance", QbzSession.trRev), section: 2 },
        { label: QbzSession.tr("Navigation", QbzSession.trRev), section: 10 },
        { label: QbzSession.tr("Offline", QbzSession.trRev), section: 3 },
        { label: QbzSession.tr("Storage", QbzSession.trRev), section: 13 },
        { label: QbzSession.tr("Local Library", QbzSession.trRev), section: 4 },
        { label: QbzSession.tr("Blacklist", QbzSession.trRev), section: 5 },
        { label: QbzSession.tr("Integrations", QbzSession.trRev), section: 6 },
        { label: QbzSession.tr("Import / Export", QbzSession.trRev), section: 9 },
        { label: QbzSession.tr("Updates", QbzSession.trRev), section: 12 },
        { label: QbzSession.tr("Developer", QbzSession.trRev), section: 7 }
    ].concat(QbzOrbit.enabled ? [{ label: "Orbit", section: 11 }] : []).concat(root.sandboxed ? [{ label: (doc.dev || ({})).installMethod === "snap"
        ? QbzSession.tr("Snap", QbzSession.trRev) : QbzSession.tr("Flatpak", QbzSession.trRev), section: 8 }] : [])
    readonly property int section: QbzBridge.settingsSection === 11 && !QbzOrbit.enabled ? 0 : QbzBridge.settingsSection
    readonly property bool migrationRunning:
        (doc.importExport || ({})).migrationRunning === true

    readonly property bool sandboxed: {
        const m = (doc.dev || ({})).installMethod || ""
        return m === "flatpak" || m === "snap"
    }

    function reload() {
        try {
            root.doc = JSON.parse(QbzBridge.settingsJson)
        } catch (e) {
            root.doc = ({})
        }
    }
    Component.onCompleted: reload()
    Connections {
        target: QbzBridge
        function onSettingsJsonChanged() { root.reload() }
    }

    // ============================ the view ================================
    Column {
        id: settingsContent
        anchors.fill: parent
        spacing: 0
        // A migration may be hidden, but its settings tree remains inert until
        // Rust finishes. Global navigation is outside this view and stays live.
        enabled: !root.migrationRunning

        // --- Header (92px; NavButtons is a 0px placeholder in this port) --
        Item {
            width: parent.width
            height: root.kioskHost ? 64 : 92
            Text {
                x: root.kioskHost ? 16 : 32
                // padding-top 11 + 12px gap below the (0px) NavButtons row.
                y: root.kioskHost ? (64 - height) / 2 : 23
                text: QbzSession.tr("Settings", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: root.kioskHost ? (theme.fontTitle) * 1.2 : (theme.fontTitle)
                font.weight: theme.weightBold
            }
            // The kiosk section selector. The "Share logs" icon button that
            // used to sit to its right is gone: it called QbzShell.logOpen(),
            // whose viewer modal is mounted by the desktop AppShell only, so
            // on the kiosk it was a dead control (owner report 2026-09-07).
            QbzSelect {
                visible: root.kioskHost
                kioskHost: true
                anchors.right: parent.right
                anchors.rightMargin: 12
                anchors.verticalCenter: parent.verticalCenter
                menuWidth: Math.max(180, Math.min(300, parent.width - 270))
                searchable: options.length > 8
                options: root.kioskSections.map(function (s) { return s.label })
                currentIndex: root.kioskSections.findIndex(function (s) { return s.section === root.section })
                onSelected: function (i) {
                    QbzBridge.settingsSetSection(root.kioskSections[i].section)
                    flick.contentY = 0
                }
            }
        }

        // --- Sub-nav + active panel ---------------------------------------
        Row {
            width: parent.width
            height: parent.height - (root.kioskHost ? 64 : 92)

            // Left sub-navigation (232px).
            Item {
                id: subNav
                visible: !root.kioskHost
                width: root.kioskHost ? 0 : 232
                height: parent.height

                component SubNavItem: Rectangle {
                    property string name: ""
                    property string label: ""
                    property bool active: false
                    signal clicked()

                    width: parent ? parent.width : 0
                    height: 38
                    radius: theme.radiusSm
                    color: active ? theme.surfaceElevated
                        : snArea.containsMouse ? theme.surfaceHover : "transparent"
                    activeFocusOnTab: visible && enabled
                    border.width: activeFocus ? 2 : 0
                    border.color: theme.accent
                    Accessible.role: Accessible.Button
                    Accessible.name: label
                    Accessible.onPressAction: clicked()

                    Keys.onPressed: function (event) {
                        if (!event.isAutoRepeat
                                && (event.key === Qt.Key_Space
                                    || event.key === Qt.Key_Return
                                    || event.key === Qt.Key_Enter)) {
                            clicked()
                            event.accepted = true
                        }
                    }
                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 10
                        QbzIcon {
                            name: parent.parent.name
                            width: 16
                            height: 16
                            anchors.verticalCenter: parent.verticalCenter
                            // Theme tokens on both arms — the row sits on
                            // surface-elevated / surface-hover, so the active
                            // glyph is "textPrimary" and NOT the fixed-white
                            // "primary" bake (white-on-white on light themes).
                            tintName: parent.parent.active ? "textPrimary" : "secondary"
                        }
                        Text {
                            height: parent.height
                            text: parent.parent.label
                            color: parent.parent.active ? theme.textPrimary : theme.textSecondary
                            font.pixelSize: root.kioskHost ? (theme.fontBody) * 1.2 : (theme.fontBody)
                            font.weight: parent.parent.active ? theme.weightSemibold : theme.weightRegular
                            verticalAlignment: Text.AlignVCenter
                        }
                    }
                    MouseArea {
                        id: snArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onPressed: parent.forceActiveFocus()
                        onClicked: parent.clicked()
                    }
                }

                Column {
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.leftMargin: 24
                    anchors.rightMargin: 12
                    anchors.topMargin: 4
                    spacing: 4

                    SubNavItem {
                        name: "volume-2"
                        label: QbzSession.tr("Audio", QbzSession.trRev)
                        active: root.section === 0
                        onClicked: QbzBridge.settingsSetSection(0)
                    }
                    SubNavItem {
                        name: "play-fill"
                        label: QbzSession.tr("Playback", QbzSession.trRev)
                        active: root.section === 1
                        onClicked: QbzBridge.settingsSetSection(1)
                    }
                    SubNavItem {
                        name: "layers"
                        label: QbzSession.tr("Appearance", QbzSession.trRev)
                        active: root.section === 2
                        onClicked: QbzBridge.settingsSetSection(2)
                    }
                    SubNavItem {
                        name: "compass"
                        label: QbzSession.tr("Navigation", QbzSession.trRev)
                        active: root.section === 10
                        onClicked: QbzBridge.settingsSetSection(10)
                    }
                    SubNavItem {
                        name: "cloud-download"
                        label: QbzSession.tr("Offline", QbzSession.trRev)
                        active: root.section === 3
                        onClicked: QbzBridge.settingsSetSection(3)
                    }
                    SubNavItem {
                        name: "folder"
                        label: QbzSession.tr("Storage", QbzSession.trRev)
                        active: root.section === 13
                        onClicked: QbzBridge.settingsSetSection(13)
                    }
                    SubNavItem {
                        name: "hard-drive"
                        label: QbzSession.tr("Local Library", QbzSession.trRev)
                        active: root.section === 4
                        onClicked: QbzBridge.settingsSetSection(4)
                    }
                    SubNavItem {
                        name: "blind-eye"
                        label: QbzSession.tr("Blacklist", QbzSession.trRev)
                        active: root.section === 5
                        onClicked: QbzBridge.settingsSetSection(5)
                    }
                    SubNavItem {
                        name: "refresh-cw"
                        label: QbzSession.tr("Integrations", QbzSession.trRev)
                        active: root.section === 6
                        onClicked: QbzBridge.settingsSetSection(6)
                    }
                    SubNavItem {
                        name: "import"
                        label: QbzSession.tr("Import / Export", QbzSession.trRev)
                        active: root.section === 9
                        onClicked: QbzBridge.settingsSetSection(9)
                    }
                    SubNavItem {
                        name: "refresh-cw"
                        label: QbzSession.tr("Updates", QbzSession.trRev)
                        active: root.section === 12
                        onClicked: QbzBridge.settingsSetSection(12)
                    }
                    SubNavItem {
                        name: "bug"
                        label: QbzSession.tr("Developer", QbzSession.trRev)
                        active: root.section === 7
                        onClicked: QbzBridge.settingsSetSection(7)
                    }
                    SubNavItem {
                        visible: QbzOrbit.enabled
                        name: "compass"
                        label: "Orbit"
                        active: root.section === 11
                        onClicked: QbzBridge.settingsSetSection(11)
                    }
                    // Sandboxed installs only (Flatpak / Snap permissions).
                    SubNavItem {
                        visible: root.sandboxed
                        name: "info"
                        label: (root.doc.dev || ({})).installMethod === "snap"
                            ? QbzSession.tr("Snap", QbzSession.trRev)
                            : QbzSession.tr("Flatpak", QbzSession.trRev)
                        active: root.section === 8
                        onClicked: QbzBridge.settingsSetSection(8)
                    }
                }

                // Always-visible log affordance, pinned to the bottom of the
                // sub-nav (people filing issues could not find it under
                // Developer). Opens the on-disk log — see DeveloperSettings.
                Column {
                    anchors.bottom: parent.bottom
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.leftMargin: 24
                    anchors.rightMargin: 12
                    anchors.bottomMargin: 16
                    SubNavItem {
                        name: "cloud-upload"
                        label: QbzSession.tr("Share logs", QbzSession.trRev)
                        active: false
                        // Opens the VIEWER (filter / search / copy / bundle /
                        // upload), 1:1 with the reference. It used to hand the
                        // raw file to the desktop's file manager, which is
                        // what "Open log file" in Developer still does.
                        onClicked: QbzShell.logOpen()
                    }
                }
            }

            // Active panel — a raw Flickable (touch-drag scroll on the Pi
            // kiosk, per the Slint comment) + a ListScrollbar replica.
            Item {
                width: parent.width - subNav.width
                height: parent.height

                Flickable {
                    id: flick
                    anchors.fill: parent
                    contentWidth: width
                    contentHeight: panelCol.height + 60
                    clip: true
                    boundsBehavior: Flickable.StopAtBounds

                    // Tab can move into a control below the viewport. Reveal
                    // that control as focus changes so keyboard navigation is
                    // never technically correct but visually lost offscreen.
                    function revealFocusItem(item) {
                        if (!item)
                            return
                        var ancestor = item
                        while (ancestor && ancestor !== panelCol)
                            ancestor = ancestor.parent
                        if (ancestor !== panelCol)
                            return
                        const point = item.mapToItem(flick.contentItem, 0, 0)
                        const margin = 12
                        if (point.y < flick.contentY + margin)
                            flick.contentY = Math.max(0, point.y - margin)
                        else if (point.y + item.height > flick.contentY + flick.height - margin)
                            flick.contentY = Math.min(
                                Math.max(0, flick.contentHeight - flick.height),
                                point.y + item.height - flick.height + margin)
                    }

                    Connections {
                        target: root.Window.window
                        function onActiveFocusItemChanged() {
                            const win = root.Window.window
                            Qt.callLater(function () {
                                flick.revealFocusItem(win ? win.activeFocusItem : null)
                            })
                        }
                    }

                    Column {
                        id: panelCol
                        x: root.kioskHost ? 16 : 20
                        y: 4
                        width: flick.width - (root.kioskHost ? 32 : 60) // 20 left + 40 right padding
                        spacing: 4

                        // ── ONE PANEL IS BUILT, NOT NINE ──────────────
                        //
                        // These used to be nine siblings gated only by
                        // `visible:`, which in QML hides an item and does NOT
                        // stop it from being instantiated — the same defect
                        // 2026-08-17 found in HomeView's four Discover tabs.
                        // So every mount of Settings built the 30 KB
                        // Appearance panel, the 19 KB Plex panel and the rest,
                        // to show one of them. Measured 2026-08-21: Settings
                        // was the slowest route in the app at `built=326ms`.
                        //
                        // A Loader with `active:` is the fix that shipped for
                        // the tabs and it is the fix here. `visible: active`
                        // keeps the Column from reserving a slot for the eight
                        // that are not mounted. NOT `asynchronous: true` — see
                        // shell/ContentRouter.qml for why that was reverted.
                        //
                        // The two overlay modals below (LibFolderEditModal,
                        // DacWizardModal) are still built unconditionally and
                        // are the next item of the same kind; they own their
                        // own open state, so gating them is a separate change.
                        component Panel: Loader {
                            /// Deliberately NOT called `section`: the view root
                            /// already has one, and `root.section === section`
                            /// inside an inline component is a name-resolution
                            /// question nobody should have to answer.
                            property int panelIndex: -1
                            active: root.section === panelIndex
                            visible: active
                            width: parent ? parent.width : 0
                        }

                        Panel {
                            panelIndex: 12
                            sourceComponent: Component {
                                UpdatesSettings { width: parent.width; kioskHost: root.kioskHost }
                            }
                        }
                        OrbitSettingsPanel {
                            width: parent.width
                            selected: root.section === 11
                            kioskHost: root.kioskHost
                        }
                        Panel {
                            panelIndex: 0
                            sourceComponent: Component {
                                AudioSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                        Panel {
                            panelIndex: 1
                            sourceComponent: Component {
                                PlaybackSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    confirmHost: settingsConfirmHost
                                }
                            }
                        }
                        Panel {
                            panelIndex: 2
                            sourceComponent: Component {
                                AppearanceSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                        Panel {
                            panelIndex: 10
                            sourceComponent: Component {
                                NavigationSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                        Panel {
                            panelIndex: 3
                            sourceComponent: Component {
                                OfflineSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    confirmHost: settingsConfirmHost
                                }
                            }
                        }
                        Panel {
                            panelIndex: 13
                            sourceComponent: Component {
                                StorageSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    confirmHost: settingsConfirmHost
                                }
                            }
                        }
                        Panel {
                            panelIndex: 4
                            sourceComponent: Component {
                                LocalLibrarySettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    confirmHost: settingsConfirmHost
                                    tabOrderModal: localTabsConfigModal
                                }
                            }
                        }
                        Panel {
                            panelIndex: 5
                            sourceComponent: Component {
                                BlacklistSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                        Panel {
                            panelIndex: 6
                            sourceComponent: Component {
                                IntegrationsSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    confirmHost: settingsConfirmHost
                                }
                            }
                        }
                        Panel {
                            panelIndex: 9
                            sourceComponent: Component {
                                ImportExportSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                    migrationSetupModal: migrationSetupOverlay
                                }
                            }
                        }
                        Panel {
                            panelIndex: 7
                            sourceComponent: Component {
                                DeveloperSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                        Panel {
                            panelIndex: 8
                            active: root.section === 8 && root.sandboxed
                            sourceComponent: Component {
                                SandboxSettings { kioskHost: root.kioskHost;
                                    width: parent.width
                                    doc: root.doc
                                }
                            }
                        }
                    }
                }

                // Back/forward scroll memory (controls/ScrollMemory.qml): reports
                // this container's offset while it is the live page, and restores it
                // when a back/forward step arms this route.
                ScrollMemory { target: flick; scope: "settings" }
                QbzScrollBar {
                    target: flick
                    anchors.right: parent.right
                    anchors.rightMargin: 2
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                }
            }
        }
    }

    // Danger-zone confirmations for the whole view — declared LAST so
    // declaration order alone puts it over every panel and the sub-nav.
    // Panels that need it are handed the reference (see LocalLibrarySettings
    // / PlexSettings `confirmHost`).
    SettingsConfirmHost { kioskHost: root.kioskHost; id: settingsConfirmHost }

    // App-wide Local Library order. Mounted at the view root so its scrim
    // covers the sub-navigation and the scrolled panel alike.
    LocalTabsConfigModal { kioskHost: root.kioskHost;
        id: localTabsConfigModal
        anchors.fill: parent
    }

    // The per-folder settings modal. Same reasoning as the confirm host: it
    // must overlay the whole view, so it cannot live inside the scrolled
    // panel that opens it. The reference mounts its counterpart at the
    // AppShell root for exactly the same reason (LibFolderEditModal.slint:5-8).
    LibFolderEditModal { kioskHost: root.kioskHost; doc: root.doc }

    // The HiFi Wizard, opened from Settings > Audio. Mounted here for the same
    // reason as the two above — a modal inside the scrolled panel would be
    // sized by the content and ride the scroll — and it reads its OWN document
    // (QbzDacWizard), not the settings one, so it takes no `doc`.
    //
    // It fills the view rather than the window: Settings is the full content
    // area, and the reference is an overlay inside the app shell too, not a
    // separate window (DacWizardModal.slint:8-9).
    DacWizardModal { kioskHost: root.kioskHost; }

    MigrationSetupModal { kioskHost: root.kioskHost;
        id: migrationSetupOverlay
        anchors.fill: parent
        doc: root.doc
        confirmHost: settingsConfirmHost
    }

    // Mounted last so a long account migration has one unmistakable progress
    // surface over every Settings section. Closing it never cancels the task.
    MigrationProgressModal { kioskHost: root.kioskHost;
        anchors.fill: parent
        doc: root.doc
    }
}
