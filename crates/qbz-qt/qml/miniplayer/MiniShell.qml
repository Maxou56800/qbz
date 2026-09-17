// MiniShell — the miniplayer CARD (2026-08-03 miniplayer/tray contract A-28,
// §4.3.1), port of `crates/qbz-ui/ui/miniplayer/MiniShell.slint:15-65`.
//
// The card is the window minus the 6 px gutter, so every number here is
// relative to it (§15 trap 3). It owns three things: the card chrome, the
// optional artwork backdrop, and the surface router.
//
// The mini INHERITS the user's theme. The reference cannot — its only
// SlintTheme writer takes &AppWindow (crates/qbz/src/theme.rs:96-103), so the
// Slint miniplayer always renders the compiled-in Dark palette even for a user
// on a light theme. Here QbzShell.themeJson is process-global and every
// QbzTheme instance binds it, so reproducing that defect would take extra code
// (§13-D16).

import QtQuick
import QtQuick.Effects
import com.blitzfc.qbz
import "../immersive"
import "../shell"
import "../theme"

Rectangle {
    id: root

    QbzTheme { id: theme }

    // §8 rule 3: the window, handed down explicitly by MiniWindow. Consumed by
    // the footer's drag handle and the hover capsule, which call
    // startSystemMove() on it rather than walking a parent chain.
    property var hostWindow: null

    // A whole-card hover sensor keeps the capsule open across its buttons.
    HoverHandler { id: cardHover }

    // --- Cached per-mode values (§7-M2) ------------------------------------
    // The reference computes each of these ONCE into a named property
    // (MiniShell.slint:18, :23, :62). An always-visible surface pays a repeated
    // inline ternary on every frame, which is exactly what the perf doctrine
    // refuses.
    readonly property int surfaceId: QbzMini.surface

    // The software-renderer arm, same detection as theme/RoundedImage.qml:223.
    // Offscreen forces software, so the masked backdrop below degrades to an
    // unmasked one there — which is exactly the environment the gate runs in
    // and never a user's.
    readonly property bool _noShaders: GraphicsInfo.api === GraphicsInfo.Software

    // THE INVERSION (§15 trap 2). The footer's `mode` is numbered the OPPOSITE
    // way from the surface enum — 0 full · 1 compact · 2 micro
    // (MiniFooter.slint:212) against 0 micro · 1 compact · 2 artwork — so
    // surface 0 -> mode 2, 1 -> 1, 2/3/4 -> 0 (MiniShell.slint:17-18). Computed
    // here, once; MiniFooter (B3) mounts against it.
    readonly property int footerMode: root.surfaceId === 0
                                      ? 2 : (root.surfaceId === 1 ? 1 : 0)

    // MiniShell.slint:62, with ONE ruling applied: micro's 50 px binding inside
    // a 45 px clipped card becomes 45 (§13-D10). The micro content is 38 px
    // tall, so nothing is lost, and a 50 px child of a 45 px clip:true card is
    // not a shape worth reproducing.
    readonly property int footerHeight: root.surfaceId === 1
                                        ? 64 : (root.surfaceId === 0 ? 45 : 80)

    // EXPLICIT SOURCE — the one datum the mini needs that QbzPlayer does not
    // publish. Slint reads NowPlayingState.explicit; there is no np_explicit
    // (src/player_bridge.rs:31-122). The queue document's `current` row carries
    // the SAME datum (QueueTrack::parental_warning -> src/queue_qt.rs:47), so
    // this is the guarded try/catch + track-id guard of
    // qml/immersive/ImmersiveSongCard.qml:45-53 — the id guard is what keeps a
    // stale queue document from lending the PREVIOUS track's badge to this one.
    //
    // Derived ONCE here and passed DOWN to the surfaces as an explicit
    // property: both of them need it, and a second JSON.parse per queue publish
    // is precisely the per-frame work §7 exists to refuse. It is also §8's
    // rule 1 — the surfaces take a property, they do not read `parent`.
    readonly property bool npExplicit: {
        try {
            var d = JSON.parse(QbzQueue.queueJson)
            return !!(d && d.current && d.current.id === QbzPlayer.npTrackId
                      && d.current.explicit)
        } catch (e) {
            return false
        }
    }

    // Inherit the app background. The mini's blur button can override it
    // with its static artwork backdrop; turning that off restores the app mode.
    readonly property bool useAppBackground: theme.ambientOn && !QbzMini.backgroundBlur
    readonly property bool backdropOn: root.useAppBackground || QbzMini.backgroundBlur
    readonly property bool backdropMounted: QbzMini.open && root.backdropOn

    color: theme.surfaceMain
    border.width: 1
    border.color: theme.alphaTier(10)
    // 9 px for micro + compact, 10 px otherwise (§15 trap 6).
    radius: root.surfaceId <= 1 ? 9 : 10
    antialiasing: true
    clip: true

    // One rounded mask for either background, shared by all five surfaces.
    Item {
        id: backdrop
        objectName: "miniBackdrop"
        anchors.fill: parent
        visible: root.backdropMounted
        clip: true
        layer.enabled: root.backdropMounted && !root._noShaders
        layer.smooth: true
        layer.effect: MultiEffect {
            maskEnabled: true
            maskSource: backdropMask
            maskThresholdMin: 0.5
            maskSpreadAtMin: 1.0
        }
        Loader {
            anchors.fill: parent
            active: root.backdropMounted && root.useAppBackground
            sourceComponent: AppBackground {
                hostWindow: root.hostWindow
            }
        }
        Loader {
            anchors.fill: parent
            active: root.backdropMounted && QbzMini.backgroundBlur && QbzPlayer.npHasTrack
            sourceComponent: ImmersiveAtmosphere {
                source: QbzImmersive.atmosphereUrl
                fallbackSource: QbzPlayer.npArtworkPath
                animated: false
                dim: theme.isDark ? 0.54 : 0.0
                baseOpacity: 0.72
                warpOpacity: 0.20
            }
        }
        Rectangle {
            anchors.fill: parent
            visible: QbzMini.backgroundBlur
            color: theme.surfaceMain
            opacity: 0.75
        }
    }
    Item {
        id: backdropMask
        anchors.fill: parent
        visible: false
        layer.enabled: root.backdropMounted && !root._noShaders
        layer.smooth: true
        Rectangle {
            anchors.fill: parent
            radius: root.radius
            color: "#ffffff"
        }
    }

    // Below every interactive surface: controls retain the pointer grab.
    // Queue/lyrics reserve their content area for scrolling, even in gaps.
    MouseArea {
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton
        onPressed: function(mouse) {
            if ((root.surfaceId === 3 || root.surfaceId === 4)
                    && mouse.y < root.height - root.footerHeight) {
                mouse.accepted = false
                return
            }
            if (root.hostWindow)
                root.hostWindow.startSystemMove()
        }
    }

    // --- The surface area (MiniShell.slint:43-55) --------------------------
    // Absent in micro, where the footer IS the card. Its height is the card
    // minus the footer, which is how §4.2's "surface-area H" column is
    // reproduced: 102 at compact, 448 at the 540-tall expanded default.
    //
    // MiniFooter mounts under it; the space it occupies is reserved here, by
    // footerHeight, because that reservation is what makes the surface geometry
    // above it correct in the first place.
    Item {
        id: surfaceArea
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: root.height - root.footerHeight
        visible: root.surfaceId !== 0
        clip: true

        // The router: one gated Loader per surface, each with its own INLINE
        // component (§8 rule 4 — not one Loader with a `source` string, whose
        // item is an untyped QObject).
        //
        // Only the two DISPLAY surfaces take npExplicit. The queue rows carry
        // their own `explicit` flag per row (mini_qt.rs's MiniQueueRow) and the
        // lyrics surface renders no badge at all, so handing either of them the
        // now-playing flag would be a property nothing reads.
        Loader {
            anchors.fill: parent
            active: root.surfaceId === 1
            sourceComponent: Component {
                MiniCompactSurface { npExplicit: root.npExplicit; backdropActive: root.backdropOn }
            }
        }
        Loader {
            anchors.fill: parent
            active: root.surfaceId === 2
            sourceComponent: Component {
                MiniArtworkSurface { npExplicit: root.npExplicit; backdropActive: root.backdropOn }
            }
        }
        // Both of these carry `QbzMini.open` in their gate, and that term is
        // not decoration. The mini window is created ONCE and never destroyed
        // (Main.qml only hides it), so a Loader gated on `surfaceId` ALONE
        // keeps its surface mounted after the user leaves the mini — and both
        // of these surfaces re-parse a whole JSON document on every publish.
        // A user who opened the mini once on the queue surface would then pay
        // a full parse of a 2,000-track document on the GUI thread on every
        // enqueue and every skip, for the rest of the session, with the mini
        // nowhere on screen. §7-M1's hazard is stated in exactly those words:
        // work that continues "including while the mini is closed".
        Loader {
            anchors.fill: parent
            active: QbzMini.open && root.surfaceId === 3
            sourceComponent: Component {
                MiniQueueSurface { }
            }
        }
        Loader {
            anchors.fill: parent
            active: QbzMini.open && root.surfaceId === 4
            sourceComponent: Component {
                MiniLyricsSurface { }
            }
        }
    }

    // --- The footer (MiniShell.slint:59-63) --------------------------------
    // In micro it IS the card: `surfaceArea` is invisible at surface 0 and this
    // fills all 45 px. Its `mode` is the INVERTED number (§15 trap 2), computed
    // once above.
    //
    // Declared AFTER surfaceArea so the hover capsule — which overflows the
    // 17 px micro header by 4.5 px in each direction — paints above it.
    // INSET BY THE BORDER WIDTH on the three edges it touches, and that is a
    // measured fix, not a hunch: Qt draws a Rectangle's border INSIDE its
    // bounds, so the card's visible fill is already 1 px in on each side —
    // while a child anchored to the card's outer edges paints straight over
    // the hairline. Measured on the owner's 2026-08-04 screenshot: the artwork
    // and meta rows span 366 px, the seek and transport rows 368 px, so the
    // footer reached 1 px further out on each side and cut the card's outline
    // exactly where it began. Micro was the one mode that looked right because
    // there the footer IS the card (§13-D10) and there are no two layers to
    // misalign.
    //
    // Its corner radius shrinks by the same amount so the curve nests INSIDE
    // the card's curve instead of racing it.
    //
    // No gap opens above it: `surfaceArea` still ends at `height -
    // footerHeight`, so the inset footer's top edge sits 1 px higher and the
    // two overlap rather than part.
    MiniFooter {
        id: footer
        objectName: "miniFooter"
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.leftMargin: root.border.width
        anchors.rightMargin: root.border.width
        anchors.bottomMargin: root.border.width
        height: root.footerHeight
        backgroundActive: root.backdropOn
        mode: root.footerMode
        cardRadius: root.radius - root.border.width
        hostWindow: root.hostWindow
        windowHovered: cardHover.hovered
    }
}
