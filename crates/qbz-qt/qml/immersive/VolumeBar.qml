// VolumeBar — the immersive volume micro-slider (ImmersiveView.slint:153-238),
// immersive-local per divergence D5 (QbzSlider is integer-step and has no lock
// states). Thin white-on-glass track, hover-revealed thumb, drag-time percent
// bubble, and a LOCKED state that preserves the reported fill and dims/no-ops
// (bit-perfect ALSA-Direct or a QConnect peer that disallows remote volume).
//
// Lock FORMULA lives in the host (ImmersivePlayerBar), verbatim from
// PlayerBar.qml:92-94 (contract §5.8 / trap 8). Wiring: live changed(v) ->
// QbzPlayer.setVolume(v); drag-end released(v) -> QbzPlayer.persistVolume(v).
// NOTE: no `opacity` on the root (it would corrupt absolute positioning) —
// inner content carries the states instead.

import QtQuick

Item {
    id: root

    property real value: 0.0        // 0..1
    property bool locked: false
    signal changed(real v)          // emits 0..1 — live (every drag tick)
    signal released(real v)         // emits 0..1 — drag-end only (persist)

    // Locked peers still display their reported volume.
    readonly property real shown: Math.max(0.0, Math.min(1.0, root.value))

    function clamp01(v) {
        return Math.max(0.0, Math.min(1.0, v))
    }

    height: 12

    // Drag-time / hover percent readout, 34x20 at y=-26, #000000b3 (Qt
    // #AARRGGBB), 11px/600. Hidden when locked.
    Rectangle {
        x: root.width - width
        y: -26
        width: 34
        height: 20
        radius: 6
        color: "#b3000000"
        visible: !root.locked && (area.containsMouse || area.pressed)
        Text {
            anchors.fill: parent
            text: Math.round(root.shown * 100)
            color: "#ffffff"
            font.pixelSize: 11
            font.weight: 600
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
        }
    }

    // Track background: #ffffff1f locked / #ffffff33.
    Rectangle {
        x: 0
        y: 5
        width: root.width
        height: 2
        radius: 1
        color: root.locked ? "#1fffffff" : "#33ffffff"
    }
    // Fill: #ffffff66 locked / #ffffffb3.
    Rectangle {
        x: 0
        y: 5
        width: root.width * root.shown
        height: 2
        radius: 1
        color: root.locked ? "#66ffffff" : "#b3ffffff"
    }
    // Hover-revealed thumb, 12px white, only !locked && (hover || pressed).
    // Position computed from root.width (no layout recursion).
    Rectangle {
        x: root.width * root.shown - width / 2
        y: 0
        width: 12
        height: 12
        radius: 6
        color: "#ffffff"
        opacity: (!root.locked && (area.containsMouse || area.pressed)) ? 1.0 : 0.0
        Behavior on opacity { NumberAnimation { duration: 150 } }
    }

    // ALL pointer events gated !locked (:218-237).
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: root.locked ? Qt.ArrowCursor : Qt.PointingHandCursor
        onPressed: function (mouse) {
            if (!root.locked)
                root.changed(root.clamp01(mouse.x / width))
        }
        onPositionChanged: function (mouse) {
            if (!root.locked && pressed)
                root.changed(root.clamp01(mouse.x / width))
        }
        // Persist only the final value on drag-end (the NPB sliders'
        // released -> persistVolume; live changes still fire via changed).
        onReleased: function (mouse) {
            if (!root.locked)
                root.released(root.clamp01(mouse.x / width))
        }
    }
}
