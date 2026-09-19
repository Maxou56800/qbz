import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/miniplayer" as Mini
import "../../crates/qbz-qt/qml/shell" as Shell

Item {
    width: 400; height: 600
    QtObject {
        id: host
        property int moves: 0
        property bool pinnedOnTop: false
        function startSystemMove() { moves++ }
    }
    Mini.MiniShell {
        id: mini; width: 368
        hostWindow: host
        height: QbzMini.surface === 0 ? 45 : (QbzMini.surface === 1 ? 166 : 540)
    }
    Mini.MiniWindowControls { id: capsule; y: 560; expanded: true }
    Shell.AppBackground { id: background; visible: false; width: 100; height: 100 }
    Component {
        id: footerBackdropFixture
        Rectangle {
            width: 368; height: 80; color: "#ff0000"
            Mini.MiniFooter { anchors.fill: parent; backgroundActive: true }
        }
    }
    TestCase {
        name: "MiniplayerTheme"
        when: windowShown
        function palette(light) {
            var tiers = [4,5,6,8,10,12,15,18,20,25,30,35,40,45,50,55,60,65,70,75,80,85,90,95]
            var alpha = tiers.map(function(pct) {
                var alphaByte = Math.round(pct * 255 / 100).toString(16).padStart(2, "0")
                return "#" + alphaByte + (light ? "000000" : "ffffff")
            })
            QbzShell.themeJson = JSON.stringify({isDark: !light, alpha: alpha,
                textSecondary: light ? "#444444" : "#cccccc",
                surfaceMain: light ? "#ffffff" : "#101010",
                surfaceCard: light ? "#f2f2f2" : "#1a1a1a",
                surfaceElevated: light ? "#eeeeee" : "#2a2a2a",
                textPrimary: light ? "#111111" : "#ffffff",
                textMuted: light ? "#555555" : "#aaaaaa"})
        }
        function init() {
            failOnWarning(/.*/)
            host.moves = 0
            QbzPlayer.playCalls = 0
            QbzPlayer.seekCalls = 0
            QbzMini.open = true
            QbzMini.backgroundBlur = false
            QbzMini.surface = 2
            QbzShell.ambientMode = 0
            QbzPlayer.npHasTrack = false
            QbzPlayer.npPlaying = false
            background.visible = false
            palette(true)
            mouseMove(mini, 395, 590)
            wait(500)
        }
        function test_light_dark_live_switch_data() {
            var data = []
            for (var surface = 0; surface < 5; surface++)
                for (var blur = 0; blur < 2; blur++)
                    data.push({tag: surface + "-blur-" + blur, surface: surface, blur: !!blur})
            return data
        }
        function test_light_dark_live_switch(data) {
            QbzPlayer.npHasTrack = true
            QbzPlayer.npArtworkPath = Qt.resolvedUrl("fixtures/red.ppm").toString()
            QbzMini.surface = data.surface
            QbzMini.backgroundBlur = data.blur
            var footer = findChild(mini, "miniFooter")
            for (var light of [true, false, true]) {
                palette(light)
                waitForRendering(mini)
                var cardShot = grabImage(mini)
                var capsuleShot = grabImage(capsule)
                var ink = 0
                for (var x = 6; x < capsule.width - 6; x++)
                    for (var y = 5; y < 21; y++) {
                        var r = capsuleShot.red(x, y)
                        if (light ? r < 150 : r > 150) ink++
                    }
                verify(ink > 30, "icons must actually render with contrasting foreground")
                // Background pixels clear of text, icons and rounded edges.
                function linear(v) {
                    v /= 255
                    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)
                }
                var sampleY = Math.min(200, Math.floor(mini.height / 2))
                var bg = 0.2126 * linear(cardShot.red(2, sampleY))
                       + 0.7152 * linear(cardShot.green(2, sampleY))
                       + 0.0722 * linear(cardShot.blue(2, sampleY))
                var fg = linear(light ? 17 : 255)
                var contrast = (Math.max(bg, fg) + 0.05) / (Math.min(bg, fg) + 0.05)
                verify(contrast >= 4.5, "track/queue/lyrics text contrast: " + contrast)
                verify(light ? footer.color.r > 0.9 : footer.color.r < 0.2)
                verify(light ? capsuleShot.red(3, 13) > 200 : capsuleShot.red(3, 13) < 80,
                       "options capsule must follow the text/icon theme")
            }
        }
        function test_collapsed_trigger_data() {
            return test_light_dark_live_switch_data()
        }
        function test_collapsed_trigger(data) {
            QbzMini.surface = data.surface
            QbzMini.backgroundBlur = data.blur
            for (var light of [false, true]) {
                palette(light)
                mouseMove(mini, 395, 590)
                var controls = findChild(mini, "miniWindowControls")
                verify(controls !== null, "trigger stays mounted outside hover")
                tryCompare(controls, "expanded", false)
                waitForRendering(mini)
                var trigger = findChild(controls, "miniMenuTrigger")
                var p = trigger.mapToItem(mini, 0, 0)
                var shot = grabImage(mini)
                var ink = 0
                for (var x = 5; x < 17; x++)
                    for (var y = 5; y < 17; y++) {
                        var r = shot.red(Math.round(p.x + x), Math.round(p.y + y))
                        if (light ? r < 150 : r > 150) ink++
                    }
                verify(ink > 12, "collapsed trigger visible in real card: " + ink)
                mouseMove(trigger, 11, 11)
                tryCompare(controls, "expanded", true)
                mouseMove(mini, 2, 2)
                tryCompare(controls, "expanded", false)
            }
        }
        function test_free_area_drag_data() {
            return [{tag: "micro", surface: 0, y: 8},
                    {tag: "compact", surface: 1, y: 45},
                    {tag: "artwork", surface: 2, y: 100}]
        }
        function test_free_area_drag(data) {
            QbzMini.surface = data.surface
            mousePress(mini, 100, data.y)
            compare(host.moves, 1)
            mouseRelease(mini, 100, data.y)
        }
        function test_controls_keep_their_gestures() {
            QbzPlayer.npHasTrack = true
            mouseClick(mini, mini.width / 2, mini.height - 31)
            compare(QbzPlayer.playCalls, 1)
            compare(host.moves, 0)
            mouseDrag(mini, 100, mini.height - 74, 40, 0)
            verify(QbzPlayer.seekCalls > 0)
            compare(host.moves, 0)
            for (var surface of [3, 4]) {
                QbzMini.surface = surface
                mouseDrag(mini, 100, 150, 0, -50)
                compare(host.moves, 0, "scroll surface must never drag the window")
                mousePress(mini, 3, mini.height - 3)
                compare(host.moves, 1, "footer edge remains draggable")
                mouseRelease(mini, 3, mini.height - 3)
                host.moves = 0
            }
        }
        function test_ambient_metadata_and_chrome() {
            QbzPlayer.npHasTrack = true
            QbzShell.ambientMode = 2
            for (var light of [false, true]) {
                palette(light)
                var doc = JSON.parse(QbzShell.themeJson)
                doc.surfaceElevated = light ? "#80eeeeee" : "#802a2a2a"
                QbzShell.themeJson = JSON.stringify(doc)
                for (var surface of [1, 2]) {
                    QbzMini.surface = surface
                    var metadata = findChild(mini, "miniMetadata")
                    verify(metadata !== null)
                    if (!light) {
                        compare(metadata.headerStrong, Qt.color("#ffffff"))
                        compare(metadata.headerBody, Qt.color("#e0ffffff"))
                    } else {
                        verify(metadata.headerBody.r < 0.5, "dark text on light ambient veil")
                    }
                }
                waitForRendering(capsule)
                var shot = grabImage(capsule)
                compare(capsule.color.a, 1, "menu must ignore translucent palette alpha")
                verify(light ? shot.red(3, 13) > 220 : shot.red(3, 13) < 60)
            }
            palette(false)
            var fixture = createTemporaryObject(footerBackdropFixture, mini, {z: 20})
            verify(fixture !== null)
            waitForRendering(fixture)
            var footerShot = grabImage(fixture)
            verify(footerShot.red(3, 25) > 190, "ambient must show through the control area")
            verify(footerShot.green(3, 25) < 40)
        }
        function test_inherits_background_and_unloads_when_closed() {
            QbzPlayer.npHasTrack = true
            for (var mode = 1; mode <= 4; mode++) {
                QbzShell.ambientMode = mode
                verify(mini.useAppBackground)
                verify(mini.backdropMounted)
                QbzMini.backgroundBlur = true
                verify(!mini.useAppBackground, "mini blur explicitly overrides the app mode")
                QbzMini.backgroundBlur = false
                QbzMini.open = false
                verify(!mini.backdropMounted)
                QbzMini.open = true
            }
            QbzShell.ambientMode = 1
            QbzPlayer.npHasTrack = false
            verify(!mini.backdropOn, "no artwork means the opaque theme returns")
        }
        function test_hidden_background_stops_animation() {
            QbzShell.ambientMode = 2
            QbzPlayer.npHasTrack = true
            QbzPlayer.npPlaying = true
            background.visible = true
            verify(background.blurredModeOn)
            background.visible = false
            verify(!background.blurredModeOn)
            QbzShell.pulseMs += 33
            verify(!background.painting)
        }
    }
}
