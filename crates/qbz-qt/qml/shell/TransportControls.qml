// TransportControls — the shared transport cluster of BOTH now-playing bars
// (TransportControls.slint): track-info · shuffle · prev · play · next ·
// repeat · "+" (Add to… flyout), plus the inline favorite heart in Classic.
// Extracted verbatim from PlayerBar.qml (phase 25 size split) and mounted by
// NowPlayingBarSmall too — its cluster was a byte-for-byte twin at
// playCircle:false / classicActions:false.
//
// 2px spacing on 32px buttons = the same 34px pitch as Tauri's 30px
// `.control-btn` + 4px gap. play-circle = the filled accent disc (New/Large);
// Classic/Small get the plain 34px glyph, 20px in BOTH modes exactly like
// Tauri's size={20}.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Row {
    id: tc

    property bool playCircle: true
    /// Classic ADDS the inline favorite toggle (Tauri parity).
    property bool classicActions: false
    /// Now-playing favorite state, for the Classic heart.
    property bool favorite: false
    /// The playing track is ephemeral — the heart has nothing to write to.
    property bool ephemeral: false
    /// AppShell's one shared hover-tooltip overlay.
    property var tooltip: null
    /// A host whose zone cannot fit the ±10 s pair (see PlayerBar's column
    /// budget) sets this; the pair then hides even with the pref on.
    property bool compact: false
    readonly property bool skipTenVisible: QbzPlayer.showSkipTen && !tc.compact

    /// Emitted by "+" with the button as anchor, so the flyout is owned
    /// by the bar (one menu, both mount points).
    signal addRequested(var anchorItem)
    /// Emitted by the (i) button — the bar owns the Track Info modal.
    signal trackInfoRequested()

    QbzTheme { id: theme }

    // Both are runtime-tinted tokens now (see QbzIconButton's header), so they
    // are the live colours rather than a two-value approximation of them.
    //
    // `tintOnAccent` is the glyph sitting ON the filled play disc, whose fill
    // is theme.accent (theme.accentHover under the cursor). It used to name
    // `accentText` directly; it goes through the shared on-accent selector
    // now. That is NOT a colour change on 34 of the 35 palettes — the
    // selector's first arm IS accent-text — the point is that PLAY, the
    // checkboxes, the genre pills and the circle actions all take one
    // decision. They did not before: CircleAction.slint:72 hardcodes #ffffff
    // for the same job while QbzCheckbox.slint:24 uses accent-text, and the
    // port inherited both spellings. See theme/QbzTheme.qml, "ON AN ACCENT
    // FILL", for why accent-text alone is not enough (rose-pine-dawn: 2.56:1)
    // and why the hover fill does not get a property of its own.
    readonly property string tintStrong: "textPrimary"
    readonly property string tintOnAccent: theme.accentGlyphTint

    spacing: 2
    height: 44

    // Track info — the first control, left of shuffle (1:1 with the
    // other views; it also restores PLAY's symmetric centring).
    QbzIconButton {
        name: "info"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: tc.trackInfoRequested()
    }
    QbzIconButton {
        name: "shuffle"
        active: QbzPlayer.npShuffle
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.toggleShuffle()
        tooltip: tc.tooltip
        tooltipKey: "npb-shuffle"
        tooltipText: QbzPlayer.npShuffle
            ? QbzSession.tr("Shuffle: On", QbzSession.trRev)
            : QbzSession.tr("Shuffle: Off", QbzSession.trRev)
    }
    // True while the player is resolving/fetching the track that is about to
    // play. Not gated on `npHasTrack`: the first play of a session has no
    // track yet, and that is exactly the case that felt dead.
    readonly property bool loading: QbzPlayer.npLoading
    // Clear-while-paused removes the NPB cursor by design. Rows added
    // afterwards still make Play actionable: the backend promotes the first
    // one without resurrecting the cleared paused stream.
    readonly property bool playEnabled: QbzPlayer.npHasTrack || QbzQueue.hasPlayTarget
    // Spin phase, advanced on the shared shell pulse (QbzShell.pulseMs).
    property real spinPhase: 0
    Connections {
        target: QbzShell
        function onPulseMsChanged() {
            if (tc.loading)
                tc.spinPhase = (tc.spinPhase + 0.42) % 6.283185
        }
    }

    QbzIconButton {
        visible: tc.skipTenVisible
        name: "skip-back-10"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.seekBy(-10)
        tooltip: tc.tooltip
        tooltipKey: "npb-skip-back-10"
        tooltipText: QbzSession.tr("Back 10 seconds", QbzSession.trRev)
    }
    QbzIconButton {
        name: "skip-back"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.previous()
    }
    Rectangle {
        width: tc.playCircle ? 38 : 34
        height: tc.playCircle ? 38 : 34
        radius: tc.playCircle ? width / 2 : theme.radiusSm
        anchors.verticalCenter: parent.verticalCenter
        opacity: tc.playEnabled ? 1.0 : 0.3
        color: tc.playCircle
            ? (playArea.containsMouse && tc.playEnabled ? theme.accentHover : theme.accent)
            : ((playArea.containsMouse && tc.playEnabled) ? theme.surfaceHover : "transparent")
        QbzIcon {
            // Hidden while loading — the ring below stands in for it, so the
            // button never shows a play glyph for a track that is still being
            // fetched.
            visible: !tc.loading
            name: QbzPlayer.npPlaying ? "pause" : "play-fill"
            width: 20
            height: 20
            anchors.centerIn: parent
            tintName: tc.playCircle ? tc.tintOnAccent
                : (playArea.containsMouse ? "accent" : tc.tintStrong)
        }
        // LOADING RING. Between the click and the first sample there was NO
        // affordance anywhere in the shell, so a fetch that takes seconds — a
        // Plex part, a cold CMAF session — reads as a dead click (owner report,
        // 2026-08-13). `npLoading` already existed and was published
        // (now_playing.rs) but the only consumer was SongCard's artwork
        // spinner, which is gated on `npHasTrack` and so shows nothing on the
        // first play of a session.
        //
        // ON THE SHELL PULSE, not a NumberAnimation: a rotation animator ticks
        // at display rate and every animation frame is a whole-window present
        // (CLAUDE.md, "the repaint pulse"). At ~30 Hz this costs zero extra
        // presents because the pulse is already running.
        Item {
            anchors.fill: parent
            visible: tc.loading
            Rectangle {
                id: ring
                anchors.centerIn: parent
                width: parent.width - 8
                height: width
                radius: width / 2
                color: "transparent"
                border.width: 2
                border.color: tc.playCircle ? theme.accentText : theme.accent
                // A 270-degree arc read: three quarters of the ring at full
                // alpha and the last quarter masked by the fill behind it is
                // not expressible on a Rectangle, so the spin is carried by
                // opacity pulsing instead — one property, no extra node.
                opacity: 0.35 + 0.45 * (1 + Math.sin(tc.spinPhase)) / 2
            }
        }
        MouseArea {
            id: playArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: tc.playEnabled ? Qt.PointingHandCursor : Qt.ArrowCursor
            onClicked: if (tc.playEnabled) QbzPlayer.togglePlay()
        }
    }
    QbzIconButton {
        name: "skip-forward"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.next()
    }
    QbzIconButton {
        visible: tc.skipTenVisible
        name: "skip-forward-10"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.seekBy(10)
        tooltip: tc.tooltip
        tooltipKey: "npb-skip-forward-10"
        tooltipText: QbzSession.tr("Forward 10 seconds", QbzSession.trRev)
    }
    QbzIconButton {
        name: QbzPlayer.npRepeatMode === 2 ? "repeat-1" : "repeat"
        active: QbzPlayer.npRepeatMode > 0
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: QbzPlayer.cycleRepeat()
        tooltip: tc.tooltip
        tooltipKey: "npb-repeat"
        tooltipText: QbzPlayer.npRepeatMode === 2
            ? QbzSession.tr("Repeat: One", QbzSession.trRev)
            : (QbzPlayer.npRepeatMode === 1
                ? QbzSession.tr("Repeat: All", QbzSession.trRev)
                : QbzSession.tr("Repeat: Off", QbzSession.trRev))
    }
    // "+" — Tauri's add-to-playlist button, grouped into the shared
    // "Add to…" flyout in 2.0.
    QbzIconButton {
        id: addBtn
        name: "plus"
        btnEnabled: QbzPlayer.npHasTrack
        anchors.verticalCenter: parent.verticalCenter
        onClicked: tc.addRequested(addBtn)
    }
    FavToggle {
        visible: tc.classicActions
        favorite: tc.favorite
        disabled: tc.ephemeral
        anchors.verticalCenter: parent.verticalCenter
    }
}
