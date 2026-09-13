// Bottom now-playing / transport bar — QML port of
// crates/qbz-ui/ui/shell/PlayerBarSmall.slint (Mode C "Small"). Mounted by
// the NowPlayingBar seam when npbMode == 2 (phase 18).
//
// 3px full-width top seekbar (3-color: background / cache / progress),
// then the symmetric 3-column controls row (~39px):
//   LEFT   — the SHARED shell/SongCard.qml in its text-center (Small) arm
//            (37px bordered cover + 13px title over the [layers] · artist ·
//            album meta row) + the fixed 40px time column
//   CENTER — transport cluster (info · shuffle · prev · play · next ·
//            repeat · +)
//   RIGHT  — AudioStamp · Connect · Cast · Settings · ViewMode · Lyrics ·
//            Volume · Queue
// Total height 42px (ShellState.npb-small-height; npb-small-extra is 0 on
// Linux).
//
// BUTTON SET (phase 24): identical to PlayerBar's, in the Small bar's own
// order — the transport is LITERALLY the shared cluster since phase 25
// (shell/TransportControls.qml: info · shuffle · prev · play · next · repeat ·
// "+"), and the right cluster keeps PlayerBarSmall's mount order (Connect ·
// Cast · Settings · ViewMode · Lyrics · Volume · Queue).
// The "+" opens the same "Add to…" flyout as the full bar; the quality stamp
// is AudioStamp.qml — the inline 2-row stamp (quality line over the backend /
// mode LEDs) that PlayerBarSmall.slint mounts, shared with the Large bar.
//
// Connect is LIVE (the golden ConnectButton opens shell/QconnectFlyout.qml —
// NO tooltip, the reference asymmetry of contract §8); Settings / ViewMode
// are live too. Volume, Queue, Shuffle, Repeat, Mute, Lyrics and the add
// flyout are live. Cast opens
// shell/CastPicker.qml (QbzCast — discovery, connect/disconnect, the
// per-device quality cap) and lights while a renderer is connected. Track
// Info (the (i) button and the song-card title) opens
// shell/TrackInfoModal.qml — it needs the QbzAlbum.openTrackInfo glue, see
// that file's header.

import QtQuick
import com.blitzfc.qbz
import "../controls"
import "../theme"

Rectangle {
    id: root
    // surface-card @ 0.5 while the ambient background is active (phase 14,
    // PlayerBarSmall.slint).
    color: ambientOn ? theme.surfaceCardA50 : theme.surfaceCard
    readonly property bool ambientOn: theme.ambientOn
    /// AppShell's one shared hover-tooltip overlay.
    property Item tooltip: null


    QbzTheme { id: theme }

    // Volume lock, both halves (contract §11.3 — the same `vol-locked`
    // formula PlayerBar.slint:152-158 carries and PlayerBar.qml repeats):
    // the ALSA-Direct hw derivation inside npVolumeLocked is LIFTED while a
    // peer owns playback; npRemoteVolumeLocked is the sink-pushed "peer
    // disallows remote volume" half.
    readonly property bool volLocked:
        (QbzPlayer.npVolumeLocked && !QbzPlayer.npIsRemote)
        || QbzPlayer.npRemoteVolumeLocked

    // Responsive side fraction — IDENTICAL to PlayerBarSmall's mechanism:
    // LEFT and RIGHT columns share this fraction so the CENTER transport
    // column lands PLAY on the window centre. The breakpoints (39/30/25 at
    // 1366 / 1920) are the SHARED theme token, so this bar and PlayerBar can
    // never drift apart — see QbzTheme.npbSideFrac.
    // `root.width` is the window width minus the two side gutters
    // (AppShell `npbGutter`): the bar is anchored left-to-right on the shell
    // root, exactly like PlayerBarSmall.slint reads its own root.
    property real sideFrac: theme.npbSideFrac(root.width)
    property real colCentre: 1.0 - 2.0 * root.sideFrac
    // WIDE = inline horizontal volume slider; NARROW = button-only.
    // Same 1366 edge as the side fraction (PlayerBarSmall.slint:152).
    property bool volumeWide: root.width >= theme.npbBreakMid

    // Favorite state of the now-playing track (Slint:
    // QueueState.now-playing-favorite) — the queue document carries it on its
    // `current` row; re-parsed only when that document changes. The one-slot
    // override is the same seam PlayerBar.qml documents: a heart flipped on
    // another surface for THIS track republishes no queue document, so
    // without it the bar kept a stale glyph until the track changed.
    property string favOverrideId: ""
    property bool favOverrideValue: false
    /// The playing track is EPHEMERAL — a disc, an image or an ad-hoc folder.
    /// Same flag and same rule as the full bar: nothing that implies
    /// persistence is offered, because there is nothing to point at once the
    /// session ends. `queue_qt.rs` publishes it on every row.
    readonly property bool npEphemeral: {
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            return !!(d && d.current && d.current.isEphemeral)
        } catch (e) {
            return false
        }
    }

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

    // SOURCE of the now-playing track for the MyQBZ AddItem payload
    // ("qobuz" | "local"), read off the queue document's `current` row — the
    // same row the heart above reads — and never written as a literal. The
    // full bar carries the long-form rationale (PlayerBar.qml `npSource`); the
    // short version: `QbzPlayer` publishes no source word, `isLocal === false`
    // proves a Qobuz catalog id, and `isLocal === true` conflates four id
    // namespaces (local row / Plex / ephemeral / offline-cached Qobuz) that
    // this bar cannot separate without sniffing the id. "" = unknown, and the
    // menu then does not offer "Add to mixtape" at all.
    readonly property string npSource: {
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            if (d && d.current && d.current.id === QbzPlayer.npTrackId)
                return d.current.isLocal === true ? "" : "qobuz"
        } catch (e) {
        }
        return ""
    }

    function fmt(secs) {
        var m = Math.floor(secs / 60)
        var s = Math.floor(secs % 60)
        return m + ":" + (s < 10 ? "0" : "") + s
    }

    // --- Track Info (album/TrackInfoModal.slint) -------------------------
    // The (i) button and the song-card title open the MODAL (scrim + centered
    // card) — PlayerBarSmall.slint fires media-action("track", id,
    // "track-info") and AppShell mounts TrackInfoModal. Slint gates it on
    // !NowPlayingState.is-ephemeral (no metadata page for local / Plex /
    // ephemeral rows); the Qt port publishes no source flag, so a numeric
    // track id stands in for it. TODO(glue): publish np_source.
    readonly property bool qobuzTrack: QbzPlayer.npHasTrack
        && /^[0-9]+$/.test(QbzPlayer.npTrackId)
    function openTrackInfo() {
        if (!root.qobuzTrack)
            return
        trackInfo.openFor(QbzPlayer.npTrackId)
    }

    TrackInfoModal { id: trackInfo }

    // --- Cast picker (shell/CastPicker.slint) ----------------------------
    // Same mount reasoning as TrackInfoModal above: a Popup parented to
    // Overlay.overlay, so AppShell.qml needs no mount, and the NowPlayingBar
    // Loader guarantees exactly ONE bar (hence one instance) is alive.
    // Visibility follows QbzCast.pickerOpen; the right-cluster cast button
    // raises it through QbzCast.openPicker(), which also arms discovery.
    CastPicker { }

    // Square compact icon button (BarControls.IconButton): disabled = dim
    // 0.3 + non-clickable; active = accent tint.


    // Thin vertical separator (PlayerBarSmall's Sep): a 1px line,
    // border-subtle, ~60% of the row height, vertically centred; the
    // horizontal gaps are the caller's.
    component Sep: Item {
        property int gapLeft: 6
        property int gapRight: 6
        width: gapLeft + 1 + gapRight
        height: parent ? parent.height : 0
        Rectangle {
            x: gapLeft
            width: 1
            height: 24
            anchors.verticalCenter: parent.verticalCenter
            color: theme.borderSubtle
        }
    }

    Column {
        anchors.fill: parent
        spacing: 0

        // === A. TOP full-width seekbar (the content/bar divider) ========
        QbzCompactSeekBar {
            width: parent.width
            hasTrack: QbzPlayer.npHasTrack
            progress: QbzPlayer.npProgress
            cacheProgress: QbzPlayer.npCacheProgress
            seekableMax: QbzPlayer.npSeekableMax
            durationSecs: QbzPlayer.npDurationSecs
            onSeekRequested: function (fraction) { QbzPlayer.seek(fraction) }
        }

        // === B. CONTROLS ROW — symmetric 3 columns =======================
        Item {
            id: controlsLayout
            width: parent.width
            height: parent.height - 3

            // Intrinsic boundary of the centred transport. The metadata/time
            // group may use every pixel before it, regardless of how wide the
            // nominal centre zone is at this window size.
            readonly property real transportLeft:
                centreZone.x + centreTransport.x

            // ---- LEFT column: SongCard + Sep + time column --------------
            Item {
                id: leftZone
                anchors.left: parent.left
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: Math.max(0, controlsLayout.transportLeft - 8)
                // No clip: the Row sums to exactly this column (width-59 + 19
                // + 40, spacing 0) and the two time Texts above are now
                // bounded, which was the only escape path.

                Row {
                    anchors.fill: parent
                    spacing: 0

                    // Song card: the SHARED shell/SongCard.qml in its
                    // text-center arm — 37px cover (3px surface-card border,
                    // flush to the window's left edge) + the 13px title over
                    // the [layers] · artist · album meta row. 1:1 with
                    // PlayerBarSmall.slint:238-267, which mounts the very same
                    // SongCard with these exact knobs.
                    //
                    // This REPLACES a hand-rolled 2-line copy (art + title +
                    // artist Text) that silently dropped three things living
                    // only inside SongCard: the context glyph, the album link
                    // and the floating cover preview (SongCard's artHover is
                    // the ONLY writer of QbzShell.artPreviewShow, so the
                    // shell's ArtPreviewOverlay was simply never armed in
                    // Small). Rule 5 — reuse, don't fork.
                    //
                    // The live runway is the column minus Sep + time; the
                    // shared 80% cap keeps this mode consistent with the three
                    // full bars. Height = the 39px controls row: BOTH must be
                    // explicit, because SongCard's own defaults are 200x74 and
                    // a 74px card would push the 37px cover off-centre.
                    SongCard {
                        height: parent.height
                        readonly property real availableWidth: Math.max(0,
                            parent.width - (QbzPlayer.npHasTrack ? 59 : 0))
                        width: availableWidth * theme.npbSongCardMaxFraction
                        artSize: 37
                        artBorderWidth: 3
                        artBorderColor: theme.surfaceCard
                        artTextGap: 9
                        titleFontSize: 13
                        showBadges: false
                        textCenter: true
                        onTrackInfoRequested: root.openTrackInfo()
                    }

                    // Separator: song-card | time column (8px toward the
                    // card, 10px toward the timer).
                    Sep {
                        visible: QbzPlayer.npHasTrack
                        gapLeft: 8
                        gapRight: 10
                    }

                    // Time column — fixed 40px 2-row block (elapsed /
                    // total), monospace for stable digit positions.
                    Column {
                        visible: QbzPlayer.npHasTrack
                        width: 40
                        anchors.verticalCenter: parent.verticalCenter
                        spacing: 1
                        Text {
                            // Bounded so the column below needs no scissor: a
                            // NaN transient or a >16h duration is the only way
                            // these overflow, and truncating here lands at the
                            // same x the clip did.
                            width: parent.width
                            horizontalAlignment: Text.AlignLeft
                            elide: Text.ElideRight
                            text: root.fmt(QbzPlayer.npElapsedSecs)
                            font.family: "monospace"
                            font.pixelSize: 10
                            color: theme.textMuted
                        }
                        Text {
                            width: parent.width
                            horizontalAlignment: Text.AlignLeft
                            elide: Text.ElideRight
                            text: root.fmt(QbzPlayer.npDurationSecs)
                            font.family: "monospace"
                            font.pixelSize: 10
                            color: theme.textMuted
                        }
                    }
                }
            }

            // ---- CENTER column: transport, window-centred --------------
            Item {
                id: centreZone
                anchors.left: parent.left
                anchors.leftMargin: (root.width - 6) * root.sideFrac
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: (root.width - 6) * root.colCentre
                clip: true

                // The SHARED cluster (shell/TransportControls.qml) — the
                // Small bar's own copy was a byte-for-byte twin of the full
                // bar's at playCircle:false / classicActions:false.
                TransportControls {
                    id: centreTransport
                    anchors.centerIn: parent
                    height: 34
                    compact: root.width < 900
                    playCircle: false
                    favorite: root.npFavorite
                    ephemeral: root.npEphemeral
                    tooltip: root.tooltip
                    onAddRequested: function (anchorItem) { smallAddMenu.openBelowRight(anchorItem) }
                    onTrackInfoRequested: root.openTrackInfo()
                }
            }

            // ---- RIGHT column: the control SET, right-aligned ----------
            // Order (PlayerBarSmall mount order): AudioStamp · Connect ·
            // Cast · Settings · ViewMode · Lyrics · Volume · Queue.
            Item {
                anchors.right: parent.right
                anchors.rightMargin: 6
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: (root.width - 6) * root.sideFrac
                clip: true

                // VERTICAL CENTRING IS PER-CHILD HERE. This Row is as tall as
                // the whole ~39px controls row (`height: parent.height`), and
                // a QML Row TOP-ALIGNS every child that does not anchor itself
                // — so a 32px button with no anchor rendered at y=0, flush
                // against the 3px seekbar, with all 7px of slack below it. The
                // active-state fill then read as "too big" when it was simply
                // sitting too high (the button IS 32x32, matching
                // controls/QbzIconButton.qml's btnSize and the reference's
                // 32x32 ConnectButton Rectangle).
                // The reference has no such bug because PlayerBarSmall.slint
                // builds this cluster as a HorizontalLayout, which centres by
                // default; every wrapper there is a `VerticalLayout {
                // alignment: center; }` (e.g. Connect :366-368, cast :601-603).
                // A child of a Row MAY anchor vertically — only horizontal
                // anchoring is forbidden — which is what AudioStamp and
                // QbzSlider already did, and what every sibling below now does
                // so the whole cluster sits on one line with the volume bar.
                Row {
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    height: parent.height
                    spacing: 1

                    // AudioStamp — the inline 2-row stamp (quality line over
                    // the backend/mode LEDs), FIRST in the right set.
                    // PlayerBarSmall.slint clamps it at 150px, widening to
                    // 280px when a QConnect renderer name is shown (it grows
                    // leftward into the empty space before the centre
                    // transport; the right zone's clip is the real guard).
                    AudioStamp {
                        visible: QbzPlayer.npHasTrack
                        anchors.verticalCenter: parent.verticalCenter
                        tooltipHost: root.tooltip
                        maxWidth: (QbzPlayer.npIsRemote && QbzPlayer.npCastTarget !== "")
                            ? 280 : 150
                    }

                    // Separator: AudioStamp | icon cluster (10px toward the
                    // stamp, 8px toward the buttons).
                    Sep {
                        visible: QbzPlayer.npHasTrack
                        gapLeft: 10
                        gapRight: 8
                    }

                    // Qobuz Connect — the same golden ConnectButton as the
                    // full bar (Slint keeps a file-private copy in each bar,
                    // PlayerBarSmall.slint:52-55), minus the tooltip: the
                    // reference binds NO tooltip on the small bar's button
                    // (contract §8). Opens the shared flyout.
                    Rectangle {
                        id: smallQconnectBtn
                        readonly property bool qcActive: QbzQConnect.qconnectConnected
                        readonly property color gold: "#e0b341"
                        width: 32
                        height: 32
                        radius: theme.radiusSm
                        anchors.verticalCenter: parent.verticalCenter
                        color: qcActive ? Qt.rgba(gold.r, gold.g, gold.b, 0.16)
                            : (smallQcArea.containsMouse ? theme.surfaceHover : "transparent")
                        border.width: qcActive ? 1 : 0
                        border.color: Qt.rgba(gold.r, gold.g, gold.b, 0.45)
                        QbzIcon {
                            name: "monitor-speaker"
                            width: 16
                            height: 16
                            anchors.centerIn: parent
                            tintName: smallQconnectBtn.qcActive ? "amber"
                                : smallQcArea.containsMouse ? "textPrimary" : "secondary"
                        }
                        MouseArea {
                            id: smallQcArea
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: smallQcFlyout.openAboveRight(smallQconnectBtn)
                        }
                    }
                    // Cast (Chromecast / DLNA) — opens the picker modal and,
                    // with it, device discovery (PlayerBarSmall.slint:596-611:
                    // picker-open = true + CastActions.open()); lit while a
                    // renderer is connected.
                    QbzIconButton {
                        name: "cast"
                        active: QbzPlayer.npCastActive
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzCast.openPicker()
                    }
                    // Audio settings — opens the Normalization + Gapless
                    // flyout (PlayerBarSmall.slint:620-712). NOT a toggle, and
                    // `active` is NEUTRAL here on purpose: the .slint says so
                    // out loud at :628-630 — "this button opens a flyout, so
                    // its colour should NOT reflect a toggle (normalization)".
                    // The full bar DOES mirror it; that asymmetry is the
                    // reference's, not a slip.
                    //
                    // It previously carried `property bool normOn: false` —
                    // local state that started at false regardless of what was
                    // persisted, so the first click on this button always sent
                    // `true` and SWITCHED NORMALIZATION ON.
                    QbzIconButton {
                        id: smallAudioBtn
                        name: "settings-2"
                        active: false
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: smallAudioMenu.openBelowRight(smallAudioBtn)
                    }
                    QbzIconButton {
                        id: smallViewBtn
                        name: "layout-grid"
                        active: smallViewMenu.opened
                        anchors.verticalCenter: parent.verticalCenter
                        // In kiosk the button IS the toggle, skipping the menu
                        // (`shell/PlayerBarSmall.slint:746-750`). Every other
                        // row of that flyout is gated `!kiosk-profile` in the
                        // reference, so in kiosk it would carry a single row —
                        // which the fixed-height popup floats high above the
                        // button (the reason given at `:741-745`).
                        onClicked: {
                            if (QbzShell.kioskProfile)
                                QbzSession.toggleProfile()
                            else
                                smallViewMenu.openBelowRight(smallViewBtn)
                        }
                    }
                    // HIDDEN in kiosk (PlayerBarSmall.slint:846 gates the
                    // whole block on `!ShellState.kiosk-profile`). The kiosk
                    // shell mounts no lyrics side panel, so the button would
                    // toggle a surface that does not exist -- an inert
                    // affordance, which the kiosk contract forbids (§9.2).
                    QbzIconButton {
                        visible: !QbzShell.kioskProfile
                        name: "mic-vocal"
                        active: QbzShell.lyricsOpen
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzShell.toggleLyrics()
                    }

                    Item { width: 4; height: 1 }

                    // Volume — mute icon + (WIDE) inline horizontal slider.
                    // Both gate on root.volLocked (§11.3), like the full bar.
                    QbzIconButton {
                        name: QbzPlayer.npMuted ? "volume-x" : "volume-2"
                        btnEnabled: !root.volLocked
                        active: QbzPlayer.npMuted
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzPlayer.toggleMute()
                    }
                    // WIDE: the shared QbzSlider (PlayerBarSmall mounts the
                    // same primitive at 72px on the 0..1000 scale).
                    QbzSlider {
                        enabled: !root.volLocked
                        visible: root.volumeWide
                        width: 72
                        anchors.verticalCenter: parent.verticalCenter
                        minimum: 0
                        maximum: 1000
                        value: Math.round(QbzPlayer.npVolume * 1000)
                        onChanged: function (v) { QbzPlayer.setVolume(v / 1000.0) }
                        // Persist only the settled value, like the full bar.
                        onReleased: function (v) { QbzPlayer.persistVolume(v / 1000.0) }
                    }

                    Item { width: 4; height: 1 }

                    // Queue panel toggle. HIDDEN in kiosk for the same
                    // reason as the lyrics button above
                    // (PlayerBarSmall.slint:983) -- the kiosk reaches its
                    // queue through the Now Playing view's Up Next tab, and
                    // the side panel is never mounted.
                    QbzIconButton {
                        visible: !QbzShell.kioskProfile
                        name: "list-ordered"
                        active: QbzShell.queueOpen
                        anchors.verticalCenter: parent.verticalCenter
                        onClicked: QbzShell.toggleQueue()
                    }
                }
            }
        }
    }

    // The Now-Playing-view mode menu (phase 18 — shared with PlayerBar).
    ViewModeMenu { id: smallViewMenu }

    // Audio settings flyout — Normalization + Gapless (PlayerBarSmall.slint's
    // `audio-menu` PopupWindow). Guarded parse, the PlaylistView.qml:55-61
    // idiom: a raw JSON.parse in a binding throws on the pre-publish frame and
    // takes the whole bar down with it.
    readonly property var settingsDoc: parseSettings()
    // A-B loop entries for the "+" menu (see PlayerBar.abEntries): local
    // playback only.
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
    function parseSettings() {
        try {
            return JSON.parse(QbzBridge.settingsJson)
        } catch (e) {
            return ({})
        }
    }
    AudioSettingsMenu { id: smallAudioMenu; doc: root.settingsDoc }

    // Qobuz Connect device flyout — the ONE shared component both bars mount
    // (contract §8; the Slint `qconnect-menu` PopupWindow). Opened above-
    // right of the Connect button so it never covers its own trigger.
    QconnectFlyout { id: smallQcFlyout }

    // "Add to…" flyout behind the transport "+" (TransportControls.slint's
    // add-menu) — same seven entries, order and icons as the full bar.
    CardMenu {
        id: smallAddMenu
        menuWidth: 232
        entries: {
            // The three QUEUE actions are the only ones an ephemeral track can
            // honour; the rest write a reference that outlives the session.
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
            // "Add to playlist" is SECOND (TransportControls.slint:143 — the
            // full bar carries the rationale). Same `npSource` gate as the
            // mixtape entry: the picker's Qobuz arm takes catalog ids and an
            // `isLocal` current row is four different id spaces this bar
            // cannot tell apart, so unknown provenance drops the entry rather
            // than rendering one that cannot work.
            if (root.npSource === "qobuz") {
                m.splice(1, 0, {
                    "label": QbzSession.tr("Add to playlist", QbzSession.trRev),
                    "icon": "list-music",
                    "action": "playlist"
                })
            }
            // Only when the track's source is KNOWN (see `npSource`).
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
            return m.concat(root.abEntries())
        }
        onPicked: function (a) {
            var id = QbzPlayer.npTrackId
            if (a === "favorite") {
                if (id !== "") QbzQueue.queueToggleFavorite("track", id)
            } else if (a === "queue" || a === "later" || a === "next") {
                if (id !== "") QbzPlayer.enqueueTrack(id, a)
            } else if (a === "playlist") {
                // Only built when `npSource === "qobuz"`, so `id` is a catalog
                // id here by the gate above.
                if (id !== "") QbzPlaylistPicker.openForTrack(id)
            } else if (a === "album-favorite") {
                if (QbzPlayer.npAlbumId !== "")
                    QbzLibrary.libraryToggleFavorite("album", QbzPlayer.npAlbumId)
            } else if (a === "ab-mark") {
                QbzPlayer.abMark()
            } else if (a === "ab-clear") {
                QbzPlayer.abClear()
            } else if (a === "mixtape") {
                // MyQBZ AddItem, built here from the now-playing state:
                // `npArtworkPath` is a file:// CACHE path, so it is NOT the
                // artworkUrl — the store would keep a dead local path.
                // Source from the queue's current ROW (`npSource`), not a
                // literal; the entry is absent when that row cannot answer.
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
