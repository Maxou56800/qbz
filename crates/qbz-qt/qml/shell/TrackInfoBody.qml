// TrackInfoBody — the scrolling content of the Track Info modal (the
// `card-body` VerticalLayout of crates/qbz-ui/ui/album/TrackInfoModal.slint),
// split out of TrackInfoModal.qml for the size rule. `host` is the modal: it
// owns the parsed document, the close() and the nav actions.
//
// Sections, verbatim from the .slint:
//   loading  -> 60px padding all round, centered muted body text
//   error    -> 60px padding, 8px gap, headline + the error string (legal)
//   loaded   -> header (24 L/R, 16 T/B, 16 gap, 32x32 close X with an 18px
//               glyph) then the content block (24 L/R, 24 bottom):
//               metadata rows 24 apart in three equal columns · credits
//               behind 20 / 1px surface-elevated rule / 20, TWO independent
//               columns 24 apart with 20 between cells · copyright behind the
//               same 20 / rule / 20, 12px muted, wrapped.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Column {
    id: body
    /// The TrackInfoModal that owns the data + actions.
    required property var host

    QbzTheme { id: theme }

    /// Legibility mode for the immersive split panel (2026-08-31 visual
    /// cleanup): there the body sits directly on the ambient field, where
    /// theme tokens have no contrast guarantee over a light cover. On, every
    /// text uses a fixed light color plus the restrained native shadow the
    /// lyrics surfaces use. The desktop modal keeps the theme (default off).
    property bool overAmbient: false
    readonly property color cPrimary: overAmbient ? "#f2ffffff" : theme.textPrimary
    readonly property color cMuted: overAmbient ? "#b3ffffff" : theme.textMuted
    readonly property color cRule: overAmbient ? "#2effffff" : theme.surfaceElevated
    readonly property int cStyle: overAmbient ? Text.Raised : Text.Normal
    readonly property color cShadow: "#b0000000"
    // Links follow the ALBUM palette over ambient — the theme accent has no
    // contrast guarantee there (owner, 2026-08-31 round 2).
    AmbientAccent { id: bodyAmbientAccent }
    readonly property color cAccent: overAmbient ? bodyAmbientAccent.value : theme.accent

    readonly property var doc: host ? host.doc : ({})

    // --- Copy (2026-09-13) -------------------------------------------------
    // Basic = "{Track} - {Album} - {Artist}"; full = everything the modal
    // shows, as plain text. The glyph turns into a check for a moment.
    QbzClipboard { id: clipboard }
    property bool justCopied: false
    Timer {
        id: copiedTimer
        interval: 1400
        onTriggered: body.justCopied = false
    }
    function nonEmpty(s) { return (s || "") !== "" }
    function basicText() {
        return [body.doc.title, body.doc.album, body.doc.artist].filter(body.nonEmpty).join(" - ")
    }
    function fullText() {
        var t = QbzSession.tr
        var r = QbzSession.trRev
        var d = body.doc
        var lines = [d.title, d.album, d.artist].filter(body.nonEmpty)
        var meta = []
        if (body.nonEmpty(d.duration)) meta.push(t("Duration", r) + ": " + d.duration)
        if (body.nonEmpty(d.quality)) meta.push(t("Quality", r) + ": " + d.quality)
        if (body.nonEmpty(d.isrc)) meta.push("ISRC: " + d.isrc)
        if (body.nonEmpty(d.label)) meta.push(t("Label", r) + ": " + d.label)
        if (meta.length > 0) { lines.push(""); lines = lines.concat(meta) }
        var credits = body.host ? body.host.credits : []
        if (credits.length > 0) {
            lines.push("")
            for (var i = 0; i < credits.length; i++) {
                var c = credits[i]
                lines.push((c.role || c.roleRaw || "") + ": " + (c.names || []).join(", "))
            }
        }
        if (body.nonEmpty(d.copyright)) { lines.push(""); lines.push(d.copyright) }
        return lines.join("\n")
    }
    function copy(which) {
        var text = which === "full" ? body.fullText() : body.basicText()
        if (clipboard.copy(text)) {
            body.justCopied = true
            copiedTimer.restart()
        }
    }
    CardMenu {
        id: copyMenu
        menuWidth: 200
        entries: [
            { "label": QbzSession.tr("Copy basic data", QbzSession.trRev), "icon": "copy", "action": "basic" },
            { "label": QbzSession.tr("Copy full", QbzSession.trRev), "icon": "clipboard", "action": "full" }
        ]
        onPicked: function (a) { body.copy(a) }
    }

    // ---- Loading ---------------------------------------------------------
    Item {
        visible: body.host && body.host.loading && body.host.errorText === ""
        width: parent.width
        height: visible ? 120 + loadingText.implicitHeight : 0
        Text {
            id: loadingText
            anchors.centerIn: parent
            width: Math.max(0, parent.width - 120)
            text: QbzSession.tr("Loading track info...", QbzSession.trRev)
            color: body.cMuted
            style: body.cStyle
            styleColor: body.cShadow
            font.pixelSize: theme.fontBody
            horizontalAlignment: Text.AlignHCenter
        }
    }

    // ---- Error -----------------------------------------------------------
    Item {
        visible: body.host && body.host.errorText !== ""
        width: parent.width
        height: visible ? 120 + errCol.implicitHeight : 0
        Column {
            id: errCol
            anchors.centerIn: parent
            width: Math.max(0, parent.width - 120)
            spacing: 8
            Text {
                width: parent.width
                text: QbzSession.tr("Failed to load track info", QbzSession.trRev)
                color: body.cMuted
                style: body.cStyle
                styleColor: body.cShadow
                font.pixelSize: theme.fontBody
                horizontalAlignment: Text.AlignHCenter
            }
            Text {
                width: parent.width
                text: body.host ? body.host.errorText : ""
                color: body.cMuted
                style: body.cStyle
                styleColor: body.cShadow
                font.pixelSize: theme.fontLegal
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
            }
        }
    }

    // ---- Loaded ----------------------------------------------------------
    Column {
        id: loadedCol
        visible: body.host && !body.host.loading && body.host.errorText === ""
        width: parent.width

        // Fixed credits-column width: card minus the 24px L/R content padding
        // (48) minus the 24px inter-column gutter, halved. Pinning each credit
        // cell to this keeps the right column aligned on every row.
        readonly property int contentW: Math.max(0, width - 48)
        readonly property int creditsColW: Math.max(0, (contentW - 24) / 2)
        readonly property int metaColW: Math.max(0, (contentW - 48) / 3)

        // --- Header: title / album / artist + close X ---------------------
        Item {
            width: parent.width
            height: Math.max(headCol.implicitHeight, 32) + 32

            Column {
                id: headCol
                x: 24
                y: 16
                // 24 L + 24 R padding, 16 gap, 32 close X.
                width: Math.max(0, parent.width - 24 - 24 - 16 - 32 - 8 - 32)
                spacing: 4

                // Every field is selectable (2026-09-13): QbzSelectableText,
                // raised (plain, shadowed) on the ambient host.
                QbzSelectableText {
                    width: parent.width
                    text: body.doc.title || ""
                    color: body.cPrimary
                    raised: body.overAmbient
                    raisedShadow: body.cShadow
                    // The immersive panel reads at a distance — its header
                    // steps up (owner, 2026-08-31 round 2).
                    pixelSize: body.overAmbient ? 24 : 16
                    weight: theme.weightSemibold
                }
                QbzSelectableText {
                    visible: (body.doc.album || "") !== ""
                    width: parent.width
                    text: body.doc.album || ""
                    color: body.cMuted
                    raised: body.overAmbient
                    raisedShadow: body.cShadow
                    pixelSize: body.overAmbient ? 15 : theme.fontLegal
                }
                // Artist — a link only when an id exists.
                QbzSelectableText {
                    visible: (body.doc.artist || "") !== ""
                    width: parent.width
                    text: body.doc.artist || ""
                    color: (body.doc.artistId || "") !== "" ? body.cAccent : body.cPrimary
                    linkHref: (body.doc.artistId || "") !== "" ? "artist" : ""
                    linkHoverColor: body.cAccent
                    raised: body.overAmbient
                    raisedShadow: body.cShadow
                    pixelSize: body.overAmbient ? 20 : 16
                    weight: theme.weightSemibold
                    onLinkActivated: body.host.openArtist(body.doc.artistId || "")
                }
            }

            // Copy — basic line or the full sheet, from the flyout; the
            // glyph turns into a check for a moment as the receipt.
            Item {
                id: copyBtn
                width: 32
                height: 32
                x: parent.width - 24 - 32 - 8 - 32
                y: 16
                QbzIcon {
                    name: body.justCopied ? "check" : "copy"
                    width: 17
                    height: 17
                    anchors.centerIn: parent
                    tintName: body.justCopied ? "accent"
                        : body.overAmbient
                            ? (copyArea.containsMouse ? "white" : "muted")
                            : (copyArea.containsMouse ? "textPrimary" : "muted")
                }
                MouseArea {
                    id: copyArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: copyMenu.openBelowLeft(copyBtn)
                }
            }

            // Close X — the Slint is a bare 32x32 touch area with an 18px
            // glyph, text-muted -> text-primary, with NO hover fill;
            // QbzIconButton always paints one and idles at `secondary`, so
            // this one stays hand-rolled to keep the port 1:1.
            Item {
                width: 32
                height: 32
                x: parent.width - 24 - 32
                y: 16
                QbzIcon {
                    name: "x"
                    width: 18
                    height: 18
                    anchors.centerIn: parent
                    tintName: body.overAmbient
                        ? (closeArea.containsMouse ? "white" : "muted")
                        : (closeArea.containsMouse ? "textPrimary" : "muted")
                }
                MouseArea {
                    id: closeArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: body.host.close()
                }
            }
        }

        // --- Content block (24 L/R, 24 bottom) ----------------------------
        Item {
            width: parent.width
            height: contentCol.implicitHeight + 24

            Column {
                id: contentCol
                x: 24
                width: loadedCol.contentW

                // Metadata: fixed 3-column rows.
                Row {
                    width: parent.width
                    spacing: 24
                    InfoMetaCell {
                        cellWidth: loadedCol.metaColW
                        overAmbient: body.overAmbient
                        label: QbzSession.tr("Duration", QbzSession.trRev)
                        QbzSelectableText {
                            width: loadedCol.metaColW
                            text: body.doc.duration || ""
                            color: body.cPrimary
                            raised: body.overAmbient
                            raisedShadow: body.cShadow
                            pixelSize: 14
                        }
                    }
                    InfoMetaCell {
                        cellWidth: loadedCol.metaColW
                        overAmbient: body.overAmbient
                        label: QbzSession.tr("Quality", QbzSession.trRev)
                        QbzSelectableText {
                            width: loadedCol.metaColW
                            text: body.doc.quality || ""
                            color: body.cPrimary
                            raised: body.overAmbient
                            raisedShadow: body.cShadow
                            pixelSize: 14
                        }
                    }
                    // ISRC — the cell is dropped (not blanked) when absent;
                    // the empty third keeps the columns fixed.
                    InfoMetaCell {
                        visible: (body.doc.isrc || "") !== ""
                        cellWidth: loadedCol.metaColW
                        overAmbient: body.overAmbient
                        label: "ISRC"
                        QbzSelectableText {
                            width: loadedCol.metaColW
                            text: body.doc.isrc || ""
                            color: body.cMuted
                            raised: body.overAmbient
                            raisedShadow: body.cShadow
                            pixelSize: 14
                        }
                    }
                    Item {
                        visible: (body.doc.isrc || "") === ""
                        width: loadedCol.metaColW
                        height: 1
                    }
                }
                Item {
                    visible: (body.doc.label || "") !== ""
                    width: 1
                    height: 24
                }
                Row {
                    visible: (body.doc.label || "") !== ""
                    width: parent.width
                    spacing: 24
                    InfoMetaCell {
                        cellWidth: loadedCol.metaColW
                        overAmbient: body.overAmbient
                        label: QbzSession.tr("Label", QbzSession.trRev)
                        QbzSelectableText {
                            width: loadedCol.metaColW
                            text: body.doc.label || ""
                            color: body.cPrimary
                            linkHref: (body.doc.labelId || "") !== "" ? "label" : ""
                            linkHoverColor: body.cAccent
                            raised: body.overAmbient
                            raisedShadow: body.cShadow
                            pixelSize: 14
                            onLinkActivated: body.host.openLabel(body.doc.labelId || "")
                        }
                    }
                }

                // --- Credits: two INDEPENDENT columns ---------------------
                Item {
                    visible: body.host && body.host.credits.length > 0
                    width: 1
                    height: 20
                }
                Rectangle {
                    visible: body.host && body.host.credits.length > 0
                    width: parent.width
                    height: 1
                    color: body.cRule
                }
                Item {
                    visible: body.host && body.host.credits.length > 0
                    width: 1
                    height: 20
                }
                Row {
                    visible: body.host && body.host.credits.length > 0
                    width: parent.width
                    spacing: 24
                    Column {
                        width: loadedCol.creditsColW
                        spacing: 20
                        Repeater {
                            model: body.host ? body.host.creditsLeft : []
                            delegate: InfoCreditCell {
                                required property var modelData
                                colW: loadedCol.creditsColW
                                overAmbient: body.overAmbient
                                accentColor: body.cAccent
                                cell: modelData
                                onNameClicked: function (n, r) { body.host.openMusician(n, r) }
                            }
                        }
                    }
                    Column {
                        width: loadedCol.creditsColW
                        spacing: 20
                        Repeater {
                            model: body.host ? body.host.creditsRight : []
                            delegate: InfoCreditCell {
                                required property var modelData
                                colW: loadedCol.creditsColW
                                overAmbient: body.overAmbient
                                accentColor: body.cAccent
                                cell: modelData
                                onNameClicked: function (n, r) { body.host.openMusician(n, r) }
                            }
                        }
                    }
                }

                // --- Copyright --------------------------------------------
                Item {
                    visible: (body.doc.copyright || "") !== ""
                    width: 1
                    height: 20
                }
                Rectangle {
                    visible: (body.doc.copyright || "") !== ""
                    width: parent.width
                    height: 1
                    color: body.cRule
                }
                Item {
                    visible: (body.doc.copyright || "") !== ""
                    width: 1
                    height: 20
                }
                QbzSelectableText {
                    visible: (body.doc.copyright || "") !== ""
                    width: parent.width
                    text: body.doc.copyright || ""
                    color: body.cMuted
                    raised: body.overAmbient
                    raisedShadow: body.cShadow
                    pixelSize: 12
                }
            }
        }
    }
}
