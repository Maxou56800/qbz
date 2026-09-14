// QbzRowAdd — a row joining a QbzKeyedModel view fades in (a card also grows
// from a touch smaller) once its neighbours made room (2026-09-14). Assign
// it to the view's `add`, with `enabled: <model>.animate`; a scope reset
// never runs it.

import QtQuick

Transition {
    id: root
    property int duration: 200
    /// 1.0 for list rows; cards read better growing from slightly smaller.
    property real grow: 1.0

    NumberAnimation {
        property: "opacity"
        from: 0.0
        to: 1.0
        duration: root.duration
        easing.type: Easing.OutCubic
    }
    NumberAnimation {
        property: "scale"
        from: root.grow
        to: 1.0
        duration: root.duration
        easing.type: Easing.OutCubic
    }
}
