// QbzKeyedModel: rows reconcile into removes / inserts / moves / updates
// instead of a model swap. Guards the order and identity contract under
// random edits, the minimal-move reorder, the silent reset, a removal that
// keeps the viewport and the delegates, the fade that keeps its content, the
// pooled delegate that must come back opaque (76 blank rows, 2026-09-14), and
// the Repeater-in-a-Column shape the sidebar queue uses.
import QtQuick
import QtTest
import "../../crates/qbz-qt/qml/controls"

Item {
    id: root
    width: 400; height: 400

    property var countedRows: []
    property var fadeRows: []
    property var plainRows: []
    property int created: 0
    property int addRuns: 0
    property int removeRuns: 0
    property int moveSignals: 0

    function make(n, prefix) {
        var out = []
        for (var i = 0; i < n; i++)
            out.push({ "id": prefix + i, "title": "T" + prefix + i })
        return out
    }

    // --- counted transitions: the reset / structure tests ------------------
    QbzKeyedModel { id: counted; rows: root.countedRows; views: [countedView] }
    Connections {
        target: counted
        function onRowsMoved() { root.moveSignals++ }
    }
    ListView {
        id: countedView
        width: 200; height: 400
        model: counted
        reuseItems: true
        add: Transition {
            enabled: counted.animate
            ScriptAction { script: root.addRuns++ }
            NumberAnimation { property: "opacity"; from: 0; to: 1; duration: 60 }
        }
        remove: Transition {
            enabled: counted.animate
            ScriptAction { script: root.removeRuns++ }
            SequentialAnimation {
                NumberAnimation { property: "opacity"; to: 0; duration: 60 }
                PropertyAction { property: "opacity"; value: 1 }
            }
        }
        displaced: QbzRowDisplaced { enabled: counted.animate; duration: 60 }
        delegate: Item {
            required property string rowKey
            required property int rowRev
            readonly property var modelData: counted.row(rowKey, rowRev)
            width: 200; height: 20
            Component.onCompleted: root.created++
            ListView.onReused: { opacity = 1; scale = 1 }
        }
    }

    // --- the shipped transitions: fade / reuse tests -----------------------
    QbzKeyedModel { id: faded; rows: root.fadeRows; views: [fadeView] }
    ListView {
        id: fadeView
        x: 200
        width: 200; height: 400
        model: faded
        reuseItems: true
        remove: QbzRowRemove { enabled: faded.animate; duration: 240 }
        displaced: QbzRowDisplaced { enabled: faded.animate; duration: 240 }
        delegate: Item {
            required property string rowKey
            required property int rowRev
            readonly property var modelData: faded.row(rowKey, rowRev)
            readonly property string shown: modelData.title || ""
            width: 200; height: 20
        }
    }

    // --- no view: reconcile arithmetic -------------------------------------
    QbzKeyedModel { id: plain; rows: root.plainRows }

    // --- a Repeater in a Column (the sidebar queue shape) -------------------
    property var columnRows: []
    property int columnAdds: 0
    property int columnMoves: 0
    QbzKeyedModel { id: columnModel; rows: root.columnRows; views: [column] }
    Column {
        id: column
        x: 0; y: 0
        width: 200
        add: Transition {
            enabled: columnModel.animate
            ScriptAction { script: root.columnAdds++ }
            NumberAnimation { property: "opacity"; from: 0; to: 1; duration: 60 }
        }
        move: Transition {
            enabled: columnModel.animate
            ScriptAction { script: root.columnMoves++ }
            NumberAnimation { properties: "y"; duration: 120 }
        }
        Repeater {
            id: columnRepeater
            model: columnModel
            delegate: Rectangle {
                required property string rowKey
                required property int rowRev
                required property int index
                readonly property var modelData: columnModel.row(rowKey, rowRev)
                width: 200; height: 10
                color: "transparent"
            }
        }
    }

    TestCase {
        name: "KeyedModel"
        when: windowShown

        function settle(view) { wait(0); waitForRendering(view); wait(0) }
        function order(m) {
            var out = []
            for (var i = 0; i < m.count; i++)
                out.push(m.get(i).rowKey)
            return out
        }
        function topKey(view) {
            var it = view.itemAt(10, view.contentY + 1)
            return it ? it.rowKey : ""
        }
        function prng(seed) {
            var a = seed >>> 0
            return function () {
                a = (a + 0x6D2B79F5) >>> 0
                var t = a
                t = Math.imul(t ^ (t >>> 15), t | 1)
                t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
                return ((t ^ (t >>> 14)) >>> 0) / 4294967296
            }
        }

        function test_a_random_edits_reconcile_order_identity_and_revs() {
            var rnd = prng(20260914)
            var rows = root.make(60, "r")
            var serial = 60
            root.plainRows = rows
            plain.sync()
            for (var step = 0; step < 400; step++) {
                var next = rows.slice()
                var kind = Math.floor(rnd() * 6)
                if (kind === 0 && next.length > 0) {
                    next.splice(Math.floor(rnd() * next.length), 1 + Math.floor(rnd() * 3))
                } else if (kind === 1) {
                    var at = Math.floor(rnd() * (next.length + 1))
                    var burst = []
                    for (var b = 0; b < 1 + Math.floor(rnd() * 4); b++)
                        burst.push({ "id": "r" + (serial++), "title": "new" })
                    Array.prototype.splice.apply(next, [at, 0].concat(burst))
                } else if (kind === 2 && next.length > 1) {
                    var from = Math.floor(rnd() * next.length)
                    var moved = next.splice(from, 1)[0]
                    next.splice(Math.floor(rnd() * (next.length + 1)), 0, moved)
                } else if (kind === 3 && next.length > 0) {
                    // A repeat of an existing entity (the queue allows it).
                    var dup = next[Math.floor(rnd() * next.length)]
                    next.splice(Math.floor(rnd() * (next.length + 1)), 0,
                                { "id": dup.id, "title": dup.title })
                } else if (kind === 4 && next.length > 0) {
                    var ci = Math.floor(rnd() * next.length)
                    next[ci] = { "id": next[ci].id, "title": "changed " + step }
                } else {
                    // Same data, all-new objects (a republished document).
                    next = next.map(function (r) { return { "id": r.id, "title": r.title } })
                }
                var keys = plain.keysFor(next)
                var revsBefore = ({})
                for (var k = 0; k < plain.count; k++)
                    revsBefore[plain.get(k).rowKey] = plain.get(k).rowRev
                var before = ({})
                var prevKeys = plain.keysFor(rows)
                for (var p = 0; p < rows.length; p++)
                    before[prevKeys[p]] = rows[p]
                root.plainRows = next
                plain.sync()
                compare(order(plain), keys, "order after step " + step)
                for (var i = 0; i < next.length; i++) {
                    verify(plain.row(keys[i]) === next[i], "identity " + keys[i])
                    var prev = before[keys[i]]
                    if (prev !== undefined && revsBefore[keys[i]] !== undefined) {
                        var contentChanged = prev.title !== next[i].title
                        compare(plain.get(i).rowRev !== revsBefore[keys[i]], contentChanged,
                                "rev bump iff content changed: " + keys[i] + " step " + step)
                    }
                }
                rows = next
            }
        }

        function test_b_one_row_moved_far_is_one_move() {
            root.moveSignals = 0
            var rows = root.make(100, "m")
            root.countedRows = rows
            counted.scope = "moves"
            counted.sync()
            settle(countedView)
            root.moveSignals = 0
            var next = rows.slice()
            next.splice(90, 0, next.splice(3, 1)[0])
            root.countedRows = next
            counted.sync()
            compare(root.moveSignals, 1, "down")
            compare(order(counted), counted.keysFor(next))
            root.moveSignals = 0
            var back = next.slice()
            back.splice(3, 0, back.splice(90, 1)[0])
            root.countedRows = back
            counted.sync()
            compare(root.moveSignals, 1, "up")
            compare(order(counted), counted.keysFor(back))
        }

        function test_c_scope_change_resets_without_transitions_at_the_top() {
            root.countedRows = root.make(300, "s")
            counted.scope = "tab-a"
            counted.sync()
            settle(countedView)
            countedView.contentY = countedView.originY + 2000
            settle(countedView)
            var adds = root.addRuns
            var removes = root.removeRuns
            counted.scope = "tab-b"
            root.countedRows = root.make(120, "t")
            wait(0)
            settle(countedView)
            wait(150)
            compare(counted.count, 120)
            compare(root.addRuns, adds, "no add transition for a reset")
            compare(root.removeRuns, removes, "no remove transition for a reset")
            verify(countedView.atYBeginning, "a reset starts at the top")
            compare(topKey(countedView), "t0")
        }

        function test_c2_reset_from_empty_does_not_animate_either() {
            counted.scope = "empty"
            root.countedRows = []
            wait(0)
            settle(countedView)
            var adds = root.addRuns
            counted.scope = "filled"
            root.countedRows = root.make(50, "e")
            wait(0)
            settle(countedView)
            wait(150)
            compare(counted.count, 50)
            compare(root.addRuns, adds, "the first page of a scope appears at once")
        }

        function test_d_removal_keeps_the_viewport_and_the_delegates() {
            var rows = root.make(400, "v")
            root.countedRows = rows
            counted.scope = "viewport"
            counted.sync()
            settle(countedView)
            countedView.contentY = countedView.originY + 1000
            settle(countedView)
            var top = topKey(countedView)
            compare(top, "v50")
            var made = root.created
            var removes = root.removeRuns
            // Ten rows above the viewport and one inside it leave.
            root.countedRows = rows.filter(function (r, i) { return i >= 10 && i !== 55 })
            counted.sync()
            settle(countedView)
            wait(150)
            compare(topKey(countedView), top, "the row at the top stays at the top")
            verify(root.created - made <= 2, "no rebuild: " + (root.created - made) + " delegates created")
            verify(root.removeRuns > removes, "the visible row animated out")
        }

        function test_e_removed_row_fades_out_with_its_content() {
            root.fadeRows = root.make(40, "f")
            faded.scope = "fade"
            faded.sync()
            settle(fadeView)
            root.fadeRows = root.make(40, "f").filter(function (r) { return r.id !== "f5" })
            faded.sync()
            wait(80)
            var ghost = null
            for (var i = 0; i < fadeView.contentItem.children.length; i++) {
                var ch = fadeView.contentItem.children[i]
                if (ch.rowKey === "f5" && ch.opacity > 0.05 && ch.opacity < 0.99)
                    ghost = ch
            }
            verify(ghost !== null, "the leaving row is mid-fade")
            compare(ghost.shown, "Tf5", "and still shows its own content")
            wait(400)
        }

        function test_f_pooled_rows_come_back_opaque() {
            var rows = root.make(40, "p")
            root.fadeRows = rows
            faded.scope = "reuse"
            faded.sync()
            settle(fadeView)
            for (var pass = 0; pass < 3; pass++) {
                rows = rows.filter(function (r, i) { return i % 7 !== 3 })
                root.fadeRows = rows
                faded.sync()
                wait(400)
            }
            root.fadeRows = rows.concat(root.make(300, "q"))
            faded.sync()
            settle(fadeView)
            var blank = []
            for (var y = 0; y < 5000; y += 150) {
                fadeView.contentY = fadeView.originY + y
                wait(20)
                for (var c = 0; c < fadeView.contentItem.children.length; c++) {
                    var it = fadeView.contentItem.children[c]
                    if (it.rowKey === undefined || it.index === -1)
                        continue
                    if (fadeView.itemAtIndex(faded.indexOfKey(it.rowKey)) !== it)
                        continue
                    if (it.opacity < 0.99)
                        blank.push(it.rowKey)
                }
            }
            compare(blank.length, 0, "rows reused from the pool are opaque: " + blank.slice(0, 8).join(" "))
        }

        function test_g_paused_holds_rows_until_it_clears() {
            root.plainRows = root.make(5, "h")
            plain.scope = "paused"
            plain.sync()
            plain.paused = true
            root.plainRows = root.make(8, "h")
            plain.sync()
            compare(plain.count, 5)
            plain.paused = false
            wait(0)
            compare(plain.count, 8)
        }

        function test_h_ignored_keys_do_not_count_as_a_change() {
            plain.ignoredKeys = ["_order"]
            root.plainRows = [{ "id": "a", "title": "A", "_order": 0 }, { "id": "b", "title": "B", "_order": 1 }]
            plain.scope = "ignored"
            plain.sync()
            var revA = plain.get(0).rowRev
            root.plainRows = [{ "id": "a", "title": "A", "_order": 5 }, { "id": "b", "title": "B", "_order": 6 }]
            plain.sync()
            compare(plain.get(0).rowRev, revA)
            root.plainRows = [{ "id": "a", "title": "A2", "_order": 5 }, { "id": "b", "title": "B", "_order": 6 }]
            plain.sync()
            verify(plain.get(0).rowRev !== revA)
            plain.ignoredKeys = []
        }

        function test_j_column_repeater_resets_silently_and_slides_on_removal() {
            columnModel.scope = "page-1"
            root.columnRows = root.make(12, "c")
            wait(0)
            wait(150)
            compare(columnRepeater.count, 12)
            compare(root.columnAdds, 0, "the first page is not animated in")
            var survivor = columnRepeater.itemAt(4)
            // One row leaves: the rows below slide up, nothing is re-created.
            root.columnRows = root.make(12, "c").filter(function (r) { return r.id !== "c2" })
            columnModel.sync()
            wait(30)
            verify(root.columnMoves > 0, "the rows below slide")
            wait(200)
            compare(columnRepeater.itemAt(3), survivor, "the same row item, one slot up")
            compare(survivor.y, 30)
            // A row joins at the top: it fades in.
            var adds = root.columnAdds
            root.columnRows = [{ "id": "new", "title": "N" }].concat(root.columnRows)
            columnModel.sync()
            wait(150)
            verify(root.columnAdds > adds, "the new row fades in")
            // Another page replaces the list without animation.
            adds = root.columnAdds
            columnModel.scope = "page-2"
            root.columnRows = root.make(8, "d")
            wait(0)
            wait(150)
            compare(columnRepeater.count, 8)
            compare(root.columnAdds, adds, "a page change is not animated")
        }

        function test_i_repeated_entities_are_distinct_rows() {
            root.plainRows = [{ "id": "x" }, { "id": "y" }, { "id": "x" }]
            plain.scope = "dups"
            plain.sync()
            compare(order(plain), ["x", "y", "x#1"])
            root.plainRows = [{ "id": "y" }, { "id": "x" }]
            plain.sync()
            compare(order(plain), ["y", "x"])
        }
    }
}
