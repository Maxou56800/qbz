// QbzArrayModel: swapping the rows array must not rob the view's header of
// the keyboard (Qt 6.11 forces currentIndex 0 on a ListView.model assignment
// and focuses delegate 0), must keep the viewport where it was relative to
// the content origin, and scrollToTop() must land on the origin.
import QtQuick
import QtQml.Models
import Qt.labs.qmlmodels
import QtTest
import "../../crates/qbz-qt/qml/controls"

Item {
    id: root
    width: 600; height: 400
    property var cells: []
    function makeCells(n) {
        var out = []
        for (var i = 0; i < n; i++)
            out.push({ "kind": (i % 5 === 0) ? "track" : "gap", "i": i })
        return out
    }

    ListView {
        id: view
        width: 600; height: 400
        clip: true
        reuseItems: true
        currentIndex: -1
        cacheBuffer: 800
        model: QbzArrayModel { id: rowsModel; view: view; rows: root.cells }
        header: Item {
            width: 600; height: 300
            TextInput { objectName: "field"; width: 200; height: 30; y: 250; text: "x" }
        }
        delegate: DelegateChooser {
            role: "kind"
            DelegateChoice { roleValue: "gap"; delegate: Item { width: 600; height: 10 } }
            DelegateChoice {
                roleValue: "track"
                delegate: Item {
                    required property var modelData
                    width: 600; height: 10
                    Rectangle { width: 600; height: 50; color: "#333" }
                }
            }
        }
    }

    TestCase {
        name: "ArrayModel"
        when: windowShown
        function field() { return view.headerItem.children[0] }
        function settle() { wait(0); waitForRendering(view); wait(0) }

        function test_a_swap_keeps_header_focus_and_current_index() {
            root.cells = root.makeCells(200)
            settle()
            compare(view.count, 200)
            var input = field()
            input.forceActiveFocus()
            verify(input.activeFocus)
            root.cells = root.makeCells(150)
            settle()
            compare(view.count, 150)
            compare(view.currentIndex, -1)
            verify(input.activeFocus, "the header's field keeps the keyboard across a swap")
        }

        function test_b_swap_keeps_viewport_relative_to_origin() {
            root.cells = root.makeCells(200)
            settle()
            view.contentY = view.originY + 1000
            settle()
            var before = view.contentY - view.originY
            root.cells = root.makeCells(300)
            settle()
            fuzzyCompare(view.contentY - view.originY, before, 0.5)
            compare(view.count, 300)
        }

        function test_c_scroll_to_top_lands_on_the_origin() {
            root.cells = root.makeCells(200)
            settle()
            view.contentY = view.originY + 900
            settle()
            rowsModel.scrollToTop()
            root.cells = root.makeCells(120)
            settle()
            fuzzyCompare(view.contentY, view.originY, 0.5)
            compare(view.count, 120)
        }

        function test_d_first_rows_do_not_move_the_viewport() {
            root.cells = []
            settle()
            compare(view.count, 0)
            var y = view.contentY
            root.cells = root.makeCells(80)
            settle()
            compare(view.count, 80)
            fuzzyCompare(view.contentY, y, 0.5)
        }
    }
}
