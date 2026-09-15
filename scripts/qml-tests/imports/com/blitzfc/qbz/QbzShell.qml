pragma Singleton
import QtQuick
QtObject {
 signal trackCacheStatusChanged(string trackId, int status, real progress)
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
