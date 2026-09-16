pragma Singleton
import QtQuick
QtObject {
property bool open: true
property int surface: 2
property bool backgroundBlur: false
property string miniQueueJson: '{"currentId":"","rows":[]}'
function toggleBackgroundBlur() { backgroundBlur = !backgroundBlur }
function exit() { open = false }
function closeApp() {}
function queuePlay(id) {}
}
