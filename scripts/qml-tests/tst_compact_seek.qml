import QtQuick
import QtQuick.Controls
import QtTest
import "../../crates/qbz-qt/qml/controls" as Controls

Item {
    width: 800; height: 300
ApplicationWindow {
    id: window
    visible: true
    width: 800; height: 300
    Item {
        id: transport
        anchors.left: parent.left; anchors.right: parent.right; anchors.bottom: parent.bottom
        height: 42
        z: 1
        Controls.QbzCompactSeekBar {
            id: rail
            width: parent.width
            hasTrack: true
            durationSecs: 600
            progress: 0.5
        }
    }
    // Same sibling order as AppShell: content is declared after the transport.
    Rectangle {
        anchors.top: parent.top; anchors.left: parent.left; anchors.right: parent.right
        anchors.bottom: transport.top
        color: "#0f0f0f"
    }
    SignalSpy { id: seeks; target: rail; signalName: "seekRequested" }
    TestCase {
        name: "CompactSeek"; when: windowShown
        function init() {
            rail.hasTrack = true
            rail.seekableMax = 1
            rail.visible = true
            seeks.clear()
            mouseMove(window.contentItem, 10, 10)
        }
        function test_thumb_and_time_above_content() {
            mouseMove(rail, 600, -3)
            var tip = findChild(rail, "compactSeekTip")
            verify(tip !== null)
            tryCompare(tip, "visible", true)
            compare(findChild(rail, "compactSeekTime").text, "7:30")
            verify(tip.y + tip.height <= rail.mapToItem(tip.parent, 0, 0).y + 4)
            verify(waitForRendering(window.contentItem))
            var point = rail.mapToItem(window.contentItem, 400, -3)
            var shot = grabImage(window.contentItem)
            compare(shot.red(point.x, point.y), 255, "upper half of thumb paints above content")
            compare(shot.green(point.x, point.y), 255)
            compare(shot.blue(point.x, point.y), 255)
        }
        function test_seek_clamps_to_downloaded_audio() {
            rail.seekableMax = 0.25
            mouseClick(rail, 600, -3)
            compare(seeks.count, 1)
            compare(seeks.signalArguments[0][0], 0.25)
        }
        function test_no_track_and_exit_hide_tooltip() {
            mouseMove(rail, 400, 1)
            var tip = findChild(rail, "compactSeekTip")
            tryCompare(tip, "visible", true)
            mouseMove(window.contentItem, 10, 10)
            tryCompare(tip, "visible", false)
            rail.hasTrack = false
            mouseClick(rail, 400, 1)
            compare(seeks.count, 0)
            verify(!tip.visible)
        }
        function test_bubble_stays_within_window_edges() {
            var tip = findChild(rail, "compactSeekTip")
            mouseMove(rail, 1, 1)
            tryCompare(tip, "visible", true)
            verify(tip.x >= 0)
            mouseMove(rail, rail.width - 1, 1)
            verify(tip.x + tip.width <= window.width)
        }
    }
}
}
