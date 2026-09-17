// QbzLoadMore — THE shared "Load more" affordance, app-wide.
//
// Consolidates the FIVE hand-rolled copies this tree grew, in three visually
// distinct shapes. Every one of them is reproduced here PIXEL FOR PIXEL in its
// idle state; the arms are `bordered` and nothing else:
//   a. views/ArtistView.qml:1122-1147   — release section. Plain centred Text,
//      13px, 28 tall, hit box `implicitWidth + 24`, hover textPrimary /
//      textSecondary. NETWORK-BACKED (QbzArtist.loadReleaseSection).
//   b. views/ArtistView.qml:1596-1617   — Popular Tracks expander. Same plain
//      shape; the label toggles "Load more" / "View less" and the action is
//      CLIENT-SIDE (instant, no fetch) — so it passes `label` and never `busy`.
//   c. views/SearchView.qml:249-283     — `LoadMoreButton`: the BORDERED pill
//      (radiusSm, 32 tall, `implicitWidth + 36`, 1px borderSubtle, surfaceCard
//      idle / surfaceElevated hover) at y:6 inside a 44-tall box. Its label
//      carries the "n / total" counter, so the HOST owns the whole string.
//   d. views/LabelReleasesView.qml:269-296 — plain shape with a "Loading…"
//      arm and the MouseArea disarmed while loading (32 tall there).
//   e. views/LabelView.qml:549-583      — client-side 5 -> 20 -> 50 -> 5
//      reveal, plain shape, 28 tall.
//
// ── WHY THIS EXISTS (owner, 2026-08-02) ────────────────────────────────────
// "Los LoadMore dan una carga de golpe, se ve viejo, carguemos una o dos filas
//  skeleton loader debajo del boton y que la aparicion de lo que se cargue,
//  sea smooth."
// So: while a fetch is in flight, one row of card placeholders (or one/two row
// placeholders) sits UNDER the button. BELOW, not above — that is what the
// owner asked for, literally, and it is also the honest position: the page
// that is being fetched will be appended BELOW the content that is already on
// screen, and the button is the bottom of that content. (SearchView.qml:668
// already mounted such a block, but ABOVE its button; the consumer pass moves
// it. The 8000ms settle bound and the 224x270 pitch come from that site.)
//
// ── WHAT THIS CONTROL DOES *NOT* DO ────────────────────────────────────────
// The other half of "smooth" — the APPEARANCE of what landed — lives in the
// CONSUMERS. Only a host knows which of its delegates are new (it holds the
// index the page started at), so only a host can fade the appended delegates
// in. This control owns the placeholder and the button; it never touches the
// host's model.
// `visible` is likewise NOT decided here: every one of the five sites already
// gates its own instance (`section.hasMore`, `doc.hasMore`, `loaded < total`,
// `topTracks.length > preview`). The only visibility rule below is the
// skeleton block's own.
//
// ── COST ───────────────────────────────────────────────────────────────────
// The skeleton is the SHARED controls/QbzSkeleton.qml COMPOSITE, never a new
// primitive: ONE instance draws a whole row of cells on ONE animator (see that
// file's COST note). It is mounted through a Loader that is only `active`
// while the block is on screen, so an idle Load-more button holds no
// rectangles and no timer at all. `selfDrive: true` gives that one short-lived
// instance its own 900ms phase (there is no host phase to borrow — the button
// may be the only skeleton on the page), and `settleMs: 8000` bounds it, so a
// fetch that never returns cannot shimmer forever.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Column {
    id: root

    // --- API (four consumer views are written against these names) ---------
    /// false = the plain centred text (a, b, d, e); true = Search's pill (c).
    property bool bordered: false
    /// "" = the shared "Load more" msgid. NO new strings: the only labels this
    /// control and its hosts may use are the existing "Load more" / "Loading…"
    /// / "View less" msgids (all 8 locales already carry them).
    property string label: ""
    /// A fetch is in flight: the button is disarmed and the skeleton shows.
    property bool busy: false
    /// "" = the shared "Loading…" msgid. Plain arm only (see `_text`).
    property string busyLabel: ""
    /// "none" | "cards" | "rows".
    property string skeleton: "none"
    /// "cards": the HOST's grid pitch (SearchView.qml:673-674 = 224 x 270).
    property real cellW: 224
    property real cellH: 270
    /// "cards": ROUND placeholder cells — the artist grids' portrait
    /// footprint. The standalone block this control replaced on Search's
    /// Artists tab passed QbzSkeleton `roundCells: true`; without the
    /// pass-through its placeholders regressed to squares standing in for
    /// round portraits.
    property bool roundCells: false
    /// "cards": pitch minus this = the card drawn inside a cell. 24 is the
    /// album grids' gutter (224 - 200); Search's Artists grid packs 200-wide
    /// portraits on a 216 pitch, i.e. a 16px gutter, and deriving its cards
    /// at -24 drew them 8px small.
    property real cardGutter: 24
    /// "rows": row metrics (TrackRow is 50 tall, lists gap 6).
    property real rowH: 50
    property real rowGap: 6
    /// "rows": the art tile inside a row, when it is SMALLER than the row —
    /// QbzSkeleton's own knob (0 = square, full row height). TrackRow draws
    /// 36px art in its 50px row and the standalone block this replaced
    /// passed 36; dropping it made the placeholder art a 50px square that
    /// does not match what lands.
    property real rowArtSize: 0
    /// "rows": how many placeholder rows — the owner asked for "one or two".
    property int rowCount: 2
    /// 28 for the plain sites (a, b, e; d passes 32), 44 for the Search box (c).
    property real buttonHeight: 28

    signal clicked()

    // A host may override this; the default is what all five sites do (they
    // sit in a Column and span it). NOTE the Column-padding trap: a Column
    // with left/rightPadding does NOT narrow parent.width, so a host inside
    // one passes `width: parent.width - 64` by hand, exactly as
    // LabelReleasesView.qml:272 already does.
    width: parent ? parent.width : 0
    // No gap of our own: the "cards" block already carries cellH - cardH of
    // slack under its cells and the "rows" block a trailing rowGap, and the
    // button box carries its own vertical slack around a 13px label.
    spacing: 0

    QbzTheme { id: theme }

    // The bordered arm's label is host-supplied (it carries the "n / total"
    // counter), so it does NOT swap to a busy string — a host that wants one
    // passes it in `label`. The plain arm swaps, as LabelReleasesView:282-284.
    readonly property string _idleText: root.label !== ""
        ? root.label
        : QbzSession.tr("Load more", QbzSession.trRev)
    readonly property string _busyText: root.busyLabel !== ""
        ? root.busyLabel
        : QbzSession.tr("Loading…", QbzSession.trRev)
    readonly property string _text: (root.busy && !root.bordered)
        ? root._busyText
        : root._idleText

    // ---- the button, FIRST (the skeleton goes under it) -------------------
    Item {
        id: buttonBox
        width: root.width
        height: root.buttonHeight

        // -- plain arm (a, b, d, e) -----------------------------------------
        // ArtistView centres its hit box with two flexible spacer Items in a
        // Row; anchoring the box's horizontalCenter is the same result with
        // one item instead of three. The hit box is `implicitWidth + 24` and
        // as tall as the box, which is what a/d/e already do — b's MouseArea
        // was glued to the Text itself (no +24), so its hit area grows by
        // 12px a side; the idle pixels are identical (the Rectangle is
        // transparent) and the bigger target is the one the other four use.
        Rectangle {
            id: plainBtn
            visible: !root.bordered
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.verticalCenter: parent.verticalCenter
            width: plainText.implicitWidth + 24
            height: root.buttonHeight
            color: "transparent"
            Text {
                id: plainText
                anchors.centerIn: parent
                text: root._text
                color: plainArea.containsMouse ? theme.textPrimary : theme.textSecondary
                font.pixelSize: 13
            }
            MouseArea {
                id: plainArea
                anchors.fill: parent
                // Disarmed while the fetch is in flight — LabelReleasesView
                // .qml:291 already does this, and a disabled MouseArea also
                // stops reporting hover, so the label stays textSecondary.
                enabled: !root.busy
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.clicked()
            }
        }

        // -- bordered arm (c) — SearchView's pill, unharmonised --------------
        // Deliberately NOT folded into the plain arm: the two shapes stay two
        // shapes (radiusSm pill on surfaceCard vs bare text), only the plumbing
        // is shared.
        Rectangle {
            id: pillBtn
            visible: root.bordered
            anchors.horizontalCenter: parent.horizontalCenter
            y: 6
            width: pillText.implicitWidth + 36
            height: 32
            radius: theme.radiusSm
            color: pillArea.containsMouse ? theme.surfaceElevated : (theme.ambientOn ? theme.surfaceCardA50 : theme.surfaceCard)
            border.width: 1
            border.color: theme.borderSubtle
            Text {
                id: pillText
                anchors.centerIn: parent
                text: root._text
                color: theme.textSecondary
                font.pixelSize: 13
            }
            MouseArea {
                id: pillArea
                anchors.fill: parent
                enabled: !root.busy
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.clicked()
            }
        }
    }

    // ---- the skeleton, BELOW the button (see WHY THIS EXISTS) -------------
    Item {
        id: skelBlock
        width: root.width

        readonly property bool wanted: root.busy && root.skeleton !== "none"
        readonly property real blockHeight: root.skeleton === "cards"
            ? root.cellH                                        // exactly ONE row
            : root.rowCount * (root.rowH + root.rowGap)

        // Fade, never pop — 180ms is the duration the rest of the tree uses
        // (QbzSkeleton.qml:216, where it is what dissolves a placeholder into
        // the art). Height may snap; opacity may not. Staying `visible` while
        // the fade-out runs is what lets the exit be seen at all — `visible`
        // going false in the same frame would cut it.
        opacity: skelBlock.wanted ? 1.0 : 0.0
        Behavior on opacity { NumberAnimation { duration: 180 } }
        visible: skelBlock.wanted || skelBlock.opacity > 0.004
        // ZERO height when idle, so nothing on the page shifts.
        height: visible ? skelBlock.blockHeight : 0

        Loader {
            anchors.fill: parent
            // Nothing is instantiated for an idle button: no rectangles, no
            // timer (QbzSkeleton's own Loader would otherwise stay active,
            // since its gate is `opacity`, which does not inherit).
            active: skelBlock.visible
            sourceComponent: skelShape
        }

        Component {
            id: skelShape
            QbzSkeleton {
                variant: root.skeleton === "cards" ? "cardGrid" : "rowList"
                // "cards": the host passes the PITCH; the card drawn inside a
                // cell is the pitch minus the gutter, which is how SearchView
                // .qml:673-674 lands on QbzSkeleton's 200x246 defaults (224-24,
                // 270-24). A grid with a tighter gutter overrides `cardGutter`.
                cellW: root.cellW
                cellH: root.cellH
                cardW: Math.max(0, root.cellW - root.cardGutter)
                cardH: Math.max(0, root.cellH - root.cardGutter)
                roundCells: root.roundCells
                // "rows".
                rowH: root.rowH
                rowGap: root.rowGap
                rowArtSize: root.rowArtSize
                // One short-lived instance, its own 900ms phase: there is no
                // host phase to borrow here, and one animator is the whole
                // cost of the block.
                selfDrive: true
                // A fetch that never returns must not shimmer forever — the
                // same 8s bound SearchView.qml:676 uses on its appended-page
                // placeholder.
                settleMs: 8000
            }
        }
    }
}
