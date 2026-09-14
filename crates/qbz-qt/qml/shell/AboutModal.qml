// About QBZ — QML port of crates/qbz-ui/ui/shell/AboutModal.slint (531 lines).
//
// Opened from the header hamburger menu ("About QBZ"). Everything that is not
// a translated literal comes from ONE document, QbzAbout.aboutJson
// (src/about_qt.rs): version, platform label, build stamp, release URL,
// codename, the author chip and the row-grouped contributor chips.
//
// Structure follows LogViewerModal.qml, which already solves the four things
// this file would otherwise get wrong: the guarded JSON.parse (a bare parse in
// a binding throws on the pre-publish frame), the self-gated mount (ADR-010 —
// an invisible, non-interactive Item while closed), the faked drop shadow (the
// port has no blurred shadow) and the panel sizing. z: 3000 explicitly, per
// ADR-009 as this port spells it.
//
// ── DELIBERATE DIVERGENCES FROM THE SLINT REFERENCE ────────────────────────
//
//  1. CONTRIBUTORS USE `Flow`, NOT THE PRE-GROUPED ROWS. Slint pre-groups the
//     chips into fixed rows of five ONLY because it has no flex-wrap
//     (AboutModal.slint:432-436 says so). QML has `Flow`, which is what the
//     original Tauri build used. The rows stay in the JSON so both frontends
//     read one document shape; this file flattens them.
//  2. THE STRUCK-THROUGH "love" USES `font.strikeout`. Slint fakes it with a
//     1px Rectangle laid over the glyphs (AboutModal.slint:474-492) because
//     Slint Text has no text-decoration. Qt Text has the real property.
//  3. THE ACKNOWLEDGMENTS LINE SAYS "Qt", NOT "Slint" — see its comment below.
//  4. Colour literals are `Qt.rgba(...)`, not the reference's `#RRGGBBAA`.
//     Slint is #RRGGBBAA and Qt is #AARRGGBB, and copying a Slint literal
//     verbatim is how this port shipped invisible scrims and invisible
//     shadows across five modals, all of which read `#000000bf` / `#00000080`
//     — ZERO alpha under Qt — where 20 other files spelled the same scrim
//     `#bf000000`. Fixed 2026-08-14. Floats cannot be misread in either
//     direction, which is why everything new here uses them.
//
// The 128px logo is deliberate and load-bearing: the 512px qbz-logo.png
// already in qml/assets aliases when scaled down to 56 (AboutModal.slint:184-186
// records exactly that finding for femtovg; a 2.3x downscale is clean, a 9x one
// is not).
//
// No pills (ADR-008): the author + contributor chips are bordered radiusSm
// rectangles, and so is nothing else here.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    id: root

    // GUARDED — the property is seeded "{}" on the bridge, but a redeploy
    // order or a future "" default must not poison every binding in the file.
    readonly property var doc: {
        try {
            return JSON.parse(QbzAbout.aboutJson)
        } catch (e) {
            return ({})
        }
    }

    // The contributor chips as ONE list. See divergence 1 in the header: the
    // document keeps Slint's row grouping, `Flow` does the wrapping here.
    readonly property var contributors: {
        var rows = root.doc.contributorRows || []
        var out = []
        for (var i = 0; i < rows.length; i++)
            for (var j = 0; j < (rows[i] || []).length; j++)
                out.push(rows[i][j])
        return out
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

    // Escape closes. The LOCAL FocusScope idiom (LogViewerModal.qml:73-77)
    // rather than growing hotkeys_qt.rs's central EscapeState — that walk is
    // pinned by a unit test whose ordering is not this lane's to renegotiate.
    FocusScope {
        id: keyScope
        anchors.fill: parent
        Keys.onEscapePressed: QbzAbout.aboutClose()
    }

    // ---- Sub-components (the Slint file's LinkButton / HandleChip) --------

    // A bordered icon+label link button (the external-links row).
    component LinkButton: Rectangle {
        id: lb
        property string label: ""
        property string iconName: "external-link"
        signal clicked()

        height: 34
        // Slint sizes this to `row.preferred-width`; the Row's implicitWidth
        // is the same measure, plus the 14px side padding either side.
        width: lbRow.implicitWidth + 28
        radius: theme.radiusSm
        border.width: 1
        border.color: theme.borderSubtle
        color: lbArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

        Row {
            id: lbRow
            anchors.centerIn: parent
            spacing: 8
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: lb.iconName
                width: 15
                height: 15
                // Theme-following, both arms: the reference hands
                // Theme.text-primary / Theme.text-secondary here.
                tintName: lbArea.containsMouse ? "textPrimary" : "secondary"
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: lb.label
                color: lbArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
                font.weight: theme.weightMedium
            }
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: "external-link"
                width: 11
                height: 11
                tintName: "muted"
            }
        }

        MouseArea {
            id: lbArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: lb.clicked()
        }
    }

    // A bordered, clickable handle chip with a round GitHub avatar. NOT a pill
    // (ADR-008): radiusSm + a subtle border. The avatar arrives asynchronously
    // (about_qt.rs downloads it to the user cache and publishes a file:// path
    // — this port loads no https:// into an Image.source anywhere), so the
    // circle stays blank until then, exactly like the reference.
    component HandleChip: Rectangle {
        id: hc
        property string name: ""
        property string url: ""
        property string avatar: ""
        property int textSize: 13
        property int avatarSize: 20

        height: 28
        width: hcRow.implicitWidth + 16
        radius: theme.radiusSm
        border.width: 1
        border.color: theme.borderSubtle
        color: hcArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated

        Row {
            id: hcRow
            anchors.left: parent.left
            anchors.leftMargin: 5
            anchors.verticalCenter: parent.verticalCenter
            spacing: 7

            Rectangle {
                anchors.verticalCenter: parent.verticalCenter
                width: hc.avatarSize
                height: hc.avatarSize
                radius: width / 2
                color: theme.surfaceHover
                // QML `clip` is rectangular, so the circle has to come from a
                // mask — that is what RoundedImage exists for (see its header).
                RoundedImage {
                    anchors.fill: parent
                    source: hc.avatar
                    radius: width / 2
                }
            }
            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: hc.name
                color: hcArea.containsMouse ? theme.accent : theme.textPrimary
                font.pixelSize: hc.textSize
                font.weight: theme.weightMedium
            }
            QbzIcon {
                anchors.verticalCenter: parent.verticalCenter
                name: "external-link"
                width: 10
                height: 10
                tintName: "muted"
            }
        }

        MouseArea {
            id: hcArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: QbzShell.openExternalUrl(hc.url)
        }
    }

    // A sponsor chip — HandleChip minus the avatar circle (sponsors are
    // names, not GitHub identities; only the GitHub-sponsor rows carry a
    // url and become clickable). NOT a pill (ADR-008): radiusSm + border.
    component SponsorChip: Rectangle {
        id: sc
        property string name: ""
        property string url: ""
        readonly property bool clickable: sc.url !== ""

        height: 28
        width: scText.implicitWidth + 24
        radius: theme.radiusSm
        border.width: 1
        border.color: theme.borderSubtle
        color: (sc.clickable && scArea.containsMouse) ? theme.surfaceElevatedHover : theme.surfaceElevated

        Text {
            id: scText
            anchors.centerIn: parent
            text: sc.name
            color: (sc.clickable && scArea.containsMouse) ? theme.accent : theme.textPrimary
            font.pixelSize: 13
            font.weight: theme.weightMedium
        }
        MouseArea {
            id: scArea
            anchors.fill: parent
            enabled: sc.clickable
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: QbzShell.openExternalUrl(sc.url)
        }
    }

    // A small uppercase-ish section heading (11px semibold, +0.5 tracking).
    component SectionHeading: Text {
        color: theme.textMuted
        font.pixelSize: 11
        font.weight: theme.weightSemibold
        font.letterSpacing: 0.5
    }

    // A label/value row of the build-info grid.
    component InfoRow: Item {
        id: ir
        property string label: ""
        property string value: ""
        property bool italic: false
        property color valueColor: theme.textPrimary
        height: 18
        Text {
            id: irLabel
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            width: 84
            text: ir.label
            color: theme.textMuted
            font.pixelSize: 13
        }
        Text {
            anchors.left: irLabel.right
            anchors.leftMargin: 12
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            text: ir.value
            color: ir.valueColor
            font.pixelSize: 13
            font.italic: ir.italic
            elide: Text.ElideRight
        }
    }

    // ---- Scrim / shadow / panel ------------------------------------------

    Rectangle {
        anchors.fill: parent
        // Slint `#000000bf` CONVERTED. Floats, not a hex — see divergence 4.
        color: Qt.rgba(0, 0, 0, 0.75)
        MouseArea {
            anchors.fill: parent
            onClicked: QbzAbout.aboutClose()
            // Wheel-lock (the DiscoverConfigModal rule): a MouseArea consumes
            // presses but lets wheel events fall through, so without this the
            // page underneath kept scrolling behind the modal.
            onWheel: function (wheel) { wheel.accepted = true }
        }
    }

    // Faked 32px drop shadow (`drop-shadow-color: #00000080`) — the
    // LogViewerModal/QbzConfirmModal precedent; a real blurred shadow owes its
    // own parity pass.
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
        // The Slint reference was 840x770 (AboutModal.slint:163-164); grown
        // for the Sponsors section (QoL round) so Contributors + Sponsors fit
        // without crowding. Still capped by an 80px window margin.
        width: Math.min(root.width - 80, 920)
        height: Math.min(root.height - 80, 860)
        radius: theme.radiusMd
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.borderSubtle

        // Swallow clicks so they never reach the closing backdrop — and the
        // wheel, so a scroll outside the body Flickable (header, margins)
        // does not reach the page behind the modal.
        MouseArea {
            anchors.fill: parent
            onWheel: function (wheel) { wheel.accepted = true }
        }

        Item {
            id: stack
            anchors.fill: parent
            anchors.margins: 24

            // ---- Header: branding + close X ---------------------------
            Item {
                id: headerRow
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: 56

                Image {
                    id: logo
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    width: 56
                    height: 56
                    // The 128px source, NOT the 512px qbz-logo.png sitting
                    // beside it: a ~2.3x downscale renders crisp at 56px where
                    // the 9x downscale aliases (AboutModal.slint:184-186).
                    source: "../assets/qbz-logo-128.png"
                    fillMode: Image.PreserveAspectFit
                    smooth: true
                    mipmap: true
                }
                Column {
                    anchors.left: logo.right
                    anchors.leftMargin: 16
                    anchors.right: aboutClose.left
                    anchors.rightMargin: 16
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 2
                    Text {
                        text: "QBZ"
                        color: theme.textPrimary
                        font.pixelSize: theme.fontTitle
                        font.weight: theme.weightBold
                    }
                    Text {
                        text: "v" + (root.doc.version || "")
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                    }
                }
                Item {
                    id: aboutClose
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    width: 28
                    height: 28
                    QbzIcon {
                        anchors.centerIn: parent
                        name: "x"
                        width: 17
                        height: 17
                        tintName: aboutCloseArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: aboutCloseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: QbzAbout.aboutClose()
                    }
                }
            }

            Rectangle {
                id: headerRule
                anchors.top: headerRow.bottom
                anchors.topMargin: 16
                anchors.left: parent.left
                anchors.right: parent.right
                height: 1
                color: theme.borderSubtle
            }

            // ---- Scrolling body ---------------------------------------
            Flickable {
                id: flick
                anchors.top: headerRule.bottom
                anchors.topMargin: 16
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                clip: true
                contentWidth: width
                contentHeight: bodyCol.height
                boundsBehavior: Flickable.StopAtBounds

                Column {
                    id: bodyCol
                    width: flick.width

                    // Description.
                    Text {
                        width: parent.width
                        text: QbzSession.tr("A native music streaming client for Linux, designed for audiophiles who need bit-perfect Hi-Fi playback.", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: 14
                        wrapMode: Text.WordWrap
                    }

                    Item { width: 1; height: 16 }

                    // Qobuz legal notice (branding + trademark lines).
                    Text {
                        width: parent.width
                        text: QbzSession.tr("This application uses the Qobuz API but is not certified by Qobuz.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }
                    Item { width: 1; height: 8 }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Qobuz™ is a trademark of Qobuz. QBZ is an open-source application licensed under the MIT License and is not affiliated with, endorsed by, or certified by Qobuz.", QbzSession.trRev)
                        color: theme.textMuted
                        // 11px so the trademark notice wraps to two lines
                        // instead of three.
                        font.pixelSize: 11
                        wrapMode: Text.WordWrap
                    }

                    Item { width: 1; height: 22 }

                    // External links. "GitHub" is deliberately NOT translated
                    // in the reference; the other two are.
                    Row {
                        spacing: 12
                        LinkButton {
                            label: "GitHub"
                            onClicked: QbzShell.openExternalUrl("https://github.com/vicrodh/qbz")
                        }
                        LinkButton {
                            label: QbzSession.tr("Release", QbzSession.trRev)
                            onClicked: QbzShell.openExternalUrl(root.doc.releaseUrl || "")
                        }
                        LinkButton {
                            label: QbzSession.tr("Website", QbzSession.trRev)
                            iconName: "globe"
                            onClicked: QbzShell.openExternalUrl("https://qbz.lol")
                        }
                    }

                    Item { width: 1; height: 24 }

                    // Build info — two label/value columns side by side.
                    SectionHeading { text: QbzSession.tr("Build Info", QbzSession.trRev) }
                    Item { width: 1; height: 12 }
                    Row {
                        width: parent.width
                        spacing: 16
                        Column {
                            width: (parent.width - 16) / 2
                            spacing: 6
                            InfoRow {
                                width: parent.width
                                label: QbzSession.tr("Version", QbzSession.trRev)
                                value: root.doc.version || ""
                            }
                            InfoRow {
                                width: parent.width
                                label: QbzSession.tr("License", QbzSession.trRev)
                                // Literal in the reference too (AboutModal.slint:334).
                                value: "MIT"
                            }
                            InfoRow {
                                width: parent.width
                                label: QbzSession.tr("Build", QbzSession.trRev)
                                value: {
                                    var d = root.doc.buildDate || ""
                                    var c = root.doc.buildCommit || ""
                                    return c === "" ? d : d + " (" + c + ")"
                                }
                            }
                        }
                        Column {
                            width: (parent.width - 16) / 2
                            spacing: 6
                            InfoRow {
                                width: parent.width
                                label: QbzSession.tr("Codename", QbzSession.trRev)
                                // Hand-set literal, on the Rust side
                                // (about_qt.rs CODENAME) so the release
                                // checklist has ONE Qt place to bump.
                                value: root.doc.codename || ""
                                italic: true
                                valueColor: theme.textSecondary
                            }
                            InfoRow {
                                width: parent.width
                                label: QbzSession.tr("Platform", QbzSession.trRev)
                                value: root.doc.platformLabel || ""
                            }
                        }
                    }

                    Item { width: 1; height: 24 }

                    // Acknowledgments — one prose paragraph.
                    SectionHeading { text: QbzSession.tr("Acknowledgments", QbzSession.trRev) }
                    Item { width: 1; height: 12 }
                    Text {
                        width: parent.width
                        // The msgid names Slint because it was written for the
                        // Slint build; this IS the Qt build. "Slint" is a
                        // proper noun and survives verbatim in all seven
                        // translated catalogs (checked: es de fr pt ru ja nl),
                        // so one substitution fixes every locale and adds ZERO
                        // new msgids to the shared catalog. If a future
                        // translation ever localises the word the replace is a
                        // no-op and the string simply reads as it does today.
                        text: QbzSession.tr("Built with Rust and Slint, and powered by Rodio and Symphonia. With thanks to Qobuz, MusicBrainz, Song.link/Odesli, LRCLIB, lyrics.ovh, and Lucide.", QbzSession.trRev)
                              .replace("Slint", "Qt")
                        color: theme.textSecondary
                        font.pixelSize: 12
                        wrapMode: Text.WordWrap
                    }

                    Item { width: 1; height: 24 }

                    // Author.
                    SectionHeading { text: QbzSession.tr("Author", QbzSession.trRev) }
                    Item { width: 1; height: 12 }
                    HandleChip {
                        name: root.doc.authorName || ""
                        url: root.doc.authorUrl || ""
                        avatar: root.doc.authorAvatar || ""
                        textSize: 14
                        avatarSize: 22
                    }

                    Item { width: 1; height: 24 }

                    // Contributors — `Flow`, see divergence 1 in the header.
                    SectionHeading { text: QbzSession.tr("Contributors", QbzSession.trRev) }
                    Item { width: 1; height: 12 }
                    Flow {
                        width: parent.width
                        spacing: 8
                        Repeater {
                            model: root.contributors
                            delegate: HandleChip {
                                id: contribChip
                                required property var modelData
                                name: contribChip.modelData.name || ""
                                url: contribChip.modelData.url || ""
                                avatar: contribChip.modelData.avatar || ""
                            }
                        }
                    }

                    Item { width: 1; height: 24 }

                    // Sponsors (QoL round) — GitHub sponsors first (linked),
                    // then Ko-fi supporters by their public display name.
                    // FIXED-HEIGHT strip of exactly three chip rows
                    // (3×28 + 2×8); the strip scrolls ITSELF, the modal
                    // never grows with the list.
                    SectionHeading { text: QbzSession.tr("Sponsors", QbzSession.trRev) }
                    Item { width: 1; height: 12 }
                    Flickable {
                        width: parent.width
                        height: 100
                        clip: true
                        contentWidth: width
                        contentHeight: sponsorFlow.height
                        boundsBehavior: Flickable.StopAtBounds
                        Flow {
                            id: sponsorFlow
                            width: parent.width
                            spacing: 8
                            Repeater {
                                model: root.doc.sponsors || []
                                delegate: SponsorChip {
                                    id: sponsorChip
                                    required property var modelData
                                    name: sponsorChip.modelData.name || ""
                                    url: sponsorChip.modelData.url || ""
                                }
                            }
                        }
                    }
                    Item { width: 1; height: 8 }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("And all those anonymous sponsors...", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 12
                        font.italic: true
                        wrapMode: Text.WordWrap
                    }

                    Item { width: 1; height: 24 }
                    Rectangle { width: parent.width; height: 1; color: theme.borderSubtle }
                    Item { width: 1; height: 18 }

                    // ---- Signature ----------------------------------------
                    Row {
                        anchors.horizontalCenter: parent.horizontalCenter
                        spacing: 6
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("Made with", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 12
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            // NOT translated in the reference, deliberately.
                            text: "love"
                            color: theme.textDisabled
                            font.pixelSize: 12
                            // Slint fakes this with a hairline Rectangle laid
                            // over the glyphs; Qt has the real property.
                            font.strikeout: true
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: "hate"
                            color: theme.textSecondary
                            font.pixelSize: 12
                            font.weight: theme.weightSemibold
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            text: QbzSession.tr("in", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: 12
                        }
                        // An SVG, never a 🇲🇽 emoji — regional-indicator flags
                        // degrade to "MX" on many Linux font stacks
                        // (AboutModal.slint:506-508). The port already carries
                        // a full ISO-3166 flag set, so this reuses mx.svg
                        // rather than copying the reference's mexico-flag.svg.
                        Image {
                            anchors.verticalCenter: parent.verticalCenter
                            source: "qrc:/qt/qml/com/blitzfc/qbz/qml/assets/flags/mx.svg"
                            width: 18
                            height: 12
                            fillMode: Image.PreserveAspectFit
                            sourceSize: Qt.size(36, 24)
                        }
                    }
                    Item { width: 1; height: 8 }
                    Text {
                        width: parent.width
                        text: QbzSession.tr("Hatred towards all those companies that discriminate against the Linux community and don't provide us with a decent client for their product.", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: 11
                        horizontalAlignment: Text.AlignHCenter
                        wrapMode: Text.WordWrap
                    }

                    Item { width: 1; height: 4 }
                }
            }

            // A SIBLING of the Flickable, not an attached ScrollBar — that is
            // this port's scrollbar contract (QbzScrollBar.qml:4-6).
            QbzScrollBar {
                target: flick
                anchors.right: parent.right
                anchors.top: flick.top
                anchors.bottom: flick.bottom
            }
        }
    }
}
