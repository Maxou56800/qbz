// Shared theme tokens (phase 19: LIVE) — the color tokens now bind to
// QbzShell.themeJson, the serialized qbz-theme ThemeColors for the
// persisted ui_prefs `theme` slug (theme_qt.rs — the Qt equivalent of the
// Slint theme::push_colors: switching the theme republishes the document
// and every consumer repaints, no restart). Until the first publish lands
// the baked dark fallback below applies (the bridge seeds themeJson at
// construction, so the fallback is purely defensive).
//
// The non-color tokens (spacing / radius / typography / layout from
// foundation/*.slint) stay static regardless of theme.
//
// Kept as a plain instantiable QtObject (NOT a pragma Singleton — the
// cxx-qt-build generated qmldir cannot mark singletons).

import QtQuick
import com.blitzfc.qbz

QtObject {
    // Baked dark fallback (pre-publish only) — the old static POC tokens.
    readonly property var _fallback: ({
        "surfaceMain": "#0f0f0f", "surfaceCard": "#1a1a1a",
        "surfaceElevated": "#2a2a2a", "surfaceHover": "#10ffffff",
        "bgHover": "#2a2a2a",
        "textPrimary": "#ffffff", "textSecondary": "#cccccc",
        "textMuted": "#888888", "textDisabled": "#555555",
        "accent": "#4285f4", "accentHover": "#5a9bf4",
        "accentPressed": "#3275e4", "accentText": "#ffffff",
        "danger": "#e0564f", "dangerBg": "#26e0564f",
        "dangerBorder": "#59e0564f", "dangerHover": "#e87670",
        "warning": "#fbbf24", "warningBg": "#26fbbf24",
        "warningBorder": "#59fbbf24", "warningHover": "#fdcb47",
        "success": "#3fae6a", "successBg": "#263fae6a",
        "successBorder": "#593fae6a", "successHover": "#55bd7e",
        "borderSubtle": "#14ffffff", "borderMuted": "#38ffffff",
        "borderStrong": "#59ffffff", "focusRing": "#4285f4",
        "favorite": "#ef4444", "cardShadow": "#66000000",
        "surfaceCardA50": "#801a1a1a", "surfaceElevatedA50": "#80252525",
        "surfaceMainA50": "#800f0f0f", "surfaceMainA22": "#380f0f0f",
        "surfaceMainA30": "#4d0f0f0f", "frostBorder": "#1affffff",
        "isDark": true
    })

    readonly property var _doc: {
        if (QbzShell.themeJson === "") return _fallback
        try {
            return JSON.parse(QbzShell.themeJson)
        } catch (e) {
            return _fallback
        }
    }
    function _c(key) {
        const v = _doc[key]
        return v === undefined ? _fallback[key] : v
    }

    // --- Theme colors (live; ThemeColors contract) ----------------------
    readonly property bool isDark: _doc.isDark === undefined ? true : _doc.isDark
    readonly property color surfaceMain: _c("surfaceMain")
    readonly property color surfaceCard: _c("surfaceCard")
    readonly property color surfaceElevated: _c("surfaceElevated")
    readonly property color surfaceHover: _c("surfaceHover")
    readonly property color bgHover: _c("bgHover")
    readonly property color textPrimary: _c("textPrimary")
    // Weak-ramp hardening under ambient on LIGHT themes (owner, 2026-08-31):
    // the ambient wash sits at mid luminance, where a light theme's grey
    // secondary/muted ramp loses its contrast floor — dark themes keep their
    // registry values untouched. The step is a plain channel mix toward
    // textPrimary, so every consumer (there is one QbzTheme per component)
    // follows without a call-site change. Icons ride the tint bake, not this
    // object — if their muted tier still suffers, that is its own change.
    readonly property bool _hardenWeakRamp: ambientOn && !isDark
    function _towardPrimary(c, f) {
        var p = _colorOf(_c("textPrimary"))
        return Qt.rgba(c.r + (p.r - c.r) * f,
                       c.g + (p.g - c.g) * f,
                       c.b + (p.b - c.b) * f, c.a)
    }
    // _c() returns the document's STRING; give the mix real color channels.
    function _colorOf(v) { return Qt.color(v) }
    readonly property color textSecondary: _hardenWeakRamp
        ? _towardPrimary(_colorOf(_c("textSecondary")), 0.40)
        : _c("textSecondary")
    readonly property color textMuted: _hardenWeakRamp
        ? _towardPrimary(_colorOf(_c("textMuted")), 0.50)
        : _c("textMuted")
    readonly property color textDisabled: _c("textDisabled")
    readonly property color accent: _c("accent")
    readonly property color accentHover: _c("accentHover")
    readonly property color accentPressed: _c("accentPressed")
    readonly property color accentText: _c("accentText")
    readonly property color danger: _c("danger")
    readonly property color dangerBg: _c("dangerBg")
    readonly property color dangerBorder: _c("dangerBorder")
    readonly property color dangerHover: _c("dangerHover")
    readonly property color warning: _c("warning")
    readonly property color warningBg: _c("warningBg")
    readonly property color warningBorder: _c("warningBorder")
    readonly property color warningHover: _c("warningHover")
    readonly property color success: _c("success")
    readonly property color successBg: _c("successBg")
    readonly property color successBorder: _c("successBorder")
    readonly property color successHover: _c("successHover")
    readonly property color borderSubtle: _c("borderSubtle")
    readonly property color borderMuted: _c("borderMuted")
    readonly property color borderStrong: _c("borderStrong")
    readonly property color focusRing: _c("focusRing")
    readonly property color favorite: _c("favorite")
    readonly property color cardShadow: _c("cardShadow")
    // 24 alpha tiers ([4..95]%, white-based dark / black-based light).
    readonly property var alphaTiers: _doc.alpha === undefined ? [] : _doc.alpha
    // The ramp's percents (theme_qt.rs ALPHA_PCTS) — the index table for
    // alphaTier(), so a caller asks for the percentage the design specifies
    // ("--alpha-6") instead of a magic array index.
    readonly property var alphaPcts: [4, 5, 6, 8, 10, 12, 15, 18, 20, 25, 30, 35,
        40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95]
    /// One alpha tier by percentage. Theme-correct in both polarities (the
    /// ramp is white-based on dark themes, black-based on light).
    function alphaTier(pct) {
        var i = alphaPcts.indexOf(pct)
        return (i >= 0 && i < alphaTiers.length) ? alphaTiers[i] : "transparent"
    }
    // Ambient-background layering (phase 14; theme-derived since phase 19):
    // chrome surface-card @50%, frosted content panel surface-main @22%,
    // thin bars surface-main @30%, hairline = alpha tier 10%. The live
    // values for the QBZ_BG_* env knobs still ride QbzBridge
    // .ambientSurfaceAlpha / .ambientBarAlpha.
    //
    // The 50% tier exists on THREE tokens because the Slint picks the base by
    // the surface's tier, not by one blanket colour: chrome takes surface-card
    // (Sidebar.slint:693, HeaderBar.slint:561, PlayerBar.slint:130,
    // AppShell.slint:360), controls sitting on a panel take surface-elevated
    // (ToggleButton.slint:25, QbzSelect.slint:112, SegmentedTabBar.slint:108,
    // CircleAction.slint:53, HeaderBar.slint:728) and the Large dock's art well
    // takes surface-main (SidebarNowPlayingDock.slint:193).
    readonly property color surfaceCardA50: _c("surfaceCardA50")
    readonly property color surfaceElevatedA50: _c("surfaceElevatedA50")
    readonly property color surfaceMainA50: _c("surfaceMainA50")
    readonly property color surfaceMainA22: _c("surfaceMainA22")
    readonly property color surfaceMainA30: _c("surfaceMainA30")
    readonly property color frostBorder: _c("frostBorder")

    // THE ONE ambient predicate. Every chrome/control that goes translucent
    // under the app-wide dynamic background reads this instead of re-deriving
    // `QbzShell.ambientMode > 0 && QbzPlayer.npHasTrack` (which the port had
    // copied into 25 files, and which is exactly the kind of duplicated
    // expression that lets one surface drift out of the set — the fade
    // rectangles and the header search field were both missing it).
    // Mirrors ShellState.app-background-active (state.slint:4184) minus the
    // renderer-tier arm: the Qt field renders on the software path too
    // (ShaderEffect where shaders exist, Canvas where they do not), so the
    // feature is never taken away from a weak GPU here.
    readonly property bool ambientOn: QbzShell.ambientMode > 0 && QbzPlayer.npHasTrack

    // --- ON AN ACCENT FILL: the glyph tint and its colour twin -----------
    //
    // AN OWNER-APPROVED DEVIATION FROM SLINT. Read this before touching a
    // call site, and before "restoring" a #ffffff anywhere.
    //
    // Slint hardcodes a WHITE glyph on every accent fill:
    //   primitives/CircleAction.slint:72       (glyph on the primary disc)
    //   primitives/SelectionCheckbox.slint:37  (`tint: #ffffff`)
    //   discover/GenreFilterPopup.slint:85     (`tint: #ffffff`)
    //   discover/BrowseHeaderTools.slint:123 + :132  (glyph AND label)
    //   favorites/FavoritesView.slint:301 + :309     (glyph AND label)
    // Measured against every accent in qbz-theme's registry.rs, that white
    // falls UNDER 2.6:1 on 16 of the 35 shipped palettes — high-contrast
    // 1.70, ikari 1.74, wcag-dark 1.82, kurosaki 1.85, ayanami 1.87,
    // frost 2.00, catppuccin-mocha 2.03, aurora 2.04, macchiato 2.15,
    // frappe 2.20, warm 2.33, zoey 2.41, dracula 2.41, breeze-dark 2.49,
    // tokyo-night 2.52, rumi 2.53.
    //
    // Read the head of that list before the tail: `high-contrast` (accent
    // #62d4ff) is the WORST row in the app at 1.70:1 and `wcag-dark`
    // (#9ec1ff) is third at 1.82:1. Those are the two ACCESSIBILITY
    // palettes — the ones whose entire reason to exist is contrast — and
    // upstream paints their glyph at a ratio that fails every WCAG
    // threshold there is. Everything after them is a dark theme with a
    // bright accent. This is not the light-theme polarity bug the tint
    // vocabulary already fixed, it is a separate defect that comes from
    // upstream, so porting it 1:1 would ship it. The owner ruled the
    // divergence in ("si venía mal de origen es una posibilidad"); every
    // call site that takes it says so and cites the .slint line it departs
    // from.
    //
    // The count is 16, not the 14 an earlier pass wrote here and at five
    // other call sites: that pass had dropped exactly the two accessibility
    // rows, i.e. the strongest evidence for diverging. Every number in this
    // block was re-derived from the 35 `fn * -> ThemeColors` bodies in
    // qbz-theme/src/registry.rs (resolving the `..dark()` spread, the
    // `let accent = ...` shorthand and the StdSpec builder) and spot-checked
    // against an independent contrast tool. Re-derive before editing them.
    //
    // THE OBVIOUS FIX — "just use accent-text" — IS ALSO WRONG. That token is
    // chosen for the theme's TEXT ramp on an accent surface, not for an
    // 11-20px glyph, and it is not universally better: on Rose Pine Dawn
    // (accent #d7827e, accent-text #575279) a check glyph goes from 7.38:1
    // at #000000 to 2.56:1 — UNDER the 3:1 non-text floor. So the durable
    // answer is a SELECTOR, not a constant.
    //
    // THE RULE — theme intent first, floor-guarded:
    //   1. `accentText`, whenever it clears `accentFloor` against the LIVE
    //      accent. It is the theme author's own on-accent colour, and — via
    //      `accentGlyphColor` — it is what the sibling LABELS across the app
    //      resolve to on 34 of the 35 rows (FilterChip, CastPicker,
    //      LocalToolbar, the GenreFilterPopup chips ...), so the glyph and
    //      the label agree by construction instead of by luck.
    //   2. otherwise the extreme that survives best, "white" or "black".
    //      Only Rose Pine Dawn reaches branch 2 today (-> black, 7.38:1).
    //   3. and, if branch 2 cannot clear the floor either, a documented last
    //      resort — see `accentGlyphTint` below. Unreachable on all 35
    //      registry rows and on `custom`; reachable on `auto`.
    // Worst REST case after the selector, over all 35 registry rows: 3.04:1
    // (breeze-light). Before: 1.70:1 (high-contrast). Hover is a separate
    // number and is stated as such below — do not quote 3.04 for both.
    //
    // WHY THE FLOOR IS 3.0 AND NOT 4.5. The glyph is non-text — WCAG 1.4.11
    // asks 3:1. Raising the bar to 4.5 would push Dark / Light / OLED
    // (accent #4285f4, accent-text #ffffff, 3.56:1) off their own accent-text
    // and onto a BLACK glyph: repainting the DEFAULT theme's play button is a
    // far larger deviation than the bug being fixed. The label twin
    // deliberately inherits the same floor rather than the 4.5:1 body-text
    // one — item-for-item agreement with the glyph beats a per-role
    // threshold that would split the two halves of one pill apart again,
    // which is the defect being repaired in BrowseGenreButton / LibraryView.
    //
    // WHY THERE IS NO `accentHoverGlyphTint` SIBLING. This was checked, not
    // assumed: running the same rule against `accentHover` picks a DIFFERENT
    // arm on 6 of the 35 palettes (breeze-light, dark, light, oled,
    // duotone-snow, rose-pine-dawn), so a second property would flip the
    // glyph white->black under the cursor on the three default themes. A
    // glyph that changes colour on hover is a worse defect than the dip it
    // would fix.
    //
    // The dip, stated honestly, because an earlier pass here quoted Dark's
    // 2.83:1 as the worst hover case and it is not: holding the selector's
    // rest-time choice across both fills, the worst HOVER ratio over the 35
    // rows is BREEZE-LIGHT at 2.49:1 — its `accent-text` IS #ffffff and its
    // accentHover is #3daee9 — then Dark / Light / OLED at 2.83:1 (#ffffff
    // on #5a9bf4) and duotone-snow at 2.84:1. All four are what ships today
    // and what the owner has never reported; none of them is what the 3.04:1
    // headline above measures, which is rest only.
    //
    // The hover fill is folded into the FALLBACK arm instead: once
    // accent-text is out, the winning extreme is the one that holds on
    // accent AND accentHover, so a diverging theme's choice cannot depend on
    // which state the control is in.

    /// WCAG 2.x relative luminance of a QML `color` (whose r/g/b are 0..1
    /// sRGB floats). Spelled out on purpose: `hslLightness` is the stale
    /// approximation this tree just finished removing from ~35 call sites,
    /// and on the yellow/green accents (ikari #7eda53, kurosaki #d5be58,
    /// ayanami #e5b82e) it misses by more than a whole arm of this decision.
    function _relLum(c) {
        const r = c.r <= 0.03928 ? c.r / 12.92 : Math.pow((c.r + 0.055) / 1.055, 2.4)
        const g = c.g <= 0.03928 ? c.g / 12.92 : Math.pow((c.g + 0.055) / 1.055, 2.4)
        const b = c.b <= 0.03928 ? c.b / 12.92 : Math.pow((c.b + 0.055) / 1.055, 2.4)
        return 0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// The non-text contrast floor (WCAG 1.4.11). Read the note above before
    /// raising it: 4.5 moves the three DEFAULT themes.
    readonly property real accentFloor: 3.0

    /// The QbzIcon tint NAME for a glyph drawn on an accent fill.
    ///
    /// A NAME and not a colour, because QbzIcon resolves a baked/runtime-
    /// tinted DIRECTORY per tint (QbzIcon.qml header) — it cannot take an
    /// arbitrary hex. All three values it can return are complete 96-glyph
    /// sets: "accentText" is baked at runtime from MASTERS and falls back to
    /// primary/ or black/ by its own polarity, "white" is primary/ and
    /// "black" is black/, and those two are FIXED tints that never depend on
    /// the runtime bake being alive at all.
    ///
    /// COST: one binding per QbzTheme INSTANCE — not per icon, not per
    /// repaint. Two `_relLum()` calls (6 Math.pow) in the branch every
    /// palette but one takes; white and black need none, their luminances are
    /// exactly 1.0 and 0.0 so their ratios are closed forms.
    ///
    /// "Per instance" is NOT "once per theme switch", which is how an earlier
    /// pass phrased it. QbzTheme is a plain instantiable QtObject (see the
    /// file header) and the tree stamps out ~120 of them, several inside list
    /// DELEGATES — views/local/SelectCheck.qml owns one, so this binding is
    /// evaluated afresh on every delegate creation while a 16K-track Local
    /// Library list scrolls. It is still negligible: the same instance is
    /// already paying a `JSON.parse` of ~2.7 KB in the `_doc` binding above,
    /// which dwarfs 6 Math.pow by orders of magnitude. Said out loud so the
    /// next person profiling that scroll knows this line was measured and
    /// dismissed rather than never looked at.
    readonly property string accentGlyphTint: {
        const la = _relLum(accent) + 0.05
        const lt = _relLum(accentText) + 0.05
        if ((la > lt ? la / lt : lt / la) >= accentFloor)
            return "accentText"
        // Accent-text is not legible on this accent. Pick the extreme that
        // holds on BOTH fills a control paints (rest + hover) — see the
        // "no sibling" note above.
        const lh = _relLum(accentHover) + 0.05
        // white: (1.0 + 0.05) / (Lfill + 0.05);  black: (Lfill + 0.05) / 0.05
        const white = Math.min(1.05 / la, 1.05 / lh)
        const black = Math.min(la, lh) / 0.05
        if (Math.max(white, black) >= accentFloor)
            return white >= black ? "white" : "black"
        // LAST RESORT — neither extreme survives BOTH fills. Scoring the two
        // fills jointly takes a `min()`, and a `min()` has no floor of its
        // own: a theme whose accent is dark and whose accentHover is light
        // fails white on one and black on the other, so the "winning extreme"
        // can win at 1.1:1. Drop the hover fill and score against `accent`
        // alone, which is provably safe — max(white, black) on a SINGLE fill
        // bottoms out at 4.58:1, where 1.05/(L+0.05) meets (L+0.05)/0.05.
        // Rest legibility is the one being protected; hover legibility is
        // what gets sacrificed to buy it, deliberately and in that order,
        // because a control is at rest far more of the time than under a
        // cursor and an illegible resting glyph is the defect this whole
        // selector exists to prevent.
        //
        // REACHABILITY, checked rather than assumed. Dead on all 35
        // registry.rs rows (their floor across both fills is 4.40:1) and on
        // `custom`, whose accent_hover is the accent shifted +0.10 lightness
        // (custom.rs:175) so both fills stay in one lightness neighbourhood
        // — swept over the accent cube, that floor is 3.26:1. LIVE on `auto`:
        // there accent_hover is `scheme.selection_hover` and accent_text is
        // `scheme.selection_fg` (auto/generator.rs:261-268), and those
        // `unwrap_or_else` arms only fire when the system colour scheme is
        // SILENT. A desktop theme that names both hands us an arbitrary
        // triple with no derived relationship at all.
        return (1.05 / la) >= (la / 0.05) ? "white" : "black"
    }

    /// The COLOUR twin of `accentGlyphTint`, for the Text labels that share
    /// an accent pill with such a glyph (Text takes a colour, not a tint
    /// name). DERIVED from the decision above rather than re-deciding it, so
    /// the two halves of a pill can never disagree.
    ///
    /// The literals must be what the tint names actually paint, or the label
    /// and the glyph drift on the palettes that take the fallback arm.
    /// Verified by reading the sets: assets/icons/primary/ and
    /// assets/icons/black/ hold 96 files each with identical name sets (=
    /// the MASTERS table in src/icon_tint_qt.rs), and EVERY primary master
    /// carries the literal `#ffffff` somewhere in a paint attribute, every
    /// black master `#000000` — zero exceptions on either side.
    ///
    /// That invariant is about the literal, NOT about `fill=`, which is how
    /// an earlier pass wrote it. Only 11 of the 96 primary masters paint with
    /// `fill="#ffffff"`; the other 85 are `fill="none"` outlines carrying
    /// `stroke="#ffffff"` (3 files use both). Of the five glyphs this
    /// selector can actually reach — play-fill (QbzCircleAction's primary
    /// disc, always play-fill at every `primary: true` call site), pause
    /// (TransportControls), check + minus (QbzCheckbox, SelectCheck,
    /// GenreFilterPopup, LoginScreen) and list-filter (BrowseGenreButton,
    /// LibraryView) — exactly ONE, play-fill, is fill-painted.
    ///
    /// The behaviour was never at risk: `bake()` does a plain hex replace
    /// over the whole SVG text (src/icon_tint_qt.rs:389-391), so a stroke is
    /// recoloured with the fill and stroke-only glyphs tint correctly. Only
    /// the written claim was wrong. When adding a master, the rule to hold is
    /// "paints with the literal #ffffff SOMEWHERE", not "has fill=#ffffff".
    readonly property color accentGlyphColor:
        accentGlyphTint === "accentText"
            ? accentText
            : (accentGlyphTint === "white" ? "#ffffff" : "#000000")

    // --- Spacing (spacing.slint) --------------------------------------
    readonly property int spacingXs: 4
    readonly property int spacingSm: 8
    readonly property int spacingMd: 16
    readonly property int spacingLg: 20
    readonly property int spacingXl: 32
    readonly property int cardPadding: 52

    // --- Radius (radius.slint) ----------------------------------------
    readonly property int radiusSm: 8
    readonly property int radiusMd: 12
    readonly property int radiusLg: 16

    // --- Typography (typography.slint, boost = 1.0) -------------------
    readonly property int fontLegal: 13
    readonly property int fontLink: 14
    readonly property int fontBody: 15
    readonly property int fontSubtitle: 15
    readonly property int fontButton: 17
    readonly property int fontSection: 18
    readonly property int fontHeading: 19
    readonly property int fontTitle: 25
    readonly property int fontWordmark: 29
    readonly property int weightRegular: Font.Normal
    readonly property int weightMedium: Font.Medium
    readonly property int weightSemibold: Font.DemiBold
    readonly property int weightBold: Font.Bold

    // --- Shell layout (layout.slint + state.slint) --------------------
    readonly property int headerHeight: 42
    // Three-state sidebar rendered widths (ShellState.sidebar-rendered-width).
    readonly property int sidebarOpenWidth: 240
    // Mini rail — back to Slint's 64px (state.slint:4075) by owner decision,
    // 2026-07-29, after the 50px deviation turned out to have a cost nobody
    // had priced.
    //
    // The 50px came from an earlier owner request: the 64px rail read as a
    // wide empty gutter. But 50 made two references irreconcilable — the rail
    // centres its 34px row at 25, while the header's own sidebar toggle sits
    // at centre 30 (spacingMd + half of 28). Five pixels apart, one directly
    // above the other, which is what the owner saw and reported as "not
    // centred": symmetric padding inside the rail still looked wrong because
    // the eye lines the glyphs up against the header control, not against the
    // rail. Slint never had the problem — at 64 the row centres at 32, two
    // pixels off the header button and imperceptible. Chasing it from inside
    // the rail (12/4 padding) bought the header alignment at the price of a
    // visibly asymmetric hover pill; the root fix is the width.
    readonly property int sidebarMiniWidth: 64
    // The square row inside that rail. A LITERAL 34, not derived: at 50 the
    // `width - 2 * spacingSm` formula happened to yield 34, which is why the
    // two tokens were coupled — but Slint pins BOTH independently (rail 64px,
    // row 34px at Sidebar.slint:575/652), so the same formula at 64 would give
    // 48 and silently grow every row. Deriving it was the trap the previous
    // comment warned about; pinning it is the fix.
    readonly property int sidebarMiniRow: 34
    // Right queue/lyrics column width (AppShell).
    readonly property int queuePanelWidth: 300
    // Small now-playing bar total height (ShellState.npb-small-height,
    // npb-small-extra is 0 on Linux).
    readonly property int npbSmallHeight: 42
    /// The gutter the player bar keeps to the window's left, right and
    /// bottom edges (2026-09-13): the SAME 8 px the content pane keeps
    /// inside its frame, so the bar's ends line up with the pane's and
    /// nothing in it touches the window edge.
    readonly property int npbGutter: 8
    /// macOS only: Tahoe's Liquid Glass windows round their corners far
    /// more than before (Apple: toolbar windows take a larger radius that
    /// "can clip content that sits close to the edge of the window";
    /// estimates run from ~12-16 pt for titlebar-only windows to 50 px+
    /// with a toolbar) and the cover at the bar's left end lost its corner.
    /// Qt has no LayoutRegion to nest content into the curve, so the bar
    /// simply keeps a wider gutter there.
    readonly property int npbGutterMac: 16
    // Every other bar mode (New / Classic / Large) — AppShell.slint:396.
    // The Large dock's sidebar reservation subtracts this, so it must be the
    // SAME value the shell pins the bar to.
    readonly property int npbLargeHeight: 112

    // --- Now-playing-bar responsive breakpoints ------------------------
    // ONE family of WINDOW-width breakpoints drives BOTH bars
    // (PlayerBar.slint:142-144 and PlayerBarSmall.slint:145-147 carry the
    // identical expression, and PlayerBarSmall.slint:152 reuses the 1366
    // edge for the volume control):
    //   width <  1366  -> 39% | 22% | 39%   volume = button + flyout
    //   1366..<1920    -> 30% | 40% | 30%   volume = inline slider
    //   width >= 1920  -> 25% | 50% | 25%
    // The two side columns always share the fraction — that symmetry is what
    // lands PLAY on the window centre. These lived as copy-pasted literals in
    // PlayerBar.qml and NowPlayingBarSmall.qml; as tokens the two bars cannot
    // drift apart when the breakpoints move.
    //
    // NOT here: Classic's fixed 0.324 side column. That is a per-MODE
    // override (PlayerBar.slint:149), not a breakpoint — Classic ignores the
    // window width entirely, so folding it in would misrepresent the rule.
    readonly property int npbBreakMid: 1366
    readonly property int npbBreakWide: 1920
    // Owner visual ruling (2026-08-23): the metadata card may consume at most
    // 80% of its live runway in every NPB mode. One token keeps New, Classic,
    // Large and Small from drifting back to four different proportions.
    readonly property real npbSongCardMaxFraction: 0.80
    // `w` is the BAR's width, which is the window's width: both bars are
    // anchored left-to-right on the shell root (AppShell.qml:72-80), NOT
    // inset by the sidebar or the queue column (those anchor to npb.top).
    // Slint reads the same number off its own bar root. If a bar ever stops
    // spanning the window, this argument must switch to Window.width.
    function npbSideFrac(w) {
        return w >= npbBreakWide ? 0.25 : (w >= npbBreakMid ? 0.30 : 0.39)
    }
}
