// AlbumListHeader — primitives/AlbumListRow.slint:43-127, the column header
// above the LIST arm of an album collection (Discover Browse, Label
// Releases). 32px, 11px semibold muted labels with 0.5 letter-spacing and a
// bottom hairline.
//
// COLUMN SET, and why it is shorter than the Slint's: `AlbumListCols` there
// is rank · art · ITEM · TYPE · SOURCE · QUALITY · TRACKS · YEAR · overflow.
// The Qt transport for these pages is `home_qt::HomeCard`, which carries no
// release type and no track count, and SOURCE is hidden on both catalog
// pages anyway (`show-source: false`, DiscoverBrowseView.slint:168). Naming
// columns with nothing under them is worse than not drawing them, so the
// header lists exactly the three the rows fill. Widths are the Slint's.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    QbzTheme { id: theme }

    /// Mirrors AlbumListRow.selectMode: the rows below grow an 18px leading
    /// checkbox cell in Library > Albums multi-select, and the header has to
    /// inset by the same amount or every column label drifts off its column.
    property bool selectMode: false

    // AlbumListCols: art 52, quality 150, year 64, overflow 36, gap 12.
    readonly property int colArt: 52
    /// The release's label, between the item and its quality (2026-09-13);
    /// off on the label page itself, where every row would repeat the same.
    property bool showLabel: true
    readonly property int colLabel: showLabel ? 150 : 0
    readonly property int colQuality: 150
    readonly property int colYear: 64
    readonly property int colOverflow: 36
    readonly property int colGap: 12

    width: parent ? parent.width : 0
    height: 32
    color: "transparent"

    component ColLabel: Text {
        color: theme.textMuted
        font.pixelSize: 11
        font.weight: theme.weightSemibold
        font.letterSpacing: 0.5
        verticalAlignment: Text.AlignVCenter
        height: 32
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: 12
        anchors.rightMargin: 12
        spacing: root.colGap

        Item { visible: root.selectMode; width: visible ? 18 : 0; height: 1 }
        Item { width: root.colArt; height: 1 }
        ColLabel {
            width: parent.width - root.colArt - root.colQuality - root.colYear
                - root.colOverflow - 4 * root.colGap
                - (root.showLabel ? root.colLabel + root.colGap : 0)
                - (root.selectMode ? 18 + root.colGap : 0)
            text: QbzSession.tr("ITEM", QbzSession.trRev)
            elide: Text.ElideRight
        }
        ColLabel {
            visible: root.showLabel
            width: root.colLabel
            text: QbzSession.tr("LABEL", QbzSession.trRev)
        }
        ColLabel {
            width: root.colQuality
            text: QbzSession.tr("QUALITY", QbzSession.trRev)
        }
        ColLabel {
            width: root.colYear
            text: QbzSession.tr("YEAR", QbzSession.trRev)
            horizontalAlignment: Text.AlignHCenter
        }
        Item { width: root.colOverflow; height: 1 }
    }

    Rectangle {
        y: parent.height - 1
        width: parent.width
        height: 1
        color: theme.borderSubtle
    }
}
