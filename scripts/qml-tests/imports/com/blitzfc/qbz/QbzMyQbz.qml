pragma Singleton
import QtQuick
QtObject {
    property var selectCalls: []
    property var opened: []
    property var played: []
    function detailToggleItemSelect(position, shift) {
        selectCalls = selectCalls.concat([{ "position": position, "shift": shift }])
    }
    function openItem(source, itemType, id) { opened = opened.concat([String(id)]) }
    function openArtist(source, name, id) { opened = opened.concat(["artist:" + name]) }
    function playItem(id) { played = played.concat([String(id)]) }
    function detailToggleRowExpand(id) {}
}
