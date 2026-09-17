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
    property string npArtworkPath: ""
    property string npTitle: "Track"
    property string npArtist: "Artist"
    property string npAlbum: "Album"
    property real npDurationSecs: 240
    property real npElapsedSecs: 30
    property real npProgress: 0.125
    property real npSeekableMax: 1
    property int npRepeatMode: 0
    property bool npShuffle: false
    property int seekCalls: 0
    property int playCalls: 0
    function seek(f) { seekCalls++ }
    function togglePlay() { playCalls++ }
    function toggleShuffle() {}
    function cycleRepeat() {}
    function next() {}
    function previous() {}
    property bool npHasTrack: false
    property bool npPlaying: false
    property bool npLoading: false
    property string npTrackId: ""
}
