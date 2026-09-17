pragma Singleton
import QtQuick
QtObject {
 signal trackCacheStatusChanged(string trackId, int status, real progress)
 property bool isWindows: false
 property real ambientDim: 0.35
 property color ambientPrimary: "#c08030"
 property color ambientSecondary: "#306080"
 property color ambientAccent: "#803060"
 property real pulseMs: 0
 property string wallpaperUrl: ""
 property string wallpaperAtmosphereUrl: ""
 property real wallpaperBlur: 0.75
 function refreshWallpaper() {}
 property bool reduceMotion: false
 property bool forceCanvasArt: false
 property string restoreScope: ""
 property real scrollRestore: 0
 function reportScroll(scope, y) {}
 property string themeJson: ""; property int ambientMode: 0
 property string wallpaperWindowsJson: "[]"
 property int trackRequests: 0
 function trackWindowPosition() { trackRequests++ }
 property int dragStarts: 0
 function dragStart(id, title, subtitle, x, y, inline) { dragStarts++ }
 function dragStartLocal(id, title, subtitle, x, y, inline) { dragStarts++ }
 function dragMove(x, y) {}
 function dragEnd() {} }
