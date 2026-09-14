// ScrollMemory — per-page scroll-position memory for the back/forward stack.
//
// THE PORT of the block Slint repeats in 27 views (`property <string>
// sr-armed: NavState.restore-scope; changed viewport-height => { if
// (NavState.restore-scope == "<scope>" && ...) { viewport-y =
// NavState.scroll-restore; NavState.restore-scope = ""; } }` plus `changed
// viewport-y => NavState.report-scroll(...)`). Slint had to inline it because
// a `.slint` file cannot hand a component a reference to a Flickable; QML can,
// so this exists ONCE and every view mounts it beside its scroll container:
//
//     ScrollMemory { target: pageFlick; scope: "album" }
//
// Two halves, and they are independent:
//
//   REPORT — every `contentY` change is pushed to `QbzShell.reportScroll`,
//   which stores (scope, y) in nav_qt::LIVE. Nothing reads it until a
//   navigation stamps the page it is leaving, which is the whole point of the
//   design: the ~40 `record` call sites never learn that scroll memory exists.
//   This is a bare store per scroll frame — no notify, no binding, no
//   allocation once the page has reported once — and it is what Slint does.
//
//   RESTORE — `nav_qt::step` (back/forward) arms `QbzShell.restoreScope` with
//   the SCOPE the destination stored and `QbzShell.scrollRestore` with its
//   saved offset BEFORE it publishes `currentView`, so by the time the
//   router's Loader has built this view the arming is already on the bridge.
//
// ============================================================================
// WHY THE RESTORE RE-ASSERTS INSTEAD OF FIRING ONCE
// ============================================================================
//
// The first cut set `contentY` the moment the content was tall enough and then
// dropped the arming. That is not enough, and the reason is not specific to
// one view:
//
//   A Qt Quick item view RESETS contentY TO 0 WHENEVER ITS MODEL IS REPLACED
//   (`QQuickItemView::setModel`), and several of this app's pages assign their
//   model from a binding whose inputs arrive over the network AFTER the mount.
//
// The album page is the clean example. `AlbumView.qml:208` builds `listCells`
// from the track rows PLUS three deferred rails ("more from this artist",
// "suggestions", "similar albums"), each of which flips a loading flag and
// then lands its own document. So a freshly-mounted album page hands its
// ListView a NEW array one to four times in its first couple of seconds, and
// every one of those snaps the list back to the top — over a restore that had
// already run correctly.
//
// (The album page no longer resets: since 2026-09-13 its tape is swapped on a
// QbzArrayModel, which reaches the ListView as remove+insert changes and keeps
// the viewport — see that control for the Qt 6.11 focus theft that forced the
// move. Those swaps drift `originY` instead, which is what `relativeToOrigin`
// is for. The re-assert below stays for every page that still assigns
// `model` directly.)
//
// So the offset is held in `_wanted` and re-applied on every contentHeight
// change, every count change and every contentY move we did not make, until
// one of:
//
//   - `settleMs` elapses — the page never grew back that far (the album lost
//     tracks, a filter changed), so it lands on whatever is reachable and lets
//     go; or
//   - THE USER TAKES OVER. A wheel, a drag or a flick all raise `moving`, and
//     from that instant the position is theirs. This is the guard that keeps a
//     re-asserting restore from fighting the person using it.
//
// The shared scope is still consumed IMMEDIATELY on the first match, so two
// containers in one view can never both claim it; only this instance's private
// `_armed` latch outlives that.
//
// GATED ON `visible`. Tabbed views (Library, Local Library, Discover) mount
// more than one scroll container and only one of them holds the active tab's
// content; the others sit at contentY 0 with an empty model. Without the gate
// an inactive container's relayout would report 0 over the live page's real
// offset.
//
// GPU doctrine: nothing here animates. The restore is a handful of property
// assignments on one navigation, and at rest this component writes nothing.
//
// DIAGNOSTICS. Off by default (a LoggingCategory at Warning emits nothing at
// Info). One run says which link broke — the report, the arming or the apply:
//
//     QT_LOGGING_RULES="qbz.nav.scroll.info=true" ./target/release/qbz
//
// The Rust half of the same story is `log::debug!` under `[qbz-qt] scroll:` in
// nav_qt.rs (what got stamped, what got armed), i.e. `RUST_LOG=qbz=debug`.

import QtQuick
import com.blitzfc.qbz

Item {
    id: root

    /// The page's scroll container — a Flickable, ListView or GridView.
    property Flickable target: null
    /// This container's identity, chosen by the view and opaque to Rust: the
    /// route id for a single-container page ("album", "artist"), the route id
    /// plus the tab for a tabbed one ("library:albums", "local:tracks"). It is
    /// both what gets STORED with the offset and what `restoreScope` is
    /// matched against, so the two halves can never disagree — the only
    /// failure a typo can cause is a page that does not remember its position,
    /// never a page that jumps to somebody else's.
    ///
    /// TABBED VIEWS MUST INCLUDE THE TAB. Without it, leaving Library on
    /// Albums and coming back to a view that mounted on All would pour the
    /// Albums offset into the All feed. With it, a tab that does not match
    /// simply does not restore.
    ///
    /// An empty scope disables both halves — the opt-out for a container that
    /// is not the page (a sidebar rail, a popup list).
    property string scope: ""
    /// How long to keep re-asserting the offset against late model swaps
    /// before landing on what is reachable and letting go. 2.5 s covers the
    /// album page's three deferred rails on a slow connection; past that the
    /// page is not coming back to that height.
    property int settleMs: 2500
    /// Virtualized Kiosk rows can move ListView.originY as delegates settle.
    /// Store distance from that origin so a remount restores the same content.
    /// Desktop retains its existing absolute-coordinate contract by default.
    property bool relativeToOrigin: false

    // Zero-footprint: this is plumbing, never a visual.
    width: 0
    height: 0
    visible: false

    LoggingCategory {
        id: srLog
        name: "qbz.nav.scroll"
        defaultLogLevel: LoggingCategory.Warning
    }

    readonly property bool _live: root.target !== null && root.target.visible
    /// Set while this instance owns a restore in flight.
    property bool _armed: false
    /// The offset that restore is heading for.
    property real _wanted: 0

    function _origin() {
        return root.relativeToOrigin && root.target !== null ? root.target.originY : 0
    }
    function _report() {
        if (!root._live || root.scope === "")
            return
        QbzShell.reportScroll(root.scope,
                              root._armed ? root._wanted : root.target.contentY - root._origin())
    }

    /// Claim the arming if it is ours. Consumes the shared scope immediately
    /// (so a sibling container cannot also act on it) and moves the state into
    /// this instance's private latch.
    function _begin() {
        if (root.scope === "" || root.target === null)
            return
        if (QbzShell.restoreScope !== root.scope)
            return
        if (!root.target.visible)
            return
        var y = QbzShell.scrollRestore
        QbzShell.restoreScope = ""
        if (y <= 0)
            return
        root._wanted = y
        root._armed = true
        settle.restart()
        console.info(srLog, "[scroll] " + root.scope + " armed y=" + y
                     + " contentHeight=" + root.target.contentHeight
                     + " height=" + root.target.height)
        root._apply()
    }

    /// Re-assert the wanted offset if the content can hold it and something
    /// has moved it away. A no-op in the common case, which is what lets it be
    /// called from five signals.
    function _apply() {
        if (!root._armed || root.target === null || !root.target.visible)
            return
        if (root.target.contentHeight - root.target.height < root._wanted)
            return  // not tall enough yet — wait for the next growth
        var desiredY = root._origin() + root._wanted
        if (Math.abs(root.target.contentY - desiredY) < 0.5)
            return  // already there
        console.info(srLog, "[scroll] " + root.scope + " apply y=" + root._wanted
                     + " was=" + root.target.contentY)
        root.target.contentY = desiredY
    }

    function _disarm(why) {
        if (!root._armed)
            return
        root._armed = false
        settle.stop()
        console.info(srLog, "[scroll] " + root.scope + " disarm (" + why + ")")
    }

    /// The deadline: land on what is reachable and let go, so a page that
    /// never grows back to its old height cannot hold the latch forever.
    Timer {
        id: settle
        interval: root.settleMs
        repeat: false
        onTriggered: {
            if (root._armed && root.target !== null) {
                var reach = Math.max(0, root.target.contentHeight - root.target.height)
                var land = root._origin() + Math.min(root._wanted, reach)
                if (Math.abs(root.target.contentY - land) > 0.5)
                    root.target.contentY = land
            }
            root._disarm("deadline")
        }
    }

    Connections {
        target: root.target
        // A GridView/ListView carries every Flickable signal plus `count`; a
        // plain Flickable has no `countChanged`. The flag is what lets one
        // component serve both.
        ignoreUnknownSignals: true

        function onContentYChanged() {
            if (!root._live || root.scope === "")
                return
            // While a restore is in flight, report where the page is MEANT to
            // be. A model swap knocks contentY to 0 mid-window, and reporting
            // that would stamp 0 onto this entry if the user navigated away
            // before the re-assert landed.
            root._report()
            // The model-reset case, caught directly: contentY moved and it was
            // not us. `_apply` is idempotent — its equality guard stops the
            // write below from recursing.
            root._apply()
        }
        function onContentHeightChanged() { root._apply() }
        function onOriginYChanged() {
            if (root.relativeToOrigin) {
                root._report()
                root._apply()
            }
        }
        function onCountChanged() { root._apply() }
        function onVisibleChanged() { root._apply() }

        // USER TAKEOVER. A wheel, a drag or a flick all raise `moving`, and
        // from that moment the position belongs to the person, not to the
        // history. Without this the re-assert would yank the page back under
        // someone who started scrolling inside the settle window.
        function onMovingChanged() {
            if (root.target.moving)
                root._disarm("user took over")
        }
    }

    // The distance-based wheel animation writes contentY without setting
    // Flickable.moving. Yield before its first frame, just as for a native drag.
    Connections {
        target: root.target ? root.target.children.find(function (child) {
            return child.objectName === "qbzWheelScroll";
        }) || null : null
        function onUserScrollStarted() { root._disarm("wheel takeover"); }
    }

    Connections {
        target: QbzShell
        function onRestoreScopeChanged() { root._begin() }
    }

    // The view was built AFTER the arming (the normal back/forward path):
    // there is no property change left to react to, so the first attempt has
    // to be made here.
    Component.onCompleted: root._begin()
}
