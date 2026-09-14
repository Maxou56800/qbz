// QbzCircleAction — the circular header action, BOTH arms of Slint
// primitives/CircleAction.slint. Consolidated in phase 22 from THREE copies
// (AlbumView.CircleBtn ≡ ArtistView.CircleBtn, PlaylistView.CircleBtn =
// superset with the `primary` + `btnEnabled` arms).
//
// Numbers, read off primitives/CircleAction.slint:
//   :24  diameter — primary 44, secondary 32 (the hierarchy lives HERE, not
//        in the callers; the port had 40 and was uniformly small). Compact
//        headers may deliberately collapse the primary to the secondary
//        footprint without giving up its accent palette.
//   :66  glyph — primary 19, secondary 15; the compact primary follows the
//        secondary footprint here too.
//   :48  ON-SURFACE arm (`overlay: false`, the default): primary = accent
//        disc, secondary = surface-elevated disc, hover/active surface-hover
//   :60  ring — border-strong (the port used border-muted)
//   :70  glyph tint — active accent; else primary WHITE, secondary
//        text-primary. The port painted the on-surface primary glyph BLACK,
//        which is the OTHER arm's colour.
//   :56  OVERLAY arm (`overlay: true`) — for buttons sitting on the dark
//        artwork atmosphere (the album / artist header once the header
//        gradient is on): primary = solid white disc with a BLACK glyph,
//        secondary = #ffffff24 fill (#ffffff3d hover/active), 1.5px
//        #ffffffcc ring, white glyph.
// NOTE: the card hover-overlay circles are still the separate
// CardOverlayButton.qml — same palette, different sizing contract.
//
// --- LIGHT-THEME LEGIBILITY (Slint 2.0.2, "CircleAction buttons legible on
// --- light themes" — docs/release-2.0.2/CHANGELOG.md:213) ----------------
// The `on-surface` arm IS that fix: CircleAction.slint:30-37 exists exactly
// because "the default light-on-dark palette (white fills/rings/icons)
// vanishes on light themes' plain surface". Its secondary glyph is
// `Theme.text-primary` (CircleAction.slint:73) — the theme's max-contrast
// colour, DARK on a light theme.
//
// The port could not express that at first: icon tints were PRE-BAKED SVG
// variants only, and the "primary" bake is a hardcoded `fill="#ffffff"` — it
// is NOT text-primary, it is white. So the on-surface secondary (a
// `surface-elevated` disc, i.e. near-white on a light theme) painted a WHITE
// glyph on it: the exact vanishing act the .slint arm was written to stop.
// The stopgap was `onSurfaceTint = theme.isDark ? "primary" : "black"`, a
// 2-value approximation of text-primary.
//
// It is no longer needed. QbzIcon resolves theme-following tints through
// src/icon_tint_qt.rs, which bakes the live theme's ACTUAL text-primary hex,
// so this arm now says "textPrimary" and is exact under all 36 themes.
//
// The OVERLAY arm keeps its literals: it sits on hosts that are
// theme-independent — the artwork band, the solid white disc — and Slint
// hardcodes them for the same reason (CircleAction.slint:70-74, `#ffffff` /
// `#000000` literals next to `Theme.text-primary`). It spells them
// "white"/"black" now, not "primary", so no one re-reads the legacy alias as
// the theme token.
//
// The on-surface PRIMARY glyph USED to be in that group and is not any more.
// Its host is NOT theme-independent — it is `theme.accent` — and the fixed
// #ffffff CircleAction.slint:72 puts on it is unreadable on 16 of the 35
// shipped palettes (worst: high-contrast 1.70:1, then ikari 1.74, wcag-dark
// 1.82). It now takes `theme.accentGlyphTint`, the measured
// on-accent selector; see the note at the tint binding and the long rationale
// in theme/QbzTheme.qml.

import QtQuick
import com.blitzfc.qbz
import "../theme"

Rectangle {
    id: root
    property string name: ""
    property bool active: false
    property bool primary: false
    /// Preserve the primary action palette while using the secondary action's
    /// footprint. Album headers use this when their global compact mode is on.
    property bool compactPrimary: false
    /// Optional footprint for exceptionally dense hosts. Zero keeps the
    /// canonical 44/32px sizes; compact album headers use 28px for every
    /// action so Play no longer dominates the row.
    property int diameterOverride: 0
    property bool btnEnabled: true
    /// While the action's work is in flight the glyph swaps for a spinning
    /// arc (CircleAction.slint:18 / :77 — the reference has this arm and the
    /// port did not, so every slow header action read as a dead click).
    /// Clicks are gated for the duration: a second click cannot start the
    /// same work twice, and it cannot re-open a dropdown over a request the
    /// user already made.
    property bool loading: false
    /// Light-on-dark palette for a button painted over artwork/atmosphere
    /// (CircleAction.slint's default arm; `on-surface: false` there).
    property bool overlay: false
    signal clicked(var mouse)

    QbzTheme { id: theme }

    /// Slint's `Theme.text-primary` glyph tint (CircleAction.slint:73),
    /// runtime-tinted so it is the real token under every theme.
    readonly property string surfaceTint: "textPrimary"

    readonly property bool largePrimary: primary && !compactPrimary
    readonly property int resolvedDiameter: diameterOverride > 0
        ? diameterOverride : (largePrimary ? 44 : 32)

    width: resolvedDiameter
    height: resolvedDiameter
    radius: width / 2
    color: overlay
        ? (primary
            ? (cbArea.containsMouse && btnEnabled ? "#d6ffffff" : "#ffffff")
            : ((cbArea.containsMouse || active) ? "#3dffffff" : "#24ffffff"))
        : (primary
            ? (cbArea.containsMouse && btnEnabled ? theme.accentHover : theme.accent)
            // Resting fill translucent under the dynamic background
            // (CircleAction.slint:53-55). The overlay variant above sits on
            // artwork and keeps its white scale.
            : ((cbArea.containsMouse || active)
                ? theme.elevatedHoverFill
                : theme.elevatedFill))
    border.width: primary ? 0 : 1.5
    border.color: overlay ? "#ccffffff" : theme.borderStrong
    // A loading button keeps FULL opacity — it is working, not unavailable,
    // and dimming it would say the opposite of what the spinner says. Only
    // `btnEnabled` dims.
    opacity: btnEnabled ? 1.0 : 0.4

    // --- The spinner arm --------------------------------------------------
    // ON THE SHARED SHELL PULSE, never a RotationAnimation. Qt Quick has no
    // partial redraws, so any continuous animator presents the WHOLE window at
    // display rate — ~1.2% GPU per present on the owner's hybrid laptop
    // (qt-frontend/2026-08-11-scenegraph-batches §9, THE PULSE LAW). A 60 Hz
    // RotationAnimation for the two-to-five seconds a radio takes to build is
    // exactly the bill that doctrine exists to refuse; theme/QbzSpinner.qml
    // still uses one and is NOT reused here for that reason.
    //
    // 12 degrees per ~30 Hz pulse = one turn per second, the same rate and the
    // same arithmetic as the offline manager's downloading glyph
    // (OfflineManagerView.qml:154) and the transport's loading ring.
    //
    // The Connections is DISABLED unless the button is actually spinning and
    // on screen, so at rest this component writes nothing at all — the other
    // half of the law.
    property real spinPhase: 0
    Connections {
        target: QbzShell
        enabled: root.loading && root.visible
        function onPulseMsChanged() { root.spinPhase = (root.spinPhase + 12) % 360 }
    }
    QbzIcon {
        visible: root.loading
        name: "loader-circle"
        // CircleAction.slint:78 `size: diameter * 0.45` — 19.8 on the 44px
        // primary, 14.4 on the 32px secondary, i.e. deliberately a hair
        // smaller than the glyph it replaces so the disc does not look fuller
        // while it waits.
        width: Math.round(root.width * 0.45)
        height: Math.round(root.height * 0.45)
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        rotation: root.spinPhase
        // CircleAction.slint:80-83, the same four-way selector the glyph tint
        // uses below: dark on the solid white overlay primary, white on the
        // overlay ghost, themed on-surface.
        tintName: root.overlay
            ? (root.primary ? "black" : "white")
            : (root.primary ? theme.accentGlyphTint : root.surfaceTint)
    }

    QbzIcon {
        visible: !root.loading
        name: root.name
        width: root.diameterOverride > 0
            ? Math.round(root.resolvedDiameter * 0.46)
            : (root.largePrimary ? 19 : 15)
        height: width
        // Whole-pixel centring, both axes — CircleAction.slint:67-68 rounds
        // them explicitly for the same reason: 44-19 and 32-15 are both ODD,
        // so `anchors.centerIn` lands the glyph on 12.5px / 8.5px and the
        // software renderer resamples it soft, right next to the now
        // pixel-crisp 28px QbzNavButton chevrons.
        x: Math.round((parent.width - width) / 2)
        y: Math.round((parent.height - height) / 2)
        // CircleAction.slint:70-74:
        //   active                  -> Theme.accent
        //   on-surface + primary    -> #ffffff  <- DELIBERATE DIVERGENCE, see
        //                              below; this port uses the measured
        //                              on-accent selector instead
        //   on-surface + secondary  -> Theme.text-primary  <- the light-theme
        //                              case; a fixed white would vanish here
        //   overlay   + primary     -> #000000      (glyph on the white disc)
        //   overlay   + secondary   -> #ffffff
        //
        // The ONE divergence is the on-surface PRIMARY glyph. Its host is the
        // 44px `theme.accent` disc (`color:` above, accentHover on hover —
        // which is why the selector folds both fills in) and CircleAction.slint:72
        // hardcodes #ffffff on it — which measures 1.70:1 on high-contrast,
        // 1.74 on ikari, 1.82 on wcag-dark, 1.85 on kurosaki and so on for
        // 16 of the 35 shipped palettes (the two accessibility themes lead
        // that list, which is the strongest part of the argument).
        // `theme.accentGlyphTint` is that decision made against the
        // live accent (QbzTheme.qml, "ON AN ACCENT FILL"); it still resolves
        // to a white glyph on Dark / Light / OLED, so the default themes'
        // play button is unchanged.
        //
        // The OVERLAY arm keeps its literals: those hosts (the solid white
        // disc, the artwork band) are theme-independent, so there is nothing
        // to measure against and nothing to diverge from.
        tintName: root.active
            ? "accent"
            : (root.overlay
                ? (root.primary ? "black" : "white")
                : (root.primary ? theme.accentGlyphTint : root.surfaceTint))
    }
    MouseArea {
        id: cbArea
        anchors.fill: parent
        enabled: parent.btnEnabled && !parent.loading
        hoverEnabled: true
        cursorShape: (parent.btnEnabled && !parent.loading)
            ? Qt.PointingHandCursor : Qt.ArrowCursor
        onClicked: function (mouse) { parent.clicked(mouse) }
    }
}
