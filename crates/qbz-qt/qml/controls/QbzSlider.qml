// QbzSlider (primitives/QbzSlider.slint) — extracted from SettingsView.qml
// in phase 19. 200x22, 4px r2 track, accent fill, 16px thumb; integer
// steps. Like the Slint original the thumb follows the pointer during a
// drag (local dragValue) and commits each step via changed(int).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Item {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    id: root

    property int minimum: 0
    property int maximum: 10
    property int value: 0
    signal changed(int newValue)
    /// Fired ONCE on drag-end with the settled value. `changed` fires on every
    /// drag tick, so anything that PERSISTS must listen here instead — the
    /// volume slider writes ui_prefs.json, a document shared with the running
    /// Slint app through a whole-file read-modify-write, and a write per pixel
    /// of drag is exactly what `PlayerBar.slint:864-866` avoids ("Persist only
    /// the final value on drag-end").
    signal released(int newValue)

    QbzTheme { id: theme }

    width: 200
    height: kioskHost ? 44 : 22
    // Disabled = inert (the bit-perfect ALSA direct path locks volume).
    // `enabled` already disarms the MouseArea; this is the matching dim.
    opacity: enabled ? 1.0 : 0.3
    activeFocusOnTab: enabled
    Accessible.role: Accessible.Slider

    Rectangle {
        anchors.fill: parent
        anchors.margins: -3
        radius: theme.radiusSm
        color: "transparent"
        border.width: root.activeFocus ? 2 : 0
        border.color: theme.accent
    }

    readonly property int thumbSize: 16
    readonly property real travel: width - thumbSize
    readonly property real fraction: maximum > minimum
        ? Math.max(0, Math.min(1, (value - minimum) / (maximum - minimum))) : 0
    property bool dragging: false
    property real dragFraction: fraction
    readonly property real shownFraction: dragging ? dragFraction : fraction
    onFractionChanged: if (!dragging) dragFraction = fraction

    function commit(frac) {
        const v = Math.round(minimum + Math.max(0, Math.min(1, frac)) * (maximum - minimum))
        if (v !== value) changed(v)
    }

    function keyboardCommit(nextValue) {
        const v = Math.max(minimum, Math.min(maximum, nextValue))
        if (v !== value)
            changed(v)
        released(v)
    }

    Keys.onPressed: function (event) {
        if (!root.enabled || event.isAutoRepeat)
            return
        if (event.key === Qt.Key_Left || event.key === Qt.Key_Down)
            root.keyboardCommit(root.value - 1)
        else if (event.key === Qt.Key_Right || event.key === Qt.Key_Up)
            root.keyboardCommit(root.value + 1)
        else if (event.key === Qt.Key_Home)
            root.keyboardCommit(root.minimum)
        else if (event.key === Qt.Key_End)
            root.keyboardCommit(root.maximum)
        else
            return
        event.accepted = true
    }

    Rectangle { // track
        x: 0
        y: Math.round((parent.height - height) / 2)
        width: parent.width
        height: 4
        radius: 2
        color: theme.surfaceElevated
    }
    Rectangle { // accent fill
        x: 0
        y: Math.round((parent.height - height) / 2)
        width: parent.thumbSize / 2 + parent.shownFraction * parent.travel
        height: 4
        radius: 2
        color: theme.accent
    }
    Rectangle { // thumb
        width: parent.thumbSize
        height: parent.thumbSize
        radius: parent.thumbSize / 2
        x: parent.shownFraction * parent.travel
        anchors.verticalCenter: parent.verticalCenter
        color: theme.textPrimary
    }
    MouseArea {
        anchors.fill: parent
        cursorShape: Qt.PointingHandCursor
        onPressed: function (mouse) {
            root.forceActiveFocus()
            root.dragging = true
            root.dragFraction = Math.max(0, Math.min(1, (mouse.x - root.thumbSize / 2) / root.travel))
            root.commit(root.dragFraction)
        }
        onPositionChanged: function (mouse) {
            if (pressed) {
                root.dragFraction = Math.max(0, Math.min(1, (mouse.x - root.thumbSize / 2) / root.travel))
                root.commit(root.dragFraction)
            }
        }
        onCanceled: root.dragging = false
        onReleased: {
            root.dragging = false
            root.released(root.value)
        }
    }
}
