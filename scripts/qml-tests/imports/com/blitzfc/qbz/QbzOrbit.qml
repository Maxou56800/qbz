pragma Singleton
import QtQuick
QtObject {
    property bool enabled: false
    property bool controllingRemote: false
    property bool connectionLost: false
    property string hostName: ""
    property string stateJson: "{}"
    signal verificationRequested(string url, string token)
    function startHost(address) {}
    function stopHost() {}
    function copyAccessKey() {}
    function verifyHost(url, token) { verificationRequested(url, token) }
    function searchLibrary(query) {}
}
