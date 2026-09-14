// QbzLineEdit — THE shared text input. Two arms over ONE control, so there
// is never a second search-box implementation in the tree:
//
//   * PLAIN (default, unchanged): 240x34 elevated r8 bordered input that
//     commits on Enter AND on focus loss (the Tauri onchange semantics —
//     PlaybackSettings.slint). Settings panels use this arm.
//
//   * SEARCH (`searchMode`): leading magnifier, LIVE `edited(text)` and a
//     trailing X. With `expandable` it is primitives/ExpandableSearch.slint
//     1:1 — a 34px (30px when `sm`) magnifier slot that, on click, opens a
//     RIGHT-ANCHORED field growing LEFT over whatever sits beside it (200ms
//     ease-in-out), the magnifier cross-fading out (120ms); focus is grabbed
//     one tick later because focusing synchronously inside the open races the
//     open animation (the Slint comment at ExpandableSearch.slint:26); the X
//     clears AND closes; Esc closes. The closed footprint is fixed, so
//     nothing to the control's right ever moves.
//
// Every per-view search box (Local Library toolbars, tree rail, folder
// detail, queue) is this control.

import QtQuick
import QtQuick.Window
import com.blitzfc.qbz
import "../theme"

Rectangle {
    // Explicit density opt-in; desktop geometry remains the default.
    property bool kioskHost: false

    id: root

    // --- plain arm (API unchanged) ---------------------------------------
    property string text: ""
    property string placeholder: ""
    property bool isPassword: false
    /// Fired on focus-loss AND on Enter — the "the value settled" signal.
    signal committed(string value)
    /// Fired on ENTER ONLY. A modal that submits on Enter must NOT listen to
    /// `committed`: closing the modal removes focus, which fires `committed`
    /// again and submits the same value a second time (a rename would run
    /// twice, a create would make two collections). Emitted BEFORE `committed`
    /// so a handler that closes the modal wins the race.
    signal accepted(string value)

    // --- search arm -------------------------------------------------------
    /// Leading magnifier + live `edited` + trailing X.
    property bool searchMode: false
    /// Collapse to the magnifier slot (implies searchMode semantics).
    property bool expandable: false
    /// Bootstrap-style small variant (ExpandableSearch `sm`): 30px, not 34px.
    property bool sm: false
    /// Open width of the expandable field (ExpandableSearch max-open-width).
    property int openWidth: 196
    /// surface-elevated fill (toolbar variant) vs surface-card (rail/overlay).
    property bool elevated: true
    property bool open: false
    signal edited(string value)

    QbzTheme { id: theme }

    readonly property int collapsedSize: sm ? 30 : 34
    readonly property int glyph: sm ? 13 : 14

    // The ROOT keeps the closed footprint; only the inner field animates, so
    // a positioner never reflows while the search opens.
    width: expandable ? collapsedSize : 240
    height: kioskHost ? 44 : ((searchMode || expandable) ? collapsedSize : 34)
    color: "transparent"
    activeFocusOnTab: root.enabled && root.expandable && !root.open

    Keys.onPressed: function (event) {
        if (root.enabled && root.expandable && !root.open
                && !event.isAutoRepeat
                && (event.key === Qt.Key_Space
                    || event.key === Qt.Key_Return
                    || event.key === Qt.Key_Enter)) {
            root.open = true
            event.accepted = true
        }
    }

    /// Set by `clearSearch()`, released the moment the CALLER publishes anything
    /// into `text`. While it is set, the focus-loss re-seed Binding below is
    /// suppressed.
    ///
    /// This is what replaces the `root.text = ""` this function used to do. That
    /// assignment was a JS write to a property the call site normally holds a
    /// BINDING on (`text: root.doc.search`, `text: root.query`, …), so the first
    /// click on the × destroyed the binding PERMANENTLY: Rust could never
    /// re-seed the field again, and the re-seed Binding below then wrote the
    /// now-frozen "" back into the input on every later focus loss. The X in
    /// MyQBZ's grid + detail toolbars, DiscoverBrowse and PlaylistBrowse all
    /// rode that. Clearing is now purely local — the input goes empty and
    /// `edited("")` tells the owner, which republishes "" through the binding
    /// that is still intact.
    ///
    /// The latch is needed because the republish is not synchronous: between the
    /// click and Rust's answer `root.text` still holds the OLD query, and a
    /// focus loss in that window (expandable's `closeSearch()` drops focus by
    /// design) would put it straight back.
    property bool _cleared: false
    onTextChanged: root._cleared = false

    function clearSearch() {
        input.text = ""
        root._cleared = true
        root.edited("")
    }
    function closeSearch() {
        clearSearch()
        root.open = false
        input.focus = false
    }
    /// Hand the keyboard back to the shell root (the walk the header search
    /// does); plain unfocus when no shell root is above (previews, tests).
    function blurToShell() {
        var p = root
        while (p.parent) {
            if (p.parent.isQbzShellRoot === true) {
                p.parent.forceActiveFocus()
                return
            }
            p = p.parent
        }
        input.focus = false
    }
    // FOCUS RETENTION (2026-09-13, every search box): a field that filters a
    // list on each keystroke sits above a surface that rebuilds on each
    // keystroke, and must not lose the keyboard to that rebuild. After an
    // edit, once the event loop settles, reclaim focus unless another text
    // input took it (a click elsewhere is a decision, a rebuilt list is not).
    property bool _reclaim: false
    function _reclaimFocus() {
        if (!root._reclaim)
            return
        root._reclaim = false
        if (input.activeFocus)
            return
        var win = root.Window.window
        var current = win ? win.activeFocusItem : null
        if (current instanceof TextInput || current instanceof TextEdit)
            return
        console.log("[QbzLineEdit] focus left the field after an edit; reclaimed (holder was "
                    + (current ? current.toString() : "none") + ")")
        input.forceActiveFocus()
    }

    // Immersive-port contract D16 (2026-08-02, additive one-liner): the ONE
    // read-only seam into the inner TextInput's focus. The immersive root
    // reads it as `textInputActive` (ImmersiveView.qml); the Shift+I /
    // seek-arrow gates it used to feed are GONE, replaced by the hotkeys
    // dispatcher's central text-input gate (2026-08-03 hotkeys-port §1.4.4)
    // — the input itself stays deliberately unexposed (:186). No behavior
    // change for existing consumers.
    readonly property bool fieldActive: input.activeFocus

    /// Focus the field from OUTSIDE the control — what a modal needs on open.
    ///
    /// The deferred Timer below already existed, but it was driven only by
    /// `onOpenChanged`, and `open` is the EXPANDABLE-SEARCH arm's property;
    /// the control exposes no alias to its inner TextInput either, so a modal
    /// had no way in. Setting `open: true` on a plain field happens to work
    /// (with `expandable: false`, `box.width` is `root.width` regardless of
    /// `open`, :139) but it repurposes a search-arm property and reads as a
    /// bug. This is the seam instead. It is deferred for the same reason the
    /// expandable arm defers: focusing synchronously inside the caller's own
    /// open transition races it.
    function focusField() { focusDefer.restart() }

    // Deferred focus — see the header note.
    onOpenChanged: if (open) focusDefer.restart()
    Timer {
        id: focusDefer
        interval: 30
        repeat: false
        onTriggered: {
            // A close/navigation can win the race with this deferred request.
            if (root.visible && root.enabled && (!root.expandable || root.open))
                input.forceActiveFocus()
        }
    }

    // --- Closed: the magnifier toggle (fades, so both directions are smooth)
    Rectangle {
        visible: root.expandable
        x: root.width - width
        width: root.collapsedSize
        height: root.collapsedSize
        radius: 6
        opacity: root.open ? 0.0 : 1.0
        Behavior on opacity { NumberAnimation { duration: 120 } }
        color: openArea.containsMouse ? theme.surfaceHover : "transparent"
        QbzIcon {
            name: "search"
            width: root.sm ? 14 : 16
            height: root.sm ? 14 : 16
            anchors.centerIn: parent
            tintName: openArea.containsMouse ? "textPrimary" : "muted"
        }
        MouseArea {
            id: openArea
            anchors.fill: parent
            enabled: !root.open
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.open = true
        }
    }

    // --- The field. Right-anchored, so an expandable one grows LEFT. ------
    Rectangle {
        id: box
        x: root.width - width
        width: root.expandable ? (root.open ? root.openWidth : 0) : root.width
        height: root.height
        radius: (root.searchMode || root.expandable) ? 6 : theme.radiusSm
        // Translucent under the app-wide dynamic background, on the SAME
        // token the opaque arm uses: surface-card for the expandable search
        // overlay (ExpandableSearch.slint:80), surface-elevated for the inline
        // toolbar field (BrowseHeaderTools.slint:26).
        color: root.elevated
            ? (theme.ambientOn ? theme.surfaceElevatedA50 : theme.surfaceElevated)
            : (theme.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard)
        border.width: (root.expandable && !root.open) ? 0 : 1
        border.color: input.activeFocus ? theme.accent : theme.borderSubtle
        // Load-bearing ONLY on the expandable arm, where `width` animates
        // 0 -> openWidth past a fixed 14px magnifier and the 22/24px clear
        // slot. On the plain arm the Row sums to exactly the box and the text
        // is guarded one level down, so the scissor there was a free batch
        // root on every mounted field.
        clip: root.expandable
        Behavior on width {
            enabled: root.expandable
            NumberAnimation { duration: 200; easing.type: Easing.InOutQuad }
        }

        Row {
            anchors.fill: parent
            anchors.leftMargin: root.searchMode ? (root.expandable ? 11 : 9) : 12
            anchors.rightMargin: root.searchMode ? 5 : 12
            spacing: root.searchMode ? 7 : 0

            QbzIcon {
                visible: root.searchMode
                name: "search"
                width: root.glyph
                height: root.glyph
                anchors.verticalCenter: parent.verticalCenter
                tintName: "muted"
            }
            Item {
                // Clamped: the open animation passes through width 0 and a
                // negative width is a silent layout trap.
                width: Math.max(0, parent.width
                    - (root.searchMode ? root.glyph + 7 : 0)
                    - (clearSlot.visible ? clearSlot.width + 7 : 0))
                height: parent.height
                // No clip: TextInput scissors itself (:207) and the
                // placeholder elides. A redundant second scissor.
                TextInput {
                    QbzTextEditMenu { }
                    id: input
                    anchors.fill: parent
                    color: theme.textPrimary
                    font.pixelSize: root.kioskHost ? (root.searchMode ? 12 : theme.fontBody) * 1.2 : (root.searchMode ? 12 : theme.fontBody)
                    verticalAlignment: Text.AlignVCenter
                    clip: true
                    selectByMouse: true
                    activeFocusOnTab: root.enabled && (!root.expandable || root.open)
                    echoMode: root.isPassword ? TextInput.Password : TextInput.Normal
                    text: root.text
                    onAccepted: { root.accepted(text); root.committed(text) }
                    onActiveFocusChanged: if (!activeFocus) root.committed(text)
                    onTextEdited: {
                        root.edited(text)
                        if (root.searchMode) {
                            root._reclaim = true
                            Qt.callLater(root._reclaimFocus)
                        }
                    }
                    // Escape on a search box: empty it AND hand the keyboard
                    // back (2026-09-13). Form fields keep propagating it.
                    Keys.onEscapePressed: function (event) {
                        if (root.expandable) {
                            root.closeSearch()
                            event.accepted = true
                        } else if (root.searchMode) {
                            root.clearSearch()
                            root.blurToShell()
                            event.accepted = true
                        } else {
                            event.accepted = false
                        }
                    }
                    // External republish (e.g. Reset) re-seeds the field while
                    // it is not being edited. `!_cleared` holds it off for the
                    // frames between the × and the owner's republish, when
                    // `root.text` is still the query the user just cleared —
                    // see `_cleared`.
                    //
                    // On the CHANGE, never on the focus loss. This was a
                    // `Binding { when: !input.activeFocus }`, which re-asserts
                    // whenever its condition flips — so blurring the field
                    // wrote `root.text` back over whatever had been typed. A
                    // SEARCH field never noticed: it fires `edited()` per
                    // keystroke, so the owner republishes and `root.text` is
                    // already what the user typed. A FORM field is the
                    // opposite — it reports on `committed()` and the owner
                    // holds the value until Save — so `root.text` is still the
                    // stored value (`""` for a password, which is never
                    // prefilled), and every blur ERASED the field. Measured in
                    // the media-server panel 2026-08-20: username and password
                    // wiped each other, so the form could not be filled by
                    // hand at all.
                    //
                    // A republish IS a change to `root.text`; a focus loss is
                    // not. Reacting to the change alone keeps the documented
                    // case and drops the destructive one.
                    Connections {
                        target: root
                        function onTextChanged() {
                            if (!input.activeFocus && !root._cleared)
                                input.text = root.text
                        }
                    }
                }
                Text {
                    visible: input.text === ""
                    anchors.fill: parent
                    text: root.placeholder
                    color: theme.textMuted
                    font.pixelSize: root.kioskHost ? (root.searchMode ? 12 : theme.fontBody) * 1.2 : (root.searchMode ? 12 : theme.fontBody)
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
            }
            // Trailing X — clears (plain search) or clears AND closes
            // (expandable, back to the magnifier).
            Item {
                id: clearSlot
                visible: root.searchMode && (root.expandable || input.text !== "")
                width: visible ? (root.sm ? 22 : 24) : 0
                height: parent.height
                Rectangle {
                    anchors.centerIn: parent
                    width: root.sm ? 22 : 24
                    height: root.sm ? 22 : 24
                    radius: 4
                    color: clearArea.containsMouse ? theme.surfaceHover : "transparent"
                    QbzIcon {
                        name: "x"
                        width: 12
                        height: 12
                        anchors.centerIn: parent
                        tintName: clearArea.containsMouse ? "textPrimary" : "muted"
                    }
                    MouseArea {
                        id: clearArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.expandable ? root.closeSearch() : root.clearSearch()
                    }
                }
            }
        }
    }
}
