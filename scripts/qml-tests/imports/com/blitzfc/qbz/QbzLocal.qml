pragma Singleton
import QtQuick
QtObject {
    function artworkImmediateEnabled() { return true }
    property string mediaStatus: "{}"
    property int pairingCancels: 0
    function mediaCancelQuickConnect() { pairingCancels++ }
    function mediaQuickConnect(url) {}

    // Folder tree rail (LocalTreeRail / TreeRow).
    property bool localTreeLoading: false
    property int localTreeSelectedCount: 0
    property var treeCalls: []
    function treeToggleFolderSelect(path) { treeCalls = treeCalls.concat(["folder:" + path]) }
    function treeToggleTrackSelect(path) { treeCalls = treeCalls.concat(["track:" + path]) }
    function treeSelectRange(json) { treeCalls = treeCalls.concat(["range:" + json]) }
    function treeToggle(path, open) {}
    function treeCollapseAll() {}
    function treeSearch(query) {}
    function foldersBulkAction(action) {}
    function playFolder(path) {}
    function enqueue(kind, id, mode) {}
}
