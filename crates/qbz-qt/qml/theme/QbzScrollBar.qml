// Custom vertical scrollbar — replica of primitives/ListScrollbar.slint.
// 14px gutter (not over the rows), 6px #ffffff12 track, 8px thumb
// (#ffffff44, widens to 10px / #ffffff77 on hover|drag), 48px min thumb,
// auto-hide 900ms after the last scroll. Attach as a sibling anchored to
// the right of a Flickable/GridView/ListView and set `target`.

import QtQuick

Item {
    id: root
    required property Flickable target
    /// Emitted only for a physical wheel/touchpad gesture or a press on this
    /// gutter, never when the target is positioned from code.
    signal userScrollStarted()

    width: 14
    visible: maxScroll > 0

    readonly property real maxScroll: Math.max(0, target.contentHeight - target.height)
    // ListView/GridView origins are not guaranteed to stay at zero when their
    // model or variable-size delegates change. Use the documented Flickable
    // coordinate (`contentY - originY`) for both drawing and seeking.
    readonly property real scrollY: Math.max(0, Math.min(maxScroll, target.contentY - target.originY))
    readonly property real thumbH: target.contentHeight > 0 ? Math.max(48, target.height / target.contentHeight * height) : height
    readonly property real travel: Math.max(0, height - thumbH)
    // Shown while scrolling, hovering, or dragging (auto-hide).
    property bool scrollActive: false
    /// Keep the track and thumb painted while there is anything to scroll
    /// (the shared text modal: a long biography must read as scrollable).
    property bool alwaysShown: false
    readonly property bool shown: alwaysShown || scrollActive || barArea.containsMouse || barArea.pressed

    Connections {
        target: root.target
        function onContentYChanged() {
            root.scrollActive = true;
            hideTimer.restart();
        }
    }
    Timer {
        id: hideTimer
        interval: 900
        onTriggered: root.scrollActive = false
    }

    // One shared input seam for every page that uses the house scrollbar:
    // wheel distance/easing and touchpad momentum share one input handler.
    QbzKineticScroll {
        id: wheelScroll
        target: root.target
        onUserScrollStarted: root.userScrollStarted()
    }

    // Track.
    Rectangle {
        width: 6
        x: Math.round((parent.width - width) / 2)
        height: parent.height
        radius: 3
        color: "#12ffffff"
        opacity: root.shown ? 1.0 : 0.0
        Behavior on opacity {
            NumberAnimation {
                duration: 160
            }
        }
    }
    // Thumb.
    Rectangle {
        width: barArea.containsMouse || barArea.pressed ? 10 : 8
        x: Math.round((parent.width - width) / 2)
        height: root.thumbH
        y: root.maxScroll > 0 ? (root.scrollY / root.maxScroll) * root.travel : 0
        radius: 5
        color: barArea.containsMouse || barArea.pressed ? "#77ffffff" : "#44ffffff"
        opacity: root.shown ? 1.0 : 0.0
        Behavior on width {
            NumberAnimation {
                duration: 100
            }
        }
        Behavior on opacity {
            NumberAnimation {
                duration: 160
            }
        }
    }
    // Drag / click-to-position over the whole gutter.
    MouseArea {
        id: barArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onPressed: {
            root.userScrollStarted();
            // Public Flickable semantics already cancel on a contentY write;
            // make the takeover explicit so scrollbar drag remains exact even
            // if the first pressed position equals the current one.
            wheelScroll.stopWheel();
            root.target.cancelFlick();
            position(mouseY);
        }
        onPositionChanged: if (pressed)
            position(mouseY)
        function position(my) {
            if (root.travel <= 0)
                return;
            var frac = Math.min(1, Math.max(0, (my - root.thumbH / 2) / root.travel));
            root.target.contentY = root.target.originY + frac * root.maxScroll;
        }
    }
}
