import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/settings" as Settings
import "../../crates/qbz-qt/qml/shell" as Shell

Item {
    width: 1000; height: 800
    Settings.UpdatesSettings { id: settings; width: 900 }
    Shell.UpdatesModal { id: modal }
    TestCase {
        name: "Updates"; when: windowShown
        function init() {
            modal.kioskHost = false
            QbzAbout.checkCalls = 0
            QbzAbout.installCalls = 0
            QbzAbout.cancelCalls = 0
            QbzAbout.updatesJson = JSON.stringify({open:false, phase:"idle", currentVersion:"2.1.1"})
        }
        function test_manual_check_action() {
            var button = findChild(settings, "updateCheckButton")
            verify(button.enabled)
            mouseClick(button)
            compare(QbzAbout.checkCalls, 1)
            QbzAbout.updatesJson = JSON.stringify({busy:true, phase:"checking"})
            verify(!button.enabled)
        }
        function test_managed_install_has_release_link_without_installer() {
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"available", version:"2.1.2", canInstall:false})
            verify(modal.visible)
            verify(!findChild(modal, "updateInstallButton").visible)
            verify(findChild(modal, "updateReleaseButton").visible)
            modal.dismiss()
            verify(!modal.visible)
        }
        function test_signed_installer_action_and_commit_cannot_be_dismissed() {
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"available", version:"2.1.2", canInstall:true})
            var button = findChild(modal, "updateInstallButton")
            verify(button.visible)
            verify(waitForRendering(modal))
            mouseClick(button)
            compare(QbzAbout.installCalls, 1)
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"installing", busy:true})
            modal.dismiss()
            verify(modal.visible)
            verify(!findChild(modal, "updateCloseButton").enabled)
            verify(!button.visible)
        }
        function test_kiosk_update_buttons_have_touch_height() {
            modal.kioskHost = true
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"available", version:"2.1.2", canInstall:true})
            verify(findChild(modal, "updateInstallButton").height >= 44)
            verify(findChild(modal, "updateCloseButton").height >= 44)
        }
        function test_network_failure_is_not_reported_as_up_to_date() {
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"error", error:"HTTP 403"})
            verify(modal.statusText().indexOf("could not be completed") >= 0)
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"current"})
            verify(modal.statusText().indexOf("latest version") >= 0)
        }
        function test_flatpak_channel_update_without_github_version() {
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"available", version:"", canInstall:true, installMethod:"Flatpak"})
            var install = findChild(modal, "updateInstallButton")
            verify(install.visible)
            verify(!findChild(modal, "updateReleaseButton").visible)
            verify(waitForRendering(modal))
            mouseClick(install)
            compare(QbzAbout.installCalls, 1)
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"updating", busy:true, percent:42})
            var cancel = findChild(modal, "updateCancelButton")
            verify(cancel.visible)
            verify(waitForRendering(modal))
            mouseClick(cancel)
            compare(QbzAbout.cancelCalls, 1)
        }
        function test_msi_staged_is_not_reported_as_installed() {
            QbzAbout.updatesJson = JSON.stringify({open:true, phase:"prepared", busy:false, canInstall:false})
            verify(modal.statusText().indexOf("finish installation") >= 0)
            verify(findChild(modal, "updateCloseButton").enabled)
            verify(!findChild(modal, "updateInstallButton").visible)
            modal.dismiss()
            verify(!modal.visible)
        }
    }
}
