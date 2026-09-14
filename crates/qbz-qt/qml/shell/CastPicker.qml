// Cast (Chromecast / DLNA) device picker — 1:1 QML port of
// crates/qbz-ui/ui/shell/CastPicker.slint.
//
// HOW IT OPENS (taken from the .slint, not chosen): a MODAL — a full-window
// #000000bf scrim with a centered card. Slint mounts it last in AppShell so it
// sits on top (ADR-009: modal z >= 3000) and gates it on CastState.picker-open.
// Here it is a Popup parented to `Overlay.overlay`, the same mechanism
// shell/TrackInfoModal.qml uses, so the bars can own it without AppShell.qml
// having to mount anything — the NowPlayingBar Loader guarantees exactly ONE
// bar, hence exactly one instance, is alive.
//
// Geometry, verbatim from the .slint:
//   card width  = min(window - 80, 400)
//   card height = 420
//   card padding 18, block spacing 14, rows 52, tabs 34, banner 64
//
// STATE + ACTIONS: QbzCast (src/cast_bridge.rs), the Qt surface of the Slint
// CastState / CastActions globals. `QbzCast.devicesJson` is the ONE document
// carrying both device lists (see the bridge header for why it is not a
// QVariantList); every read is guarded, so a malformed/absent document
// degrades to the empty state instead of throwing.
//
// DISCOVERY IS PICKER-OWNED: opening calls QbzCast.openPicker() (which arms
// mDNS + SSDP) and closing calls QbzCast.closePicker() (which stops them).
// Nothing scans while this modal is shut.
//
// No pills (ADR-008): the tabs are rectangular and the counts are plain
// trailing text, exactly like the .slint.
//
// ONE DELIBERATE DEVIATION from the .slint: the connected banner and the
// Disconnect button use the theme's semantic success/danger tokens instead of
// the .slint's hardcoded #1f3d2e / #3fae6a / #b23b3b. Slint recolours against
// one palette; this port serves 36 themes including light ones, where a
// hardcoded dark green card reads as a rendering bug (the same class of
// problem QbzIconButton's tint note documents).
//
// LAYOUT NOTE: the .slint uses a VerticalLayout whose device list carries
// `vertical-stretch: 1`. QML has no stretch on a Column, so the top block is a
// Column, the error line is pinned to the card's bottom, and the list simply
// anchors between them — same result, no height arithmetic to drift.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Popup {
    id: root

    parent: Overlay.overlay
    x: 0
    y: 0
    width: parent ? parent.width : 0
    height: parent ? parent.height : 0
    padding: 0
    z: 3000
    modal: true
    // Our own scrim (the .slint's #000000bf) — the default modal dimmer would
    // darken it twice.
    dim: false
    closePolicy: Popup.CloseOnEscape

    QbzTheme { id: theme }

    // --- Open/close: QbzCast.pickerOpen is the single source of truth ------
    // The bar button calls QbzCast.openPicker(), which arms discovery AND
    // raises the flag; this mirrors the flag onto the Popup. Escape closes the
    // Popup directly, so `onClosed` hands the news back to Rust — otherwise
    // discovery would keep scanning behind a shut modal.
    Connections {
        target: QbzCast
        function onPickerOpenChanged() {
            if (QbzCast.pickerOpen)
                root.open()
            else
                root.close()
        }
    }
    onClosed: {
        if (QbzCast.pickerOpen)
            QbzCast.closePicker()
    }

    function dismiss() {
        QbzCast.closePicker()
    }

    // --- Data --------------------------------------------------------------
    readonly property var devices: {
        try {
            var d = JSON.parse(QbzCast.devicesJson || "[]")
            return Array.isArray(d) ? d : []
        } catch (e) {
            return []
        }
    }
    // Only the selected tab's renderers are listed (the .slint collapses the
    // others row by row).
    readonly property var tabDevices: devices.filter(function (d) {
        return d && d.protocol === QbzCast.selectedTab
    })
    readonly property var capOptions: {
        try {
            var o = JSON.parse(QbzCast.deviceCapOptions || "[]")
            return Array.isArray(o) ? o : []
        } catch (e) {
            return []
        }
    }
    readonly property bool tabEmpty:
        (QbzCast.selectedTab === "chromecast" && QbzCast.chromecastCount === 0)
        || (QbzCast.selectedTab === "dlna" && QbzCast.dlnaCount === 0)

    // Quality-line suffix (#638). Over-cap wins: a locally-existing copy
    // (cache / offline store) served above the requested tier — named plainly,
    // never as an error (caps govern requests only; the bytes go out as-is).
    // Otherwise, when the per-renderer cap shaped the request (cause 3), say
    // so — the user's own choice, never an implied device limitation.
    readonly property string qualitySuffix: {
        if (QbzCast.qualityOverCap) {
            if (QbzCast.qualityOrigin === "offline")
                return " · " + QbzSession.tr("offline copy — cap not applied", QbzSession.trRev)
            if (QbzCast.qualityOrigin === "cache")
                return " · " + QbzSession.tr("cached copy — cap not applied", QbzSession.trRev)
            return ""
        }
        if (QbzCast.qualityLimitCause === 3)
            return " · " + QbzSession.tr("capped for this device", QbzSession.trRev)
        return ""
    }

    background: Rectangle { color: "#bf000000" }

    contentItem: Item {

        // Scrim — click closes + stops discovery.
        MouseArea {
            anchors.fill: parent
            onClicked: root.dismiss()
        }

        Rectangle {
            id: card
            width: Math.min(root.width - 80, 400)
            height: 420
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            radius: theme.radiusMd
            color: theme.surfaceCard
            border.width: 1
            border.color: theme.borderSubtle
            clip: true

            // Swallow clicks so they don't reach the scrim.
            MouseArea { anchors.fill: parent }

            // ---- Top block: header · banner · quality · cap · tabs -------
            Column {
                id: topBlock
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.margins: 18
                spacing: 14

                // Header.
                Item {
                    width: parent.width
                    height: 28

                    Row {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 8
                        QbzIcon {
                            name: "cast"
                            width: 18
                            height: 18
                            tintName: "textPrimary"
                            anchors.verticalCenter: parent.verticalCenter
                        }
                        Text {
                            text: QbzSession.tr("Cast", QbzSession.trRev)
                            color: theme.textPrimary
                            font.pixelSize: theme.fontHeading
                            font.weight: theme.weightSemibold
                            anchors.verticalCenter: parent.verticalCenter
                        }
                    }

                    Rectangle {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: 28
                        height: 28
                        radius: theme.radiusSm
                        color: closeArea.containsMouse ? theme.surfaceHover : "transparent"
                        QbzIcon {
                            name: "x"
                            width: 17
                            height: 17
                            anchors.centerIn: parent
                            tintName: closeArea.containsMouse ? "textPrimary" : "muted"
                        }
                        MouseArea {
                            id: closeArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.dismiss()
                        }
                    }
                }

                // Connected banner (replaces the tabs + list).
                Rectangle {
                    visible: QbzCast.connected
                    width: parent.width
                    height: visible ? 64 : 0
                    radius: theme.radiusSm
                    color: theme.successBg
                    border.width: 1
                    border.color: theme.successBorder

                    Rectangle {
                        id: liveDot
                        anchors.left: parent.left
                        anchors.leftMargin: 12
                        anchors.verticalCenter: parent.verticalCenter
                        width: 10
                        height: 10
                        radius: 5
                        color: theme.success
                    }

                    Column {
                        anchors.left: liveDot.right
                        anchors.leftMargin: 10
                        anchors.right: disconnectBtn.left
                        anchors.rightMargin: 10
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 2
                        Text {
                            text: QbzSession.tr("Connected to", QbzSession.trRev)
                            color: theme.textMuted
                            font.pixelSize: theme.fontLegal
                        }
                        Text {
                            width: parent.width
                            text: QbzCast.deviceName
                            color: theme.textPrimary
                            font.pixelSize: theme.fontBody
                            font.weight: theme.weightSemibold
                            elide: Text.ElideRight
                        }
                    }

                    Rectangle {
                        id: disconnectBtn
                        anchors.right: parent.right
                        anchors.rightMargin: 12
                        anchors.verticalCenter: parent.verticalCenter
                        width: disconnectLabel.implicitWidth + 24
                        height: 32
                        radius: theme.radiusSm
                        color: disconnectArea.containsMouse ? theme.dangerHover : theme.danger
                        Text {
                            id: disconnectLabel
                            anchors.centerIn: parent
                            text: QbzSession.tr("Disconnect", QbzSession.trRev)
                            color: "#ffffff"
                            font.pixelSize: theme.fontLegal
                            font.weight: theme.weightSemibold
                        }
                        MouseArea {
                            id: disconnectArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: QbzCast.disconnectDevice()
                        }
                    }
                }

                // Delivered quality under the banner, when known — MEASURED
                // from the served bytes (#638 fix 1), plus the disclosure
                // suffix (over-cap local copy, or the cap at work).
                Text {
                    visible: QbzCast.connected && QbzCast.qualityDetail !== ""
                    width: parent.width
                    // No `height:` binding: a Text without one takes its
                    // implicit height and a Column skips invisible items;
                    // the explicit binding raised a "binding loop for
                    // property height" warning on every connect.
                    text: QbzSession.tr("Streaming {} · {}", QbzSession.trRev)
                        .replace("{}", QbzCast.protocol === "dlna" ? "DLNA" : "Chromecast")
                        .replace("{}", QbzCast.qualityDetail) + root.qualitySuffix
                    color: theme.textMuted
                    font.pixelSize: theme.fontLegal
                    wrapMode: Text.WordWrap
                }

                // Per-renderer quality cap (#638 fix 4) — a manual per-device
                // choice of the tier QBZ REQUESTS when casting here, persisted
                // per renderer id (hidden when the device has no stable
                // identity). Label-left / control-trailing, per the
                // ui-control-alignment standard.
                Item {
                    visible: QbzCast.connected && QbzCast.deviceCapAvailable
                    width: parent.width
                    height: visible ? 34 : 0

                    Text {
                        anchors.left: parent.left
                        anchors.right: capSelect.left
                        anchors.rightMargin: 10
                        anchors.verticalCenter: parent.verticalCenter
                        text: QbzSession.tr("Quality for this device", QbzSession.trRev)
                        color: theme.textPrimary
                        font.pixelSize: theme.fontBody
                        elide: Text.ElideRight
                    }
                    QbzSelect {
                        id: capSelect
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        menuWidth: 200
                        popupWidth: 260
                        options: root.capOptions
                        currentIndex: QbzCast.deviceCapIndex
                        onSelected: function (index) {
                            QbzCast.setDeviceQualityCap(QbzCast.deviceCapKey, index)
                        }
                    }
                }

                // Tabs (only while nothing is connected).
                Item {
                    visible: !QbzCast.connected
                    width: parent.width
                    height: visible ? 34 : 0

                    Row {
                        anchors.fill: parent
                        spacing: 8
                        CastTab {
                            width: (parent.width - 8) / 2
                            label: "Chromecast"
                            count: QbzCast.chromecastCount
                            selected: QbzCast.selectedTab === "chromecast"
                            onClicked: QbzCast.setTab("chromecast")
                        }
                        CastTab {
                            width: (parent.width - 8) / 2
                            label: "DLNA"
                            count: QbzCast.dlnaCount
                            selected: QbzCast.selectedTab === "dlna"
                            onClicked: QbzCast.setTab("dlna")
                        }
                    }
                }
            }

            // ---- Error line (pinned to the card's bottom) ----------------
            // The raw last error (connect refused, unsupported source, a
            // renderer that stopped answering) — the same raw string the
            // .slint renders.
            Text {
                id: errorLine
                visible: QbzCast.error !== ""
                anchors.bottom: parent.bottom
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.margins: 18
                text: QbzCast.error
                color: theme.danger
                font.pixelSize: theme.fontLegal
                wrapMode: Text.WordWrap
            }

            // ---- Device list (the .slint's vertical-stretch: 1 block) ----
            Rectangle {
                id: listBox
                visible: !QbzCast.connected
                anchors.top: topBlock.bottom
                anchors.topMargin: 14
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: errorLine.visible ? errorLine.top : parent.bottom
                anchors.bottomMargin: errorLine.visible ? 14 : 18
                anchors.leftMargin: 18
                anchors.rightMargin: 18
                radius: theme.radiusSm
                color: theme.surfaceElevated
                clip: true

                Flickable {
                    id: listFlick
                    anchors.fill: parent
                    anchors.margins: 6
                    contentWidth: width
                    contentHeight: rows.height
                    boundsBehavior: Flickable.StopAtBounds
                    clip: true

                    Column {
                        id: rows
                        width: listFlick.width
                        spacing: 2

                        Repeater {
                            model: root.tabDevices

                            // One discovered-device row (the .slint's
                            // CastDeviceRow). A DLNA renderer without
                            // AVTransport is listed but NOT clickable — it
                            // cannot play, and pretending otherwise would be
                            // a control that does nothing.
                            delegate: Rectangle {
                                id: deviceRow
                                required property var modelData
                                readonly property bool canPlay: modelData.canPlay !== false

                                width: rows.width
                                height: 52
                                radius: theme.radiusSm
                                color: (rowArea.containsMouse && canPlay)
                                    ? theme.surfaceHover : "transparent"

                                QbzIcon {
                                    id: rowIcon
                                    name: "cast"
                                    width: 18
                                    height: 18
                                    tintName: "secondary"
                                    anchors.left: parent.left
                                    anchors.leftMargin: 10
                                    anchors.verticalCenter: parent.verticalCenter
                                }
                                Column {
                                    anchors.left: rowIcon.right
                                    anchors.leftMargin: 10
                                    anchors.right: parent.right
                                    anchors.rightMargin: 10
                                    anchors.verticalCenter: parent.verticalCenter
                                    spacing: 2
                                    Text {
                                        width: parent.width
                                        text: deviceRow.modelData.name || ""
                                        color: deviceRow.canPlay ? theme.textPrimary : theme.textMuted
                                        font.pixelSize: theme.fontBody
                                        elide: Text.ElideRight
                                    }
                                    Text {
                                        width: parent.width
                                        text: deviceRow.canPlay
                                            ? (deviceRow.modelData.ip || "")
                                            : QbzSession.tr("{} (not playable)", QbzSession.trRev)
                                                .replace("{}", deviceRow.modelData.ip || "")
                                        color: theme.textMuted
                                        font.pixelSize: theme.fontLegal
                                        elide: Text.ElideRight
                                    }
                                }
                                MouseArea {
                                    id: rowArea
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    enabled: deviceRow.canPlay
                                    cursorShape: deviceRow.canPlay
                                        ? Qt.PointingHandCursor : Qt.ArrowCursor
                                    onClicked: QbzCast.connectDevice(
                                        deviceRow.modelData.id || "",
                                        deviceRow.modelData.protocol || "")
                                }
                            }
                        }
                    }
                }

                QbzScrollBar {
                    target: listFlick
                    anchors.right: parent.right
                    anchors.rightMargin: 4
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                }

                // Empty / scanning overlay (the selected tab has no devices).
                // No MouseArea: the list underneath is empty anyway, and
                // swallowing clicks here would be invisible dead weight.
                Column {
                    anchors.centerIn: parent
                    width: parent.width - 24
                    spacing: 4
                    visible: root.tabEmpty
                    Text {
                        width: parent.width
                        horizontalAlignment: Text.AlignHCenter
                        text: QbzCast.scanning
                            ? QbzSession.tr("Searching…", QbzSession.trRev)
                            : QbzSession.tr("No devices found", QbzSession.trRev)
                        color: theme.textSecondary
                        font.pixelSize: theme.fontBody
                    }
                    Text {
                        width: parent.width
                        horizontalAlignment: Text.AlignHCenter
                        text: QbzSession.tr("Looking for Chromecast & DLNA on your network", QbzSession.trRev)
                        color: theme.textMuted
                        font.pixelSize: theme.fontLegal
                        wrapMode: Text.WordWrap
                    }
                }
            }
        }
    }

    // A rectangular tab button with a trailing count (no pills, ADR-008).
    component CastTab: Rectangle {
        id: tabRoot
        property string label: ""
        property int count: 0
        property bool selected: false
        signal clicked()

        height: 34
        radius: theme.radiusSm
        color: selected ? theme.accent
            : (tabArea.containsMouse ? theme.surfaceElevatedHover : theme.surfaceElevated)

        Row {
            anchors.centerIn: parent
            spacing: 6
            Text {
                text: tabRoot.label
                // Selected -> this text sits ON `theme.accent`, so it takes
                // the on-accent selector like every other accent-filled
                // control (theme/QbzTheme.qml, "ON AN ACCENT FILL"). This was
                // the LAST accent fill in the tree still reading raw
                // accent-text, i.e. the one place the app contradicted its
                // own rule. shell/CastPicker.slint:86 is a hardcoded #ffffff
                // — 1.70:1 on high-contrast — so this diverges from upstream
                // by measurement, same as the rest.
                color: tabRoot.selected ? theme.accentGlyphColor : theme.textPrimary
                font.pixelSize: theme.fontBody
                font.weight: tabRoot.selected ? theme.weightSemibold : Font.Normal
                anchors.verticalCenter: parent.verticalCenter
            }
            Text {
                visible: tabRoot.count > 0
                text: tabRoot.count
                // Same selector as the label beside it — the two halves of a
                // tab cannot disagree. This was the worst on-accent number
                // left anywhere: raw accent-text at 80% composites to
                // #715c7a on rose-pine-dawn's #d7827e, i.e. 2.10:1; through
                // the selector it composites to #2b1a19 and 5.84:1.
                color: tabRoot.selected ? theme.accentGlyphColor : theme.textMuted
                // The 80% is NOT ours to drop: CastPicker.slint:93 spells the
                // count `#ffffffcc`, and that de-emphasis against the
                // full-strength label is the tab's whole visual hierarchy.
                // It does cost contrast, and the residue is stated rather
                // than hidden: on breeze-light the selector returns #ffffff
                // (its own accent-text), which at 80% over accent #1d99f3
                // composites to #d2ebfd = 2.47:1, under the 3:1 floor. That
                // is the ALPHA's doing, not the colour's — the same label at
                // full strength is 3.04:1 and clears. Fixing it means either
                // dropping upstream's alpha or giving de-emphasised
                // on-accent text its own selector; both are owner calls, and
                // neither is this round's approved deviation.
                opacity: tabRoot.selected ? 0.8 : 1.0
                font.pixelSize: theme.fontLegal
                anchors.verticalCenter: parent.verticalCenter
            }
        }
        MouseArea {
            id: tabArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: tabRoot.clicked()
        }
    }
}
