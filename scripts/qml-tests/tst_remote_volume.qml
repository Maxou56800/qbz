import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/miniplayer" as Mini
import "../../crates/qbz-qt/qml/immersive" as Immersive

Item {
    width: 600; height: 240
    Mini.MiniVolume { id: mini; x: 30; y: 50; popupOpen: true }
    Immersive.VolumeBar {
        id: immersive
        x: 50; y: 150; width: 160
        value: QbzPlayer.npVolume
        locked: mini.volLocked
    }
    SignalSpy { id: immersiveChanges; target: immersive; signalName: "changed" }
    SignalSpy { id: immersiveReleases; target: immersive; signalName: "released" }
    TestCase {
        name: "RemoteVolume"
        when: windowShown
        function init() {
            QbzPlayer.npVolumeLocked = true
            QbzPlayer.npIsRemote = true
            QbzPlayer.npRemoteVolumeLocked = true
            QbzPlayer.npVolume = 0.2
            QbzPlayer.volumeCalls = 0
            QbzPlayer.muteCalls = 0
            mini.popupOpen = true
            immersiveChanges.clear()
            immersiveReleases.clear()
        }
        function test_denied_peer_preserves_level_and_rejects_interaction() {
            var slider = findChild(mini, "miniVolumeSlider")
            var mute = findChild(mini, "miniVolumeMute")
            compare(slider.enabled, false)
            compare(mute.btnEnabled, false)
            compare(slider.value, 200)
            compare(immersive.shown, 0.2)
            mouseDrag(slider, 20, 10, 50, 0)
            mouseClick(mute, 10, 10)
            mouseDrag(immersive, 30, 6, 70, 0)
            compare(QbzPlayer.volumeCalls, 0)
            compare(QbzPlayer.muteCalls, 0)
            compare(immersiveChanges.count, 0)
            compare(immersiveReleases.count, 0)
        }
        function test_allowed_peer_unlocks_despite_local_output_lock() {
            QbzPlayer.npRemoteVolumeLocked = false
            var slider = findChild(mini, "miniVolumeSlider")
            compare(slider.enabled, true)
            mouseClick(slider, 60, 10)
            mouseClick(findChild(mini, "miniVolumeMute"), 10, 10)
            mouseClick(immersive, 100, 6)
            verify(QbzPlayer.volumeCalls > 0)
            compare(QbzPlayer.muteCalls, 1)
            verify(immersiveChanges.count > 0)
        }
        function test_permission_revoked_during_drag_stops_further_commands() {
            QbzPlayer.npRemoteVolumeLocked = false
            mousePress(immersive, 30, 6)
            verify(immersiveChanges.count > 0)
            immersiveChanges.clear()
            QbzPlayer.npRemoteVolumeLocked = true
            mouseMove(immersive, 100, 6)
            mouseRelease(immersive, 100, 6)
            compare(immersiveChanges.count, 0)
            compare(immersiveReleases.count, 0)
        }
        function test_mini_drag_cancellation_restores_reported_level() {
            QbzPlayer.npRemoteVolumeLocked = false
            var slider = findChild(mini, "miniVolumeSlider")
            mousePress(slider, 60, 10)
            verify(slider.dragging)
            var calls = QbzPlayer.volumeCalls
            QbzPlayer.npRemoteVolumeLocked = true
            compare(slider.dragging, false)
            mouseMove(slider, 80, 10)
            mouseRelease(slider, 80, 10)
            compare(QbzPlayer.volumeCalls, calls)
            compare(slider.shownFraction, 0.2)
        }
        function test_return_to_local_restores_output_lock() {
            QbzPlayer.npRemoteVolumeLocked = false
            QbzPlayer.npIsRemote = false
            compare(findChild(mini, "miniVolumeSlider").enabled, false)
            QbzPlayer.npVolumeLocked = false
            compare(findChild(mini, "miniVolumeSlider").enabled, true)
        }
    }
}
