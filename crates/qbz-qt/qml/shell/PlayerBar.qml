// PlayerBar — the full 112px now-playing bar for modes 0 (New), 1
// (Classic) and 3 (Large) — QML port of crates/qbz-ui/ui/shell/
// PlayerBar.slint (phase 18). Mode 2 (Small) mounts NowPlayingBarSmall
// instead (the NowPlayingBar seam picks the body).
//
// Composition (1:1): a full-width SeekBar with times (6px margins; in
// Large the left edge insets by the sidebar width so the seek arm clears
// the docked cover), then the controls row in three responsive symmetric
// columns (the side columns share a fraction so PLAY lands on the window
// centre at every width):
//   width <  1366  -> 39% | 22% | 39%
//   1366..<1920    -> 30% | 40% | 30%
//   width >= 1920  -> 25% | 50% | 25%
// Classic uses a fixed 32.4/35.2/32.4 split (col-side 0.324).
//
//   New (0):     LEFT SongCard · CENTER transport (filled disc) · RIGHT cluster
//   Classic (1): LEFT transport · elastic glass SongCard · RIGHT cluster
//   Large (3):   LEFT SongCard WITHOUT the cover (it lives in the sidebar
//                dock, shifted right to clear it) · CENTER transport ·
//                RIGHT inline AudioStamp + cluster.
//
// QUALITY STACK (which bar mounts which component — SongCard.slint /
// PlayerBar.slint / PlayerBarSmall.slint):
//   New (0), Classic (1) -> shell/SongCardStamp.qml, mounted INSIDE the song
//                           card: QualityBadgeFull (icon + stacked
//                           tier/detail) over the dot LEDs.
//   Large (3), Small (2) -> shell/AudioStamp.qml, the inline 2-row stamp.
// They are DIFFERENT components in Slint and they stay different here.
//
// BUTTON SET (phase 24 — 1:1 with the Tauri NowPlayingBar.svelte, via the
// Slint TransportControls/PlayerBar that descend from it):
//   transport: info · shuffle · prev · play · next · repeat · "+" (Add to…)
//              (+ the inline favorite heart in Classic, like Tauri)
//   right:     Connect · Cast · Lyrics · Now-Playing view · Audio settings
//              (normalization) · [6px] · Mute · slider · −/+ steppers ·
//              [6px] · Queue
// Connect BEFORE Cast is the reference's order, not a preference: PlayerBar.
// slint mounts the ConnectButton at :395-401 and the cast IconButton at
// :646-653. This file had the pair inverted (Cast first) until the order was
// homologated; NowPlayingBarSmall.qml always matched (PlayerBarSmall.slint
// ConnectButton :368, cast :603).
// Tauri's Miniplayer + Full screen buttons live inside the Now-Playing view
// flyout (ViewModeMenu) since 2.0 — that ONE button holds both.
//
// Wired: transport (shuffle/prev/play/next/repeat), the add flyout
// (library / queue / play next / play later / album), seek, volume + mute +
// steppers, lyrics, queue, the Now-Playing-view flyout (npbSetMode),
// normalization, open album/artist via the SongCard meta links, and Track
// Info — the (i) button and the song-card title open shell/TrackInfoModal.qml
// (needs the QbzAlbum.openTrackInfo glue; see that file's header). Cast opens
// shell/CastPicker.qml (QbzCast — discovery, connect/disconnect, the
// per-device quality cap) and lights while a renderer is connected. Qobuz
// Connect is LIVE: the golden ConnectButton opens shell/QconnectFlyout.qml
// (QbzQConnect — device list, set-active, connect/disconnect) and carries
// the "Qobuz Connect: On/Off" hover bubble via the shell's tooltip overlay.
// Add-to-playlist and add-to-mixtape are live through the two app-wide
// pickers (QbzPlaylistPicker / QbzMyQbzAdd).
//
// SIZE (project rule): the inline SongCard / TransportControls / FavToggle
// components moved out to shell/SongCard.qml, shell/TransportControls.qml and
// shell/FavToggle.qml in phase 25 — TransportControls is now shared with the
// Small bar instead of being duplicated there.

import QtQuick
import QtQuick.Controls
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    // No background of its own: the chrome band is NowPlayingBar.qml's root
    // (surface-card @ 0.5 under the ambient background, opaque surface-card
    // otherwise), full width behind this inset layout — see THE BAND there.
    color: "transparent"
    readonly property bool ambientOn: theme.ambientOn
    readonly property bool largeActive: QbzShell.npbMode === 3 && QbzShell.sidebarState === 0
    readonly property bool isClassic: QbzShell.npbMode === 1
    // Feature parked for a later release. The renderer and preference remain
    // intact; every full-height NPB uses the classic seekbar for now.
    readonly property bool waveformVisible: false

    // The shell's shared hover-tooltip overlay (controls/QbzTooltip.qml),
    // fed in through NowPlayingBar.qml's Binding. Consumed by the dynamic
    // Shuffle/Repeat state bubbles and by the Qobuz Connect button's
    // "Qobuz Connect: On/Off" bubble.
    property Item tooltip: null

    // Volume lock, both halves (contract §11.3 = PlayerBar.slint:152-158's
    // `vol-locked`): npVolumeLocked carries the ALSA-Direct hw derivation
    // INSIDE it, and that local bit-perfect lock is LIFTED when a peer owns
    // playback (the `&& !npIsRemote` term) so the user CAN adjust the remote
    // renderer; npRemoteVolumeLocked is the sink-pushed "peer disallows
    // remote volume" half. The natural misread (`!npVolumeLocked &&
    // !npIsRemote && ...`) would disable the slider for EVERY peer, breaking
    // §7 volume routing to volume-ALLOWING peers.
    readonly property bool volLocked:
        (QbzPlayer.npVolumeLocked && !QbzPlayer.npIsRemote)
        || QbzPlayer.npRemoteVolumeLocked

    QbzTheme { id: theme }

    // Responsive zones (PlayerBar.slint). Classic fixes col-side at 0.324;
    // New/Large use the width-driven side fraction. Both keep the two side
    // columns EQUAL so the centre column stays dead centre. The breakpoints
    // are the SHARED theme token (QbzTheme.npbSideFrac) — the Small bar reads
    // the same one, so the two bars cannot drift apart.
    // `root.width` is the window width minus the two side gutters
    // (NowPlayingBar.qml `gutter`): the bar is anchored left-to-right on the shell
    // root, exactly like PlayerBar.slint reads its own root.
    property real sideFrac: theme.npbSideFrac(root.width)
    property real colSide: isClassic ? 0.324 : sideFrac
    property real colCentre: 1.0 - 2.0 * colSide
    // The docked-cover width the Large seek arm + song card must clear
    // (sidebar rendered width; 240 when open).
    readonly property int dockWidth: largeActive ? 240 : 0

    readonly property var settingsDoc: parseSettings()
    function parseSettings() {
        try {
            return JSON.parse(QbzBridge.settingsJson)
        } catch (e) {
            return ({})
        }
    }
    // (The old backendPipewire / dacPassthrough LED sources are gone: the
    // stamp's LEDs now read np_output_backend_* / np_output_mode_* off
    // QbzPlayer, which is where Slint reads them too — the settings document
    // never knew whether the backend was actually ENGAGED.)
    // AppearanceState.show-volume-steppers (PlayerBar.slint gates the −/+
    // pair on it; Tauri always showed them).
    readonly property bool showVolumeSteppers: settingsDoc.showVolumeSteppers === true
    // A-B loop entries for the "+" menu: one contextual verb + clear. Local
    // playback only — the loop cannot follow a cast / Connect renderer.
    function abEntries() {
        if (QbzPlayer.npIsRemote || QbzPlayer.npCastActive)
            return []
        var m = [{ "sep": true }]
        if (QbzPlayer.abState === 0)
            m.push({ "label": QbzSession.tr("Set loop start (A)", QbzSession.trRev), "icon": "repeat", "action": "ab-mark" })
        else if (QbzPlayer.abState === 1)
            m.push({ "label": QbzSession.tr("Set loop end (B)", QbzSession.trRev), "icon": "repeat", "action": "ab-mark" })
        if (QbzPlayer.abState !== 0)
            m.push({ "label": QbzSession.tr("Clear loop", QbzSession.trRev), "icon": "x", "action": "ab-clear" })
        return m
    }

    // Favorite state of the now-playing track (Slint:
    // QueueState.now-playing-favorite). The queue document carries it on its
    // `current` row; this re-parses ONLY when that document changes.
    //
    // The bar's OWN heart settles fine on its own — `queue_qt::toggle_favorite`
    // republishes the queue document straight after the write. What it could
    // not see was a heart flipped ANYWHERE ELSE for the same track (the album
    // page's row, a search result, a card): nothing republishes the queue for
    // those, so the bar sat on a stale glyph for the rest of the track. The
    // one-slot override below closes it, keyed on the track id so it expires
    // by itself the moment playback moves on — no map to keep clean.
    property string favOverrideId: ""
    property bool favOverrideValue: false
    readonly property bool npFavorite: {
        if (root.favOverrideId !== "" && root.favOverrideId === QbzPlayer.npTrackId)
            return root.favOverrideValue
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            return !!(d && d.current && d.current.isFavorite)
        } catch (e) {
            return false
        }
    }
    Connections {
        target: QbzLibrary
        function onLibraryFavoriteChanged(key, value) {
            var id = QbzPlayer.npTrackId
            if (id !== "" && key === "track:" + id) {
                root.favOverrideId = id
                root.favOverrideValue = value
            }
        }
    }

    /// The playing track is EPHEMERAL — a disc, an image or an ad-hoc folder.
    ///
    /// It leaves no trace in QBZ: no `library.db` row, no catalog id, nothing
    /// a favourite or a playlist entry could point at tomorrow. So every
    /// affordance that implies PERSISTENCE has to go, and the queue ones stay
    /// — which is exactly what `QueueSidebar` already does with the same flag
    /// (`queue_qt.rs` publishes `isEphemeral` on every row; this bar simply
    /// never read it).
    ///
    /// Read from the queue's `current` row rather than guessed from the id
    /// range: Rust tests BOTH halves there (the row's own source tag and the
    /// id floor), and a second, weaker copy of that test in QML is a second
    /// thing to get wrong.
    readonly property bool npEphemeral: {
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            return !!(d && d.current && d.current.isEphemeral)
        } catch (e) {
            return false
        }
    }

    // SOURCE of the now-playing track — the word the MyQBZ AddItem payload
    // needs ("qobuz" | "local"), NEVER a literal.
    //
    // This bar's "row" is the queue document's `current` row, the same row the
    // heart above reads. `QbzPlayer` publishes no source word at all
    // (player_bridge.rs:31-114 — there is no `np_source`), so the only
    // published fact about the current track's provenance is that row's
    // `isLocal` (queue_qt.rs:46-50, straight off `QueueTrack::is_local`).
    //
    // `isLocal === false` is unambiguous: the queue id IS a Qobuz catalog id.
    // `isLocal === true` is NOT one thing — it covers a `library.db` row id, a
    // `PLEX_TRACK_ID_FLOOR`-namespaced id, an `EPHEMERAL_ID_FLOOR` id, and the
    // offline cache, whose queue id is a QOBUZ id (local_playback.rs:53-58).
    // Those four need four different payloads and this bar cannot tell them
    // apart without sniffing the id shape, which is exactly the scattered
    // knowledge the source seam exists to end. So it answers "" = UNKNOWN, and
    // the menu below drops "Add to mixtape" instead of stamping a guess: an
    // action that cannot work must not be offered, and an ephemeral track must
    // leave no trace outside the queue.
    //
    // The id guard is not paranoia-for-free: it is what keeps a stale queue
    // document from lending the PREVIOUS track's provenance to this one. Every
    // track edge republishes both (playback_qt.rs:1119-1120 and siblings), so
    // it holds in steady state.
    readonly property string npSource: {
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            if (d && d.current && d.current.id === QbzPlayer.npTrackId)
                return d.current.isLocal === true ? "" : "qobuz"
        } catch (e) {
        }
        return ""
    }

    // Audio settings — the button OPENS A FLYOUT (Normalization + Gapless),
    // it is not itself a toggle: `PlayerBar.slint:666-706`. Its `active`
    // colour does mirror normalization here (`:677
    // active: SettingsState.normalization`) — the Small bar deliberately does
    // NOT, see its own note.
    //
    // The state is read from the PUBLISHED document. It used to be shadowed
    // locally (`normTouched` / `normLocal`) because settingsJson did not carry
    // it, and that is what made normalization impossible to turn off: on a
    // fresh launch the fallback read `undefined`, the button drew OFF while
    // the backend was ON, and the first click SET IT ON. The document is now
    // seeded at shell entry (main.rs enter_shell) and republished after every
    // apply (settings_qt::apply_audio), so there is nothing left to shadow.
    readonly property bool normalizationOn: settingsDoc.normalization === true

    function fmt(secs) {
        var m = Math.floor(secs / 60)
        var s = Math.floor(secs % 60)
        return m + ":" + (s < 10 ? "0" : "") + s
    }

    // --- Seek clamp (closes PARITY-DEBT #15, QML half) ---------------------
    // SeekBar.slint:24-26 / 93-98: while a track is still downloading the seek
    // target is LOCKED to the furthest fraction that has arrived, and the
    // cursor turns not-allowed over the region that has not. The source is
    // NowPlayingState.seekable-max (state.slint:4402), fed by
    // playback.rs:5304 `buffer_progress.clamp(0,1)` and published here as
    // QbzPlayer.npSeekableMax.
    //
    // A fully-available track (local, cached, or a finished download) reports
    // 1.0, so Math.min() is a no-op and seeking stays completely free — the
    // clamp can never fight a local track.
    function clamp01(v) {
        return Math.min(Math.max(v, 0), 1)
    }
    // SeekBar.slint:98 — Math.min(clamp01(mouse-x / width), seekable-max).
    function seekTarget(fraction) {
        return Math.min(root.clamp01(fraction), QbzPlayer.npSeekableMax)
    }
    // SeekBar.slint:93 — clamp01(mouse-x / width) > seekable-max.
    function beyondSeekable(fraction) {
        return root.clamp01(fraction) > QbzPlayer.npSeekableMax
    }

    // --- Track Info (album/TrackInfoModal.slint) ---------------------------
    // The (i) button and the song-card title open the MODAL (scrim + centered
    // card), which is what the .slint does: the bars fire
    // media-action("track", id, "track-info") and AppShell mounts
    // TrackInfoModal gated on TrackInfoState.open. Slint gates the trigger on
    // NowPlayingState.source == "qobuz" (no DB row / metadata page for local /
    // Plex / ephemeral); the Qt port publishes no np_source, so a numeric
    // track id stands in for it. TODO(glue): publish np_source.
    function openTrackInfo() {
        if (!QbzPlayer.npHasTrack)
            return
        var id = QbzPlayer.npTrackId
        if (!/^[0-9]+$/.test(id))
            return
        // A local/media-server track has a numeric library ROW id (not a Qobuz
        // catalog id), so it cannot use the Qobuz Track Info modal — that one
        // fed a local id rendered empty. Route it to the Local Media Info modal
        // (source/server/format/paths). The (i) button and the song-card title
        // both land here, so both are fixed.
        var isLocal = false
        try {
            var q = JSON.parse(QbzQueue.queueJson)
            isLocal = !!(q && q.current && q.current.isLocal)
        } catch (e) {}
        if (isLocal)
            QbzLocal.openMediaInfoForTrack(parseInt(id, 10))
        else
            trackInfo.openFor(id)
    }

    TrackInfoModal { id: trackInfo }

    // --- Cast picker (shell/CastPicker.slint) ------------------------------
    // Mounted here for the same reason TrackInfoModal is: it is a Popup
    // parented to Overlay.overlay, so AppShell.qml needs no mount, and the
    // NowPlayingBar Loader guarantees exactly ONE bar (hence one instance) is
    // alive. Visibility follows QbzCast.pickerOpen — the cast button in the
    // right cluster raises it through QbzCast.openPicker(), which is also
    // what arms device discovery.
    CastPicker { }

    // --- Shared bits -------------------------------------------------------
    // SongCard / TransportControls / FavToggle were inline components here
    // until phase 25; they now live in their own files (shell/SongCard.qml,
    // shell/TransportControls.qml, shell/FavToggle.qml) and TransportControls
    // is shared with the Small bar.

    // ============================ the bar =================================
    Column {
        anchors.fill: parent
        spacing: 0

        Item { width: 1; height: 6 }

        // Full-width seek bar (times at the ends). In Large the seek arm is
        // LEFT-INSET by the docked-cover width so it begins right of the
        // dock (AppShell.slint's large-active padding).
        Item {
            id: seekRow
            width: parent.width
            height: 32

            // FLUIDITY (#660-reporter follow-up): the fill + handle used to
            // track the BACKEND-reported npProgress, and a drag called
            // QbzPlayer.seek() on every move. So the handle lagged the cursor
            // by the seek round-trip and the backend was flooded with seeks —
            // "slow and clunky, clicks not exact". Now a press starts a LOCAL
            // scrub: the handle/fill follow the cursor instantly and ONE seek
            // is committed on release (a plain click commits at the click
            // point). `settling` holds the visual at the committed fraction
            // until npProgress catches up so it never flashes back. The audio
            // path and the seek call itself are unchanged.
            property bool scrubbing: false
            property bool settling: false
            property real scrubFraction: 0
            // Live hover position (fraction) for the time tooltip; -1 = none.
            property real hoverFraction: -1
            readonly property real visualProgress: (scrubbing || settling)
                ? scrubFraction
                : Math.min(Math.max(QbzPlayer.npProgress, 0), 1)

            Timer {
                id: settleTimeout
                interval: 1500
                onTriggered: seekRow.settling = false
            }
            Connections {
                target: QbzPlayer
                enabled: seekRow.settling
                function onNpProgressChanged() {
                    if (Math.abs(QbzPlayer.npProgress - seekRow.scrubFraction) < 0.02) {
                        seekRow.settling = false
                        settleTimeout.stop()
                    }
                }
            }

            Text {
                x: root.dockWidth > 0 ? root.dockWidth + 6 : 6
                anchors.verticalCenter: parent.verticalCenter
                visible: QbzPlayer.npHasTrack
                // While scrubbing, the elapsed readout follows the cursor.
                text: (seekRow.scrubbing || seekRow.settling)
                    ? root.fmt(seekRow.scrubFraction * QbzPlayer.npDurationSecs)
                    : root.fmt(QbzPlayer.npElapsedSecs)
                color: theme.textMuted
                font.pixelSize: 11
            }
            Rectangle {
                id: seekTrack
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: (root.dockWidth > 0 ? root.dockWidth + 6 : 6) + (QbzPlayer.npHasTrack ? 46 : 0)
                anchors.rightMargin: 6 + (QbzPlayer.npHasTrack ? 46 : 0)
                height: root.waveformVisible ? 30 : 4
                radius: 2
                anchors.verticalCenter: parent.verticalCenter
                color: root.waveformVisible ? "transparent" : theme.surfaceElevated
                SeekWaveformItem {
                    anchors.fill: parent
                    visible: root.waveformVisible
                    values: QbzPlayer.npSeekWaveform
                    playedProgress: seekRow.visualProgress
                    cacheProgress: QbzPlayer.npCacheProgress
                    baseColor: theme.surfaceElevated
                    cacheColor: Qt.rgba(theme.textMuted.r, theme.textMuted.g,
                                        theme.textMuted.b, 0.35)
                    playedColor: theme.accent
                    renderMode: 0
                }
                // Buffered / cache line — SeekBar.slint:58 paints it
                // text-muted @0.35, NOT border-muted: border-muted is an
                // alpha-over-surface token (white-based on dark themes,
                // black-based on light), so it inverted against the
                // surface-elevated rail on light themes. The Small bar
                // already used the right pair.
                Rectangle {
                    visible: !root.waveformVisible
                    width: parent.width * Math.min(Math.max(QbzPlayer.npCacheProgress, 0), 1)
                    height: parent.height
                    radius: 2
                    color: Qt.rgba(theme.textMuted.r, theme.textMuted.g,
                                   theme.textMuted.b, 0.35)
                    // Alpha in the material, not a container `opacity`: an
                    // always-on opacity node pins this quad in its own batch,
                    // and the three seek quads use compatible materials
                    // (QSGSmoothColorMaterial::compare() returns 0), so folding
                    // it lets rail + cache + progress merge.
                }
                Rectangle {
                    visible: !root.waveformVisible
                    width: parent.width * seekRow.visualProgress
                    height: parent.height
                    radius: 2
                    color: theme.accent
                }
                Rectangle {
                    visible: QbzPlayer.npHasTrack
                    width: 12
                    height: 12
                    radius: 6
                    color: theme.textPrimary
                    x: parent.width * seekRow.visualProgress - width / 2
                    anchors.verticalCenter: parent.verticalCenter
                }
                // A-B loop: the tinted section + two ticks. Static bindings only
                // (no animation), so nothing here touches the repaint pulse.
                Rectangle {
                    visible: QbzPlayer.abState === 2 && QbzPlayer.npDurationSecs > 0
                    x: parent.width * (QbzPlayer.abStartSecs / Math.max(QbzPlayer.npDurationSecs, 1))
                    width: Math.max(2, parent.width
                        * ((QbzPlayer.abEndSecs - QbzPlayer.abStartSecs) / Math.max(QbzPlayer.npDurationSecs, 1)))
                    height: parent.height
                    radius: 2
                    color: Qt.rgba(theme.accent.r, theme.accent.g, theme.accent.b, 0.28)
                }
                Repeater {
                    model: QbzPlayer.abState === 0 ? []
                         : (QbzPlayer.abState === 1 ? [QbzPlayer.abStartSecs]
                                                    : [QbzPlayer.abStartSecs, QbzPlayer.abEndSecs])
                    Rectangle {
                        required property int modelData
                        visible: QbzPlayer.npDurationSecs > 0
                        width: 2
                        height: 10
                        radius: 1
                        color: theme.accent
                        x: seekTrack.width * (modelData / Math.max(QbzPlayer.npDurationSecs, 1)) - 1
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }
            }
            Text {
                anchors.right: parent.right
                anchors.rightMargin: 6
                anchors.verticalCenter: parent.verticalCenter
                visible: QbzPlayer.npHasTrack
                text: "-" + root.fmt(Math.max(0, QbzPlayer.npDurationSecs - QbzPlayer.npElapsedSecs))
                color: theme.textMuted
                font.pixelSize: 11
            }
            MouseArea {
                anchors.left: seekTrack.left
                anchors.right: seekTrack.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                // Hover is what keeps mouseX live for the cursor binding
                // below (PlayerBar.slint:180 -> SeekBar.slint's TouchArea
                // always tracks mouse-x).
                hoverEnabled: true
                // No-drop cursor over the not-yet-downloaded region
                // (SeekBar.slint:93-95). The npHasTrack arm is the Qt bar's
                // own idle guard and stays first.
                cursorShape: !QbzPlayer.npHasTrack
                    ? Qt.ArrowCursor
                    : (root.beyondSeekable(mouseX / width) ? Qt.ForbiddenCursor
                                                           : Qt.PointingHandCursor)
                id: seekArea
                // A press starts a LOCAL scrub (the handle follows the cursor
                // with zero round-trip lag); the real seek is committed ONCE on
                // release — a plain click commits at the click point. seekTarget
                // clamps to what has downloaded while streaming (SeekBar.slint:
                // 96-99), so a scrub cannot pass the buffered edge.
                onPressed: {
                    if (!QbzPlayer.npHasTrack)
                        return
                    seekRow.settling = false
                    seekRow.scrubbing = true
                    seekRow.scrubFraction = root.seekTarget(mouseX / width)
                }
                onPositionChanged: {
                    if (!QbzPlayer.npHasTrack)
                        return
                    if (pressed)
                        seekRow.scrubFraction = root.seekTarget(mouseX / width)
                    else
                        seekRow.hoverFraction = root.clamp01(mouseX / width)
                }
                onReleased: {
                    if (!seekRow.scrubbing)
                        return
                    seekRow.scrubbing = false
                    if (QbzPlayer.npHasTrack) {
                        // Hold the visual at the committed point until the
                        // backend position catches up, so it never flashes back.
                        seekRow.settling = true
                        settleTimeout.restart()
                        QbzPlayer.seek(seekRow.scrubFraction)
                    }
                }
                onCanceled: {
                    seekRow.scrubbing = false
                    seekRow.settling = false
                }
                onExited: seekRow.hoverFraction = -1
            }

            // Scrub / hover time bubble (best-effort parity with the web
            // player). It floats above the rail with a downward caret. It is
            // parented to the window OVERLAY, not to the bar: AppShell mounts
            // the content frame AFTER the NowPlayingBar, so in ambient mode the
            // frame's drop shadow drew OVER a bar-child tooltip (the "noise").
            // The overlay is the top-most layer, above that shadow. Opaque fill
            // for the same reason (a real blur needs a shader, which draws
            // nothing on the software renderer).
            Item {
                id: seekTip
                parent: Overlay.overlay
                z: 10000
                readonly property real frac: seekRow.scrubbing
                    ? seekRow.scrubFraction
                    : seekRow.hoverFraction
                readonly property color tipFill: theme.surfaceElevated
                visible: QbzPlayer.npHasTrack && frac >= 0
                    && (seekRow.scrubbing || seekArea.containsMouse)
                width: bubble.width
                height: bubble.height + caret.height
                // Map the handle's top point into overlay coordinates so the
                // caret still lands on the rail from the top layer. Re-maps when
                // `frac` changes (the bar itself is static at the window foot).
                readonly property point anchor: Overlay.overlay
                    ? seekTrack.mapToItem(Overlay.overlay, seekTrack.width * frac, 0)
                    : Qt.point(0, 0)
                x: Math.max(4, Math.min(
                    (Overlay.overlay ? Overlay.overlay.width : width) - width - 4,
                    anchor.x - width / 2))
                // Caret tip sits right on the rail — no gap (removed per owner).
                y: anchor.y - height + 3

                Rectangle {
                    id: bubble
                    width: tipText.implicitWidth + 14
                    height: 18
                    radius: 4
                    color: seekTip.tipFill
                    border.width: 1
                    border.color: theme.borderMuted
                    Text {
                        id: tipText
                        anchors.centerIn: parent
                        text: root.fmt((seekTip.frac < 0 ? 0 : seekTip.frac) * QbzPlayer.npDurationSecs)
                        color: theme.textPrimary
                        font.pixelSize: 11
                    }
                }
                // Downward caret: a rotated square whose lower half shows below
                // the bubble as the notch. Same fill, no border, so the bubble's
                // border reads as one clean edge above it.
                Rectangle {
                    id: caret
                    width: 9
                    height: 9
                    rotation: 45
                    color: seekTip.tipFill
                    anchors.horizontalCenter: bubble.horizontalCenter
                    y: bubble.height - height / 2
                }
            }
        }

        Item { width: 1; height: 2 }

        // --- Controls: the responsive symmetric zones -----------------------
        Item {
            id: controlsLayout
            width: parent.width
            height: parent.height - 40

            // The three zones keep PLAY centred and the right cluster pinned,
            // but the SongCard is sized against the controls themselves, not
            // against an arbitrary zone edge. This lets metadata consume the
            // centre column's empty runway without ever crossing transport.
            readonly property real transportLeft:
                centreZone.x + centreTransport.x
            readonly property real classicTransportRight: Math.min(
                leftZone.x + leftZone.width,
                leftZone.x + classicTransport.x + classicTransport.width)
            readonly property real rightControlsLeft:
                rightZone.x + Math.max(0, rightControls.x)

            // LEFT column.
            Item {
                id: leftZone
                anchors.left: parent.left
                anchors.leftMargin: 6
                width: (parent.width - 12) * root.colSide
                height: parent.height

                // New (0) AND Large (3): the song card (Large drops the cover
                // — it lives in the dock — and shifts right to clear it). Its
                // right edge follows the ACTUAL transport bounds; subtracting
                // the 240px dock from this zone was the old Large-mode bug that
                // left long titles only ~260px on a 1700px window.
                SongCard {
                    visible: !root.isClassic
                    x: root.largeActive ? root.dockWidth + 8 : 0
                    readonly property real availableWidth: Math.max(0,
                        controlsLayout.transportLeft - 10 - leftZone.x - x)
                    width: availableWidth * theme.npbSongCardMaxFraction
                    anchors.verticalCenter: parent.verticalCenter
                    showArt: !root.largeActive
                    showBadges: !root.largeActive
                    tooltip: root.tooltip
                    onTrackInfoRequested: root.openTrackInfo()
                }
                // Classic: transport cluster hugging the left edge (plain
                // play glyph + inline favorite, the Tauri arrangement).
                TransportControls {
                    id: classicTransport
                    visible: root.isClassic
                    compact: root.width < 1100
                    anchors.left: parent.left
                    anchors.verticalCenter: parent.verticalCenter
                    playCircle: false
                    classicActions: true
                    favorite: root.npFavorite
                    ephemeral: root.npEphemeral
                    tooltip: root.tooltip
                    onAddRequested: function (anchorItem) { addMenu.openBelowRight(anchorItem) }
                    onTrackInfoRequested: root.openTrackInfo()
                }
            }

            // CENTRE column.
            Item {
                id: centreZone
                x: 6 + (parent.width - 12) * root.colSide
                width: (parent.width - 12) * root.colCentre
                height: parent.height
                clip: true

                // New (0) AND Large (3): centred transport, PLAY on the
                // window centre.
                TransportControls {
                    id: centreTransport
                    visible: !root.isClassic
                    compact: root.width < 1366
                    anchors.centerIn: parent
                    playCircle: true
                    favorite: root.npFavorite
                    tooltip: root.tooltip
                    onAddRequested: function (anchorItem) { addMenu.openBelowRight(anchorItem) }
                    onTrackInfoRequested: root.openTrackInfo()
                }
            }

            // RIGHT column — secondary actions + volume, clustered at the
            // right wall.
            Item {
                id: rightZone
                anchors.right: parent.right
                anchors.rightMargin: 6
                width: (parent.width - 12) * root.colSide
                height: parent.height
                clip: true

                Row {
                    id: rightControls
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: 1

                    // Large: the inline 2-row AudioStamp just LEFT of the icon
                    // cluster at a fixed 12px gap (the badges move OUT of the
                    // song card in Large). PlayerBar.slint mounts it with
                    // max-width: 140px.
                    AudioStamp {
                        visible: root.largeActive && QbzPlayer.npHasTrack
                        maxWidth: 140
                        tooltipHost: root.tooltip
                        anchors.verticalCenter: parent.verticalCenter
                    }
                    Item { visible: root.largeActive; width: 12; height: 1 }

                    // Qobuz Connect — the Slint ConnectButton (PlayerBar.
                    // slint:32-77): monitor-speaker glyph, GOLDEN over a soft
                    // golden tint whenever a session is on — whether qbz is
                    // the renderer or controlling a peer (not just is-remote).
                    // Opens the shared flyout (QconnectFlyout.qml). This is
                    // NOT a QbzIconButton: the reference's active state is a
                    // gold tint + 1px border + gold glyph, which the shared
                    // button's accent-glyph `active` cannot express. The
                    // "amber" tint IS the reference's gold (#e0b341).
                    //
                    // FIRST of the icon cluster, ahead of Cast — the order the
                    // reference lays out (PlayerBar.slint:395-401 Connect,
                    // :646-653 Cast) and the one the Small bar already used.
                    Rectangle {
                        id: qconnectBtn
                        readonly property bool qcActive: QbzQConnect.qconnectConnected
                        readonly property color gold: "#e0b341"
                        width: 32
                        height: 32
                        radius: theme.radiusSm
                        anchors.verticalCenter: parent.verticalCenter
                        color: qcActive ? Qt.rgba(gold.r, gold.g, gold.b, 0.16)
                            : (qcArea.containsMouse ? theme.surfaceHover : "transparent")
                        border.width: qcActive ? 1 : 0
                        border.color: Qt.rgba(gold.r, gold.g, gold.b, 0.45)
                        QbzIcon {
                            name: "monitor-speaker"
                            width: 16
                            height: 16
                            anchors.centerIn: parent
                            tintName: qconnectBtn.qcActive ? "amber"
                                : qcArea.containsMouse ? "textPrimary" : "secondary"
                        }
                        MouseArea {
                            id: qcArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                if (root.tooltip)
                                    root.tooltip.hide("qconnect")
                                qcFlyout.openAboveRight(qconnectBtn)
                            }
                            // The ONE bar tooltip the reference binds (always
                            // shows on hover, even while connected — Slint
                            // PlayerBar.slint:64-74 says why out loud).
                            onContainsMouseChanged: {
                                if (!root.tooltip)
                                    return
                                if (containsMouse)
                                    root.tooltip.showAbove(qconnectBtn, "qconnect",
                                        QbzQConnect.qconnectConnected
                                            ? QbzSession.tr("Qobuz Connect: On", QbzSession.trRev)
                                            : QbzSession.tr("Qobuz Connect: Off", QbzSession.trRev))
                                else
                                    root.tooltip.hide("qconnect")
                            }
                        }
                    }

                    // Cast (Chromecast / DLNA) — SECOND, straight after
                    // Connect. Opens the picker modal and, with it, discovery
                    // (PlayerBar.slint:646-664: picker-open = true +
                    // CastActions.open()); lit while a renderer is connected.
                    QbzIconButton {
                        name: "cast"
                        active: QbzPlayer.npCastActive
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzCast.openPicker()
                    }

                    // Lyrics.
                    QbzIconButton {
                        name: "mic-vocal"
                        active: QbzShell.lyricsOpen
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzShell.toggleLyrics()
                    }

                    // Now-Playing view — the mode flyout (phase 18). Tauri's
                    // Miniplayer + Full screen buttons live inside it.
                    QbzIconButton {
                        id: viewBtn
                        name: "layout-grid"
                        active: viewMenu.opened
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: viewMenu.openBelowRight(viewBtn)
                    }

                    // Audio settings — opens the Normalization + Gapless
                    // flyout (PlayerBar.slint:666-706). NOT a toggle.
                    QbzIconButton {
                        id: audioBtn
                        name: "settings-2"
                        active: root.normalizationOn
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: audioMenu.openBelowRight(audioBtn)
                    }

                    Item { width: 6; height: 1 }

                    // Volume: mute · slider · −/+ steppers (the steppers are
                    // the Tauri volume-step buttons, gated by the appearance
                    // preference like the Slint bar). All four controls gate
                    // on root.volLocked (§11.3 — the local ALSA-hw lock lifts
                    // under a peer; a volume-disallowing peer locks).
                    QbzIconButton {
                        name: QbzPlayer.npMuted ? "volume-x" : "volume-2"
                        btnEnabled: !root.volLocked
                        active: QbzPlayer.npMuted
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzPlayer.toggleMute()
                    }
                    QbzSlider {
                        enabled: !root.volLocked
                        width: 81
                        anchors.verticalCenter: parent.verticalCenter
                        minimum: 0
                        // 0..1000: whole steps on a 0..100 scale quantize the
                        // volume to 1%; 0.1% steps drag fluidly.
                        maximum: 1000
                        value: Math.round(QbzPlayer.npVolume * 1000)
                        onChanged: function (v) { QbzPlayer.setVolume(v / 1000.0) }
                        // Persist only the settled value (PlayerBar.slint:864-866).
                        onReleased: function (v) { QbzPlayer.persistVolume(v / 1000.0) }
                    }
                    QbzIconButton {
                        visible: root.showVolumeSteppers
                        name: "minus"
                        iconSize: 15
                        anchors.verticalCenter: parent.verticalCenter
                        btnEnabled: !root.volLocked
                        onClicked: QbzPlayer.setVolume(Math.max(0.0, QbzPlayer.npVolume - 0.05))
                    }
                    QbzIconButton {
                        visible: root.showVolumeSteppers
                        name: "plus"
                        iconSize: 15
                        anchors.verticalCenter: parent.verticalCenter
                        btnEnabled: !root.volLocked
                        onClicked: QbzPlayer.setVolume(Math.min(1.0, QbzPlayer.npVolume + 0.05))
                    }

                    Item { width: 6; height: 1 }

                    // Queue panel toggle.
                    QbzIconButton {
                        name: "list-ordered"
                        active: QbzShell.queueOpen
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzShell.toggleQueue()
                    }
                }
            }

            // Classic lives between the REAL left transport and right action
            // cluster. It may use at most 80% of that live gap, centred: the
            // remaining 10% on each side keeps both the cover edge and the
            // in-card AudioStamp visibly detached from their neighbour controls.
            // Unlike the old fixed 560px cap this still scales with volume
            // steppers, remote state and the actual window width.
            SongCard {
                visible: root.isClassic
                glass: true
                readonly property real availableWidth: Math.max(0,
                    controlsLayout.rightControlsLeft
                        - controlsLayout.classicTransportRight)
                width: availableWidth * theme.npbSongCardMaxFraction
                x: controlsLayout.classicTransportRight
                    + (availableWidth - width) / 2
                anchors.verticalCenter: parent.verticalCenter
                tooltip: root.tooltip
                onTrackInfoRequested: root.openTrackInfo()
            }
        }

        Item { width: 1; height: 6 }
    }

    // The Now-Playing-view mode menu (shared with the Small bar).
    ViewModeMenu { id: viewMenu }

    // Audio settings flyout — Normalization + Gapless (PlayerBar.slint's
    // `audio-menu` PopupWindow). Fed the already-parsed document so it does
    // not re-parse settingsJson on every open.
    AudioSettingsMenu { id: audioMenu; doc: root.settingsDoc }

    // Qobuz Connect device flyout — the ONE shared component both bars mount
    // (contract §8; the Slint `qconnect-menu` PopupWindow, PlayerBar.slint:
    // 412-642). Opened above-right of the Connect button.
    QconnectFlyout { id: qcFlyout }

    // "Add to…" flyout behind the transport "+" (TransportControls.slint's
    // add-menu), on the shared CardMenu surface. Same seven entries, same
    // order, same icons.
    // The Copy submenu of the add-menu writes through this carrier.
    QbzClipboard { id: npClipboard }

    CardMenu {
        id: addMenu
        menuWidth: 232
        entries: {
            // The three QUEUE actions are the only ones an ephemeral track can
            // honour: everything else here writes a reference that outlives the
            // session, and there is nothing to reference once the disc comes
            // out. They are ABSENT rather than rendered-and-inert, the same
            // rule the `npSource` gates below already follow.
            var m = []
            if (!root.npEphemeral) {
                m.push({
                    "label": root.npFavorite
                        ? QbzSession.tr("Remove from library", QbzSession.trRev)
                        : QbzSession.tr("Add to library", QbzSession.trRev),
                    "icon": root.npFavorite ? "heart-filled" : "heart",
                    "action": "favorite"
                })
            }
            m.push({ "label": QbzSession.tr("Add to queue", QbzSession.trRev), "icon": "list-end", "action": "queue" })
            m.push({ "label": QbzSession.tr("Play later", QbzSession.trRev), "icon": "list-plus", "action": "later" })
            m.push({ "label": QbzSession.tr("Play next", QbzSession.trRev), "icon": "list-start", "action": "next" })
            if (root.npEphemeral)
                return m.concat(root.abEntries())
            // "Add to playlist" sits SECOND in TransportControls.slint:143 —
            // spliced in there rather than appended, because the flyout's
            // order is part of the parity. It rides the SAME `npSource` gate
            // as "Add to mixtape" and for the same reason: the picker's Qobuz
            // arm takes catalog ids, and an `isLocal` current row is any of
            // four different id spaces this bar cannot tell apart (see
            // `npSource`). Unknown provenance -> the entry is ABSENT, never
            // rendered-and-inert.
            if (root.npSource === "qobuz") {
                m.splice(1, 0, {
                    "label": QbzSession.tr("Add to playlist", QbzSession.trRev),
                    "icon": "list-music",
                    "action": "playlist"
                })
            }
            // Only when the track's source is KNOWN (see `npSource`): a row we
            // cannot address is not offered.
            if (root.npSource !== "") {
                m.push({
                    "label": QbzSession.tr("Add to mixtape", QbzSession.trRev),
                    "icon": "cassette-tape",
                    "action": "mixtape"
                })
            }
            if (QbzPlayer.npAlbumId !== "") {
                m.push({
                    "label": QbzSession.tr("Add album to collection", QbzSession.trRev),
                    "icon": "disc-3",
                    "action": "album-favorite"
                })
            }
            // "Buy on Qobuz" (2026-09-13): catalog tracks only; store_qt
            // resolves whether the release is sold and toasts when not.
            if (root.npSource === "qobuz") {
                m.push({
                    "label": QbzSession.tr("Buy on Qobuz", QbzSession.trRev),
                    "icon": "shopping-bag",
                    "action": "buy"
                })
            }
            // Copy (2026-09-13): a hover-opened submenu — the track name, or
            // "Track - Album - Artist".
            m.push({
                "label": QbzSession.tr("Copy", QbzSession.trRev),
                "icon": "copy",
                "action": "copy",
                "submenu": [
                    { "label": QbzSession.tr("Track name", QbzSession.trRev), "icon": "copy", "action": "copy-title" },
                    { "label": QbzSession.tr("Track - Album - Artist", QbzSession.trRev), "icon": "clipboard", "action": "copy-full" }
                ]
            })
            return m.concat(root.abEntries())
        }
        onPicked: function (a) {
            var id = QbzPlayer.npTrackId
            if (a === "favorite") {
                if (id !== "") QbzQueue.queueToggleFavorite("track", id)
            } else if (a === "queue" || a === "later" || a === "next") {
                if (id !== "") QbzPlayer.enqueueTrack(id, a)
            } else if (a === "playlist") {
                // The entry only exists when `npSource === "qobuz"`, so `id`
                // is a catalog id here by the gate above.
                if (id !== "") QbzPlaylistPicker.openForTrack(id)
            } else if (a === "album-favorite") {
                if (QbzPlayer.npAlbumId !== "")
                    QbzLibrary.libraryToggleFavorite("album", QbzPlayer.npAlbumId)
            } else if (a === "buy") {
                if (id !== "") QbzAlbum.buyTrack(id)
            } else if (a === "copy-title" || a === "copy-full") {
                var parts = [QbzPlayer.npTitle || ""]
                if (a === "copy-full") {
                    if ((QbzPlayer.npAlbum || "") !== "") parts.push(QbzPlayer.npAlbum)
                    if ((QbzPlayer.npArtist || "") !== "") parts.push(QbzPlayer.npArtist)
                }
                npClipboard.copy(parts.join(" - "))
            } else if (a === "ab-mark") {
                QbzPlayer.abMark()
            } else if (a === "ab-clear") {
                QbzPlayer.abClear()
            } else if (a === "mixtape") {
                // MyQBZ AddItem, built here from the now-playing state:
                // `npArtworkPath` is a file:// CACHE path, so it is NOT the
                // artworkUrl — the store would keep a dead local path.
                // The source comes from the queue's current ROW (`npSource`),
                // never from a literal; the entry is not even in the menu when
                // that row cannot answer.
                if (id !== "" && root.npSource !== "")
                    QbzMyQbzAdd.open(JSON.stringify([{
                        "itemType": "track", "source": root.npSource,
                        "sourceItemId": id, "title": QbzPlayer.npTitle,
                        // artworkUrl STAYS EMPTY here, deliberately: the player bridge exposes only np_artwork_path.
                        // A file:// cache path must NOT be stored — the collection's
                        // artwork_url is a snapshot other machines read — so this needs a
                        // remote-url field on the document first. The five sister sites
                        // that HAD one were stamped 2026-08-22.
                        "subtitle": QbzPlayer.npArtist, "artworkUrl": "",
                        "year": null, "trackCount": null
                    }]))
            }
        }
    }
}
