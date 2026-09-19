// MiniWindowControls — the hover capsule (2026-08-03 miniplayer/tray contract
// A-28, §4.3.4), port of
// `crates/qbz-ui/ui/miniplayer/MiniWindowControls.slint` (184 L, read whole).
//
// A collapsed 26 px disc pinned to the RIGHT that unfolds LEFT into ten
// buttons: five surface tabs, a separator, then expand / pin / background /
// move / close. INLINE CHROME, NOT A POPUP — the reference's header says so at
// :1-4 and gives the reason: a popup surface would steal focus from a frameless
// always-on-top window.
//
// THE PIN IS NEW WORK, NOT A PORT (K1, §4.8, §13-D15). Three of the reference's
// own comments promise always-on-top — state.slint:4497, miniplayer.rs:170-171
// and this component's own header, which describes the cluster as
// "expand/pin/move/close" — and ZERO code implements it. The shipped Slint
// cluster has nine buttons; this one has ten, and the width arithmetic below
// carries the K1 column of §4.3.4's table (cluster 243, expanded 272), not the
// nine-button one (219 / 248).
//
// The trigger stays mounted while the mini is open. Leaving the card resets
// expansion without hiding the entry point to the window controls.
//
// THE WIDTH IS COMPUTED FROM CONSTANTS, NOT FROM THE CHILDREN (§8 rule 1).
// The reference reads `cluster.preferred-width` (:81); a QML port that reached
// for `cluster.implicitWidth` through a `parent` chain would be exactly the
// binding qmllint cannot verify. `clusterWidth` is spelled out with its
// arithmetic instead, and `implicitWidth`/`implicitHeight` are what the Loader
// sizes itself from.
//
// ONE HOVER SENSOR, NOT TEN. The reference ORs ten per-button `has-hover`
// flags (:58-61) because a Slint Rectangle has no whole-area hover. A single
// HoverHandler on the chrome reports for the item's whole area regardless of
// what is stacked above it (the Cortinilla.qml:215-222 / SuggestionsPanel.qml
// :316-325 precedent, both measured), which is the same behaviour minus nine
// bindings on an always-visible surface (§7-M2, §7-M7). The only difference is
// the 2-3 px gaps BETWEEN buttons, where the reference would start its 250 ms
// collapse and cancel it on the next motion.
//
// The drop shadow (:93-95, discretely switched on `expanded` — never animated)
// is dropped by the port's standing convention for Slint shadows
// (QbzToast.qml:30, QbzTooltip.qml:51-61, MyQbzModals.qml:23).

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root
    objectName: "miniWindowControls"

    /// The lit tab. Passed in rather than read off QbzMini here so the mount
    /// site owns the binding (:56, `active-surface`).
    property int activeSurface: 2
    /// The MINI window, handed down MiniWindow -> MiniShell -> MiniFooter ->
    /// here (§8 rule 3). `a-move` calls startSystemMove() on it; a
    /// parent.parent chain is what that rule exists to forbid.
    property var hostWindow: null

    QbzTheme { id: theme }

    // 5 tabs x 22 + 4 gaps x 2 = 118 · separator 1 · 5 actions x 22 + 4 x 2 =
    // 118 · two 3 px gaps in the outer row = 118 + 3 + 1 + 3 + 118.
    readonly property int clusterWidth: 243

    property bool windowHovered: true
    onWindowHoveredChanged: {
        if (!windowHovered) {
            collapseTimer.stop()
            expanded = false
        }
    }
    property bool expanded: false

    // A1 (§4.12): 220 ms, cubic-bezier(0.4, 0, 0.2, 1). The ONLY animated
    // geometry in the miniplayer.
    property real slotW: root.expanded ? root.clusterWidth : 0
    Behavior on slotW {
        NumberAnimation {
            duration: 220
            easing.type: Easing.Bezier
            easing.bezierCurve: [0.4, 0, 0.2, 1, 1, 1]
        }
    }

    // 2 left pad + animated slot + a DISCRETELY switched 3 px gap + the 22 px
    // trigger + 2 right pad (:88). Collapsed 26, expanded 272.
    implicitWidth: 2 + root.slotW + (root.expanded ? 3 : 0) + 22 + 2
    implicitHeight: 26

    radius: 13
    antialiasing: true
    color: Qt.rgba(theme.surfaceElevated.r, theme.surfaceElevated.g, theme.surfaceElevated.b, 1.0)
    border.width: 1
    border.color: theme.alphaTier(20)

    HoverHandler {
        onHoveredChanged: {
            if (hovered) {
                collapseTimer.stop()
                root.expanded = true
            } else {
                collapseTimer.restart()
            }
        }
    }

    // T1 (§4.12): the 250 ms collapse grace. A Timer is a QtObject and has NO
    // `parent` — an unqualified `parent` in here would resolve up and out of
    // the block (§8 rule 2, §15 trap 29), so it says `root`.
    Timer {
        id: collapseTimer
        interval: 250
        repeat: false
        onTriggered: root.expanded = false
    }

    // The cluster slot: clipped so the buttons slide out of a shrinking box
    // rather than being scaled.
    Item {
        id: slot
        x: 2
        y: Math.round((root.height - height) / 2)
        width: root.slotW
        height: 22
        clip: true
        // A2 (§4.12): 160 ms, default easing.
        opacity: root.expanded ? 1.0 : 0.0
        Behavior on opacity { NumberAnimation { duration: 160 } }

        Row {
            id: cluster
            height: 22
            spacing: 3

            // --- Group A: the five surface tabs -------------------------
            // QML writes the surface by ASSIGNMENT. There is NO setSurface
            // member on QbzMini — the property carries a custom WRITE whose
            // Rust name collides with the generated setter's, so the funnel
            // (persist + the surface-3 queue kick) is reachable only this way
            // (src/mini_bridge.rs:46-57).
            Row {
                height: 22
                spacing: 2

                CapBtn {
                    name: "minus"
                    active: root.activeSurface === 0
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.surface = 0
                }
                CapBtn {
                    name: "disc-3"
                    active: root.activeSurface === 1
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.surface = 1
                }
                CapBtn {
                    name: "image"
                    active: root.activeSurface === 2
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.surface = 2
                }
                CapBtn {
                    name: "list-music"
                    active: root.activeSurface === 3
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.surface = 3
                }
                CapBtn {
                    name: "align-start-vertical"
                    active: root.activeSurface === 4
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.surface = 4
                }
            }

            Rectangle {
                width: 1
                height: 12
                color: theme.alphaTier(15)
                anchors.verticalCenter: parent.verticalCenter
            }

            // --- Group B: the actions -----------------------------------
            Row {
                height: 22
                spacing: 2

                // `expand()` in the reference IS exit — `mp.on_expand(|| exit())`
                // (crates/qbz/src/miniplayer.rs:295).
                CapBtn {
                    name: "maximize-2"
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.exit()
                }
                // THE PIN, on Windows only (owner decision D8, 2026-08-27).
                // `Qt.WindowStaysOnTopHint` is HWND_TOPMOST there and works;
                // everything below is why it is absent everywhere else, and
                // that reasoning still stands unchanged.
                CapBtn {
                    name: "pin"
                    visible: QbzShell.isWindows
                    active: root.hostWindow ? root.hostWindow.pinnedOnTop : false
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: {
                        if (root.hostWindow)
                            root.hostWindow.pinnedOnTop = !root.hostWindow.pinnedOnTop
                    }
                }
                // NO PIN ELSEWHERE. Owner ruling K1 shipped always-on-top as NEW work
                // (the reference promises it in three comments and implements
                // it nowhere), and the owner smoked it on real Plasma Wayland
                // on 2026-08-04: `Qt.WindowStaysOnTopHint` did not keep the
                // window above others. Their call was explicit — "si no
                // podemos conseguir el keep on top, entonces mejor quitar el
                // botón" — so the button, the property, the invokable and the
                // pref are gone rather than left as a control that lies.
                //
                // The mechanism, so nobody re-adds it blind: on Wayland a
                // client cannot raise itself above other clients. There is no
                // "stay on top" in the core protocol; it needs a compositor
                // policy or a KWin window rule. Qt has nothing to send.
                CapBtn {
                    name: "layers"
                    active: QbzMini.backgroundBlur
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.toggleBackgroundBlur()
                }
                // PRESSED, NOT CLICKED (§15 trap 8): startSystemMove() hands
                // the pointer grab to the compositor, so the release never
                // comes back and a clicked handler would never fire.
                CapBtn {
                    name: "move"
                    iconSize: 12
                    dragCursor: true
                    anchors.verticalCenter: parent.verticalCenter
                    onPressed: {
                        if (root.hostWindow)
                            root.hostWindow.startSystemMove()
                    }
                }
                CapBtn {
                    name: "x"
                    danger: true
                    anchors.verticalCenter: parent.verticalCenter
                    onClicked: QbzMini.closeApp()
                }
            }
        }
    }

    // The trigger opens on hover or click. 22 x 22 overrides CapBtn's 22 x 20 so
    // the collapsed capsule is a circle inside the 26 px radius-13 chrome.
    CapBtn {
        objectName: "miniMenuTrigger"
        name: "square"
        idleOpacity: 1.0
        onClicked: {
            collapseTimer.stop()
            root.expanded = true
        }
        iconSize: 12
        width: 22
        height: 22
        x: root.width - 2 - 22
        y: Math.round((root.height - height) / 2)
    }
}
