// Local Library FIXED CHROME (LocalLibraryView.slint:710-1125) — the
// mandatory two-row header this app's list views all share:
//
//   row 1 (46px)  "Local Library" + the settings gear + the `Open` menu +
//                 the `Refresh` menu
//
//                 `Open` is ALWAYS present: it is the one door for every
//                 medium that can become an ephemeral session (a folder
//                 today; a CD and a SACD image next). It replaces the
//                 folder-open icon that used to sit in the Folders tree rail,
//                 where it was reachable only from one arm of one tab.
//
//                 `Refresh` replaces the Plex-only re-sync icon (#573). Every
//                 row is gated on a source actually being configured, so a
//                 user with no media servers still sees exactly one usable
//                 entry (local folders) plus "all".
//   row 2 (44px)  the segmented tab bar with count badges on the left, the
//                 per-tab toolbar floated right
//   then the 1px divider.
//
// Total height 91px — the content area subtracts exactly this.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"

Item {
    id: root

    property var view: null

    QbzTheme { id: theme }

    height: 91

    // ---- Row 1: title + gear + Plex sync ----
    Item {
        width: parent.width
        height: 46
        Row {
            id: headerActions
            x: 32
            anchors.verticalCenter: parent.verticalCenter
            spacing: 12
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: QbzSession.tr("Local Library", QbzSession.trRev)
                color: theme.textPrimary
                font.pixelSize: theme.fontSection
                font.weight: theme.weightBold
            }
            // Folder management, scan and maintenance do NOT live in this
            // view (Slint header note) — the gear routes out.
            QbzNavButton {
                id: gearBtn
                name: "settings-2"
                anchors.verticalCenter: parent.verticalCenter
                // Land on Settings > Local Library (section 4 in
                // settings/SettingsView.qml), not on the Audio root.
                onClicked: {
                    QbzBridge.settingsSetSection(4)
                    QbzShell.navigateTo("settings")
                }
                HoverHandler {
                    onHoveredChanged: tips.hover(hovered, gearBtn, "local-gear",
                        QbzSession.tr("Manage the folders your library is built from",
                                      QbzSession.trRev))
                }
            }
            // ---- Refresh ▾ -------------------------------------------
            // Before Open, per the owner's layout: the two maintenance glyphs
            // (settings, resync) sit together, and the ACTION that produces
            // new content reads last and carries a label.
            Item {
                width: 28
                height: 28
                anchors.verticalCenter: parent.verticalCenter
                QbzNavButton {
                    id: refreshBtn
                    anchors.fill: parent
                    name: "refresh-cw"
                    // The button stays reachable while a sync runs — the
                    // individual rows are what go muted, so the user can still
                    // start a DIFFERENT source's sync (they are independent
                    // jobs on independent stores).
                    onClicked: refreshMenu.openBelowLeft(refreshBtn)
                    HoverHandler {
                        onHoveredChanged: tips.hover(hovered, refreshBtn, "refresh-chip",
                            QbzSession.tr("Re-scan your folders and media servers for new music",
                                          QbzSession.trRev))
                    }
                }
                QbzSpinner {
                    // ANY sync in flight, not just Plex's: the media-server
                    // sweep has its own flag and its own progress string.
                    visible: QbzLocal.plexSyncing || QbzLocal.mediaSyncing
                    anchors.centerIn: parent
                    size: 15
                }
            }

            // ---- [icon] Open ▾ ---------------------------------------
            // A LABELLED chip, not a bare glyph: this is the one door for
            // every medium, and a folder icon alone reads as "browse folders"
            // next to a tab literally called Folders. 30px / radius 6 is the
            // small toolbar-control contract this header's own selects use.
            Rectangle {
                id: openBtn
                anchors.verticalCenter: parent.verticalCenter
                width: openRow.width + 20
                height: 30
                radius: 6

                // An open is in flight. Spinning a drive up and reading a TOC
                // takes SECONDS, and until this existed those seconds looked
                // exactly like a click that missed — so the chip both says so
                // and stops taking a second click onto a drive it is already
                // reading.
                readonly property bool busy: QbzLocal.localDiscOpening

                color: (!openBtn.busy && (openArea.containsMouse || openMenu.opened))
                    ? theme.elevatedHoverFill
                    : theme.elevatedFill

                Row {
                    id: openRow
                    anchors.centerIn: parent
                    spacing: 6
                    // The spinner takes the ICON's slot rather than being
                    // appended: the chip is 15px wider either way, so the
                    // label and the chevron do not shuffle sideways the
                    // instant the disc is picked.
                    Item {
                        width: 15
                        height: 15
                        anchors.verticalCenter: parent.verticalCenter
                        QbzIcon {
                            anchors.fill: parent
                            name: "folder-open"
                            visible: !openBtn.busy
                            tintName: openArea.containsMouse ? "textPrimary" : "secondary"
                        }
                        QbzSpinner {
                            anchors.centerIn: parent
                            size: 15
                            // `visible` also stops the rotation (QbzSpinner
                            // gates on it), so an idle chip presents nothing.
                            visible: openBtn.busy
                        }
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Open", QbzSession.trRev)
                        color: openArea.containsMouse ? theme.textPrimary : theme.textSecondary
                        font.pixelSize: 13
                    }
                    QbzIcon {
                        name: "chevron-down"
                        width: 13
                        height: 13
                        anchors.verticalCenter: parent.verticalCenter
                        tintName: openArea.containsMouse ? "textPrimary" : "secondary"
                    }
                }

                MouseArea {
                    id: openArea
                    anchors.fill: parent
                    enabled: !openBtn.busy
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: openMenu.openBelowLeft(openBtn)
                    // What this door is FOR, in one line. "Open" next to a tab
                    // called Folders reads as "browse my library", which is
                    // the opposite of what it does.
                    onContainsMouseChanged: tips.hover(containsMouse, openBtn, "open-chip",
                        openBtn.busy
                            ? QbzSession.tr("Reading the disc…", QbzSession.trRev)
                            : QbzSession.tr("Play a folder, CD or disc image without adding it to your library",
                                            QbzSession.trRev))
                }
            }
        }

        // The unused centre of the title row carries scan state without
        // changing the 91px chrome contract. It reads counters only; no model
        // row or delegate is created, so large-library virtualization remains
        // untouched. On a narrow window the toolbar keeps priority and the
        // compact indicator yields rather than overlapping controls.
        LocalScanProgress {
            anchors.left: headerActions.right
            anchors.leftMargin: 28
            anchors.right: parent.right
            anchors.rightMargin: 32
            anchors.verticalCenter: parent.verticalCenter
            visible: active && width >= 260
        }
    }

    // ---- Row 2: tabs (left) + per-tab toolbar (right) ----
    Item {
        y: 46
        width: parent.width
        height: 44

        QbzTabBar {
            x: 32
            anchors.verticalCenter: parent.verticalCenter
            counts: true
            underline: true
            activeId: root.view.activeTab
            // NOTE the `root.view.` prefix: QbzTabBar has its OWN `counts`
            // property (the badge arm), so an unqualified `counts` here
            // would resolve to that bool, not to the view's document.
            // The fifth tab EXISTS only while an ephemeral session does. It is
            // appended rather than rendered disabled, because a tab for a
            // medium that is not inserted is worse than no tab: it invites a
            // click that can only fail. `ephemeralActive` is the same flag the
            // pane itself keys on, so the tab and its body cannot disagree.
            tabs: {
                var byId = {
                    "albums": { "id": "albums", "label": QbzSession.tr("Albums", QbzSession.trRev), "count": root.view.counts.albums || 0 },
                    "artists": { "id": "artists", "label": QbzSession.tr("Artists", QbzSession.trRev), "count": root.view.counts.artists || 0 },
                    "genres": { "id": "genres", "label": QbzSession.tr("Library Explorer", QbzSession.trRev), "count": root.view.explorerLeadingCount },
                    "folders": { "id": "folders", "label": QbzSession.tr("Folders", QbzSession.trRev), "count": root.view.counts.folders || 0 },
                    "tracks": { "id": "tracks", "label": QbzSession.tr("Tracks", QbzSession.trRev), "count": root.view.counts.tracks || 0 }
                }
                var t = []
                for (var i = 0; i < root.view.localTabOrder.length; i++) {
                    var id = root.view.localTabOrder[i]
                    if (byId[id]) t.push(byId[id])
                }
                if (root.view.ephemeralActive)
                    t.push({ "id": "ephemeral",
                             "label": root.view.ephemeralLabel,
                             "count": root.view.ephemeralTrackCount })
                return t
            }
            onSelected: function (id) { root.view.activateTab(id) }
        }

        LocalToolbar {
            x: parent.width - width - 32
            anchors.verticalCenter: parent.verticalCenter
            view: root.view
        }
    }

    Rectangle {
        y: 90
        width: parent.width
        height: 1
        color: theme.borderSubtle
    }

    // ---------------------------- the two menus ---------------------------
    // Both are CardMenu (the shared ⋯ surface): an `entries` model in, a
    // `picked(action)` out. No new component, and the rows inherit the app's
    // one menu chrome.

    CardMenu {
        id: openMenu
        menuWidth: 220
        // W16: the audio-CD row is dropped on Windows -- qbz-disc's CDDA
        // backend is `mod unsupported` there and answers NoDrive to
        // everything. SACD is an image file, so it stays on every platform.
        entries: [
            { "label": QbzSession.tr("Open folder…", QbzSession.trRev),
              "icon": "folder-open", "action": "folder" }
        ].concat(QbzShell.isWindows ? [] : [
            { "label": QbzSession.tr("Open audio CD", QbzSession.trRev),
              "icon": "disc", "action": "cd" }
        ]).concat([
            { "label": QbzSession.tr("Open SACD image…", QbzSession.trRev),
              "icon": "disc-3", "action": "sacd" }
        ])
        onPicked: function (action) {
            if (action === "folder") QbzLocal.ephemeralOpen()
            else if (action === "cd") QbzLocal.ephemeralOpenCd()
            else if (action === "sacd") QbzLocal.ephemeralOpenSacd()
        }
    }

    CardMenu {
        id: refreshMenu
        menuWidth: 240
        // Built as a list so the optional rows can be appended rather than
        // rendered muted: a user with no Plex must not see a Plex row at all.
        //
        // `cloud-download`, not `server`: QbzIcon draws PRE-BAKED svgs from
        // `qml/assets/icons/<tint>/`, and a name with no bake renders NOTHING
        // — no warning, no error, no failing test. `server` has no bake, and
        // the three rows below shipped iconless in the first smoke because of
        // it. Check the tint directory before inventing a glyph name.
        // `mediaHasJellyfin` / `mediaHasSubsonic` are the SAME gates the media
        // server settings panel publishes, so this menu cannot drift from what
        // is actually configured.
        entries: {
            var out = [
                { "label": QbzSession.tr("Resync all", QbzSession.trRev),
                  "icon": "refresh-cw", "action": "all",
                  "enabled": !QbzLocal.mediaSyncing && !QbzLocal.plexSyncing },
                { "sep": true },
                { "label": QbzSession.tr("Resync local folders", QbzSession.trRev),
                  "icon": "hard-drive", "action": "local" }
            ]
            if (QbzLocal.plexAvailable)
                out.push({ "label": QbzSession.tr("Resync Plex", QbzSession.trRev),
                           "icon": "cloud-download", "action": "plex",
                           "enabled": !QbzLocal.plexSyncing })
            if (QbzLocal.mediaHasJellyfin)
                out.push({ "label": QbzSession.tr("Resync Jellyfin", QbzSession.trRev),
                           "icon": "cloud-download", "action": "jellyfin",
                           "enabled": !QbzLocal.mediaSyncing })
            if (QbzLocal.mediaHasSubsonic)
                out.push({ "label": QbzSession.tr("Resync Navidrome", QbzSession.trRev),
                           "icon": "cloud-download", "action": "subsonic",
                           "enabled": !QbzLocal.mediaSyncing })
            return out
        }
        onPicked: function (action) {
            // "library-scan" with an empty payload = every ENABLED folder,
            // which is what the settings panel's own Scan button sends
            // (LibraryFolderTable.qml:75).
            if (action === "local" || action === "all")
                QbzBridge.settingsString("library-scan", "")
            if ((action === "plex" || action === "all") && QbzLocal.plexAvailable)
                QbzLocal.syncPlex()
            // A button labelled Resync is an authoritative reconciliation,
            // not a delta. Jellyfin's DateLastSaved delta cannot report rows
            // whose provider ids disappeared (for example after a library
            // rebuild), so using it here retained the old ids beside the new
            // ones and rendered one physical copy as two versions. Automatic
            // background catch-up may still use deltas; an explicit repair
            // must observe the complete source before it authorizes pruning.
            if ((action === "jellyfin" || action === "all") && QbzLocal.mediaHasJellyfin)
                QbzLocal.mediaSync("jellyfin", true)
            if ((action === "subsonic" || action === "all") && QbzLocal.mediaHasSubsonic)
                QbzLocal.mediaSync("subsonic", true)
        }
    }

    // Hover tooltips for this header's controls. The overlay takes no pointer
    // and owns no animator, so an idle one costs nothing.
    QbzTooltip {
        id: tips
        anchors.fill: parent
        z: 900
    }

}
