// MiniFooter — the miniplayer's control strip (2026-08-03 miniplayer/tray
// contract A-28, §4.3.2), port of
// `crates/qbz-ui/ui/miniplayer/MiniFooter.slint` (396 L, read whole).
//
// THREE LAYOUTS IN ONE COMPONENT, and its `mode` is numbered the OPPOSITE way
// from the surface enum (§15 trap 2): 0 full · 1 compact · 2 micro, against the
// surface's 0 micro · 1 compact · 2 artwork. MiniShell computes the inversion
// once into `footerMode` and this file consumes it — it never reads
// QbzMini.surface itself.
//
// In micro the footer IS the card: surface 0 has no surface area at all, the
// window is 380 x 57 and this component fills the 45 px card (§13-D10).
//
// THE PHANTOM SPACER (§15 trap 12). In full and compact the transport is
// centred by a trailing invisible box the width of the volume button
// (:318-319), NOT by a centred layout — the reference's own comment at :294-296
// says so. Drop it and the transport drifts right by half the volume button.
// MICRO HAS NO VOLUME CONTROL AND THEREFORE NO PHANTOM: it is the one mode that
// is genuinely centred, which is why the rule is scoped to full/compact.
//
// EVERY PER-MODE NUMBER IS A CACHED READONLY PROPERTY (§7-M2). The reference
// caches six and writes a seventh — the volume trigger's glyph size — inline at
// its mount (:306); six-plus-one is the inconsistency LESSONS L5 calls the
// smell, so all of them are cached here, including the paddings and the seek
// sizes the reference also spells inline.
//
// The menu trigger remains visible while the mini is open; leaving the card
// collapses its controls.
//
// The two mode branches are Loaders for the same reason Slint uses `if`
// (:263, :332) rather than a visibility flip — an always-visible surface pays
// for every live binding it keeps, and only one of the two layouts is ever on
// screen.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root

    /// 0 full · 1 compact · 2 micro — MiniShell's `footerMode`, already
    /// inverted. NOT the surface id.
    property int mode: 0
    /// The MINI window (§8 rule 3), for startSystemMove().
    property var hostWindow: null
    /// Leaving the card collapses the menu without hiding its trigger.
    property bool windowHovered: false

    QbzTheme { id: theme }

    // --- The cached per-mode tokens (§7-M2, §4.3.2's table) ----------------
    readonly property bool compactMode: root.mode === 1
    readonly property int tbtn: root.compactMode ? 26 : 30
    readonly property int tgap: root.compactMode ? 14 : 18
    readonly property int icShuffle: root.compactMode ? 13 : 16
    readonly property int icSkip: root.compactMode ? 15 : 18
    readonly property int icPlay: root.compactMode ? 16 : 20
    readonly property int ctrlH: root.compactMode ? 34 : 40
    /// THE SEVENTH (:306) — inline in the reference, cached here.
    readonly property int icVolume: root.compactMode ? 15 : 18
    // The paddings and the seek sizing, same treatment (:266-274, :284).
    readonly property int padH: root.compactMode ? 10 : 14
    readonly property int padBottom: root.compactMode ? 6 : 10
    readonly property int seekH: root.compactMode ? 10 : 14
    readonly property int seekTrack: root.compactMode ? 2 : 3
    readonly property int timeFont: root.compactMode ? 10 : 11

    readonly property bool hasTrack: QbzPlayer.npHasTrack
    readonly property bool canPlay: root.hasTrack || QbzQueue.hasPlayTarget

    // --- The pending-seek machine (:214-249) -------------------------------
    // All three constants are load-bearing: the -1.0 SENTINEL (every test is
    // `>= 0`, so 0.0 is a legitimate position and not "none"), the 0.02
    // convergence window, and the 8 s watchdog.
    //
    // The problem it solves: the engine takes a moment to seek, and
    // QbzPlayer.npProgress keeps reporting the OLD position until it lands. A
    // bar bound straight to npProgress therefore snaps back under the cursor
    // after every click. `effProgress` holds the requested position until the
    // player converges on it — or until the watchdog gives up, which is what
    // keeps a failed seek from freezing the bar forever.
    property real dragPreview: -1.0
    property real pendingSeek: -1.0
    property bool dragging: false
    readonly property real effProgress: (root.dragging && root.dragPreview >= 0)
                                        ? root.dragPreview
                                        : (root.pendingSeek >= 0 ? root.pendingSeek
                                                                 : QbzPlayer.npProgress)

    // The reference watches a mirrored copy of the progress (`watch-progress`,
    // :222-228) because a Slint global cannot carry a `changed` handler; in Qt
    // the singleton's own notify signal is the same edge, with no mirror.
    Connections {
        target: QbzPlayer
        function onNpProgressChanged() {
            if (root.pendingSeek >= 0
                    && Math.abs(QbzPlayer.npProgress - root.pendingSeek) < 0.02) {
                root.pendingSeek = -1.0
                // The reference leaves the watchdog armed here (:223-228 clears
                // only the sentinel) and lets it fire into a no-op. Stopping it
                // on convergence is one wakeup fewer on a window whose whole
                // point is to be left running (§7).
                pendingTimer.stop()
            }
        }
    }

    // T2 (§4.12) — 8 s one-shot, re-armed on every commit. A Timer has no
    // `parent` (§8 rule 2); everything it touches is reached through `root`.
    Timer {
        id: pendingTimer
        interval: 8000
        repeat: false
        onTriggered: root.pendingSeek = -1.0
    }

    function previewSeek(f) {
        root.dragging = true
        root.dragPreview = f
    }

    function commitSeek(f) {
        root.dragging = false
        root.dragPreview = -1.0
        root.pendingSeek = f
        QbzPlayer.seek(f)
        pendingTimer.restart()
    }

    /// m:ss. The four now-playing bars each declare this locally
    /// (PlayerBar.qml:211-215 and its three twins); there is no shared
    /// formatter in the tree, so the footer declares ONE rather than a copy per
    /// row (§7-M2, L5).
    function fmt(secs) {
        var m = Math.floor(secs / 60)
        var s = Math.floor(secs % 60)
        return m + ":" + (s < 10 ? "0" : "") + s
    }

    property bool backgroundActive: false
    readonly property bool lightForeground: backgroundActive && theme.isDark
    color: root.backgroundActive
           ? Qt.rgba(theme.surfaceCard.r, theme.surfaceCard.g, theme.surfaceCard.b, 0.20)
           : theme.surfaceCard

    /// The CARD's corner radius, handed down by MiniShell.
    ///
    /// The footer must round its own corners, because the card CANNOT round
    /// them for it: `qml/theme/RoundedImage.qml:3-6` records as measured fact
    /// on this Qt build that a Rectangle with `radius` + `clip: true` does NOT
    /// clip its children to the curve — "the child paints square over the
    /// rounded fill". This footer is an opaque Rectangle anchored to the card's
    /// bottom edge, so without this it paints square corners straight over the
    /// card's rounded ones. That is exactly what the owner photographed against
    /// a white background on 2026-08-04: every surface rounded on top and
    /// SQUARE at the bottom, and the micro card — where the footer IS the whole
    /// card — square on all four.
    ///
    /// Per-corner radii need Qt 6.7+; this tree builds against 6.11.
    property real cardRadius: 0

    // In micro the footer is the entire card, so all four corners are its own.
    // In full and compact it owns the bottom two and the surface area above it
    // owns nothing (that Item is transparent and paints no corner).
    topLeftRadius: root.mode === 2 ? root.cardRadius : 0
    topRightRadius: root.mode === 2 ? root.cardRadius : 0
    bottomLeftRadius: root.cardRadius
    bottomRightRadius: root.cardRadius

    // ======================================================================
    // FULL / COMPACT (:263-329)
    // ======================================================================
    Loader {
        active: root.mode !== 2
        anchors.fill: parent
        anchors.topMargin: 0
        anchors.leftMargin: root.padH
        anchors.rightMargin: root.padH
        anchors.bottomMargin: root.padBottom

        sourceComponent: Component {
            Item {
                MiniSeek {
                    lightForeground: root.lightForeground
                    id: seek
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.right: parent.right
                    height: root.seekH
                    trackHeight: root.seekTrack
                    progress: root.effProgress
                    onPreview: function (f) { root.previewSeek(f) }
                    onCommit: function (f) { root.commitSeek(f) }
                }

                // The times row (:280-292). It takes whatever the seek bar and
                // the controls row leave, which is how the reference's
                // VerticalLayout distributes it — 16 px at full, 14 at compact.
                Item {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: seek.bottom
                    anchors.bottom: ctrlRow.top

                    Text {
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        text: root.fmt(QbzPlayer.npElapsedSecs)
                        color: root.lightForeground ? "#e0ffffff" : theme.textSecondary
                        font.pixelSize: root.timeFont
                    }
                    Text {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        // EXACTLY ONE minus sign (§15 trap 14). The trap is
                        // about the SLINT property `NowPlayingState.remaining`,
                        // which arrives pre-signed from
                        // `crates/qbz/src/playback.rs:1679-1682`; the Qt bridge
                        // publishes no such property (there is no np_remaining
                        // in src/player_bridge.rs), so this is the tree's own
                        // idiom — PlayerBar.qml:339 — which supplies the sign
                        // itself.
                        text: "-" + root.fmt(Math.max(0, QbzPlayer.npDurationSecs
                                                         - QbzPlayer.npElapsedSecs))
                        color: root.lightForeground ? "#e0ffffff" : theme.textSecondary
                        font.pixelSize: root.timeFont
                    }
                }

                // The controls row (:297-328).
                Item {
                    id: ctrlRow
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    height: root.ctrlH

                    MiniVolume {
                        lightForeground: root.lightForeground
                        id: vol
                        // Above the transport, so its popup and the
                        // click-outside layer inside it are not painted under
                        // the buttons they have to cover.
                        z: 2
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        btn: root.tbtn
                        iconSize: root.icVolume
                    }

                    // THE PHANTOM SPACER (:318-319). Invisible, no children,
                    // exactly the volume button's width — it exists so the
                    // transport's two stretch spacers are balanced and the
                    // cluster lands on the WINDOW's centre line rather than on
                    // the centre of what is left after the volume button.
                    Item {
                        id: phantom
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        width: root.tbtn
                        height: root.tbtn
                    }

                    MiniTransport {
                        lightForeground: root.lightForeground
                        id: transport
                        anchors.verticalCenter: parent.verticalCenter
                        // Centred in the band BETWEEN the volume button and the
                        // phantom — the two equal stretch spacers, written out.
                        // Because the two ends are the same width this lands on
                        // the row's centre, which is the point of the phantom.
                        x: vol.x + vol.width
                           + Math.round((phantom.x - vol.x - vol.width - width) / 2)
                        btn: root.tbtn
                        gap: root.tgap
                        icShuffle: root.icShuffle
                        icSkip: root.icSkip
                        icPlay: root.icPlay
                        btnEnabled: root.hasTrack
                        playEnabled: root.canPlay
                    }

                    // The capsule (:322-327): right-aligned, vertically centred
                    // on the controls row, above everything including the
                    // volume popup's dismiss layer.
                    Loader {
                        z: 3
                        active: QbzMini.open
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        sourceComponent: Component {
                            MiniWindowControls {
                                windowHovered: root.windowHovered
                                activeSurface: QbzMini.surface
                                hostWindow: root.hostWindow
                            }
                        }
                    }
                }
            }
        }
    }

    // ======================================================================
    // MICRO (:332-395) — 1 + 17 + 16 + 4 = 38 px of content in the 45 px card
    // ======================================================================
    Loader {
        active: root.mode === 2
        anchors.fill: parent
        anchors.leftMargin: 8
        anchors.rightMargin: 8
        anchors.topMargin: 1
        // No bottom margin — the reference sets none (:335-338).

        sourceComponent: Component {
            Item {
                // The header: one elided line, and the whole of it is a drag
                // handle.
                Item {
                    id: microHeader
                    anchors.top: parent.top
                    anchors.left: parent.left
                    anchors.right: parent.right
                    height: 17

                    Text {
                        anchors.fill: parent
                        // The separator is a LITERAL " - " (space, hyphen-minus,
                        // space) — :347, not an en dash and not translated.
                        text: QbzPlayer.npHasTrack
                              ? (QbzPlayer.npArtist !== ""
                                 ? QbzPlayer.npTitle + " - " + QbzPlayer.npArtist
                                 : QbzPlayer.npTitle)
                              : QbzSession.tr("No track playing", QbzSession.trRev)
                        color: root.lightForeground ? "#e0ffffff" : theme.textSecondary
                        font.pixelSize: 11
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                        elide: Text.ElideRight
                    }

                    // THE DRAG HANDLE (:356-362). PRESSED, NOT CLICKED (§15
                    // trap 8): startSystemMove() hands the pointer grab to the
                    // compositor, so the release never returns to Qt and a
                    // clicked handler would never fire. It covers the whole
                    // header because the reference's TouchArea sets no geometry
                    // — in micro there is nothing else in this strip to hit.
                    MouseArea {
                        anchors.fill: parent
                        onPressed: {
                            if (root.hostWindow)
                                root.hostWindow.startSystemMove()
                        }
                    }

                    // The capsule, with the EXTRA 8 px inset the full/compact
                    // mount does not have (:364).
                    //
                    // Vertically centred on the CARD, not on this 17 px header
                    // — owner call, 2026-08-04. The reference centres it on the
                    // header, but in micro the header is a 17 px strip at the
                    // TOP of a 45 px card while the capsule is 26 px tall, so
                    // the reference's own arithmetic leaves it overflowing
                    // upward and visibly high. `root` is the footer, which in
                    // micro IS the whole card (§13-D10), so its centre is the
                    // card's centre. Qt anchors require a parent or sibling;
                    // the footer is neither here. Use its mapped centre.
                    Loader {
                        z: 1
                        active: QbzMini.open
                        anchors.right: parent.right
                        anchors.rightMargin: 8
                        y: root.mapToItem(microHeader, 0, root.height / 2).y - height / 2
                        sourceComponent: Component {
                            MiniWindowControls {
                                windowHovered: root.windowHovered
                                activeSurface: QbzMini.surface
                                hostWindow: root.hostWindow
                            }
                        }
                    }
                }

                // The controls strip (:371-384). NO VOLUME, therefore NO
                // PHANTOM — micro is the one mode that is genuinely centred
                // (`HorizontalLayout { alignment: center }`, :373-374).
                Item {
                    id: microCtrl
                    anchors.top: microHeader.bottom
                    anchors.left: parent.left
                    anchors.right: parent.right
                    height: 16

                    MiniTransport {
                        lightForeground: root.lightForeground
                        anchors.horizontalCenter: parent.horizontalCenter
                        anchors.verticalCenter: parent.verticalCenter
                        btn: 16
                        gap: 9
                        icShuffle: 8
                        icSkip: 9
                        icPlay: 9
                        btnEnabled: root.hasTrack
                        playEnabled: root.canPlay
                    }
                }

                // The bottom seek (:387-394): 4 px tall, a 1 px SQUARE-ENDED
                // track. The reference's comment calls it "full-bleed"; the
                // layout's 8 px side padding says otherwise and the CODE wins
                // (§12-P6).
                MiniSeek {
                    lightForeground: root.lightForeground
                    anchors.top: microCtrl.bottom
                    anchors.left: parent.left
                    anchors.right: parent.right
                    height: 4
                    trackHeight: 1
                    rounded: false
                    progress: root.effProgress
                    onPreview: function (f) { root.previewSeek(f) }
                    onCommit: function (f) { root.commitSeek(f) }
                }
            }
        }
    }
}
