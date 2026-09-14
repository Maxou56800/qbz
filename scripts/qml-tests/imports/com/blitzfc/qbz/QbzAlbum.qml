pragma Singleton
import QtQuick
QtObject {
    property var opened: []
    function openAlbum(id) { opened = opened.concat([String(id)]) }
    function openAlbumFrom(id, title, artist) { opened = opened.concat([String(id)]) }
}
