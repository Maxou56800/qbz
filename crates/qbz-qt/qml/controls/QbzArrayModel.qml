// QbzArrayModel — a DelegateModel over a JS array that a view can SWAP
// (2026-09-13). Assigning a fresh array straight to `ListView.model` after
// the view has completed has two side effects in Qt 6.11
// (qquickitemview.cpp, QQuickItemViewPrivate::connectModel):
//
//   1. `currentIndex` is forced to 0 and updateCurrent() calls
//      setFocus(true) on delegate 0. The view is a focus scope, so a search
//      box living in its header loses the keyboard on every rebuild — the
//      album track filter lost focus on each keystroke (qt.quick.focus
//      trace: "focus trackCell in scope pageFlick", activeFocusItem
//      TextInput -> trackCell). A `currentIndex: -1` on the view does not
//      survive the assignment.
//   2. setModel() clears the view and resets contentY to the content start.
//
// Swapping the array HERE instead reaches the view as remove+insert changes:
// `currentIndex: -1` stays honoured (nothing is ever focused) and the view
// keeps its rows. ListView re-seats the inserted rows around the viewport,
// which drifts the content origin, so the viewport is re-expressed relative
// to that origin (0 = the header's top edge) before the change and restored
// once it is laid out. A swap that replaces the page calls `scrollToTop()`.
//
// Probe that measured both behaviours against Qt 6.11.2:
// scripts/qml-tests/tst_array_model.qml.

import QtQuick
import QtQml.Models

DelegateModel {
    id: swap

    /// The rows. Replace the array freely; the view keeps its place.
    property var rows: []
    /// The ListView (or GridView) this model feeds — needed for the restore.
    property Item view: null

    model: rows

    property bool _live: false
    property int _rowsBefore: 0
    property real _offset: 0
    property int _epoch: 0
    property bool _toTop: false

    Component.onCompleted: {
        swap._live = true
        swap._rowsBefore = swap.count
    }

    onModelChanged: {
        if (!swap._live || !swap.view)
            return
        var had = swap._rowsBefore
        swap._rowsBefore = swap.count
        // Nothing to keep: the page had no rows (first document, or the
        // empty document a detail publishes while the next one loads), or a
        // page replacement already asked for the top.
        if (had === 0 || swap._toTop)
            return
        // Before the pending remove+insert is laid out: how far the viewport
        // sits below the content origin.
        swap._offset = swap.view.contentY - swap.view.originY
        var epoch = ++swap._epoch
        Qt.callLater(function () {
            if (epoch !== swap._epoch || !swap.view)
                return
            swap.view.forceLayout()
            var minY = swap.view.originY
            var maxY = minY + Math.max(0, swap.view.contentHeight - swap.view.height)
            swap.view.contentY = Math.max(minY, Math.min(minY + swap._offset, maxY))
        })
    }

    /// A swap that replaces the page (a different document): the viewport
    /// goes to the top once the new rows are laid out. Works whether it is
    /// called before or after the `rows` assignment in the same turn.
    function scrollToTop() {
        swap._toTop = true
        var epoch = ++swap._epoch
        Qt.callLater(function () {
            swap._toTop = false
            if (epoch !== swap._epoch || !swap.view)
                return
            swap.view.forceLayout()
            swap.view.positionViewAtBeginning()
        })
    }
}
