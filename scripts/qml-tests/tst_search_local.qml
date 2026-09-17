import QtQuick
import QtTest
import "../../crates/qbz-qt/qml/views" as Views

Item {
    width: 900; height: 900
    Views.SearchLocalResults {
        id: results
        width: 850
        revision: "host-a-query-12"
        sections: [
            {kind:"local-album", hasMore:true, rows:[{kind:"album", title:"Album", flatIndex:0}]},
            {kind:"local-artist", hasMore:false, rows:[{kind:"artist", title:"Artist", flatIndex:1}]},
            {kind:"local", hasMore:true, rows:[
                {kind:"track", title:"Files", flatIndex:2},
                {kind:"track", title:"Plex", flatIndex:3},
                {kind:"track", title:"Jellyfin", flatIndex:4},
                {kind:"track", title:"Subsonic", flatIndex:5},
                {kind:"track", title:"Future source", flatIndex:6}
            ]}
        ]
    }
    SignalSpy { id: activated; target: results; signalName: "activated" }
    TestCase {
        name: "LocalSearchResults"; when: windowShown
        function init() { results.tab = 0; activated.clear(); verify(waitForRendering(results)) }
        function test_local_only_results_render_without_qobuz_rows() {
            verify(findChild(results, "localSearchSection-local-album").visible)
            verify(findChild(results, "localSearchSection-local-artist").visible)
            verify(findChild(results, "localSearchSection-local").visible)
            var future = findChild(results, "localSearchHit-6")
            verify(future !== null)
            mouseClick(future)
            compare(activated.count, 1)
            compare(activated.signalArguments[0][0], "host-a-query-12")
            compare(activated.signalArguments[0][1], 6)
            compare(activated.signalArguments[0][2], "play")
        }
        function test_category_tabs_keep_the_local_kind() {
            results.tab = 1
            verify(findChild(results, "localSearchSection-local-album").visible)
            verify(!findChild(results, "localSearchSection-local").visible)
            mouseClick(findChild(results, "localSearchHit-0"))
            compare(activated.signalArguments[0][0], "host-a-query-12")
            compare(activated.signalArguments[0][1], 0)
            compare(activated.signalArguments[0][2], "open")
            results.tab = 3
            verify(findChild(results, "localSearchSection-local-artist").visible)
            verify(!findChild(results, "localSearchSection-local-album").visible)
            results.tab = 4
            verify(!findChild(results, "localSearchSection-local-artist").visible)
            tryCompare(results, "height", 0)
        }
        function test_menu_actions_keep_the_same_source_snapshot() {
            mouseClick(findChild(results, "localSearchMenu-6"))
            var menu = findChild(results, "localSearchMenuLoader-6").item
            verify(menu !== null)
            tryCompare(menu, "opened", true)
            compare(menu.entries[1].action, "next")
            mouseClick(menu.contentItem, 80, 49)
            compare(activated.count, 1)
            compare(activated.signalArguments[0][0], "host-a-query-12")
            compare(activated.signalArguments[0][1], 6)
            compare(activated.signalArguments[0][2], "next")
            mouseClick(findChild(results, "localSearchHit-6"), 40, 30, Qt.RightButton)
            tryCompare(menu, "opened", true)
            compare(activated.count, 1, "right-click opens the menu without playing")
            menu.close()
        }
    }
}
