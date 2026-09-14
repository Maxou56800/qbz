// Excel-style multi-select regression suite: the shared selection rule
// (controls/SelectionModel.qml) and the REAL row, card, checkbox and rail
// components of the select-mode surfaces. Every click target a select-mode
// surface offers must deliver Shift (a range from the anchor) and Ctrl (one
// more row) to the selection; a target that swallows the modifier, or that
// navigates away instead of selecting, is exactly the regression this guards.
import QtQuick
import QtTest
import com.blitzfc.qbz
import "../../crates/qbz-qt/qml/controls"

Item {
    id: root
    width: 1200; height: 1400

    // ---- the selection rule itself ---------------------------------------------
    SelectionModel { id: rule }
    SelectionModel { id: keyedRule; idKey: "trackId" }
    TestCase {
        name: "SelectionRule"

        function init() {
            rule.anchorId = ""
            keyedRule.anchorId = ""
        }

        function test_numeric_ids_range() {
            var rows = [{ "id": 11 }, { "id": 12 }, { "id": 13 }, { "id": 14 }]
            var m = rule.next({}, 11, rows, Qt.NoModifier)
            m = rule.next(m, 13, rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "11,12,13")
        }

        function test_rows_without_id_are_skipped() {
            var rows = [{ "id": "a" }, { "kind": "header" }, { "id": "b" }]
            var m = rule.next({}, "a", rows, Qt.NoModifier)
            m = rule.next(m, "b", rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "a,b")
        }

        function test_id_key() {
            var rows = [{ "trackId": 7 }, { "trackId": 8 }, { "trackId": 9 }]
            var m = keyedRule.next({}, 9, rows, Qt.NoModifier)
            m = keyedRule.next(m, 7, rows, Qt.ShiftModifier)
            compare(Object.keys(m).sort().join(","), "7,8,9")
        }
    }
}
