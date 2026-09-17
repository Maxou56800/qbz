// MiniVolume — the footer's volume trigger and its popup (2026-08-03
// miniplayer/tray contract A-29, §4.3.3), port of `component MiniVolume
// inherits Rectangle` at `crates/qbz-ui/ui/miniplayer/MiniFooter.slint:106-164`.
//
// THE POPUP OPENS TO THE RIGHT, not below: `x: root.btn + 8px`,
// `y: (root.height - 34px) / 2` (:122-123) — 8 px past the trigger's right edge
// and vertically centred on the trigger's own row, so it sits over the
// transport rather than over the seek bar.
//
// INHERITED ODDITY, REPRODUCED (§12-P5): the popup's mute button is hard-coded
// `volume-x.svg` and never reflects the mute state (:139), where the TRIGGER
// does swap between volume-x and volume-2 (:114). Not a bug worth diverging
// for — it is a mute BUTTON, not a mute indicator.
//
// WHY THE DISMISS LAYER IS A HUGE RECTANGLE. Slint gets close-on-click-outside
// from `PopupWindow`'s `close-policy` (:126). Qt's equivalent would be a
// QtQuick.Controls Popup, whose overlay would take focus from the frameless
// mini and whose window-level surface is exactly what §4.3.4 refuses for the
// capsule. So dismissal is a plain MouseArea sized far past this component and
// CLIPPED BY THE CARD — MiniShell sets `clip: true`, and Qt clips hit-testing to
// the clip rect as well as painting, so the layer covers the whole card and
// nothing beyond it. It sits under the popup (z 1 vs 2) and over the trigger,
// which is why clicking the trigger a second time closes rather than reopens —
// 1:1 with a close-on-click-outside policy consuming that click.
//
// The reference's 24 px drop shadow (:132-133) is dropped by the port's
// standing convention for Slint shadows (QbzToast.qml:30, QbzTooltip.qml:51-61,
// MyQbzModals.qml:23, AddToMixtapeModal.qml:19).
//
// NOT PORTED, and it is a REFERENCE GAP rather than an omission: the main bar
// persists the settled volume on drag-end (`QbzSlider.onReleased` ->
// QbzPlayer.persistVolume, qml/shell/PlayerBar.qml:582), and the Slint mini
// does NOT — its slider binds `changed` only (:151) even though
// `NowPlayingState.persist-volume` exists beside `set-volume`
// (state.slint:4482-4483). Reported as a divergence PROPOSAL rather than taken,
// because §13.0 rule 5 is that a divergence never lands without its §13 row.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    property bool lightForeground: false

    readonly property bool volLocked: (QbzPlayer.npVolumeLocked && !QbzPlayer.npIsRemote) || QbzPlayer.npRemoteVolumeLocked
    property int btn: 30
    /// The TRIGGER's glyph size — the SEVENTH per-mode value, cached by the
    /// footer beside the other six (contract §4.3.2).
    property int iconSize: 18
    /// Read by MiniFooter so it can raise this whole subtree above the
    /// transport while the popup is up.
    property bool popupOpen: false

    function closePopup() { root.popupOpen = false }

    QbzTheme { id: theme }

    width: root.btn
    height: root.btn
    color: "transparent"

    TBtn {
        lightForeground: root.lightForeground
        name: (QbzPlayer.npMuted || QbzPlayer.npVolume === 0) ? "volume-x" : "volume-2"
        iconSize: root.iconSize
        btn: root.btn
        onClicked: root.popupOpen = true
    }

    // Close-on-click-outside. See the header for why it is shaped like this.
    MouseArea {
        z: 1
        visible: root.popupOpen
        enabled: root.popupOpen
        x: -1000
        y: -1000
        width: 2000
        height: 2000
        // hoverEnabled stays FALSE so the card's HoverHandler — the thing that
        // mounts the capsule — keeps seeing the pointer through this layer.
        onClicked: root.closePopup()
    }

    Rectangle {
        id: popup
        z: 2
        visible: root.popupOpen
        x: root.btn + 8
        y: Math.round((root.height - height) / 2)
        width: 172
        height: 34
        radius: 17
        antialiasing: true
        color: theme.surfaceCard
        border.width: 1
        border.color: theme.alphaTier(12)

        // Swallow clicks on the popup's own background so they do not fall
        // through to the dismiss layer (the Cortinilla.qml:227-232 idiom).
        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.AllButtons
            onClicked: {}
        }

        Row {
            x: 8
            anchors.verticalCenter: parent.verticalCenter
            spacing: 7

            TBtn {
                // Always volume-x — see the header (§12-P5).
                name: "volume-x"
                iconSize: 14
                btn: 20
                anchors.verticalCenter: parent.verticalCenter
                objectName: "miniVolumeMute"
                btnEnabled: !root.volLocked
                onClicked: QbzPlayer.toggleMute()
            }

            QbzSlider {
                objectName: "miniVolumeSlider"
                enabled: !root.volLocked
                width: 96
                anchors.verticalCenter: parent.verticalCenter
                minimum: 0
                // 0..1000, not 0..100: the slider is integer-stepped, so a
                // 0..100 scale quantizes the volume to 1 % where 0.1 % drags
                // fluidly (:146-149).
                maximum: 1000
                value: Math.round(QbzPlayer.npVolume * 1000)
                onChanged: function (v) { QbzPlayer.setVolume(v / 1000.0) }
            }

            Text {
                // 26, not the reference's `min-width: 22`: 8 + 20 + 7 + 96 + 7
                // + 26 = 164 leaves exactly the 8 px right padding the Slint
                // layout ends with, where a hard 22 would leave 12. The number
                // it renders is at most three digits at 10 px.
                width: 26
                anchors.verticalCenter: parent.verticalCenter
                text: Math.round(QbzPlayer.npVolume * 100)
                color: theme.textMuted
                font.pixelSize: 10
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
            }
        }
    }
}
