pragma Singleton
import QtQuick
QtObject {
    property bool npVolumeLocked: false
    property bool npRemoteVolumeLocked: false
    property bool npIsRemote: false
    property bool npMuted: false
    property real npVolume: 0.2
    property int volumeCalls: 0
    property int muteCalls: 0
    function setVolume(value) { volumeCalls++ }
    function toggleMute() { muteCalls++ }
    property bool npHasTrack: false
    property bool npPlaying: false
    property bool npLoading: false
    property string npTrackId: ""
}
