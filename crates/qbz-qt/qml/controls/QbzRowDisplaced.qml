// QbzRowDisplaced — the neighbours of an inserted, removed or moved row slide
// to their new place (QbzKeyedModel, 2026-09-14). Assign it to a ListView or
// GridView's `displaced` and `move`, with `enabled: <model>.animate`.
//
// A row displaced while it was still fading in or out would otherwise keep
// that partial opacity or scale for good (Qt's ViewTransition notes), so the
// slide also settles both back to 1.
//
// Not a continuous animation: it runs once per structural change, for the
// rows actually on screen, and leaves nothing ticking (the repaint pulse
// rule in CLAUDE.md is about animations that keep presenting frames).

import QtQuick

Transition {
    id: root
    property int duration: 220

    NumberAnimation {
        properties: "x,y"
        duration: root.duration
        easing.type: Easing.OutCubic
    }
    NumberAnimation {
        property: "opacity"
        to: 1.0
        duration: root.duration
        easing.type: Easing.OutCubic
    }
    NumberAnimation {
        property: "scale"
        to: 1.0
        duration: root.duration
        easing.type: Easing.OutCubic
    }
}
