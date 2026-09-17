pragma Singleton
import QtQuick
QtObject {
    property string updatesJson: "{}"
    property int checkCalls: 0
    property int installCalls: 0
    property int cancelCalls: 0
    property int ignoreCalls: 0
    function updatesCheck() { checkCalls++ }
    function updatesInstall() { installCalls++ }
    function updatesCancel() { cancelCalls++ }
    function updatesIgnore() { ignoreCalls++ }
    function updatesClose() {
        var doc = JSON.parse(updatesJson)
        doc.open = false
        updatesJson = JSON.stringify(doc)
    }
    function updatesSetLaunch(enabled) {}
    function whatsNewOpen() {}
}
