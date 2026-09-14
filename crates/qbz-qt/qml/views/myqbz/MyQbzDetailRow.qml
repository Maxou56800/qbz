// MyQbzDetailRow — ONE item row of the My QBZ detail list, the port of the
// local `DetailRow` component in myqbz/MixtapeDetailView.slint:190-492.
//
// 8-column template, 1:1 with the .slint (:219-491): padding 12/12,
// spacing 12, cells 40 / 1fr / 140 / 80 / 160 / 72 / 60 / 40. Row height 56,
// bottom hairline `Theme.alpha-4`, hover/selected fill `surface-hover`.
//
// Plus a NINTH, leading 24px cell that no reference has: the accordion
// chevron (owner-authorised divergence, see its own comment below). It is
// mounted ONLY in the details arm (`showExpander`, owner 2026-07-31); when it
// is, it shifts the eight reference columns 36px right and `titleColWidth`
// plus the host's `ColHead` row are retuned together.
//
// --- TWO THINGS THAT ARE LOAD-BEARING -----------------------------------
//
// 1. Z-ORDER. `MixtapeDetailView.slint:203-217` documents the bug: the
//    row-wide click target MUST sit BEHIND the per-cell hit areas (checkbox,
//    artwork play, title, subtitle, ⋯) or it eats every one of their clicks
//    and the whole row just opens the album. In QML declaration order IS
//    z-order, so `rowArea` is declared FIRST and additionally pinned `z: -1`
//    — the same belt-and-braces `rows/TrackRow.qml:644-652` uses.
//
// 2. `rev` IS A REAL DEPENDENCY OF EVERY `item` READ. The host holds the
//    model array with a stable identity and PATCHES the row objects in place
//    when Rust republishes a single-row change (`resolve_items`,
//    `ensure_expanded`, a select toggle) — replacing the array would push a
//    new one into `model:` and `QQuickItemView::setModel()` resets contentY
//    to 0 (the analysis at `views/LibraryView.qml:174-221`). A plain JS
//    object mutation fires NO notifier (`rows/TrackRow.qml:128-151` measured
//    both halves), so each read below is routed through a declared property
//    whose binding also reads `root.rev`. The `root.rev >= 0` guard is
//    ALWAYS true — it exists only to make `rev` a captured dependency. Do
//    not "simplify" it away.
//
// The row calls `QbzMyQbz` directly, exactly as the .slint calls
// `MyQbzDetailActions` directly — no signal relay.
//
// Deliberate deviations, all four documented in spec 01 §7.8: the shared
// `controls/CardMenu.qml` replaces the .slint's local `RowMenuItem`
// (33/r5/15px-glyph/8px-pad vs 32/r6/13/12; label rests `textSecondary` not
// `text-primary`; the icon tint tracks hover; a separator is a 7px band with
// a 1px `borderSubtle` rule instead of a bare 1px `surface-elevated` line).
// TRACK-RULES §5 (reuse before duplicate) outranks those deltas.

import QtQuick
import com.blitzfc.qbz
import "../../controls"
import "../../theme"
import "../local"

Rectangle {
    id: root

    /// One `DetailRow` object out of `detailJson.items` (spec 02 §5.2).
    property var item: ({})
    /// 1-based display ordinal among the VISIBLE rows (.slint `index`, :192).
    /// Named `ordinal`, not `index`, so a delegate can pass `index + 1`
    /// without shadowing the delegate's own context `index`.
    property int ordinal: 0
    property bool selectMode: false
    /// Does THIS row carry the accordion chevron cell?
    ///
    /// The accordion is the owner's feature, but it is scoped to the DETAILS
    /// arm (the `expanded` segment) — the plain LIST arm never had a use for it
    /// and the owner asked for it back out of there (2026-07-31). Off, the
    /// leading 24px cell is `visible: false`, which in a `Row` drops the cell
    /// AND its 12px gap, so the eight reference columns land exactly where
    /// `MixtapeDetailView.slint:219-491` puts them again (`titleColWidth`
    /// below switches back to the reference's 700 with it).
    ///
    /// The STATE is untouched: `rowOpen` stays the one notion of open and Rust
    /// stays its only writer. Rust also AND-s `rowOpen` with the details arm
    /// (`myqbz_detail_qt.rs` `to_item` / `resync_row_open`), so with the chevron
    /// gone from list mode no row renders open there and nothing is orphaned —
    /// while the user's open set itself SURVIVES the round trip (it is persisted
    /// per collection now, `collection_open_rows.json`).
    property bool showExpander: false
    /// The host's shared 3-phase clock for `QbzLoadingDots` (ONE timer per
    /// view — spec 01 §7.5 / §15.5).
    property int dotPhase: 0
    /// In-place-patch revision. See note 2 in the header.
    property int rev: 0

    QbzTheme { id: theme }

    // --- Every `item` read, made notifiable (header note 2) --------------
    readonly property int itemPosition: root.rev >= 0 ? (root.item.position || 0) : 0
    readonly property string itemType: root.rev >= 0 ? (root.item.itemType || "") : ""
    readonly property string itemSource: root.rev >= 0 ? (root.item.source || "") : ""
    readonly property string sourceItemId: root.rev >= 0 ? (root.item.sourceItemId || "") : ""
    readonly property string rowTitle: root.rev >= 0 ? (root.item.title || "") : ""
    readonly property string rowSubtitle: root.rev >= 0 ? (root.item.subtitle || "") : ""
    readonly property bool subtitleIsLink: root.rev >= 0 && root.item.subtitleIsLink === true
    readonly property string artistId: root.rev >= 0 ? (root.item.artistId || "") : ""
    readonly property string albumId: root.rev >= 0 ? (root.item.albumId || "") : ""
    // The parent collection kind ("mixtape" | "collection" | "artist_collection"),
    // passed from the view so the menu says "Remove from Mixtape" vs "…collection".
    property string detailKind: ""
    // Fix #766: a playlist inside a MIXTAPE has varied per-track quality, so its
    // quality cell is hidden there (collections keep it).
    readonly property bool hideQuality: root.detailKind === "mixtape" && root.itemType === "playlist"
    readonly property string sourceKind: root.rev >= 0 ? (root.item.sourceKind || "") : ""
    readonly property var sourceKinds: root.rev >= 0 ? (root.item.sourceKinds || []) : []
    readonly property string typeLabel: root.rev >= 0 ? (root.item.typeLabel || "") : ""
    readonly property string qualityTier: root.rev >= 0 ? (root.item.qualityTier || "") : ""
    readonly property string qualityDetail: root.rev >= 0 ? (root.item.qualityDetail || "") : ""
    readonly property bool qualityResolving: root.rev >= 0 && root.item.qualityResolving === true
    readonly property string tracksText: root.rev >= 0 ? (root.item.tracksText || "") : ""
    readonly property string yearText: root.rev >= 0 ? (root.item.yearText || "") : ""
    readonly property string artPath: root.rev >= 0 ? (root.item.artPath || "") : ""
    readonly property bool selected: root.rev >= 0 && root.item.selected === true
    /// Albums and playlists have an inline track list; a single track has
    /// nothing to show, so it gets no chevron (Rust decides — `canExpand`).
    readonly property bool canExpand: root.rev >= 0 && root.item.canExpand === true
    /// THE accordion state, and the ONLY one: Rust derives it from the shared
    /// `OPEN_ROWS` set — the chevron's own writes, persisted per collection —
    /// so this row never needs to know what the view mode is.
    readonly property bool rowOpen: root.rev >= 0 && root.item.rowOpen === true

    /// track → music, playlist → list-music, else disc (.slint :263-267).
    readonly property string typeGlyph: root.itemType === "track" ? "music"
        : (root.itemType === "playlist" ? "list-music" : "disc")

    // Hover: OR'd across the cell areas because a child MouseArea with
    // `hoverEnabled` steals `containsMouse` from the row's own area — the
    // same reason `rows/TrackRow.qml:213-214` OR's its four.
    readonly property bool hovered: rowArea.containsMouse || artArea.containsMouse
        || titleArea.containsMouse || subArea.containsMouse || menuArea.containsMouse
        || chevArea.containsMouse

    width: parent ? parent.width : 0
    height: 56
    color: (root.hovered || root.selected) ? theme.surfaceHover : "transparent"

    /// Title column = rowWidth − padding(24) − fixed cells(592) − gaps(84)
    /// = width − 700, the reference's own arithmetic (:219-491).
    ///
    /// 736 while `showExpander`: the accordion chevron adds a NINTH cell (24px)
    /// and a ninth 12px gap in front of the ordinal, shifting the 8-column
    /// template 36px right. `MyQbzDetailView.qml`'s `ColHead` row carries the
    /// identical leader and the identical switch, and the two must be changed
    /// together or the header stops lining up with the rows.
    readonly property int titleColWidth: Math.max(0,
        root.width - (root.showExpander ? 736 : 700))

    // Bottom hairline — `#ffffff0a` in the .slint (:200) == `alpha-4`, taken
    // from the polarity-baked ramp so it is visible on light themes too. The
    // `.length` guard covers the pre-publish frame QbzTheme documents.
    Rectangle {
        y: parent.height - 1
        width: parent.width
        height: 1
        color: theme.alphaTiers.length > 0 ? theme.alphaTier(4)
            : (theme.isDark ? "#0affffff" : "#0a000000")
    }

    // Row-wide target — FIRST + `z: -1` (header note 1). Right press opens
    // the SAME menu at the pointer, the port's standing rule
    // (`controls/QbzContextMenu.qml:20-22`); the .slint has no right-click
    // here, so this is an additive port convention kept consistent with
    // `rows/TrackRow.qml`.
    MouseArea {
        id: rowArea
        anchors.fill: parent
        z: -1
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: Qt.PointingHandCursor
        onPressed: function (mouse) {
            if (mouse.button === Qt.RightButton) { rowMenu.openAtCursor(rowArea, mouse.x, mouse.y); mouse.accepted = true }
        }
        onClicked: function (mouse) {
            if (mouse.button !== Qt.LeftButton) return
            if (root.selectMode)
                QbzMyQbz.detailToggleItemSelect(root.itemPosition,
                                                (mouse.modifiers & Qt.ShiftModifier) !== 0)
            else root.openThis()
        }
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: 12

        // --- Col 0 (24) — THE ACCORDION CHEVRON, details arm only ---------
        //
        // ADDITIVE, and a deliberate divergence from BOTH references
        // (owner-authorised 2026-07-30): neither `MixtapeDetailView.slint` nor
        // the Tauri build ever had a per-row accordion — both render inline
        // tracks only as a whole-list VIEW MODE, explicitly "no chevron". The
        // owner asked for the accordion anyway. The view mode survives
        // untouched but no longer means "open all" (owner 2026-08-01): the arm
        // opens fully CLOSED and what the user opens here is remembered, per
        // collection, across view switches and restarts.
        //
        // SCOPED to the details arm 2026-07-31 (`showExpander`): the plain LIST
        // arm shows no chevron and is the reference's 8-column row again. Do
        // not remove the accordion itself — only this gate is the owner's.
        //
        // The MouseArea is INVISIBLE on a non-expandable row, and an invisible
        // item is not hit-tested — so a track row's chevron cell falls through
        // to `rowArea` behind it and clicking there still opens the item, which
        // is the gesture the owner asked to keep. On an expandable row this
        // area sits at the default z, above `rowArea`'s `z: -1`, and swallows
        // the click: the chevron toggles ONLY this row, it never navigates.
        Item {
            visible: root.showExpander
            width: 24
            height: parent.height
            QbzIcon {
                visible: root.canExpand
                anchors.centerIn: parent
                name: "chevron-right"
                width: 14
                height: 14
                tintName: (root.rowOpen || chevArea.containsMouse) ? "textPrimary" : "muted"
                // Points right when closed, down when open. `rotation` is a
                // plain QQuickItem property and QbzIcon is an Image, so this
                // costs no effect pass and no extra glyph.
                rotation: root.rowOpen ? 90 : 0
                Behavior on rotation {
                    NumberAnimation { duration: 140; easing.type: Easing.OutCubic }
                }
            }
            MouseArea {
                id: chevArea
                visible: root.canExpand
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: QbzMyQbz.detailToggleRowExpand(root.sourceItemId)
            }
        }

        // --- Col 1 (40) — ordinal / selection checkbox ------------------
        Item {
            width: 40
            height: parent.height
            Text {
                visible: !root.selectMode
                anchors.centerIn: parent
                text: root.ordinal
                color: theme.textMuted
                font.pixelSize: 13
            }
            SelectCheck {
                visible: root.selectMode
                anchors.centerIn: parent
                on: root.selected
                onToggled: function (mods) {
                    QbzMyQbz.detailToggleItemSelect(root.itemPosition,
                                                    (mods & Qt.ShiftModifier) !== 0)
                }
            }
        }

        // --- Col 2 (1fr) — artwork + title / subtitle -------------------
        Row {
            width: root.titleColWidth
            height: parent.height
            spacing: 12

            Rectangle {
                width: 36
                height: 36
                anchors.verticalCenter: parent.verticalCenter
                radius: 4
                color: theme.surfaceElevated
                clip: true
                RoundedImage {
                    visible: root.artPath !== ""
                    anchors.fill: parent
                    source: root.artPath
                    radius: 4
                }
                QbzIcon {
                    visible: root.artPath === ""
                    anchors.centerIn: parent
                    name: root.typeGlyph
                    width: 18
                    height: 18
                    tintName: "muted"
                }
                // Hover play overlay (.slint :275-291).
                Rectangle {
                    anchors.fill: parent
                    color: "#99000000"
                    opacity: artArea.containsMouse ? 1.0 : 0.0
                    Behavior on opacity { NumberAnimation { duration: 150 } }
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "play-fill"
                        width: 16
                        height: 16
                        tintName: "white"
                    }
                }
                MouseArea {
                    id: artArea
                    anchors.fill: parent
                    // In select mode the whole row selects (Shift / Ctrl
                    // included): the artwork, title and artist stop playing
                    // or navigating so the click reaches the row body.
                    enabled: !root.selectMode
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzMyQbz.playItem(root.sourceItemId)
                }
            }

            Column {
                width: Math.max(0, parent.width - 48)
                anchors.verticalCenter: parent.verticalCenter
                spacing: 2
                // Title — its OWN 18px hit area (.slint :298-313).
                Item {
                    width: parent.width
                    height: 18
                    Text {
                        anchors.fill: parent
                        text: root.rowTitle
                        color: titleArea.containsMouse ? theme.accent : theme.textPrimary
                        font.pixelSize: 14
                        font.weight: theme.weightMedium
                        elide: Text.ElideRight
                        verticalAlignment: Text.AlignVCenter
                    }
                    MouseArea {
                        id: titleArea
                        anchors.fill: parent
                        enabled: !root.selectMode
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.openThis()
                    }
                }
                // Subtitle — 16px hit area, pointer only when it is a link.
                Item {
                    visible: root.rowSubtitle !== ""
                    width: parent.width
                    height: 16
                    Text {
                        anchors.fill: parent
                        text: root.rowSubtitle
                        color: (root.subtitleIsLink && subArea.containsMouse)
                            ? theme.accent : theme.textMuted
                        font.pixelSize: 12
                        elide: Text.ElideRight
                        verticalAlignment: Text.AlignVCenter
                    }
                    MouseArea {
                        id: subArea
                        anchors.fill: parent
                        enabled: !root.selectMode
                        hoverEnabled: true
                        cursorShape: root.subtitleIsLink ? Qt.PointingHandCursor : Qt.ArrowCursor
                        onClicked: {
                            if (root.subtitleIsLink)
                                QbzMyQbz.openArtist(root.itemSource, root.rowSubtitle, root.artistId)
                        }
                    }
                }
            }
        }

        // --- Col 3 (140) — type ----------------------------------------
        Item {
            width: 140
            height: parent.height
            Row {
                anchors.verticalCenter: parent.verticalCenter
                spacing: 6
                QbzIcon {
                    name: root.typeGlyph
                    width: 13
                    height: 13
                    anchors.verticalCenter: parent.verticalCenter
                    tintName: "muted"
                }
                Text {
                    text: root.typeLabel
                    color: theme.textMuted
                    font.pixelSize: 10
                    font.weight: theme.weightSemibold
                    font.letterSpacing: 1.2
                    anchors.verticalCenter: parent.verticalCenter
                }
            }
        }

        // --- Col 4 (80) — source glyph (the `!== ""` gate is the .slint's,
        // :370) ----------------------------------------------------------
        Item {
            width: 80
            height: parent.height
            // #766: a playlist can mix Qobuz + local/media-server tracks, so the
            // source column shows one glyph per DISTINCT source. Falls back to
            // the single resolved kind while the list is empty (pre-resolve).
            Row {
                anchors.verticalCenter: parent.verticalCenter
                spacing: 4
                Repeater {
                    model: root.sourceKinds.length > 0
                           ? root.sourceKinds
                           : (root.sourceKind !== "" ? [root.sourceKind] : [])
                    SourceIcon {
                        kind: modelData
                        // Monochrome tinted marks (colour logos are for cards).
                        mono: true
                        glyphSize: 15
                        qobuzSize: 16
                        localTint: "muted"
                    }
                }
            }
        }

        // --- Col 5 (160) — quality ---------------------------------------
        Item {
            width: 160
            height: parent.height
            QualityBadgeFull {
                visible: root.qualityTier !== "" && !root.hideQuality
                x: 0
                anchors.verticalCenter: parent.verticalCenter
                tier: root.qualityTier
                detail: root.qualityDetail
            }
            // The .slint's `LoadingDots` inherits Rectangle, so it FILLS this
            // 160px cell and centres its dot row in it (LoadingDots.slint
            // :26-35) — hence `anchors.fill`, not `x: 0`.
            QbzLoadingDots {
                visible: root.qualityTier === "" && root.qualityResolving && !root.hideQuality
                anchors.fill: parent
                phase: root.dotPhase
            }
        }

        // --- Col 6 (72) — tracks, right-aligned --------------------------
        Text {
            width: 72
            height: parent.height
            text: root.tracksText
            color: theme.textSecondary
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignRight
            verticalAlignment: Text.AlignVCenter
        }

        // --- Col 7 (60) — year, right-aligned ----------------------------
        Text {
            width: 60
            height: parent.height
            text: root.yearText
            color: theme.textSecondary
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignRight
            verticalAlignment: Text.AlignVCenter
        }

        // --- Col 8 (40) — ⋯ menu -----------------------------------------
        Item {
            width: 40
            height: parent.height
            Rectangle {
                id: menuBtn
                x: parent.width - width
                width: 24
                height: 24
                anchors.verticalCenter: parent.verticalCenter
                radius: 4
                color: menuArea.containsMouse ? theme.surfaceHover : "transparent"
                QbzIcon {
                    anchors.centerIn: parent
                    name: "ellipsis"
                    width: 14
                    height: 14
                    tintName: menuArea.containsMouse ? "textPrimary" : "muted"
                }
                MouseArea {
                    id: menuArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    // Slint anchors the 184px panel `x: btn.width - 184;
                    // y: 28` on this 24px button (:438-439) — which is exactly
                    // what `openBelowRight` computes (24 + 4 = 28).
                    onClicked: rowMenu.openBelowRight(menuBtn)
                }
            }
        }
    }

    // Row ⋯ menu (.slint :437-488), 184px, in the .slint's order.
    CardMenu {
        id: rowMenu
        menuWidth: 184
        entries: root.rowMenuModel()
        onPicked: function (a) { root.rowMenuAction(a) }
    }

    // A track opens the ALBUM it is on (its resolved `albumId`), routed
    // source-aware by `open_item("album", …)` (Qobuz vs local). An album or
    // playlist opens itself. A track whose album has NOT resolved yet (empty id)
    // is ignored — like the artist link — instead of sending "" and landing on a
    // blank page (#766, problem 1).
    function openThis() {
        if (root.itemType === "track") {
            if (root.albumId !== "")
                QbzMyQbz.openItem(root.itemSource, "album", root.albumId)
        } else {
            QbzMyQbz.openItem(root.itemSource, root.itemType, root.sourceItemId)
        }
    }

    function rowMenuModel() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        return [
            { "label": t("Play", r), "icon": "play-fill", "action": "play" },
            { "label": t("Play next", r), "icon": "list-start", "action": "play-next" },
            { "label": t("Play later", r), "icon": "list-plus", "action": "play-later" },
            { "label": t("Add to queue", r), "icon": "list-end", "action": "add-to-queue" },
            { "sep": true },
            { "label": root.detailKind === "mixtape"
                       ? t("Remove from Mixtape", r) : t("Remove from collection", r),
              "icon": "trash-2", "action": "remove", "danger": true },
        ]
    }

    function rowMenuAction(a) {
        if (a === "play") QbzMyQbz.playItem(root.sourceItemId)
        else if (a === "remove") QbzMyQbz.removeItem(root.itemPosition)
        else QbzMyQbz.itemAction(root.sourceItemId, a)
    }
}
