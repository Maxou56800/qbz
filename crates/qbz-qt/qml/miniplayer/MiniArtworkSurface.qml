// MiniArtworkSurface — surface 2, THE DEFAULT (2026-08-03 miniplayer/tray
// contract §4.2.2), port of
// `crates/qbz-ui/ui/miniplayer/MiniArtworkSurface.slint` (73 L, read whole).
//
// A large centred cover over centred metadata, in the resizable expanded
// window. DISPLAY ONLY.
//
// DEFERRED POLISH, REPRODUCED 1:1: the Tauri build ran a hover-triggered
// marquee per overflowing text line; this elides, exactly as the reference does
// and for the reason its own header records (:4-6). Do not add the marquee here
// before it lands there.
//
// Where compact falls back to a "—" string, this surface uses CONDITIONAL
// MOUNTS: an empty artist or album makes the row disappear and the Column's
// 2 px spacing collapse with it. In a QML Column an invisible child is skipped
// along with its spacing, so `visible:` is the exact equivalent of Slint's
// `if artist != "":`.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    objectName: "miniMetadata"

    QbzTheme { id: theme }

    /// Handed down by MiniShell (see MiniCompactSurface for why it is a
    /// property and not a `parent` read).
    property bool npExplicit: false
    property bool backdropActive: false
    // AlbumView header foregrounds on the dark ambient backdrop.
    readonly property bool lightText: backdropActive && theme.isDark
    readonly property color headerStrong: lightText ? "#ffffff" : theme.textPrimary
    readonly property color headerBody: lightText ? "#e0ffffff" : theme.textSecondary

    // Max 320, 1:1, shrinking with the resizable window: the card is 40 px
    // wider than the cover at the padding, so this is 320 at the 368 px card
    // and 288 at the 340 px minimum width (:14). Cached, per §7-M2.
    readonly property int coverSize: Math.min(root.width - 40, 320)

    // VerticalLayout { padding-left/right 20; padding-top 16;
    //                  padding-bottom 12; spacing 12; alignment: center }
    // (:16-24). `alignment: center` centres the content inside the PADDED box,
    // and that box is asymmetric (16 above, 12 below), so the stack sits 2 px
    // below the item's own centre. Written out rather than anchored to
    // verticalCenter so the asymmetry is visible instead of accidental.
    Column {
        id: stack
        x: 20
        width: root.width - 40
        y: 16 + Math.max(0, (root.height - 28 - stack.height) / 2)
        spacing: 12

        // HorizontalLayout { alignment: center } (:26-27).
        Item {
            width: stack.width
            height: root.coverSize
            MiniCoverArt {
                anchors.horizontalCenter: parent.horizontalCenter
                width: root.coverSize
                height: root.coverSize
                // Overrides the 7 px default (:30).
                radius: 10
                source: QbzPlayer.npArtworkPath
            }
        }

        // The meta block: VerticalLayout { spacing: 2px } — note it carries NO
        // `alignment`, unlike compact's (:37-38). Each row centres itself.
        Column {
            width: stack.width
            spacing: 2

            // Title + badge row, spacing 5, title at stretch 1 (:40-53). The
            // title is centred WITHIN its stretched box while the badge sits at
            // the row's right edge — that is what the reference's layout does,
            // not an approximation of it.
            Item {
                width: parent.width
                height: title.implicitHeight

                Text {
                    id: title
                    anchors.left: parent.left
                    width: parent.width - (badge.visible ? badge.width + 5 : 0)
                    text: QbzPlayer.npHasTrack
                          ? QbzPlayer.npTitle
                          : QbzSession.tr("No track playing", QbzSession.trRev)
                    color: root.headerStrong
                    font.pixelSize: 13
                    font.weight: Font.DemiBold
                    horizontalAlignment: Text.AlignHCenter
                    elide: Text.ElideRight
                }

                MiniExplicitBadge {
                    id: badge
                    visible: root.npExplicit
                    anchors.left: title.right
                    anchors.leftMargin: 5
                    anchors.verticalCenter: title.verticalCenter
                }
            }

            // CONDITIONAL (:56-62) — the row disappears when empty, and there
            // is no fallback string anywhere on this surface.
            Text {
                width: parent.width
                visible: QbzPlayer.npArtist !== ""
                text: QbzPlayer.npArtist
                color: root.headerBody
                font.pixelSize: 12
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
            }

            // CONDITIONAL (:64-70), and it exists ONLY on this surface —
            // compact has no album line at all. alphaTier(55), NOT textMuted.
            Text {
                width: parent.width
                visible: QbzPlayer.npAlbum !== ""
                text: QbzPlayer.npAlbum
                color: root.headerBody
                font.pixelSize: 12
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
            }
        }
    }
}
