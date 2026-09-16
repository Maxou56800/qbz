// MiniCompactSurface — surface 1 (2026-08-03 miniplayer/tray contract §4.2.1),
// port of `crates/qbz-ui/ui/miniplayer/MiniCompactSurface.slint` (57 L, read
// whole).
//
// A horizontal metadata strip in the 380x178 window. DISPLAY ONLY: no
// controls, no IPC, no marquee — long text ellipsis-truncates
// (MiniCompactSurface.slint:1-3). No album line, no hover, no click target, no
// context menu. Everything interactive lives in the footer.
//
// The one divergence from its artwork twin, and it is the reference's: the
// artist line here is ALWAYS mounted with a "—" fallback, where
// MiniArtworkSurface drops the row entirely. Reproduced exactly (§4.2.2).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root
    objectName: "miniMetadata"

    QbzTheme { id: theme }

    /// Handed down by MiniShell, which derives it once from the queue
    /// document — QbzPlayer publishes no np_explicit (§8 rule 1: an explicit
    /// property, never a `parent` read).
    property bool npExplicit: false
    property bool backdropActive: false
    // AlbumView header foregrounds on the dark ambient backdrop.
    readonly property bool lightText: backdropActive && theme.isDark
    readonly property color headerStrong: lightText ? "#ffffff" : theme.textPrimary
    readonly property color headerBody: lightText ? "#e0ffffff" : theme.textSecondary

    // The cover is square: the surface height minus the 12 px padding on both
    // sides, floored at 40 (MiniCompactSurface.slint:11) -> 78 px at the 102 px
    // surface. Cached, per §7-M2.
    readonly property int art: Math.max(40, root.height - 24)

    // HorizontalLayout { padding: 12px; spacing: 12px; alignment: start }
    // (:13-18), expressed as anchors: `alignment: start` means the cover leads
    // and the text block takes the rest, which is what the anchor chain below
    // does without a layout item in between.
    MiniCoverArt {
        id: cover
        x: 12
        anchors.verticalCenter: parent.verticalCenter
        width: root.art
        height: root.art
        // Overrides the 7 px default (:23).
        radius: 8
        source: QbzPlayer.npArtworkPath
    }

    Column {
        id: meta
        anchors.left: cover.right
        anchors.leftMargin: 12
        anchors.right: parent.right
        anchors.rightMargin: 12
        // VerticalLayout { alignment: center; spacing: 2px } (:29-31).
        anchors.verticalCenter: parent.verticalCenter
        spacing: 2

        // Title + badge row: HorizontalLayout { spacing: 5px } with the title
        // at stretch 1 and the badge at stretch 0 (:33-46) — so the badge sits
        // 5 px after the title's BOX, at the right edge, not flush against the
        // glyphs.
        Item {
            id: titleRow
            width: meta.width
            height: title.implicitHeight

            Text {
                id: title
                anchors.left: parent.left
                // badge.width is the constant 13, so this is not a loop.
                width: parent.width - (badge.visible ? badge.width + 5 : 0)
                text: QbzPlayer.npHasTrack
                      ? QbzPlayer.npTitle
                      : QbzSession.tr("No track playing", QbzSession.trRev)
                color: root.headerStrong
                font.pixelSize: 13
                font.weight: Font.DemiBold
                elide: Text.ElideRight
                verticalAlignment: Text.AlignVCenter
            }

            MiniExplicitBadge {
                id: badge
                visible: root.npExplicit
                anchors.left: title.right
                anchors.leftMargin: 5
                anchors.verticalCenter: title.verticalCenter
            }
        }

        // ALWAYS mounted, with a literal em-dash fallback (:49-54) — NOT
        // translated, and NOT the conditional mount the artwork surface uses.
        Text {
            width: meta.width
            text: QbzPlayer.npArtist !== "" ? QbzPlayer.npArtist : "—"
            color: root.headerBody
            font.pixelSize: 11
            elide: Text.ElideRight
        }
    }
}
