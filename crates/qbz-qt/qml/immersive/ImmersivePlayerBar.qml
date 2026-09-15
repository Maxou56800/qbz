// ImmersivePlayerBar — the immersive compact player (contract
// 2026-08-02-immersive-port §5.7; ImmersiveView.slint:240-458). Mounted by
// ImmersiveView as layer 5, centered, y = height-90-24, fading with the
// auto-hide chrome (the host drives the opacity binding; this component
// knows nothing about the hide timer).
//
// Bar: width min(viewport-32, 600) x 90, radius 16, #00000080, border
// #ffffff2e, shadow 32/#00000066/8 (:257-265). Composition 1:1:
//   LEFT ghosts (x=24/56/88/120, y=22): favorite / shuffle / repeat / ∞
//   CENTER: elapsed · prev · play-disc-48 · next · remaining (11px #ffffffbf)
//   RIGHT: volume ghost (width-156) · VolumeBar 64 (width-112) · ⋯ (width-42)
//   TinyBar seek at y=71, width-32 — changed(v) -> seek(min(v, npSeekableMax))
//
// GhostButton / NavButton / PlayDisc are hand-rolled here (NOT QbzIconButton):
// the Slint immersive chrome paints FIXED white-alpha glyphs/fills on the
// dark overlay under EVERY theme (#ffffffa6 idle etc.), and QbzIconButton's
// theme-following tints + fills would go dark on light themes. The header's
// capsule glyphs (ImmersiveHeader.qml) made the same call. All colors are
// Slint RRGGBBAA converted to Qt #AARRGGBB.

import QtQuick
import QtQuick.Effects
import com.blitzfc.qbz
import "../controls"
import "../theme"

Item {
    id: root

    // The host viewport width (the overlay root's width) — drives the
    // min(viewport-32, 600) bar width without a parent-binding loop.
    property real viewportWidth: 720
    // The Slint bar wakes the auto-hide chrome when the ⋯ menu opens
    // (:372-374); the host owns the hide timer.
    signal wakeRequested()

    implicitWidth: bar.width
    implicitHeight: bar.height

    // Volume lock, both halves, VERBATIM (contract §5.8 / trap 8;
    // PlayerBar.qml:84-94): npVolumeLocked carries the ALSA-Direct hw
    // derivation INSIDE it, and that local bit-perfect lock is LIFTED when a
    // peer owns playback (the `&& !npIsRemote` term) so the user CAN adjust
    // the remote renderer; npRemoteVolumeLocked is the sink-pushed "peer
    // disallows remote volume" half.
    readonly property bool volLocked:
        (QbzPlayer.npVolumeLocked && !QbzPlayer.npIsRemote)
        || QbzPlayer.npRemoteVolumeLocked

    // The now-playing favorite + ephemeral flags ride the queue document's
    // `current` row (Slint: QueueState.now-playing-favorite /
    // NowPlayingState.is-ephemeral; the PlayerBar.qml npFavorite + the
    // QueuePanel.qml currentEphemeral precedents).
    readonly property var queueDoc: {
        try {
            return JSON.parse(QbzQueue.queueJson)
        } catch (e) {
            return ({})
        }
    }
    readonly property bool npFavorite: !!(queueDoc.current && queueDoc.current.isFavorite)
    readonly property bool npEphemeral: !!(queueDoc.current && queueDoc.current.isEphemeral === true)

    function fmt(secs) {
        var m = Math.floor(secs / 60)
        var s = Math.floor(secs % 60)
        return m + ":" + (s < 10 ? "0" : "") + s
    }

    // Software-renderer detection (the ImmersiveView _noShaders idiom) —
    // the two drop shadows are MultiEffect blurs; no shaders -> plain
    // offset rects.
    readonly property bool _noShaders: GraphicsInfo.api === GraphicsInfo.Software
        || GraphicsInfo.api === GraphicsInfo.Null

    // --- Bar shadow: 32 / #00000066 / offset-y 8 (:262-265) --------------
    Rectangle {
        x: 0
        y: 8
        width: bar.width
        height: bar.height
        radius: 16
        color: "#66000000"
        layer.enabled: !root._noShaders
        layer.effect: MultiEffect {
            blurEnabled: true
            blurMax: 64
            blur: 0.5 // 32/64
        }
    }

    Rectangle {
        id: bar
        width: Math.min(root.viewportWidth - 32, 600)
        height: 90
        radius: 16
        color: "#80000000"
        border.width: 1
        border.color: "#2effffff"

        // --- Inline chrome controls (see the file header for why these are
        // NOT QbzIconButton) ----------------------------------------------

        // GhostButton (:27-56): 28px circle, transparent idle, #ffffff40
        // fill while ACTIVE, glyph 12px white / white-65% (opacity on the
        // glyph — there is no 65%-white tint bake).
        component GhostBtn: Rectangle {
            property string icon: ""
            property bool active: false
            property bool btnEnabled: true
            signal clicked()

            width: 28
            height: 28
            radius: 14
            opacity: btnEnabled ? 1.0 : 0.4
            color: active ? "#40ffffff" : "transparent"
            QbzIcon {
                name: parent.icon
                width: 12
                height: 12
                anchors.centerIn: parent
                tintName: "white"
                opacity: (parent.active || ghostArea.containsMouse) ? 1.0 : 0.65
            }
            MouseArea {
                id: ghostArea
                anchors.fill: parent
                enabled: parent.btnEnabled
                hoverEnabled: true
                cursorShape: parent.btnEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                onClicked: if (parent.btnEnabled) parent.clicked()
            }
        }

        // NavButton (:58-81): 28px circle, fill #ffffff26 -> #ffffff40 on
        // hover, glyph 14px white.
        component NavBtn: Rectangle {
            property string icon: ""
            signal clicked()

            width: 28
            height: 28
            radius: 14
            color: navArea.containsMouse ? "#40ffffff" : "#26ffffff"
            QbzIcon {
                name: parent.icon
                width: 14
                height: 14
                anchors.centerIn: parent
                tintName: "white"
            }
            MouseArea {
                id: navArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: parent.clicked()
            }
        }

        // --- LEFT utility group (x=24/56/88/120, y=22; :268-300) ---------
        GhostBtn {
            x: 24
            y: 22
            icon: root.npFavorite ? "heart-filled" : "heart"
            active: root.npFavorite
            // Gated npHasTrack && !ephemeral (FavToggle.qml:47-48 precedent;
            // an ephemeral play must leave nothing behind).
            btnEnabled: QbzPlayer.npHasTrack && !root.npEphemeral
            onClicked: QbzQueue.queueToggleFavorite("track", QbzPlayer.npTrackId)
        }
        GhostBtn {
            x: 56
            y: 22
            icon: "shuffle"
            active: QbzPlayer.npShuffle
            onClicked: QbzPlayer.toggleShuffle()
        }
        GhostBtn {
            x: 88
            y: 22
            icon: "repeat"
            active: QbzPlayer.npRepeatMode !== 0
            onClicked: QbzPlayer.cycleRepeat()
        }
        GhostBtn {
            x: 120
            y: 22
            icon: "infinity"
            active: root.queueDoc.infinitePlay === true
            onClicked: QbzQueue.queueToggleInfinitePlay()
        }

        // --- CENTER playback group (:303-336): positioned from the play
        // disc so play is exactly centered ------------------------------
        Text {
            x: bar.width / 2 - 108
            y: 31
            width: 42
            text: root.fmt(QbzPlayer.npElapsedSecs)
            color: "#bfffffff"
            font.pixelSize: 11
            horizontalAlignment: Text.AlignRight
        }
        NavBtn {
            x: bar.width / 2 - 64
            y: 22
            icon: "skip-back"
            onClicked: QbzPlayer.previous()
        }
        // PlayDisc (:83-108): 48px white disc, hover #ffffffe6, glyph 20px
        // black (play nudged +1px right), shadow 12/#0000004d/4.
        Item {
            x: bar.width / 2 - 24
            y: 12
            width: 48
            height: 48
            Rectangle {
                anchors.fill: parent
                y: 4
                radius: 24
                color: "#4d000000"
                layer.enabled: !root._noShaders
                layer.effect: MultiEffect {
                    blurEnabled: true
                    blurMax: 24
                    blur: 0.5 // 12/24
                }
            }
            Rectangle {
                anchors.fill: parent
                radius: 24
                color: playArea.containsMouse ? "#e6ffffff" : "#ffffff"
                QbzIcon {
                    name: QbzPlayer.npPlaying ? "pause" : "play-fill"
                    width: 20
                    height: 20
                    x: Math.round((parent.width - width) / 2) + (QbzPlayer.npPlaying ? 0 : 1)
                    y: Math.round((parent.height - height) / 2)
                    tintName: "black"
                }
                MouseArea {
                    id: playArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: QbzPlayer.togglePlay()
                }
            }
        }
        NavBtn {
            x: bar.width / 2 + 36
            y: 22
            icon: "skip-forward"
            onClicked: QbzPlayer.next()
        }
        Text {
            x: bar.width / 2 + 76
            y: 31
            width: 46
            text: "-" + root.fmt(Math.max(0, QbzPlayer.npDurationSecs - QbzPlayer.npElapsedSecs))
            color: "#bfffffff"
            font.pixelSize: 11
        }

        // --- RIGHT volume/menu group (:339-372) ---------------------------
        GhostBtn {
            x: bar.width - 156
            y: 22
            // A read-only renderer still reports its actual level and mute.
            icon: (QbzPlayer.npMuted || QbzPlayer.npVolume === 0)
                ? "volume-x" : "volume-2"
            btnEnabled: !root.volLocked
            onClicked: QbzPlayer.toggleMute()
        }
        VolumeBar {
            x: bar.width - 112
            y: 31
            width: 64
            value: QbzPlayer.npVolume
            locked: root.volLocked
            onChanged: function (v) {
                if (!root.volLocked)
                    QbzPlayer.setVolume(v)
            }
            onReleased: function (v) {
                if (!root.volLocked)
                    QbzPlayer.persistVolume(v)
            }
        }
        // The ⋯ trigger + the ONE-entry menu ("Exit Immersive" — exit path
        // 1, §5.4). Slint shows the popup ABOVE the bar (:375-390); the
        // shared CardMenu/QbzContextMenu surface clamps into the window.
        Rectangle {
            id: menuBtn
            x: bar.width - 42
            y: 22
            width: 28
            height: 28
            radius: 14
            color: menuArea.containsMouse ? "#26ffffff" : "transparent"
            QbzIcon {
                name: "ellipsis"
                width: 12
                height: 12
                anchors.centerIn: parent
                tintName: "white"
                opacity: 0.65
            }
            MouseArea {
                id: menuArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    root.wakeRequested()
                    // Above the trigger, right edges aligned (Slint: x=-172,
                    // y=-58 for a 200x46 popup off a 28px button).
                    playerMenu.openAtCursor(menuBtn,
                        menuBtn.width - playerMenu.menuWidth,
                        -playerMenu.implicitHeight - 12)
                }
            }
        }
        CardMenu {
            id: playerMenu
            menuWidth: 200
            entries: [
                { "label": QbzSession.tr("Exit Immersive", QbzSession.trRev),
                  "icon": "x", "action": "exit" }
            ]
            onPicked: function (a) {
                if (a === "exit")
                    QbzImmersive.open = false
            }
        }

        // --- TinyBar seek (:450-457): full width-32 at y=71 --------------
        TinyBar {
            x: 16
            y: 71
            width: bar.width - 32
            value: QbzPlayer.npProgress
            onChanged: function (v) {
                QbzPlayer.seek(Math.min(v, QbzPlayer.npSeekableMax))
            }
        }
    }
}
