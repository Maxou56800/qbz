import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/settings" as Settings
import "../../crates/qbz-qt/qml/shell" as Shell

Item {
    width: 1000; height: 1000
    Settings.OrbitSettingsPanel { id: panel; width: 900; selected: true }
    Shell.OrbitBanner { id: banner; width: 900 }
    TestCase {
        name: "OrbitLaboratory"; when: windowShown
        function init() {
            QbzOrbit.enabled = false
            QbzOrbit.stateJson = "{}"
            panel.selected = true
            banner.remoteActive = false
            banner.connectionLost = false
            banner.hostName = ""
        }
        function test_panel_is_not_constructed_without_launch_flag() {
            compare(panel.item, null)
            verify(!panel.visible)
            QbzOrbit.enabled = true
            tryVerify(function() { return panel.item !== null })
            panel.selected = false
            compare(panel.item, null)
            panel.selected = true
            tryVerify(function() { return panel.item !== null })
            QbzOrbit.enabled = false
            compare(panel.item, null)
        }
        function test_verified_host_does_not_claim_remote_control() {
            QbzOrbit.enabled = true
            QbzOrbit.stateJson = JSON.stringify({peer:{name:"Other computer", tracks:12}, status:"verified"})
            tryVerify(function() { return panel.item !== null })
            compare(panel.item.doc.peer.name, "Other computer")
            verify(!banner.visible)
            compare(banner.height, 0)
        }
        function test_remote_banner_survives_clicks_and_connection_loss() {
            banner.hostName = "Studio QBZ"
            banner.remoteActive = true
            verify(banner.visible)
            verify(banner.height >= 38)
            var label = findChild(banner, "orbitContextLabel")
            verify(label.text.indexOf("Studio QBZ") >= 0)
            mouseClick(banner, 800, 15)
            verify(banner.visible)
            banner.connectionLost = true
            verify(banner.visible)
            verify(label.text.indexOf("Remote controls are unavailable") >= 0)
            banner.remoteActive = false
            compare(banner.height, 0)
        }
    }
}
