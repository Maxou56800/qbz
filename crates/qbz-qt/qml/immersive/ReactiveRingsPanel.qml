// ReactiveRingsPanel — the fused Qt port of Tauri's EnergyBandsPanel and
// TransientPulsePanel. Five persistent concentric rings breathe from the five
// semantic energy bands; detected onsets launch a bounded pool of expanding
// pulse rings. Both halves use the artwork palette already published by the
// Qt shell, so no image sampling or audio-backend work is duplicated here.
//
// THE PULSE LAW: QbzShaderScene.packJson is parsed and STASHED on publish.
// State is applied only on QbzShell.pulseMs, in the same event-loop turn as
// every other visualizer. There is no private Timer, animation driver or
// data-fed Behavior. The pool is static in the scene graph (12 delegates),
// and it parks as soon as its last pulse ring fades.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz

Rectangle {
    id: root

    color: "#000000"
    clip: true

    readonly property bool windowShowing: root.Window.window
        ? (root.Window.window.visibility !== Window.Minimized
           && root.Window.window.visibility !== Window.Hidden)
        : true
    readonly property bool active: root.visible && QbzImmersive.open
        && root.windowShowing
    readonly property real minDim: Math.min(root.width, root.height)

    // Applied audio state. The upstream processor has already smoothed these
    // five semantic bands; applying the freshest pack directly keeps this
    // panel intentionally quicker than the 90ms bar visualizers.
    property var bandEnergy: [0, 0, 0, 0, 0]
    property real globalEnergy: 0
    property real colorPhase: 0
    property real previousBass: 0
    property real previousHit: 0
    property int hitCooldown: 0

    // Expanding ring records use radius/speed as fractions of minDim, so the
    // same pulse reads identically on a laptop and a 4K television.
    property var pulseRings: []
    property int nextColor: 0
    readonly property int maxPulseRings: 12

    // The publish-edge stash. Nothing binds it; writing it alone cannot dirty
    // the scene. It is consumed on the shared pulse below.
    property var pendingPack: null

    // Force the QString bridge values through QML's color type before the JS
    // interpolation helper reads r/g/b channels.
    readonly property color primaryColor: QbzShell.ambientPrimary
    readonly property color accentColor: QbzShell.ambientAccent
    readonly property color secondaryColor: QbzShell.ambientSecondary
    // `ringPalette`, not `palette`: Item already owns a `palette` (QPalette)
    // and shadowing it drew a propertyCache warning on every start.
    readonly property var ringPalette: [
        root.primaryColor,
        root.accentColor,
        root.secondaryColor,
        Qt.lighter(root.primaryColor, 1.32),
        Qt.lighter(root.accentColor, 1.24),
        Qt.lighter(root.secondaryColor, 1.30)
    ]

    readonly property var bandNames: ["SUB", "BASS", "MID", "PRES", "AIR"]

    function paletteColor(position, alpha) {
        var count = root.ringPalette.length
        var wrapped = position % count
        if (wrapped < 0)
            wrapped += count
        var lo = Math.floor(wrapped)
        var hi = (lo + 1) % count
        var mix = wrapped - lo
        var a = Qt.lighter(root.ringPalette[lo], 1.0)
        var b = Qt.lighter(root.ringPalette[hi], 1.0)
        return Qt.rgba(a.r + (b.r - a.r) * mix,
                       a.g + (b.g - a.g) * mix,
                       a.b + (b.b - a.b) * mix,
                       Math.max(0, Math.min(1, alpha)))
    }

    function stashCurrentPack() {
        try {
            root.pendingPack = JSON.parse(QbzShaderScene.packJson)
        } catch (e) {
            // A malformed pack keeps the last good visual state.
        }
    }

    onVisibleChanged: if (visible) stashCurrentPack()
    Component.onCompleted: stashCurrentPack()

    Connections {
        target: QbzShaderScene
        function onPackJsonChanged() { root.stashCurrentPack() }
    }

    Connections {
        target: QbzPlayer
        function onNpTrackIdChanged() {
            // A new palette starts from a clean pulse history; otherwise the
            // previous song's final bass edge can manufacture a first-frame hit.
            root.previousBass = 0
            root.previousHit = 0
            root.hitCooldown = 0
            root.pulseRings = []
        }
    }

    Connections {
        target: QbzShell
        function onPulseMsChanged() {
            if (!root.active)
                return

            var pack = root.pendingPack
            var spawn = false
            var intensity = 0
            if (pack !== null) {
                root.pendingPack = null
                var lo = pack.energyLo || [0, 0, 0, 0]
                var hi = pack.energyHi || [0, 0, 0, 0]
                var energy = [lo[0] || 0, lo[1] || 0, lo[2] || 0,
                              lo[3] || 0, hi[0] || 0]
                root.bandEnergy = energy
                root.globalEnergy = (energy[0] + energy[1] + energy[2]
                                     + energy[3] + energy[4]) / 5

                // Tauri's supplementary bass-delta detector, strengthened by
                // the host-side AC-coupled onset. A two-frame cooldown still
                // resolves dense drums while preventing one envelope from
                // launching a ring on every decay sample.
                var bass = (energy[0] * 2.0 + energy[1] * 1.5) / 3.5
                var bassDelta = bass - root.previousBass
                root.previousBass = bass
                var beatAc = pack.beatAc || 0
                var transientAmp = pack.transient || 0
                var hit = Math.max(beatAc, transientAmp * 0.78)
                if (root.hitCooldown > 0)
                    root.hitCooldown--
                if (root.hitCooldown <= 0
                        && (bassDelta > 0.045
                            || (hit > 0.16 && hit - root.previousHit > 0.035))) {
                    spawn = true
                    intensity = Math.min(1, Math.max(bassDelta * 4.2, hit))
                    root.hitCooldown = 2
                }
                root.previousHit = hit

                // Move colour only while real audio is moving. A silent or
                // paused panel therefore converges to zero writes/presents.
                if (QbzPlayer.npPlaying && root.globalEnergy > 0.006)
                    root.colorPhase = (root.colorPhase + 0.008
                        + root.globalEnergy * 0.035 + beatAc * 0.055) % 6
            }

            var old = root.pulseRings
            var next = []
            for (var i = 0; i < old.length; ++i) {
                var ring = old[i]
                var radius = ring.radius + ring.speed
                var alpha = ring.alpha * 0.94
                if (alpha >= 0.012 && radius <= ring.maxRadius * 1.35) {
                    next.push({
                        "radius": radius,
                        "maxRadius": ring.maxRadius,
                        "alpha": alpha,
                        "lineWidth": ring.lineWidth,
                        "speed": ring.speed,
                        "color": ring.color
                    })
                }
            }
            if (spawn && QbzPlayer.npPlaying) {
                next.push({
                    "radius": 0.11,
                    "maxRadius": 0.34 + intensity * 0.23,
                    "alpha": 0.58 + intensity * 0.40,
                    "lineWidth": 2.0 + intensity * 4.5,
                    "speed": 0.006 + intensity * 0.013,
                    "color": root.nextColor
                })
                root.nextColor = (root.nextColor + 1) % root.ringPalette.length
                if (next.length > root.maxPulseRings)
                    next.shift()
            }
            if (next.length > 0 || old.length > 0 || spawn)
                root.pulseRings = next
        }
    }

    // A cheap radial glow made from four persistent discs. It retains the
    // centre bloom of both Tauri panels without a full-screen blur pass.
    Repeater {
        model: 4
        delegate: Rectangle {
            required property int index
            readonly property real energy: root.globalEnergy
            readonly property real discSize: root.minDim
                * (0.10 + (3 - index) * 0.047 + energy * 0.035)
            width: discSize
            height: discSize
            x: (root.width - width) / 2
            y: (root.height - height) / 2
            radius: width / 2
            color: root.paletteColor(index * 0.7 + root.colorPhase,
                (0.018 + energy * 0.045) * (index + 1))
        }
    }

    // The transient half: a static 12-delegate pool, each record expanding
    // outward and fading on the shared pulse.
    Repeater {
        model: root.maxPulseRings
        delegate: Item {
            required property int index
            readonly property bool populated: index < root.pulseRings.length
            readonly property var ring: populated ? root.pulseRings[index] : null
            readonly property real diameter: populated
                ? ring.radius * root.minDim * 2 : 0
            visible: populated
            x: (root.width - diameter) / 2
            y: (root.height - diameter) / 2
            width: diameter
            height: diameter

            Rectangle {
                anchors.fill: parent
                anchors.margins: -3
                radius: width / 2
                color: "transparent"
                border.width: parent.ring ? parent.ring.lineWidth + 6 : 0
                border.color: parent.ring
                    ? root.paletteColor(parent.ring.color + root.colorPhase,
                        parent.ring.alpha * 0.20) : "transparent"
            }
            Rectangle {
                anchors.fill: parent
                radius: width / 2
                color: "transparent"
                border.width: parent.ring ? Math.max(1, parent.ring.lineWidth) : 0
                border.color: parent.ring
                    ? root.paletteColor(parent.ring.color + root.colorPhase,
                        parent.ring.alpha) : "transparent"
            }
        }
    }

    // The energy-band half: five concentric rings, ordered outer (sub) to
    // inner (air), each with a low-cost halo arc and a crisp core arc.
    Repeater {
        model: 5
        delegate: Item {
            required property int index
            readonly property real energy: root.bandEnergy[index] || 0
            readonly property real radiusF: 0.115 + (4 - index) * 0.066
                + energy * 0.058
            readonly property real diameter: radiusF * root.minDim * 2
            x: (root.width - diameter) / 2
            y: (root.height - diameter) / 2
            width: diameter
            height: diameter

            Rectangle {
                anchors.fill: parent
                anchors.margins: -(3 + parent.energy * 5)
                radius: width / 2
                color: "transparent"
                border.width: 7 + parent.energy * 9
                border.color: root.paletteColor(index + root.colorPhase,
                    0.055 + parent.energy * 0.18)
            }
            Rectangle {
                anchors.fill: parent
                radius: width / 2
                color: "transparent"
                border.width: 2 + parent.energy * 4
                border.color: root.paletteColor(index + root.colorPhase,
                    0.20 + parent.energy * 0.72)
            }
        }
    }

    Row {
        visible: root.height >= 560
        anchors.horizontalCenter: parent.horizontalCenter
        y: root.height / 2 + root.minDim * 0.41
        Repeater {
            model: 5
            delegate: Text {
                required property int index
                width: 46
                text: root.bandNames[index]
                horizontalAlignment: Text.AlignHCenter
                color: root.paletteColor(index + root.colorPhase,
                    0.32 + (root.bandEnergy[index] || 0) * 0.55)
                font.pixelSize: 10
                font.family: "monospace"
                font.letterSpacing: 0.8
            }
        }
    }
}
