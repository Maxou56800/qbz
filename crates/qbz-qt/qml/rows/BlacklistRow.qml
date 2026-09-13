// BlacklistRow — the Blacklist manager's row, ONE file with THREE arms.
//
// The Slint reference declares three components
// (settings/BlacklistManagerView.slint: ArtistRow :31-135, AlbumRow :138-246,
// DismissedRow :251-336) and they share the frame, the hover rule, the text
// column and the remove button VERBATIM — they differ only in the leading
// visual and in which meta lines exist. Collapsing them is the phase-21/22
// TrackRow unification precedent (4 copies -> 1, -716 lines) that
// TRACK-RULES §5 cites.
//
// Common frame, all three arms:
//   height 64 · radius Radius.md (12) · hovered ? surface-elevated
//   : surface-card, where hovered = BODY hover OR REMOVE-button hover
//   (:36, :143, :254) · padding left 16 / right 8 (:49-50) · gap 12 (:51)
//   · body cursor pointer, click opens the artist / album (:57-60)
//   · text column vertically centred, spacing 2 (:83-85)
//
//   arm         leading visual                       lines
//   artist      44x44 circle (r22) surface-elevated  name 15/medium ·
//               + blind-eye 20x20 muted (:67-80)     "Added {}" 13 muted ·
//                                                    notes 13 muted ITALIC
//                                                    when has-notes (:99-105)
//   album       48x48 rounded-8 clipped tile:        title 15/medium ·
//               blind-eye 20x20 muted UNDERNEATH,    artist 13 TEXT-SECONDARY ·
//               the cover on top (:176-194)          "Added {}" 13 muted
//   dismissed   44x44 circle + `user` 20x20 muted    name 15/medium ONLY, and
//               — NOT blind-eye, this is not the     the column has NO spacing
//               blacklist (:283-296, note :248-250)  (:298-307)
//
// Remove / undo button, identical in all three (:113-132, :224-243, :314-333):
// 36x36, Radius.sm, danger-bg on hover else transparent, `x` glyph 18x18,
// pointer cursor, OPTIMISTIC — no confirm.
//
// TINT TRAP: the Slint tints that glyph `Theme.danger` on hover. There is NO
// `danger` tint in this port — the vocabulary is a closed list of NAMES
// (src/icon_tint_qt.rs:276-293, theme/QbzIcon.qml:32-58) and a name outside it
// resolves to a missing directory, so the glyph renders NOTHING with NOTHING
// logged (QbzIcon.qml:93-105). The port's red glyph tint is `favorite`
// (#ef4444 vs the Slint's #e0564f — the already-accepted substitution,
// controls/CardMenu.qml:20-26, controls/SettingsButton.qml:38).
// That also needs assets/icons/favorite/x.svg, which the deliberately partial
// 52-of-96 `favorite/` dir does not carry — the runtime bake produces it, but
// the qrc FLOOR is reachable (no writable cache dir, a pruned tint dir, a
// pre-publish theme document) and on the floor the glyph vanishes.
//
// The cover is theme/RoundedImage.qml (the Canvas raster clip — QML `clip` is
// rectangular and shader masks draw nothing on this port's software path). The
// blind-eye fallback sits UNDER it and is never hidden, exactly the Slint
// layering, so there is no placeholder to gate on `ready` here.

// KIOSK ARM (2026-09-07, K6): `kioskHost` defaults to FALSE and every metric
// below falls back to the desktop number it had before the property existed —
// 64px frame, 36px remove button, fontBody/fontLegal, RoundedImage. The kiosk
// arm raises the frame to the contract's 76px (§2.6: the whole row IS the
// primary target, and it must clear 64), the remove button to 44, the type to
// 1.2x, and swaps the cover for kiosk/KioskArtwork through an explicitly
// conditional Loader — RoundedImage has NO sourceSize by construction
// (theme/RoundedImage.qml:402) so a 48px tile decodes the full original.

import QtQuick
import com.blitzfc.qbz
import "../kiosk"
import "../theme"

Rectangle {
    id: root

    /// "artist" | "album" | "dismissed"
    property string arm: "artist"
    /// One row object out of blacklistJson's artists[] / albums[] / dismissed[].
    property var row: ({})
    /// Resolved cover path for the album arm ("" = show the blind-eye only).
    property string coverSource: ""
    /// Explicit density opt-in. Default = desktop, unchanged.
    property bool kioskHost: false

    signal selectRequested()
    signal removeRequested()

    QbzTheme { id: theme }

    readonly property bool hovered: bodyArea.containsMouse || removeArea.containsMouse
    readonly property bool isAlbum: root.arm === "album"
    readonly property int leadSize: root.isAlbum ? 48 : 44

    height: root.kioskHost ? 76 : 64
    radius: theme.radiusMd
    color: root.hovered ? theme.surfaceElevated : theme.surfaceCard

    // ---- body (clickable: opens the artist / album page) -----------------
    Item {
        id: bodyRegion
        anchors.left: parent.left
        anchors.leftMargin: 16
        anchors.top: parent.top
        anchors.bottom: parent.bottom
        anchors.right: removeSlot.left
        anchors.rightMargin: 12

        MouseArea {
            id: bodyArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.selectRequested()
        }

        Row {
            anchors.fill: parent
            spacing: 12

            // Leading visual.
            Item {
                id: leadWrap
                width: root.leadSize
                height: parent.height
                Rectangle {
                    anchors.centerIn: parent
                    width: root.leadSize
                    height: root.leadSize
                    // Radius.sm on the album tile, a full circle otherwise.
                    radius: root.isAlbum ? theme.radiusSm : (root.leadSize / 2)
                    color: theme.surfaceElevated
                    // No clip: the children are a 20px centred QbzIcon and a
                    // self-confining RoundedImage. One batch root per row.

                    QbzIcon {
                        anchors.centerIn: parent
                        name: root.arm === "dismissed" ? "user" : "blind-eye"
                        width: 20
                        height: 20
                        tintName: "muted"
                    }
                    // Desktop keeps RoundedImage VERBATIM — same object, same
                    // `visible` binding, same fade. Kiosk adds the
                    // decode-sized primitive behind a Loader that is inert
                    // (`active: false`) on every desktop mount.
                    RoundedImage {
                        visible: root.isAlbum && root.coverSource !== ""
                            && !root.kioskHost
                        anchors.fill: parent
                        source: root.kioskHost ? "" : root.coverSource
                        radius: theme.radiusSm
                    }
                    Loader {
                        anchors.fill: parent
                        active: root.isAlbum && root.coverSource !== ""
                            && root.kioskHost
                        sourceComponent: KioskArtwork {
                            source: root.coverSource
                            radius: theme.radiusSm
                        }
                    }
                }
            }

            // Name / meta column.
            Column {
                width: Math.max(0, bodyRegion.width - leadWrap.width - 12)
                anchors.verticalCenter: parent.verticalCenter
                // The dismissed arm is one line and its Slint column sets no
                // spacing at all (:298-307).
                spacing: root.arm === "dismissed" ? 0 : 2

                Text {
                    width: parent.width
                    text: root.isAlbum ? (root.row.title || "") : (root.row.name || "")
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost
                        ? theme.fontBody * 1.2 : theme.fontBody
                    font.weight: theme.weightMedium
                    elide: Text.ElideRight
                }
                // Album arm only — and text-SECONDARY, not muted (:206-210).
                Text {
                    visible: root.isAlbum
                    width: parent.width
                    text: root.row.artist || ""
                    color: theme.textSecondary
                    font.pixelSize: root.kioskHost
                        ? theme.fontLegal * 1.2 : theme.fontLegal
                    elide: Text.ElideRight
                }
                // The dismissal store carries no timestamp, so this line has no
                // dismissed arm (reco_dismiss's DismissedArtist has no addedAt).
                Text {
                    visible: root.arm !== "dismissed"
                    width: parent.width
                    // Unix seconds from the store, formatted in the user's
                    // locale here (the Rust side carries no month names).
                    text: QbzSession.tr("Added {}", QbzSession.trRev)
                        .replace("{}", (root.row.addedAt || 0) > 0
                            ? Qt.formatDate(new Date(root.row.addedAt * 1000), Locale.LongFormat)
                            : "")
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost
                        ? theme.fontLegal * 1.2 : theme.fontLegal
                    elide: Text.ElideRight
                }
                // Notes render on the ARTIST rows only. The album row carries
                // the field in the store but the Slint renders added-display in
                // that slot instead (:196-218) — port the asymmetry as-is.
                Text {
                    visible: root.arm === "artist" && root.row.hasNotes === true
                    width: parent.width
                    text: root.row.notes || ""
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    font.italic: true
                    elide: Text.ElideRight
                }
            }
        }
    }

    // ---- remove / undo (optimistic, no confirm) ---------------------------
    Item {
        id: removeSlot
        // Secondary control: the contract's 44px floor in kiosk (§2.6).
        width: root.kioskHost ? 44 : 36
        height: root.kioskHost ? 44 : 36
        anchors.right: parent.right
        anchors.rightMargin: 8
        anchors.verticalCenter: parent.verticalCenter

        Rectangle {
            anchors.fill: parent
            radius: theme.radiusSm
            color: removeArea.containsMouse ? theme.dangerBg : "transparent"
            QbzIcon {
                anchors.centerIn: parent
                name: "x"
                width: 18
                height: 18
                // NOT "danger" — that tint does not exist; see the header.
                tintName: removeArea.containsMouse ? "favorite" : "muted"
            }
            MouseArea {
                id: removeArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.removeRequested()
            }
        }
    }
}
