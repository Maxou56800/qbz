pragma Singleton
import QtQuick
QtObject {
property string docJson: '{"lines":[],"synced":false}'
property bool showTranslation: false
property bool liteFill: false
property bool playing: false
property real positionMs: 0
function activeIndexAt(ms) { return -1 }
function fillFractionAt(index, ms) { return 0 }
}
