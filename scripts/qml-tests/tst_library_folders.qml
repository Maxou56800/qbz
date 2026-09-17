import QtQuick
import QtQuick.Controls
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/settings" as Settings

Item {
    width: 1300; height: 1000
    Settings.LibraryFolderTable { id: table; width: 1250 }
    TestCase {
        name: "LibraryFolders"; when: windowShown
        function init() {
            table.width = 1250
            QbzBridge.stringCalls = []
            table.lib = {folders: ["active", "hidden", "missing", "disconnected", "denied", "unavailable", "checking"].map(function(state, i) {
                return {id:i+1, displayName:"Folder " + i, path:"/music/" + i,
                    isNetwork:i===0, enabled:state!=="hidden", accessible:i<2,
                    status:state, lastScan:0}
            })}
            wait(0)
        }
        function test_status_text_never_overlaps_actions_at_wide_or_narrow_width() {
            for (const width of [1250, 560]) {
                table.width = width
                wait(0)
                for (var i=1; i<=7; i++) {
                    var status = findChild(table,"folderStatus-"+i)
                    var actions = findChild(table,"folderActions-"+i)
                    verify(status !== null)
                    verify(status.width >= status.implicitWidth)
                    verify(status.x >= actions.x + actions.width + 12)
                    compare(actions.children.length,4)
                    for (const button of actions.children)
                        verify(button.x >= 0 && button.x + button.width <= actions.width)
                }
            }
        }
        function test_all_statuses_have_explanations_and_failures_are_red() {
            for (var i=1; i<=7; i++) {
                const status = findChild(table,"folderStatus-"+i)
                verify(status.ToolTip.text.length > 20)
                verify(status.text.length > 0)
                if (i>=3 && i<=6) verify(status.color.r > status.color.g)
            }
            compare(findChild(table,"folderStatus-3").text,"Missing")
        }
        function test_folder_type_icons_have_tooltips() {
            const network = findChild(table,"folderType-1")
            const local = findChild(table,"folderType-2")
            compare(network.name,"server")
            compare(network.ToolTip.text,"Network folder")
            compare(local.name,"hard-drive")
            compare(local.ToolTip.text,"Local folder")
        }
        function test_browsed_path_only_prepares_add() {
            table.lib = {folders:[], picked_path:"/mnt/music"}
            compare(table.pendingPath,"/mnt/music")
            compare(QbzBridge.stringCalls.length,0)
        }
    }
}
