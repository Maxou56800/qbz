// MiniTransport — the five-button transport row (2026-08-03 miniplayer/tray
// contract A-29, §4.3.3), port of `component MiniTransport inherits
// HorizontalLayout` at `crates/qbz-ui/ui/miniplayer/MiniFooter.slint:52-103`.
//
// Mounted THREE times with three different size sets: full (btn 30), compact
// (btn 26) and micro (btn 16) — which is why every dimension is a property and
// none is a literal.
//
// NO LOADING STATE ON PLAY (§12-P4). The reference mirrors
// `NowPlayingState.loading` into the mini (`crates/qbz/src/miniplayer.rs:559`)
// and never renders it; the main bar's spinner has no counterpart here. Do not
// add one.
//
// The repeat button reuses `icShuffle` for its glyph size — it has no size
// token of its own in the reference (:100), and that is not an oversight worth
// "fixing": shuffle and repeat are the two outer buttons and they match.

import QtQuick
import com.blitzfc.qbz

Row {
    id: root
    property bool lightForeground: false

    property int btn: 30
    property int icShuffle: 16
    property int icSkip: 18
    property int icPlay: 20
    property int gap: 18
    /// Gated on `NowPlayingState.has-track` by the footer (:315).
    property bool btnEnabled: true
    /// Play additionally accepts an idle, populated queue. The other four
    /// controls remain tied to a real current track.
    property bool playEnabled: root.btnEnabled

    spacing: root.gap

    TBtn {
        lightForeground: root.lightForeground
        name: "shuffle"
        iconSize: root.icShuffle
        btn: root.btn
        active: QbzPlayer.npShuffle
        btnEnabled: root.btnEnabled
        onClicked: QbzPlayer.toggleShuffle()
    }
    TBtn {
        lightForeground: root.lightForeground
        name: "skip-back"
        iconSize: root.icSkip
        btn: root.btn
        btnEnabled: root.btnEnabled
        onClicked: QbzPlayer.previous()
    }
    TBtn {
        lightForeground: root.lightForeground
        name: QbzPlayer.npPlaying ? "pause" : "play-fill"
        iconSize: root.icPlay
        btn: root.btn
        btnEnabled: root.playEnabled
        onClicked: QbzPlayer.togglePlay()
    }
    TBtn {
        lightForeground: root.lightForeground
        name: "skip-forward"
        iconSize: root.icSkip
        btn: root.btn
        btnEnabled: root.btnEnabled
        onClicked: QbzPlayer.next()
    }
    TBtn {
        lightForeground: root.lightForeground
        // 0 off · 1 all · 2 one (src/player_bridge.rs:52).
        name: QbzPlayer.npRepeatMode === 2 ? "repeat-1" : "repeat"
        iconSize: root.icShuffle
        btn: root.btn
        active: QbzPlayer.npRepeatMode !== 0
        btnEnabled: root.btnEnabled
        onClicked: QbzPlayer.cycleRepeat()
    }
}
