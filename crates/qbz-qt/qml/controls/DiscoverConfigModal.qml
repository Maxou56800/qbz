// DiscoverConfigModal — QML port of
// crates/qbz-ui/ui/discover/DiscoverConfigModal.slint (the per-tab
// "Customize" modal opened by the Discover toolbar gear): reorder +
// show/hide the active tab's rails, with a perf-warning banner, an
// enabled/total count and a Reset-to-defaults footer.
//
// No Save button — every toggle / move / reset persists live to
// discover_prefs.db and re-renders the three tabs from the cached section
// data (src/discover_config_qt.rs), so the list stays authoritative.
//
// Mount as the LAST child of the view root (declaration order IS z-order)
// with `anchors.fill: parent`; ADR-009's ">= 3000" is satisfied structurally
// AND by the explicit z below.
//
// The modal has TWO modes, and the tab decides which:
//   * the three section tabs (home / editorPicks / forYou) get the reorder
//     list, its perf banner, the enabled/total count and the reset footer;
//   * Recommendations has no configurable sections, so all of that is hidden
//     and it gets the explainer + cache-window select + "Refresh now" instead.
// 1:1 with the reference, which gates the same six blocks on the same
// condition (DiscoverConfigModal.slint:187/213/221/250/261/326).
//
// (The note that used to sit here said the external reco engine was "not
// ported" and the gear was disabled on that tab as a result. The engine has
// been ported for a long time — src/recommendations_qt.rs — so the note was a
// fossil describing a state of the world that had stopped being true, and the
// disabled gear it justified was the only thing keeping the window a user
// chose in one build from being changeable in this one.)

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    id: root

    // "home" | "editorPicks" | "forYou" — the tab whose sections are edited.
    property string tab: "home"
    readonly property bool opened: root._open
    property bool _open: false

    QbzTheme { id: theme }

    readonly property var doc: {
        try {
            return JSON.parse(QbzBridge.discoverConfigJson)
        } catch (e) {
            return {}
        }
    }
    // Only render rows once Rust has published THIS tab's payload.
    readonly property bool mine: (doc.tab || "") === root.tab
    readonly property var rows: root.mine ? (doc.rows || []) : []
    /// Items per rail when the user has not chosen — the first select entry's
    /// number. From Rust so it cannot drift from the cap that is applied.
    readonly property int defaultRailSize: doc.defaultRailSize || 10

    function open(forTab) {
        root.tab = forTab
        QbzBridge.discoverConfigOpen(forTab)
        root._open = true
    }
    function close() {
        root._open = false
    }

    visible: root._open
    enabled: root._open
    z: 3100

    // Scrim — declared FIRST so the panel below it in source order (and thus
    // above it in z) keeps its own presses.
    //
    // `radius` is load-bearing, not decoration. This Item fills the VIEW
    // root, which fills AppShell's rounded content frame, and Qt Quick's
    // `clip` is a rectangular scissor that does not follow `radius` — so an
    // opaque full-bleed child paints straight into the frame's four bezel
    // corners and the panel reads square. AppShell mounts four corner nubs
    // that cover exactly this, but ONLY while the ambient background is off
    // (with ambient on, the gutter must show the ambient field through the
    // corners, so an opaque nub would be the regression there). This scrim at
    // 75% opacity was the one remaining offender in the ambient-on case: every
    // view root already rounds its own fill for precisely this reason
    // (HomeView.qml:radius 12 and its comment), the header atmosphere is
    // suppressed under ambient, and window-level overlays are supposed to
    // cover the corners. Rounding the scrim's own fill fixes it in BOTH
    // ambient states and needs no colour guessing — the corners keep showing
    // whatever is genuinely behind them.
    Rectangle {
        anchors.fill: parent
        radius: theme.radiusMd
        color: "#bf000000"
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
            // Eat the WHEEL as well as the click. A MouseArea consumes presses
            // but lets wheel events fall straight through, so with this open
            // the pointer still scrolled the page underneath — and over the
            // panel it moved the panel's own list AND the page behind it at the
            // same time. These overlays are plain Items, not Popups, so there
            // is no modal grab doing it for them: each has to stop the wheel at
            // its own surface.
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    Rectangle {
        id: panel
        width: Math.min(root.width - 80, 520)
        height: Math.min(panelCol.implicitHeight + 40, root.height * 0.78)
        x: Math.round((root.width - width) / 2)
        y: Math.round((root.height - height) / 2)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle
        clip: true

        // Swallow clicks so they never reach the scrim — and the wheel, so a
        // scroll that starts outside the section list (the title, the banner,
        // the footer) does not reach the page behind the modal.
        MouseArea {
            anchors.fill: parent
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Column {
            // NOT `body`: WarningBanner below has a `body` PROPERTY, and an
            // id that shadows a child's property name is a trap waiting for
            // the next reader.
            id: panelCol
            x: 20
            y: 20
            width: parent.width - 40
            spacing: 14

            // --- Title + close --------------------------------------------
            Item {
                width: parent.width
                height: 28
                Text {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Customize", QbzSession.trRev)
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                }
                Rectangle {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    radius: 6
                    color: cfgCloseArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: cfgCloseArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: cfgCloseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.close()
                    }
                }
            }

            // --- Perf-warning banner (the shared WarningBanner replica) ----
            // This and the next three blocks are section-config only.
            WarningBanner {
                visible: root.tab !== "recommendations"
                width: parent.width
                variant: "warning"
                body: QbzSession.tr("Enabling more sections increases load time.", QbzSession.trRev)
            }

            // --- Count line -----------------------------------------------
            Text {
                visible: root.tab !== "recommendations"
                width: parent.width
                text: QbzSession.tr("{} of {} enabled", QbzSession.trRev)
                        .replace("{}", root.mine ? (doc.enabled || 0) : 0)
                        .replace("{}", root.mine ? (doc.total || 0) : 0)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                font.weight: theme.weightMedium
            }

            // --- Section list ---------------------------------------------
            // Explicit, content-derived height capped to what is left of the
            // panel; the Flickable scrolls past that.
            //
            // The subtrahend is the FIXED-CHROME BUDGET (title + banner + count
            // + footer + margins), and it is load-bearing: the panel clips
            // (`clip: true` above), so a row added over this list without
            // raising the number does not overflow visibly — the last sections
            // and the Reset footer simply stop existing.
            Item {
                visible: root.tab !== "recommendations"
                width: parent.width
                height: Math.max(0, Math.min(listCol.height, root.height * 0.78 - 260))
                clip: true
                Flickable {
                    id: listFlick
                    anchors.fill: parent
                    contentWidth: width
                    contentHeight: listCol.height
                    boundsBehavior: Flickable.StopAtBounds
                    Column {
                        id: listCol
                        width: listFlick.width
                        spacing: 2
                        Repeater {
                            model: root.rows
                            delegate: Rectangle {
                                id: cfgRow
                                required property var modelData
                                required property int index
                                width: listCol.width
                                height: 40
                                radius: theme.radiusSm
                                color: cfgRowArea.containsMouse ? theme.surfaceHover : "transparent"

                                // Whole-row tap toggles (primary affordance).
                                // Declared FIRST so the checkbox and the two
                                // reorder buttons below sit ON TOP of it and
                                // keep their own presses.
                                MouseArea {
                                    id: cfgRowArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: QbzBridge.discoverToggleSection(root.tab, cfgRow.modelData.id)
                                }

                                Row {
                                    anchors.fill: parent
                                    anchors.leftMargin: 10
                                    anchors.rightMargin: 6
                                    spacing: 12

                                    QbzCheckbox {
                                        anchors.verticalCenter: parent.verticalCenter
                                        checked: cfgRow.modelData.enabled
                                        onToggled: QbzBridge.discoverToggleSection(root.tab, cfgRow.modelData.id)
                                    }
                                    Text {
                                        width: parent.width - 18 - 124 - 28 - 28 - 4 * 12
                                        height: parent.height
                                        text: cfgRow.modelData.label
                                        color: cfgRow.modelData.enabled ? theme.textPrimary : theme.textMuted
                                        font.pixelSize: theme.fontBody
                                        font.weight: theme.weightMedium
                                        verticalAlignment: Text.AlignVCenter
                                        elide: Text.ElideRight
                                    }
                                    // How many items THIS rail shows. Per rail
                                    // and not one number for the page: that is
                                    // the shape Tauri had
                                    // (`HomeSettingsModal.svelte`), and it is
                                    // the point of the feature — a 25-wide
                                    // "New Releases" next to a 10-wide
                                    // "Recently Played" is why anyone opens
                                    // this.
                                    //
                                    // Option ORDER is the contract with
                                    // RAIL_SIZE_PRESETS in
                                    // qbz-app/src/settings/discover_prefs.rs.
                                    //
                                    // The first entry IS the default, and it
                                    // says the number. It used to read "All",
                                    // which was a lie twice over: an "all" in
                                    // Qobuz terms is what the View-all page
                                    // holds, and /discover/index hands each rail
                                    // a different count anyway (measured:
                                    // new_releases 26, press_awards 10). The
                                    // default is a real uniform cap of ten now,
                                    // so the label is simply true — and ten
                                    // stops being a separate option because it
                                    // IS the first one.
                                    //
                                    // The number comes from Rust
                                    // (`DEFAULT_RAIL_SIZE`, on the doc) rather
                                    // than being typed here, so the label and
                                    // the cap cannot drift apart.
                                    QbzSelect {
                                        // 124/150, not 82/110: the widest entry
                                        // is "Default (10)" and at 82 both the
                                        // closed control and the popup elided
                                        // it to "Defaul…", which is the one
                                        // string in the list that has to be
                                        // readable. Sized for the longest
                                        // TRANSLATED label, not the English one
                                        // — "Predeterminado (10)" is half again
                                        // as long, hence the elide staying as
                                        // the backstop rather than a tighter
                                        // fit.
                                        width: 124
                                        anchors.verticalCenter: parent.verticalCenter
                                        sm: true
                                        menuWidth: 150
                                        enabled: cfgRow.modelData.enabled
                                        options: [
                                            QbzSession.tr("Default ({})", QbzSession.trRev)
                                                .replace("{}", root.defaultRailSize),
                                            "15",
                                            "20",
                                            "25",
                                        ]
                                        currentIndex: cfgRow.modelData.sizeIndex || 0
                                        onSelected: function (i) {
                                            QbzHome.discoverSetRailSize(root.tab, cfgRow.modelData.id, i)
                                        }
                                    }
                                    ReorderButton {
                                        anchors.verticalCenter: parent.verticalCenter
                                        glyph: "chevron-up"
                                        buttonEnabled: cfgRow.index > 0
                                        onClicked: QbzBridge.discoverMoveSection(root.tab, cfgRow.modelData.id, -1)
                                    }
                                    ReorderButton {
                                        anchors.verticalCenter: parent.verticalCenter
                                        glyph: "chevron-down"
                                        buttonEnabled: cfgRow.index < root.rows.length - 1
                                        onClicked: QbzBridge.discoverMoveSection(root.tab, cfgRow.modelData.id, 1)
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Empty state — a tab whose sections have not been fetched yet.
            // NOT on Recommendations: `DiscoveryTab::from_key` has no arm for
            // it (discover_prefs.rs:44), so its row list is empty BY DESIGN and
            // "No sections to configure yet" would be a permanent lie there.
            Text {
                visible: root.rows.length === 0 && root.tab !== "recommendations"
                width: parent.width
                height: 34
                text: QbzSession.tr("No sections to configure yet.", QbzSession.trRev)
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                verticalAlignment: Text.AlignVCenter
            }

            // --- Recommendations arm: explainer + cache window + refresh ---
            // Recommendations has no sections to order, so the gear opens on
            // THIS instead. Both controls write through QbzHome:
            // `recoSetCacheTtlIndex` persists to the shared per-user discover
            // prefs (the same file the Slint build reads, so a window chosen
            // in either is the window both honour), and `recoRefreshNow`
            // rebuilds every row past the results blob.
            Text {
                visible: root.tab === "recommendations"
                width: parent.width
                text: QbzSession.tr("QBZ uses your Last.fm and ListenBrainz account data when they're connected (if they aren't, we recommend connecting them to enrich this section). Here you set how often that data is fetched — it defaults to 48 hours, or you can request an update right now.", QbzSession.trRev)
                color: theme.textSecondary
                font.pixelSize: theme.fontBody
                wrapMode: Text.WordWrap
            }

            Item {
                visible: root.tab === "recommendations"
                width: parent.width
                height: 34

                Text {
                    id: ttlLabel
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    text: QbzSession.tr("Cache window", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                }
                QbzSelect {
                    anchors.left: ttlLabel.right
                    anchors.leftMargin: 8
                    anchors.verticalCenter: parent.verticalCenter
                    sm: true
                    menuWidth: 140
                    // Option ORDER is the contract with TTL_HOURS in
                    // src/recommendations_qt.rs — the bridge publishes an
                    // index, not hours, so reordering these silently changes
                    // what every stored value means.
                    options: [
                        QbzSession.tr("24 hours", QbzSession.trRev),
                        QbzSession.tr("36 hours", QbzSession.trRev),
                        QbzSession.tr("48 hours", QbzSession.trRev),
                        QbzSession.tr("72 hours", QbzSession.trRev),
                    ]
                    currentIndex: QbzHome.recoCacheTtlIndex
                    onSelected: function (i) { QbzHome.recoSetCacheTtlIndex(i) }
                }

                // Pinned right, like the reference's stretch spacer.
                Rectangle {
                    id: refreshBtn
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: refreshRow.width + 24
                    height: 34
                    radius: theme.radiusSm
                    color: refreshArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
                    border.width: 1
                    border.color: theme.borderSubtle
                    Row {
                        id: refreshRow
                        anchors.centerIn: parent
                        spacing: 8
                        QbzIcon {
                            name: "refresh-cw"
                            width: 15
                            height: 15
                            anchors.verticalCenter: parent.verticalCenter
                            tintName: "secondary"
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("Refresh now", QbzSession.trRev)
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                        }
                    }
                    MouseArea {
                        id: refreshArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzHome.recoRefreshNow()
                    }
                }
            }

            // --- Footer: Refresh content + Reset to defaults ---------------
            Row {
                visible: root.tab !== "recommendations"
                spacing: 8

                // REFRESH CONTENT. The discover index is fetched ONCE per shell
                // entry — `main.rs` latches `HOME_LOADED` and never calls
                // `reload_home` again — so a session shows the page it opened
                // with: nothing new from the catalogue, and none of the plays
                // made since, because the recently-played rails are built from
                // the local history at the same moment. Everything else in this
                // modal re-renders from the cached candidates, which is exactly
                // what a user cannot use to get fresh ones.
                //
                // The invokable already existed for the error-state retry
                // (HomeView.qml); this is the door for it.
                FooterButton {
                    glyph: "refresh-cw"
                    label: QbzSession.tr("Refresh content", QbzSession.trRev)
                    onClicked: QbzHome.reloadHome()
                }
                FooterButton {
                    glyph: "rotate-ccw"
                    label: QbzSession.tr("Reset to defaults", QbzSession.trRev)
                    onClicked: QbzBridge.discoverResetTab(root.tab)
                }
            }
        }
    }

    // The footer's outlined 34px button. Extracted when the footer grew a
    // second one — the Reset button's markup copied verbatim, so the pair
    // cannot drift.
    component FooterButton: Rectangle {
        id: footerBtn
        property string glyph: ""
        property string label: ""
        signal clicked()

        width: footerRow.width + 24
        height: 34
        radius: theme.radiusSm
        color: footerArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated
        border.width: 1
        border.color: theme.borderSubtle

        Row {
            id: footerRow
            anchors.centerIn: parent
            spacing: 8
            QbzIcon {
                name: footerBtn.glyph
                width: 15
                height: 15
                anchors.verticalCenter: parent.verticalCenter
                tintName: "secondary"
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: footerBtn.label
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
            }
        }
        MouseArea {
            id: footerArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: footerBtn.clicked()
        }
    }

    // 28x28 ghost reorder button (the Slint ReorderButton): dims and drops
    // the pointer cursor at the list boundary.
    component ReorderButton: Rectangle {
        id: reorderRoot
        property string glyph: "chevron-up"
        property bool buttonEnabled: true
        signal clicked()

        width: 28
        height: 28
        radius: theme.radiusSm
        opacity: reorderRoot.buttonEnabled ? 1.0 : 0.3
        color: (reorderRoot.buttonEnabled && reorderArea.containsMouse)
            ? theme.surfaceHover : "transparent"

        QbzIcon {
            anchors.centerIn: parent
            name: reorderRoot.glyph
            width: 16
            height: 16
            tintName: "secondary"
        }
        MouseArea {
            id: reorderArea
            anchors.fill: parent
            enabled: reorderRoot.buttonEnabled
            hoverEnabled: reorderRoot.buttonEnabled
            cursorShape: reorderRoot.buttonEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
            onClicked: reorderRoot.clicked()
        }
    }
}
