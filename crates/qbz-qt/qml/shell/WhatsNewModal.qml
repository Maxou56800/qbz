// What's New — QML port of crates/qbz-ui/ui/shell/WhatsNewModal.slint.
//
// Opened from the header hamburger menu ("What's New"). src/whats_new_qt.rs
// fetches the GitHub release matching the running version on every open,
// renders its markdown body into a FLAT BLOCK MODEL, and publishes it as
// QbzAbout.whatsNewJson: `{ open, loading, version, date, hasBody, toc[],
// blocks[] }`. The block render is the centrepiece of this file.
//
// ── WHY THIS IS NOT `Text.MarkdownText` ────────────────────────────────────
//
// The renderer's subset is NOT Markdown, and Qt's is a DIFFERENT subset. Off
// the same release body they disagree visibly:
//
//   * `**bold**` and `` `code` `` are STRIPPED by the reference, styled by Qt;
//   * an indent-0 `- item` is a level-0 SECTION + a TOC entry in the
//     reference, a bullet in Qt;
//   * `#` and `##` collapse to ONE level in the reference, two in Qt;
//   * a whole-line `[text](url)` is a KIND_LINK BLOCK in the reference, an
//     inline <a> needing onLinkActivated in Qt.
//
// So the parse stays in Rust (whats_new_qt.rs, unit-tested against a real
// TAG_DETAILS.md body) and this file only lays out blocks. Kinds: 0 section,
// 1 bullet, 2 paragraph, 3 whole-line link.
//
// ── THE TOC IS DECORATIVE, AS IT IS IN THE REFERENCE ───────────────────────
//
// WhatsNewModal.slint:122-126 records that Slint cannot scroll a Flickable to
// an arbitrary nested child, so the chips are an OVERVIEW, not jump links.
// Qt could do the real thing (the slug id is already in the block model), but
// that is a behaviour change, not a port — it is flagged in the delivery doc
// rather than smuggled in here.
//
// Structure, scrim, shadow and self-gate follow LogViewerModal.qml; z: 3000
// explicitly, per ADR-009 as this port spells it. Colour literals are
// `Qt.rgba(...)` because Slint is #RRGGBBAA and Qt is #AARRGGBB (see
// AboutModal.qml's header for the five modals that shipped an invisible scrim
// or shadow from exactly that copy, fixed 2026-08-14).
//
// No pills (ADR-008): the TOC chips are bordered radiusSm rows.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    readonly property var doc: {
        try {
            return JSON.parse(QbzAbout.whatsNewJson)
        } catch (e) {
            return ({})
        }
    }

    anchors.fill: parent
    z: 3000
    visible: root.doc.open === true
    enabled: root.visible

    QbzTheme { id: theme }

    onVisibleChanged: {
        if (visible)
            keyScope.forceActiveFocus()
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent
        Keys.onEscapePressed: QbzAbout.whatsNewClose()
    }

    Rectangle {
        anchors.fill: parent
        // Slint `#000000bf` CONVERTED.
        color: Qt.rgba(0, 0, 0, 0.75)
        MouseArea {
            anchors.fill: parent
            onClicked: QbzAbout.whatsNewClose()
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked 32px drop shadow (`drop-shadow-color: #00000080`).
    Rectangle {
        anchors.centerIn: panel
        width: panel.width + 8
        height: panel.height + 8
        radius: theme.radiusMd
        color: Qt.rgba(0, 0, 0, 0.5)
    }

    Rectangle {
        id: panel
        anchors.centerIn: parent
        // 820x700 capped, minus an 80px window margin — WhatsNewModal.slint:36-37.
        width: Math.min(root.width - 80, 820)
        height: Math.min(root.height - 80, 700)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        MouseArea {
            anchors.fill: parent
            // Wheel-lock (the DiscoverConfigModal rule).
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Item {
            id: stack
            anchors.fill: parent
            anchors.margins: 24

            // ---- Header: title + close X ------------------------------
            Item {
                id: headerRow
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: 28

                Text {
                    anchors.left: parent.left
                    anchors.right: wnClose.left
                    anchors.rightMargin: 10
                    anchors.verticalCenter: parent.verticalCenter
                    text: {
                        var v = root.doc.version || ""
                        if (v === "")
                            return QbzSession.tr("What's new", QbzSession.trRev)
                        var d = root.doc.date || ""
                        // `date` is the release's civil date as written
                        // (YYYY-MM-DD); render it in the user's locale.
                        var m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(d)
                        if (m)
                            d = Qt.formatDate(new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])),
                                              Locale.LongFormat)
                        return QbzSession.tr("What's new in v{}", QbzSession.trRev).replace("{}", v)
                            + (d === "" ? "" : " (" + d + ")")
                    }
                    color: theme.textPrimary
                    font.pixelSize: theme.fontHeading
                    font.weight: theme.weightSemibold
                    elide: Text.ElideRight
                }
                Item {
                    id: wnClose
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: wnCloseArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: wnCloseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzAbout.whatsNewClose()
                    }
                }
            }

            // ---- Body region (stretches between header and footer) ----
            Item {
                id: bodyBox
                anchors.top: headerRow.bottom
                anchors.topMargin: 16
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: footerRow.top
                anchors.bottomMargin: 16

                // Loading state.
                Text {
                    anchors.centerIn: parent
                    visible: root.doc.loading === true
                    text: QbzSession.tr("Loading…", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                }

                // Empty state — no release body available. Reached on every
                // DEV build: the fetch returns nothing for a draft or
                // prerelease tag (whats_new_qt.rs), by design.
                Text {
                    anchors.centerIn: parent
                    visible: root.doc.loading !== true && root.doc.hasBody !== true
                    width: Math.min(parent.width, 360)
                    text: QbzSession.tr("Release notes are not available.", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                    wrapMode: Text.WordWrap
                    horizontalAlignment: Text.AlignHCenter
                }

                // Rendered release body.
                Flickable {
                    id: flick
                    anchors.fill: parent
                    visible: root.doc.loading !== true && root.doc.hasBody === true
                    clip: true
                    contentWidth: width
                    contentHeight: bodyCol.height
                    boundsBehavior: Flickable.StopAtBounds

                    Column {
                        id: bodyCol
                        width: flick.width

                        // ---- TOC: the level-0 headings as a bordered index.
                        // Stacked, not wrapped, and NOT clickable — see the
                        // file header.
                        Column {
                            width: parent.width
                            spacing: 6
                            visible: (root.doc.toc || []).length > 0
                            Repeater {
                                model: root.doc.toc || []
                                delegate: Rectangle {
                                    id: tocChip
                                    required property var modelData
                                    height: 26
                                    width: tocLabel.implicitWidth + 24
                                    radius: theme.radiusSm
                                    border.width: 1
                                    border.color: theme.borderSubtle
                                    color: theme.surfaceElevated
                                    Text {
                                        id: tocLabel
                                        anchors.left: parent.left
                                        anchors.leftMargin: 12
                                        anchors.verticalCenter: parent.verticalCenter
                                        text: tocChip.modelData.label || ""
                                        color: theme.textSecondary
                                        font.pixelSize: 11
                                        font.weight: theme.weightMedium
                                    }
                                }
                            }
                            Item { width: 1; height: 8 }
                            Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
                            Item { width: 1; height: 8 }
                        }

                        // ---- Body blocks.
                        Repeater {
                            model: root.doc.blocks || []
                            delegate: Item {
                                id: blk
                                required property var modelData
                                readonly property int kind: modelData.kind || 0
                                readonly property int level: modelData.level || 0
                                // Vertical rhythm comes from the paddings, as
                                // in the reference: sections get extra top
                                // space, everything else a uniform 7px below.
                                readonly property int padTop: blk.kind === 0 ? 16 : 0
                                readonly property int padBottom: blk.kind === 0 ? 6 : 7

                                width: bodyCol.width
                                height: line.implicitHeight + blk.padTop + blk.padBottom

                                Row {
                                    id: line
                                    y: blk.padTop
                                    // Bullet indentation: level 1 = first nesting.
                                    x: blk.kind === 1 ? blk.level * 16 : 0
                                    width: blk.width - x
                                    spacing: 8

                                    Text {
                                        id: glyph
                                        visible: blk.kind === 1
                                        width: visible ? implicitWidth : 0
                                        text: blk.level >= 2 ? "◦" : "•"
                                        color: blk.level >= 2 ? theme.textMuted : theme.textSecondary
                                        font.pixelSize: blk.level >= 2 ? 11 : 12
                                    }

                                    Text {
                                        id: blockText
                                        width: line.width
                                               - (glyph.visible ? glyph.width + line.spacing : 0)
                                        text: blk.modelData.text || ""
                                        wrapMode: Text.WordWrap
                                        color: blk.kind === 3
                                            ? (linkArea.containsMouse ? theme.accentHover : theme.accent)
                                            : blk.kind === 0
                                                ? theme.textPrimary
                                                : blk.kind === 1
                                                    ? (blk.level >= 2 ? theme.textMuted : theme.textSecondary)
                                                    : theme.textSecondary
                                        font.pixelSize: blk.kind === 0
                                            ? 14
                                            : blk.kind === 1
                                                ? (blk.level >= 2 ? 11 : 12)
                                                : 13
                                        font.weight: blk.kind === 0
                                            ? theme.weightBold
                                            : blk.kind === 3
                                                ? theme.weightMedium
                                                : theme.weightRegular

                                        // Whole-line link (kind 3) — the whole
                                        // line is the hit target, matching the
                                        // reference's TouchArea over the text.
                                        MouseArea {
                                            id: linkArea
                                            anchors.fill: parent
                                            enabled: blk.kind === 3
                                            visible: enabled
                                            hoverEnabled: true
                                            cursorShape: Qt.PointingHandCursor
                                            onClicked: QbzShell.openExternalUrl(blk.modelData.url || "")
                                        }
                                    }
                                }
                            }
                        }

                        Item { width: 1; height: 8 }
                    }
                }

                QbzScrollBar {
                    target: flick
                    visible: flick.visible && flick.contentHeight > flick.height
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                }
            }

            // ---- Footer: the accent Close button ----------------------
            Item {
                id: footerRow
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 36

                Rectangle {
                    anchors.right: parent.right
                    height: 36
                    width: closeLabel.implicitWidth + 36
                    radius: theme.radiusSm
                    color: footerArea.containsMouse ? theme.accentHover : theme.accent
                    Text {
                        id: closeLabel
                        anchors.centerIn: parent
                        text: QbzSession.tr("Close", QbzSession.trRev)
                        // The measured on-accent selector, not a raw
                        // accent-text (theme/QbzTheme.qml, "ON AN ACCENT
                        // FILL") — the port-wide rule for a label on an accent
                        // fill, and the one place the reference's
                        // Theme.accent-text is knowingly diverged from.
                        color: theme.accentGlyphColor
                        font.pixelSize: 14
                        font.weight: theme.weightMedium
                    }
                    MouseArea {
                        id: footerArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzAbout.whatsNewClose()
                    }
                }
            }
        }
    }
}
