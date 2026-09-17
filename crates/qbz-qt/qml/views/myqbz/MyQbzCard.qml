// MyQbzCard — the Mixtape/Collection index tile. ONE file for BOTH of
// MyQbzShared.slint's index widgets, because they render the same fields with
// the same affordance and differ only in shape:
//   * default arm  = `MyQbzCard` (:76-116)     — the 208x296 grid card;
//   * `compact`    = `MyQbzListRow` (:120-163) — the 64px list row.
// Slint keeps them apart only because its layouts are declarative; folding
// them here is the same call MyQbzGridView.qml makes for the two views.
//
// Grid card: 208 wide x (184 + 12 + 12 + 18 + 2*20 + 18 + 12) = 296 tall, r8,
// bg hover -> surface-hover, border 1px that ALSO goes surface-hover on hover
// (:82-84); inner padding 12 all round, spacing 8, top-aligned: 184 mosaic, a
// 2px spacer, a 40px 2-line name clamp (so cards with a one-line and a two-line
// name still align), then the muted subtitle.
// List row: 64 tall, r6, bg hover -> surface-hover else TRANSPARENT, padding
// 12 left/right, spacing 12, a 48px mosaic and a 2px-spaced text column.
//
// Both are entirely `item.*` driven (the GridDoc `GridCard` object) and the
// WHOLE tile is the hit target with a pointer cursor. There is NO context menu
// on either shape in Slint and no hover animation is declared — do not add
// either.
//
// The two arms live behind a Loader because the mosaic is a Canvas with up to
// nine image probes: mounting both trees would double that per delegate.

import QtQuick
import com.blitzfc.qbz
import "../../cards"
import "../../theme"

Rectangle {
    id: root

    /// One GridDoc `GridCard`: id, name, kind, label, meta, itemCount,
    /// hasCustomCover, customCoverPath, coverCount, cellUrls, cellPaths.
    property var item: ({})
    /// false = 208x296 grid card, true = 64px list row.
    property bool compact: false
    signal clicked()

    QbzTheme { id: theme }

    // implicitWidth (not width): the grid delegate wants the fixed 208 and the
    // list delegate assigns the view's width over it.
    implicitWidth: 208
    height: root.compact ? 64 : (184 + 12 + 12 + 18 + 2 * 20 + 18 + 12)
    radius: root.compact ? 6 : 8
    color: cardArea.containsMouse ? theme.surfaceHover
        : (root.compact ? "transparent" : theme.surfaceCard)
    border.width: root.compact ? 0 : 1
    border.color: cardArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

    // `label · meta`, but never a bare " · ": a default/half-built `item` (a
    // standalone instance, or a pre-publish frame) has neither half and would
    // otherwise render the separator on its own.
    readonly property string subtitle: {
        var l = root.item.label || ""
        var m = root.item.meta || ""
        return (l !== "" && m !== "") ? (l + " · " + m) : (l + m)
    }

    Loader {
        anchors.fill: parent
        sourceComponent: root.compact ? rowBody : cardBody
    }

    // --- Grid card ------------------------------------------------------
    Component {
        id: cardBody
        Column {
            x: 12
            y: 12
            width: parent.width - 24
            spacing: 8

            CollectionMosaic {
                gridArtwork: true
                size: 184
                urls: root.item.cellUrls || []
                paths: root.item.cellPaths || []
                coverCount: root.item.coverCount || 0
                kind: root.item.kind || ""
                itemCount: root.item.itemCount || 0
                hasCustomCover: root.item.hasCustomCover === true
                customCoverPath: root.item.customCoverPath || ""
            }
            Item { width: 1; height: 2 }
            Text {
                width: parent.width
                // 2 lines RESERVED, not "up to 2" — the clamp is what keeps a
                // row of cards aligned (MyQbzShared.slint:102).
                height: 40
                text: root.item.name || ""
                color: theme.textPrimary
                font.pixelSize: theme.fontBody
                font.weight: theme.weightSemibold
                wrapMode: Text.WordWrap
                elide: Text.ElideRight
            }
            Text {
                width: parent.width
                text: root.subtitle
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                elide: Text.ElideRight
            }
        }
    }

    // --- List row -------------------------------------------------------
    Component {
        id: rowBody
        Row {
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 12
            spacing: 12

            CollectionMosaic {
                gridArtwork: true
                anchors.verticalCenter: parent.verticalCenter
                size: 48
                urls: root.item.cellUrls || []
                paths: root.item.cellPaths || []
                coverCount: root.item.coverCount || 0
                kind: root.item.kind || ""
                itemCount: root.item.itemCount || 0
                hasCustomCover: root.item.hasCustomCover === true
                customCoverPath: root.item.customCoverPath || ""
            }
            Column {
                anchors.verticalCenter: parent.verticalCenter
                width: Math.max(0, parent.width - 48 - 12)
                spacing: 2
                Text {
                    width: parent.width
                    text: root.item.name || ""
                    color: theme.textPrimary
                    font.pixelSize: theme.fontBody
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: root.subtitle
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    elide: Text.ElideRight
                }
            }
        }
    }

    MouseArea {
        id: cardArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.clicked()
    }
}
