pragma Singleton
import QtQuick
QtObject {
    property var opened: []
    function openArtist(id) { opened = opened.concat([String(id)]) }
}
