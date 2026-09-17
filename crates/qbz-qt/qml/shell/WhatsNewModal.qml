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
import QtQuick.Window
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

    // ── THE DECK ───────────────────────────────────────────────────────────
    //
    // `doc.deck` is the version's curated cards (whats_new_qt.rs `deck_for`),
    // empty for a version that has none. The release-notes body this file has
    // always rendered becomes the LAST card, so nothing is lost and a version
    // without a deck opens straight into it, exactly as before.
    //
    // Card changes are INSTANT on purpose. Qt Quick has no partial redraws, so
    // a continuous transition would present the whole window every frame for
    // its duration (the shell's GPU bill is presents/s); the repaint-pulse
    // contract keeps continuous motion on QbzShell.pulseMs, and a modal that
    // is open for ten seconds does not earn an exception. There is also no
    // auto-advance timer, for the same reason.
    readonly property var deck: root.doc.deck || []
    property int cardIndex: 0
    readonly property bool showingNotes: root.deck.length === 0 || root.cardIndex >= root.deck.length
    readonly property int cardCount: root.deck.length + 1
    readonly property var card: root.showingNotes ? null : root.deck[root.cardIndex]

    anchors.fill: parent
    z: 3000
    visible: root.doc.open === true
    enabled: root.visible

    QbzTheme { id: theme }

    onVisibleChanged: {
        if (visible) {
            root.cardIndex = 0
            keyScope.forceActiveFocus()
        }
    }

    FocusScope {
        id: keyScope
        anchors.fill: parent
        Keys.onEscapePressed: QbzAbout.whatsNewClose()
        Keys.onLeftPressed: root.goBack()
        Keys.onRightPressed: root.goNext()
    }

    function goBack() {
        if (root.cardIndex > 0)
            root.cardIndex -= 1
    }

    function goNext() {
        if (root.cardIndex < root.cardCount - 1)
            root.cardIndex += 1
        else
            QbzAbout.whatsNewClose()
    }

    // A card's button lands the user ON the setting it just described. The
    // section is applied through the bridge and never by assigning
    // SettingsView.section, which would destroy that binding (see the note in
    // SettingsView.qml); -1 opens Settings without choosing a section.
    function openCardTarget(section) {
        QbzAbout.whatsNewClose()
        if (section >= 0)
            QbzBridge.settingsSetSection(section)
        QbzShell.navigateTo("settings")
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
        // 700 tall capped, minus an 80px window margin — WhatsNewModal.slint:36-37.
        // WIDER than the Slint original's 820: the card art is 16:9 and the
        // image covers its box (PreserveAspectCrop), so at 820 the box sat at
        // ~1.34 and Qt cropped roughly a quarter of the frame's width — taking
        // with it what the right edge of a screenshot is there to show (the
        // renderer name on the iOS capture). At 1040 the box lands near 1.72
        // and the crop all but disappears.
        width: Math.min(root.width - 80, 1040)
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

                // ---- The deck's card -------------------------------------
                Item {
                    id: cardBox
                    anchors.fill: parent
                    visible: !root.showingNotes

                    Rectangle {
                        anchors.fill: parent
                        radius: theme.radiusSm
                        color: theme.surfaceElevated
                        clip: true

                        Image {
                            id: cardArt
                            anchors.fill: parent
                            source: root.card ? root.card.image : ""
                            // The art is 1600x900 (@2x of the card box); ask
                            // for the painted size so the decoded pixmap is
                            // not held at twice the memory it needs.
                            sourceSize.width: Math.round(width * Screen.devicePixelRatio)
                            sourceSize.height: Math.round(height * Screen.devicePixelRatio)
                            fillMode: Image.PreserveAspectCrop
                            asynchronous: true
                            cache: true
                        }

                        // The scrim is painted HERE and not baked into the
                        // artwork: one set of images then reads correctly in
                        // both themes, and the text keeps its contrast even
                        // where a card's left half is lighter than planned.
                        // The art brief reserves that half for exactly this.
                        Rectangle {
                            anchors.fill: parent
                            gradient: Gradient {
                                orientation: Gradient.Horizontal
                                GradientStop { position: 0.0; color: Qt.rgba(0, 0, 0, 0.88) }
                                GradientStop { position: 0.48; color: Qt.rgba(0, 0, 0, 0.62) }
                                GradientStop { position: 1.0; color: Qt.rgba(0, 0, 0, 0.12) }
                            }
                        }

                        Column {
                            anchors.left: parent.left
                            anchors.leftMargin: 28
                            anchors.right: parent.horizontalCenter
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: 10

                            Text {
                                text: QbzSession.tr("New in {}", QbzSession.trRev)
                                    .replace("{}", root.doc.version || "")
                                color: theme.accent
                                font.pixelSize: 11
                                font.weight: theme.weightBold
                                font.letterSpacing: 1.2
                            }

                            Text {
                                width: parent.width
                                text: root.card ? root.card.title : ""
                                color: "#ffffff"
                                font.pixelSize: 26
                                font.weight: theme.weightBold
                                wrapMode: Text.WordWrap
                            }

                            Text {
                                width: parent.width
                                text: root.card ? root.card.body : ""
                                // Not theme.textSecondary: this sits on the
                                // artwork, not on a themed surface.
                                color: Qt.rgba(1, 1, 1, 0.82)
                                font.pixelSize: 14
                                lineHeight: 1.35
                                wrapMode: Text.WordWrap
                            }

                            Item { width: 1; height: 4; visible: cardAction.visible }

                            Rectangle {
                                id: cardAction
                                visible: root.card !== null && (root.card.actionLabel || "") !== ""
                                height: 34
                                width: cardActionLabel.implicitWidth + 32
                                radius: theme.radiusSm
                                color: cardActionArea.containsMouse ? theme.accentHover : theme.accent
                                Text {
                                    id: cardActionLabel
                                    anchors.centerIn: parent
                                    text: root.card ? (root.card.actionLabel || "") : ""
                                    color: theme.accentGlyphColor
                                    font.pixelSize: 13
                                    font.weight: theme.weightMedium
                                }
                                MouseArea {
                                    id: cardActionArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.openCardTarget(
                                        root.card ? (root.card.actionSection === undefined
                                            ? -1 : root.card.actionSection) : -1)
                                }
                            }
                        }
                    }
                }

                // Loading state.
                Text {
                    anchors.centerIn: parent
                    visible: root.showingNotes && root.doc.loading === true
                    text: QbzSession.tr("Loading…", QbzSession.trRev)
                    color: theme.textMuted
                    font.pixelSize: theme.fontBody
                }

                // Empty state — no release body available. Reached on every
                // DEV build: the fetch returns nothing for a draft or
                // prerelease tag (whats_new_qt.rs), by design.
                Text {
                    anchors.centerIn: parent
                    visible: root.showingNotes && root.doc.loading !== true && root.doc.hasBody !== true
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
                    visible: root.showingNotes && root.doc.loading !== true && root.doc.hasBody === true
                    clip: true
                    contentWidth: width
                    contentHeight: bodyCol.height
                    boundsBehavior: Flickable.StopAtBounds

                    Column {
                        id: bodyCol
                        // The panel widened for the card art (see `panel`),
                        // and release notes are prose: a line that spans the
                        // whole 1040 is measurably harder to read than the
                        // same text at a book measure. Cap the column and
                        // centre it; narrow windows keep the full width.
                        width: Math.min(flick.width, 720)
                        x: Math.round((flick.width - width) / 2)

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

            // ---- Footer: dots, Back, and the accent Next/Close --------
            Item {
                id: footerRow
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                height: 36

                // Position dots. Bordered circles, not filled pills: the
                // inactive ones read as an outline, ADR-008 untouched.
                Row {
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 7
                    visible: root.deck.length > 0

                    Repeater {
                        model: root.cardCount
                        Rectangle {
                            width: 7
                            height: 7
                            radius: 3.5
                            color: index === root.cardIndex ? theme.accent : "transparent"
                            border.width: 1
                            border.color: index === root.cardIndex ? theme.accent : theme.borderSubtle
                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.cardIndex = index
                            }
                        }
                    }
                }

                Rectangle {
                    id: backButton
                    anchors.right: primaryButton.left
                    anchors.rightMargin: 8
                    anchors.verticalCenter: parent.verticalCenter
                    visible: root.deck.length > 0 && root.cardIndex > 0
                    height: 36
                    width: backLabel.implicitWidth + 32
                    radius: theme.radiusSm
                    color: backArea.containsMouse ? theme.elevatedHoverFill : "transparent"
                    border.width: 1
                    border.color: theme.borderSubtle
                    Text {
                        id: backLabel
                        anchors.centerIn: parent
                        text: QbzSession.tr("Back", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 14
                        font.weight: theme.weightMedium
                    }
                    MouseArea {
                        id: backArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.goBack()
                    }
                }

                Rectangle {
                    id: primaryButton
                    anchors.right: parent.right
                    height: 36
                    width: closeLabel.implicitWidth + 36
                    radius: theme.radiusSm
                    color: footerArea.containsMouse ? theme.accentHover : theme.accent
                    Text {
                        id: closeLabel
                        anchors.centerIn: parent
                        // One button, three jobs: advance the deck, open the
                        // notes as its last card, and close from there.
                        text: root.showingNotes
                            ? QbzSession.tr("Close", QbzSession.trRev)
                            : (root.cardIndex === root.deck.length - 1
                                ? QbzSession.tr("Release notes", QbzSession.trRev)
                                : QbzSession.tr("Next", QbzSession.trRev))
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
                        onClicked: root.showingNotes ? QbzAbout.whatsNewClose() : root.goNext()
                    }
                }
            }
        }
    }
}
