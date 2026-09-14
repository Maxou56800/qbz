// WallpaperField: the wallpaper crop sits under the window. On X11 / macOS /
// Windows the offset is the window's own position; on Wayland it is the
// compositor's rectangle for this window (picked by size out of this
// process's windows); with neither, the crop is centred. An invisible field
// reads nothing and asks for nothing.
import QtQuick
import QtQuick.Window
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/shell"

Item {
    id: root
    width: 800; height: 600

    QtObject {
        id: fakeWindow
        property real x: 300
        property real y: 100
        property real width: 800
        property real height: 600
    }

    WallpaperField {
        id: field
        width: 800; height: 600
        hostWindow: fakeWindow
        source: Qt.resolvedUrl("fixtures/red.ppm")
        isWayland: false
    }

    TestCase {
        name: "WallpaperField"
        when: windowShown

        function init() {
            QbzShell.wallpaperWindowsJson = "[]"
            field.visible = true
            field.isWayland = false
            fakeWindow.x = 300
            fakeWindow.y = 100
        }

        function test_a_x11_follows_the_window_position() {
            compare(field.offX, 300 - Screen.virtualX)
            compare(field.offY, 100 - Screen.virtualY)
            fakeWindow.x = 20
            compare(field.offX, 20 - Screen.virtualX)
        }

        function test_b_wayland_uses_the_compositor_rect_for_this_window() {
            var before = QbzShell.trackRequests
            field.isWayland = true
            verify(QbzShell.trackRequests > before, "asks Rust to follow the window")
            // Without a report the crop is centred.
            verify(!field.positionKnown)
            compare(field.offX, Math.max(0, (field.screenW - 800) / 2))
            // Two of our windows: the miniplayer and the main window.
            QbzShell.wallpaperWindowsJson = JSON.stringify([
                { "x": 10, "y": 10, "w": 320, "h": 96 },
                { "x": 640, "y": 120, "w": 800, "h": 600 }
            ])
            verify(field.positionKnown)
            compare(field.offX, 640 - Screen.virtualX)
            compare(field.offY, 120 - Screen.virtualY)
            // The window moved: the compositor says so.
            QbzShell.wallpaperWindowsJson = JSON.stringify([{ "x": 900, "y": 40, "w": 800, "h": 600 }])
            compare(field.offX, 900 - Screen.virtualX)
        }

        function test_c_wayland_ignores_windows_that_are_not_this_size() {
            field.isWayland = true
            QbzShell.wallpaperWindowsJson = JSON.stringify([{ "x": 10, "y": 10, "w": 320, "h": 96 }])
            verify(!field.positionKnown, "the miniplayer is not this window")
        }

        function test_d_invisible_field_reads_nothing() {
            field.isWayland = true
            QbzShell.wallpaperWindowsJson = JSON.stringify([{ "x": 640, "y": 120, "w": 800, "h": 600 }])
            field.visible = false
            compare(field.compositorRects.length, 0)
            verify(!field.positionKnown)
        }
    }
}
