// QbzRowRemove — a row leaving a QbzKeyedModel view fades out, a card also
// shrinks a touch (2026-09-14). Assign it to the view's `remove`, with
// `enabled: <model>.animate`. The view keeps the delegate alive until this
// ends; its content stays because a removed row's roles are not re-notified.
//
// THE LAST STEP PUTS OPACITY AND SCALE BACK. With `reuseItems` the view pools
// the delegate the moment this transition finishes (QQuickItemViewPrivate::
// viewItemTransitionFinished -> releaseItem -> setVisible(false), all in one
// call), and a pooled delegate is reused as it was left: without the restore
// the next row it renders is invisible. Measured 2026-09-14: 76 blank rows
// after three removals and a scroll. The restore runs before the release, in
// the same call chain, so it never paints.

import QtQuick

Transition {
    id: root
    property int duration: 180
    /// 1.0 for list rows; cards read better with a slight shrink.
    property real shrink: 1.0

    SequentialAnimation {
        ParallelAnimation {
            NumberAnimation {
                property: "opacity"
                to: 0.0
                duration: root.duration
                easing.type: Easing.OutCubic
            }
            NumberAnimation {
                property: "scale"
                to: root.shrink
                duration: root.duration
                easing.type: Easing.OutCubic
            }
        }
        PropertyAction { property: "opacity"; value: 1.0 }
        PropertyAction { property: "scale"; value: 1.0 }
    }
}
