// PurchaseTrackRow — one row of the Purchases TRACKS tab. 1:1 port of
// purchases/PurchasesView.slint's `PurchaseTrackRow` (:400-560).
//
// NOT rows/TrackRow.qml. The .slint's own header names the reason and it holds
// here too: this row's columns are Purchases-only (a BARE quality string, a
// purchase-date column, a download slot) and it has neither the number/play
// cell, nor the heart, nor the ⋯ menu that the shared row is built around —
// §15.1 explicitly forbids a track context menu on this screen. It also has no
// column header above it, so `rows/TrackCols.qml`'s whole reason to exist (a
// header and a row that cannot disagree) does not apply.
//
// 56px, Radius.sm: 40x40 art · title over "artist · album" · a 64px quality
// column · a 44px duration column · a 100px purchase-date column · a 30px
// download slot.
//
// STATE, and one deliberate reduction from the .slint. The reference's download
// slot is a FOUR-state control (idle / downloading / complete / failed) that
// opens an anchored format picker. §G carries no picker seam — `list_json`
// tracks publish `downloaded` and nothing else about a download, and §G.1 has
// no picker invokable — so the slot here is a read-only INDICATOR: a success
// check on a downloaded row, an empty reserve otherwise. The column geometry is
// the reference's, so the control can be armed later without moving a pixel.
// (Flagged to the orchestrator rather than invented.)
//
// A DOUBLE click on the row body plays (the track-row policy, #790: a single
// click does nothing), and only when `streamable` — the purchases-list default
// is TRUE (§G.2), so an unplayable row is one the API really did mark that way.

import QtQuick
import com.blitzfc.qbz
import "../../theme"

Rectangle {
    id: root

    /// One row of `list_json.tracks` (§G.2).
    property var track: ({})
    property string artSource: ""
    /// Row index — the odd-row zebra every other track list draws.
    property int rowIndex: 0
    signal playRequested()

    QbzTheme { id: theme }

    readonly property bool streamable: root.track.streamable !== false
    readonly property bool downloaded: root.track.downloaded === true
    /// The now-playing row, read from the shared player state — the .slint's
    /// `PurchasesState.active-track-id` has no equivalent in `list_json`, and
    /// `QbzPlayer.npTrackId` is what every other row in this port asks
    /// (rows/TrackRow.qml:324).
    readonly property bool active: QbzPlayer.npTrackId === (root.track.id || "")
        && (root.track.id || "") !== ""

    /// §10-C / §G.3: `title (version)`. Published empty today on the list
    /// document, so the call is inert until the producer fills it — which is
    /// exactly why it must be here and not hard-coded away.
    function displayTitle() {
        var ttl = root.track.title || ""
        var ver = root.track.version || ""
        return ver !== "" ? ttl + " (" + ver + ")" : ttl
    }

    function fmtDuration(secs) {
        var s = Math.max(0, Math.floor(secs || 0))
        var m = Math.floor(s / 60)
        var r = s % 60
        return m + ":" + (r < 10 ? "0" : "") + r
    }

    function purchaseDate() {
        var ts = root.track.purchasedAt || 0
        if (ts <= 0)
            return ""
        var ms = ts > 100000000000 ? ts : ts * 1000
        return new Date(ms).toLocaleDateString(Qt.locale(), Locale.ShortFormat)
    }

    width: parent ? parent.width : 0
    height: 56
    radius: theme.radiusSm
    // .slint:414 — the active row takes alpha-10; hover only applies to a row
    // the click can actually act on.
    color: root.active ? theme.alphaTier(10)
         : ((rowArea.containsMouse && root.streamable) ? theme.surfaceHover
         : (root.rowIndex % 2 === 1 ? theme.alphaTier(4) : "transparent"))
    // .slint:418 — a downloaded row reads as "already dealt with".
    opacity: root.downloaded ? 0.75 : 1.0

    // Declared FIRST so the cells that want their own pointer (none today, but
    // the download slot lands here) are hit-tested after it, exactly like the
    // .slint's ordering note.
    MouseArea {
        id: rowArea
        anchors.fill: parent
        enabled: root.streamable
        hoverEnabled: true
        onDoubleClicked: root.playRequested()
    }

    Row {
        anchors.fill: parent
        anchors.leftMargin: 8
        anchors.rightMargin: 12
        spacing: 12

        // 40x40 artwork.
        Item {
            width: 40
            height: parent.height
            Rectangle {
                width: 40
                height: 40
                anchors.verticalCenter: parent.verticalCenter
                radius: theme.radiusSm
                color: theme.surfaceElevated
                RoundedImage {
                    id: art
                    anchors.fill: parent
                    // `artPath`, never `artworkUrl` — see PurchaseGridCard.
                    source: root.artSource !== "" ? root.artSource
                                                  : (root.track.artPath || "")
                    radius: theme.radiusSm
                }
                QbzIcon {
                    visible: !art.ready
                    name: "music"
                    width: 16
                    height: 16
                    anchors.centerIn: parent
                    tintName: "muted"
                }
            }
        }

        // Title over the "artist · album" meta line — the stretch column.
        Column {
            width: Math.max(0, parent.width - 40
                - (qualityCell.visible ? 64 + 12 : 0)
                - 44 - 100 - 30 - 4 * 12)
            anchors.verticalCenter: parent.verticalCenter
            spacing: 2
            Text {
                width: parent.width
                text: root.displayTitle()
                color: root.active ? theme.accent : theme.textPrimary
                font.pixelSize: theme.fontBody
                font.weight: theme.weightMedium
                elide: Text.ElideRight
            }
            // `artist · album`. The .slint makes the artist a link; the list
            // document carries no `artistId` (§G.2), so there is no page to
            // route to and a pointer that promises one would lie. Plain text
            // until the seam grows the field.
            Text {
                width: parent.width
                text: {
                    var a = root.track.artist || ""
                    var al = root.track.album || ""
                    return al !== "" ? (a + " · " + al) : a
                }
                color: theme.textMuted
                font.pixelSize: theme.fontLegal
                elide: Text.ElideRight
            }
        }

        // Quality column — BARE, and dropped entirely when the row has none
        // (.slint:498).
        Text {
            id: qualityCell
            visible: (root.track.qualityTier || "") !== ""
            width: 64
            height: parent.height
            text: root.track.qualityTier || ""
            color: theme.textSecondary
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignRight
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }

        // Duration.
        Text {
            width: 44
            height: parent.height
            text: root.fmtDuration(root.track.duration)
            color: theme.textMuted
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignRight
            verticalAlignment: Text.AlignVCenter
        }

        // Purchase date.
        Text {
            width: 100
            height: parent.height
            text: root.purchaseDate()
            color: theme.textMuted
            font.pixelSize: theme.fontLegal
            horizontalAlignment: Text.AlignRight
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
        }

        // Download slot — see the header: the reference's 4-state control,
        // reduced to its "complete" arm because §G publishes no other state.
        Item {
            width: 30
            height: parent.height
            QbzIcon {
                visible: root.downloaded
                name: "check"
                width: 16
                height: 16
                anchors.centerIn: parent
                // The .slint paints this one `Theme.success`; QbzIcon's tint
                // vocabulary has no success arm, and "accent" is the token this
                // port uses for a positive glyph on a theme surface.
                tintName: "accent"
            }
        }
    }
}
