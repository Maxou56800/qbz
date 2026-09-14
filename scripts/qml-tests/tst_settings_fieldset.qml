// SettingsFieldset: the bordered, collapsible group that holds the options of
// ONE settings control (Appearance > Theme: the theme's editor, the Wallpaper
// background's image and blur). The host owns the persisted collapsed state;
// the fieldset shows or hides its rows and asks for the toggle.
import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/controls"

Item {
    id: root
    width: 700; height: 600

    property bool storedCollapsed: false
    property int toggles: 0

    Column {
        width: 600
        SettingsFieldset {
            id: fieldset
            title: "Custom"
            collapsed: root.storedCollapsed
            onToggleRequested: {
                root.toggles++
                root.storedCollapsed = !root.storedCollapsed
            }
            Rectangle { id: rowA; width: parent.width; height: 52; color: "transparent" }
            Rectangle { id: rowB; width: parent.width; height: 64; color: "transparent" }
        }
        Rectangle { id: below; width: 600; height: 20; color: "transparent" }
    }

    TestCase {
        name: "SettingsFieldset"
        when: windowShown

        function init() {
            root.storedCollapsed = false
            root.toggles = 0
            wait(0)
        }

        function test_expanded_shows_its_rows_inside_the_box() {
            verify(rowA.visible && rowB.visible)
            compare(rowA.width, fieldset.width - 2 * fieldset.inset, "rows take the inner width")
            var bottom = rowB.mapToItem(fieldset, 0, rowB.height).y
            verify(bottom <= fieldset.height, "the last row sits inside the border")
            compare(below.y, fieldset.height, "the next setting starts after the box")
        }

        function test_header_click_asks_for_the_toggle_and_folds_the_body() {
            var expandedHeight = fieldset.height
            mouseClick(fieldset, 60, 20)
            compare(root.toggles, 1)
            verify(fieldset.collapsed)
            verify(!rowA.visible && !rowB.visible, "a folded box hides its rows")
            verify(fieldset.height < expandedHeight)
            // Column lays out in the next polish pass.
            tryCompare(below, "y", fieldset.height, 1000, "the settings below move up")
            mouseClick(fieldset, 60, 20)
            compare(root.toggles, 2)
            verify(rowA.visible)
            compare(fieldset.height, expandedHeight)
        }

        function test_keyboard_toggles_from_the_focused_header() {
            mouseClick(fieldset, 60, 20)
            compare(root.toggles, 1)
            keyClick(Qt.Key_Space)
            compare(root.toggles, 2)
            keyClick(Qt.Key_Return)
            compare(root.toggles, 3)
        }

        function test_a_click_on_a_row_does_not_toggle() {
            var p = rowA.mapToItem(fieldset, 20, 20)
            mouseClick(fieldset, p.x, p.y)
            compare(root.toggles, 0)
        }
    }
}
