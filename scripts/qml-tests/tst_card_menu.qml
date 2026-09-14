// CardMenu submenus: hovering the child must keep BOTH menus open (the
// rows' MouseAreas must count as menu hover), moving to a plain row in the
// parent closes the child only, and leaving both closes both.
import QtQuick
import QtTest
import "../../crates/qbz-qt/qml/controls"

Item {
    id: root
    width: 700; height: 500

    Item { id: anchorItem; x: 120; y: 80; width: 10; height: 10 }

    CardMenu {
        id: menu
        entries: [
            { "label": "One", "icon": "", "action": "one" },
            { "label": "Copy", "icon": "", "action": "copy", "submenu": [
                { "label": "Sub A", "icon": "", "action": "sub-a" },
                { "label": "Sub B", "icon": "", "action": "sub-b" }
            ] },
            { "label": "Three", "icon": "", "action": "three" }
        ]
    }
    SignalSpy { id: picks; target: menu; signalName: "picked" }

    TestCase {
        name: "CardMenuSubmenu"
        when: windowShown
        // Row i's centre inside a menu (5px padding, 33px rows).
        function rowY(m, i) { return m.y + 5 + 33 * i + 16 }

        function test_a_hover_keeps_the_pair_open() {
            menu.openAtCursor(anchorItem, 0, 0)
            wait(50)
            verify(menu.opened)
            mouseMove(root, menu.x + 40, rowY(menu, 1))
            wait(120)
            verify(menu.subOpen, "hovering the Copy row opens the child")
            var sub = menu._sub
            verify(sub.opened)
            mouseMove(root, sub.x + 40, rowY(sub, 0))
            wait(600)
            verify(menu.opened, "the parent stays open while the child's first row is hovered")
            verify(sub.opened, "the child stays open while its first row is hovered")
            mouseMove(root, sub.x + 40, rowY(sub, 1))
            wait(600)
            verify(menu.opened && sub.opened, "still open on the child's second row")
            // Back onto a plain parent row: the child goes away, the parent stays.
            mouseMove(root, menu.x + 40, rowY(menu, 0))
            wait(120)
            verify(menu.opened)
            verify(!menu.subOpen, "a plain row puts the child away")
            // Leave both: both close.
            mouseMove(root, menu.x + 40, rowY(menu, 1))
            wait(120)
            verify(menu.subOpen)
            mouseMove(root, 650, 450)
            wait(700)
            verify(!menu.opened, "leaving both closes the parent")
        }

        function test_b_picking_in_the_child_closes_everything_and_forwards() {
            picks.clear()
            menu.openAtCursor(anchorItem, 0, 0)
            wait(50)
            mouseMove(root, menu.x + 40, rowY(menu, 1))
            wait(120)
            var sub = menu._sub
            verify(sub && sub.opened)
            mouseMove(root, sub.x + 40, rowY(sub, 1))
            wait(100)
            mouseClick(root, sub.x + 40, rowY(sub, 1))
            wait(100)
            compare(picks.count, 1)
            compare(picks.signalArguments[0][0], "sub-b")
            verify(!menu.opened)
            verify(!sub.opened)
        }
    }
}
