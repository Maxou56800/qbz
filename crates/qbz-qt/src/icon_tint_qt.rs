//! Runtime icon tinting — the Qt equivalent of Slint's render-time `colorize`.
//!
//! # Why this file exists
//!
//! Slint's `QbzIcon` (ui/primitives/QbzIcon.slint) is three lines: an `Image`
//! with `colorize: <live Theme token>`. The renderer recolours the glyph every
//! frame, so an icon is ALWAYS the current theme's colour, for all 36 themes
//! and for a user's custom accent.
//!
//! Qt gives this port no such capability on the path it actually runs on: a
//! `ColorOverlay` / `MultiEffect` / shader draws NOTHING on the software
//! renderer (verified with a probe during the POC), which is why the port
//! shipped PRE-BAKED SVG variants instead — `qml/assets/icons/<tint>/`, one
//! directory per tint, each a fixed hex burned into the file.
//!
//! A fixed hex cannot serve 36 themes. The bakes were cut against the DARK
//! palette, so on every LIGHT theme `primary` (#ffffff), `secondary`
//! (#cccccc) and `muted` (#888888) are near-white glyphs on a near-white
//! surface (the header nav row, the Settings sub-nav and the app menu the
//! owner reported), and `accent` is always #4285f4 no matter what the theme's
//! accent is.
//!
//! # What this does
//!
//! On demand, per (tint token, live theme), it writes a COMPLETE tinted icon
//! set to the user cache dir and hands QML a `file://` directory URL:
//!
//!     <cache>/qbz/icon-tints/<rrggbb>/<name>.svg
//!
//! The source of every glyph is the existing `primary/` bake (compiled in
//! with `include_str!`), whose only paint literal is `#ffffff` — the tint is
//! a literal string substitution, no XML parsing, so a glyph can never come
//! out geometrically different from the bake that already ships.
//!
//! The directory is keyed by the COLOUR, not by the theme, so the 36 themes
//! share whatever hexes they have in common (most share `#ffffff` /
//! `#0f0f0f` text ramps and a handful of accents), and switching back to a
//! previously used theme is a single `is_file()` check.
//!
//! Everything else stays exactly as it was: this module never touches the
//! qrc bakes, and every failure path (no cache dir, read-only home, an
//! unparsable theme document, a tint that is deliberately fixed) returns an
//! EMPTY string, which `QbzIcon.qml` reads as "use the qrc bake" — the
//! pre-existing behaviour. There is no state in which an icon can vanish
//! because of this file.
//!
//! # Which tints follow the theme, and the one that deliberately does not
//!
//! THEME-FOLLOWING (baked here): `textPrimary`, `secondary` / `textSecondary`,
//! `muted` / `textMuted`, `disabled` / `textDisabled`, `accent`, `accentText`,
//! `warning`, `favorite`, `success`, `danger` — each named after its
//! `ThemeColors` key (theme_qt.rs `ThemeTokens`).
//!
//! `success` / `danger` arrived with the shared toast (primitives/Toast.slint
//! tints its per-kind glyph with Theme.success / Theme.danger). They are the
//! only two theme-following tints with NO qrc bake directory of their own, so
//! the FLOOR does not carry them: `QbzIcon.qml`'s `qrcTint` switch falls to its
//! `default:` arm, returns the tint name unchanged, and asks for
//! `assets/icons/success/<name>.svg` — a directory that does not exist, which
//! means the glyph is simply ABSENT with nothing logged. Reachable exactly when
//! the runtime bake is (no writable cache dir, a pre-publish theme document, a
//! pruned directory). `QbzIcon.qml` needs two arms in that switch —
//! `case "danger": return "favorite"` (#ef4444 vs #e0564f, the port-wide
//! substitution) and `success` onto a dir whose polarity is legible (there is
//! no green bake) — before the floor can be trusted for these two.
//!
//! FIXED (kept on their qrc bake): `black`, `amber` — and `primary`.
//!
//! `primary` is the interesting one, and it is NOT an oversight. The port's
//! `"primary"` tint has always been a literal `#ffffff`, and roughly a third
//! of its call sites depend on that: glyphs on artwork scrims (`#a6000000`,
//! `#b3000000`), on gradient discs, on `theme.accent` fills, on the close
//! button's fixed `#e81123` hover. Those are dark under EVERY theme, so
//! resolving `primary` to `Theme.text-primary` would paint a dark glyph on a
//! dark host — it would move the bug rather than fix it. Slint does not have
//! this problem because it passes the two meanings separately
//! (CircleAction.slint:70-74 hands `#ffffff` to the glyph on the accent disc
//! and `Theme.text-primary` to the one on the surface).
//!
//! So the vocabulary splits the two meanings instead of overloading one name:
//!   - `white`     — the honest name for the fixed light glyph (same qrc
//!                   bake `primary` already resolves to);
//!   - `accentText`— the glyph colour ON an accent fill, theme-following;
//!   - `textPrimary` — Slint's `Theme.text-primary`, theme-following, what
//!                   every "raise this glyph to max contrast on a THEME
//!                   surface" call site actually wanted.
//! `primary` stays a legacy alias of `white` so no existing call site moves
//! until it is migrated, and a migrated one is a pure rename with no polarity
//! ternary anywhere.
//!
//! # Cost
//!
//! One pass of 99 small string substitutions + writes (~390 KB) the first
//! time a colour is seen, on the Qt thread inside a binding evaluation
//! (measured in single-digit ms on any SSD); nothing at all afterwards. The
//! cache is pruned to `MAX_TINT_DIRS` directories, oldest first.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_icon_tint {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type QbzIconTint = super::QbzIconTintRust;

        /// The directory URL holding `<name>.svg` for `tint` under the theme
        /// described by `theme_json`, or "" when the caller must fall back to
        /// the compiled-in qrc bake.
        ///
        /// `theme_json` is `QbzShell.themeJson` — it is BOTH the colour
        /// source and the QML binding dependency that makes every icon
        /// re-resolve on a live theme switch (the same trick
        /// `QbzSession.tr(msgid, trRev)` uses for the language switch).
        #[qinvokable]
        fn dir_for(self: &QbzIconTint, tint: QString, theme_json: QString) -> QString;
    }
}

/// Rust side of the tint bridge — the state is process-global (below) because
/// the singleton is queried from hundreds of icon bindings and must not pay a
/// per-instance re-parse.
#[derive(Default)]
pub struct QbzIconTintRust {
    _reserved: bool,
}

// ---------------------------------------------------------------------------
// The master glyphs
// ---------------------------------------------------------------------------

/// Every icon the app can ask for, as the `primary/` bake's source text.
///
/// This IS the shipped `qml/assets/icons/primary/` directory (124 files),
/// compiled in. Two consequences worth knowing:
///   - every runtime tint is COMPLETE, so the partial-directory trap the qrc
///     bakes have (`favorite/` = 58 files, `amber/` = 6, plus the two
///     single-glyph brand dirs `green/` and `orange/`) does not exist here;
///   - a new icon dropped into `primary/` needs a line ADDED here, otherwise
///     that one glyph silently drops back to its qrc bake (visible, just not
///     theme-correct). `QbzIcon.qml` handles the miss; it does not go blank.
#[rustfmt::skip]
const MASTERS: &[(&str, &str)] = &[
    ("add-to-list", include_str!("../qml/assets/icons/primary/add-to-list.svg")),
    ("align-start-vertical", include_str!("../qml/assets/icons/primary/align-start-vertical.svg")),
    ("arrow-down-up", include_str!("../qml/assets/icons/primary/arrow-down-up.svg")),
    ("arrow-up-down", include_str!("../qml/assets/icons/primary/arrow-up-down.svg")),
    ("audio-lines", include_str!("../qml/assets/icons/primary/audio-lines.svg")),
    ("award", include_str!("../qml/assets/icons/primary/award.svg")),
    ("blind-eye", include_str!("../qml/assets/icons/primary/blind-eye.svg")),
    ("book-open", include_str!("../qml/assets/icons/primary/book-open.svg")),
    ("bug", include_str!("../qml/assets/icons/primary/bug.svg")),
    ("cassette-tape", include_str!("../qml/assets/icons/primary/cassette-tape.svg")),
    ("cast", include_str!("../qml/assets/icons/primary/cast.svg")),
    ("cd", include_str!("../qml/assets/icons/primary/cd.svg")),
    ("chart-no-axes-column", include_str!("../qml/assets/icons/primary/chart-no-axes-column.svg")),
    ("check", include_str!("../qml/assets/icons/primary/check.svg")),
    ("chevron-down", include_str!("../qml/assets/icons/primary/chevron-down.svg")),
    ("chevron-left", include_str!("../qml/assets/icons/primary/chevron-left.svg")),
    ("chevron-right", include_str!("../qml/assets/icons/primary/chevron-right.svg")),
    ("chevron-up", include_str!("../qml/assets/icons/primary/chevron-up.svg")),
    ("circle-alert", include_str!("../qml/assets/icons/primary/circle-alert.svg")),
    ("circle-check-big", include_str!("../qml/assets/icons/primary/circle-check-big.svg")),
    ("circle-stop", include_str!("../qml/assets/icons/primary/circle-stop.svg")),
    ("clipboard", include_str!("../qml/assets/icons/primary/clipboard.svg")),
    ("clock", include_str!("../qml/assets/icons/primary/clock.svg")),
    ("cloud-download", include_str!("../qml/assets/icons/primary/cloud-download.svg")),
    ("cloud-off", include_str!("../qml/assets/icons/primary/cloud-off.svg")),
    ("cloud-sync", include_str!("../qml/assets/icons/primary/cloud-sync.svg")),
    ("cloud-upload", include_str!("../qml/assets/icons/primary/cloud-upload.svg")),
    ("compass", include_str!("../qml/assets/icons/primary/compass.svg")),
    ("copy", include_str!("../qml/assets/icons/primary/copy.svg")),
    ("disc-3", include_str!("../qml/assets/icons/primary/disc-3.svg")),
    ("disc", include_str!("../qml/assets/icons/primary/disc.svg")),
    ("disc-folder", include_str!("../qml/assets/icons/primary/disc-folder.svg")),
    ("element-connect", include_str!("../qml/assets/icons/primary/element-connect.svg")),
    ("ellipsis", include_str!("../qml/assets/icons/primary/ellipsis.svg")),
    ("crown", include_str!("../qml/assets/icons/primary/crown.svg")),
    ("external-link", include_str!("../qml/assets/icons/primary/external-link.svg")),
    ("eye-off", include_str!("../qml/assets/icons/primary/eye-off.svg")),
    ("eye", include_str!("../qml/assets/icons/primary/eye.svg")),
    ("folder-open", include_str!("../qml/assets/icons/primary/folder-open.svg")),
    ("folder-plus", include_str!("../qml/assets/icons/primary/folder-plus.svg")),
    ("folder", include_str!("../qml/assets/icons/primary/folder.svg")),
    // The HiFi Wizard's header glyph. Unlike every other master this one is a
    // FILLED path set (`fill="#ffffff"`), not a stroked outline — the literal
    // substitution does not care, but a future "why is this file shaped
    // differently" reads better answered here.
    ("gandalf", include_str!("../qml/assets/icons/primary/gandalf.svg")),
    ("globe", include_str!("../qml/assets/icons/primary/globe.svg")),
    // ── Media-server BRAND marks, monochrome ──────────────────────────────
    // Unlike the rest of this table these are logos, not UI glyphs, and they
    // are here for one reason: they arrived as BLACK ink and are invisible on
    // every dark theme untinted (rendered and looked at, 2026-08-20). This
    // pipeline is the port's only shader-free way to recolour an SVG per
    // theme, so the marks were normalised to the #ffffff master ink and joined
    // it. Their COLOUR variants stay in `assets/brand/` and are drawn untinted
    // like Plex's and Qobuz's — see `controls/SourceIcon.qml`.
    ("jellyfin", include_str!("../qml/assets/icons/primary/jellyfin.svg")),
    ("navidrome", include_str!("../qml/assets/icons/primary/navidrome.svg")),
    ("grip-vertical", include_str!("../qml/assets/icons/primary/grip-vertical.svg")),
    ("hard-drive", include_str!("../qml/assets/icons/primary/hard-drive.svg")),
    ("heart-filled", include_str!("../qml/assets/icons/primary/heart-filled.svg")),
    ("heart", include_str!("../qml/assets/icons/primary/heart.svg")),
    ("home-gear", include_str!("../qml/assets/icons/primary/home-gear.svg")),
    ("image", include_str!("../qml/assets/icons/primary/image.svg")),
    ("image-plus", include_str!("../qml/assets/icons/primary/image-plus.svg")),
    ("import", include_str!("../qml/assets/icons/primary/import.svg")),
    ("infinity", include_str!("../qml/assets/icons/primary/infinity.svg")),
    ("info", include_str!("../qml/assets/icons/primary/info.svg")),
    ("keyboard", include_str!("../qml/assets/icons/primary/keyboard.svg")),
    ("layers", include_str!("../qml/assets/icons/primary/layers.svg")),
    ("layout-grid", include_str!("../qml/assets/icons/primary/layout-grid.svg")),
    ("library", include_str!("../qml/assets/icons/primary/library.svg")),
    ("library-big", include_str!("../qml/assets/icons/primary/library-big.svg")),
    ("link", include_str!("../qml/assets/icons/primary/link.svg")),
    ("list-end", include_str!("../qml/assets/icons/primary/list-end.svg")),
    ("list-filter", include_str!("../qml/assets/icons/primary/list-filter.svg")),
    ("list-music", include_str!("../qml/assets/icons/primary/list-music.svg")),
    ("list-ordered", include_str!("../qml/assets/icons/primary/list-ordered.svg")),
    ("list-plus", include_str!("../qml/assets/icons/primary/list-plus.svg")),
    ("list-start", include_str!("../qml/assets/icons/primary/list-start.svg")),
    ("list", include_str!("../qml/assets/icons/primary/list.svg")),
    ("list-x", include_str!("../qml/assets/icons/primary/list-x.svg")),
    ("loader-circle", include_str!("../qml/assets/icons/primary/loader-circle.svg")),
    ("log-out", include_str!("../qml/assets/icons/primary/log-out.svg")),
    ("map-pin", include_str!("../qml/assets/icons/primary/map-pin.svg")),
    ("maximize-2", include_str!("../qml/assets/icons/primary/maximize-2.svg")),
    ("menu", include_str!("../qml/assets/icons/primary/menu.svg")),
    ("mic-vocal", include_str!("../qml/assets/icons/primary/mic-vocal.svg")),
    ("minimize-2", include_str!("../qml/assets/icons/primary/minimize-2.svg")),
    ("minus", include_str!("../qml/assets/icons/primary/minus.svg")),
    ("monitor", include_str!("../qml/assets/icons/primary/monitor.svg")),
    ("monitor-speaker", include_str!("../qml/assets/icons/primary/monitor-speaker.svg")),
    ("moon", include_str!("../qml/assets/icons/primary/moon.svg")),
    ("move", include_str!("../qml/assets/icons/primary/move.svg")),
    ("mp3", include_str!("../qml/assets/icons/primary/mp3.svg")),
    ("music-library-2", include_str!("../qml/assets/icons/primary/music-library-2.svg")),
    ("music-note-slider", include_str!("../qml/assets/icons/primary/music-note-slider.svg")),
    ("music", include_str!("../qml/assets/icons/primary/music.svg")),
    ("network", include_str!("../qml/assets/icons/primary/network.svg")),
    ("panel-left", include_str!("../qml/assets/icons/primary/panel-left.svg")),
    ("panel-right-close", include_str!("../qml/assets/icons/primary/panel-right-close.svg")),
    ("panel-right-open", include_str!("../qml/assets/icons/primary/panel-right-open.svg")),
    ("pause", include_str!("../qml/assets/icons/primary/pause.svg")),
    ("pen-line", include_str!("../qml/assets/icons/primary/pen-line.svg")),
    ("picture-in-picture-2", include_str!("../qml/assets/icons/primary/picture-in-picture-2.svg")),
    ("pin-filled", include_str!("../qml/assets/icons/primary/pin-filled.svg")),
    ("pin", include_str!("../qml/assets/icons/primary/pin.svg")),
    ("play-fill", include_str!("../qml/assets/icons/primary/play-fill.svg")),
    ("plus", include_str!("../qml/assets/icons/primary/plus.svg")),
    ("power", include_str!("../qml/assets/icons/primary/power.svg")),
    ("qbz-symbolic", include_str!("../qml/assets/icons/primary/qbz-symbolic.svg")),
    ("radio", include_str!("../qml/assets/icons/primary/radio.svg")),
    ("refresh-cw", include_str!("../qml/assets/icons/primary/refresh-cw.svg")),
    ("repeat-1", include_str!("../qml/assets/icons/primary/repeat-1.svg")),
    ("repeat", include_str!("../qml/assets/icons/primary/repeat.svg")),
    ("rotate-ccw", include_str!("../qml/assets/icons/primary/rotate-ccw.svg")),
    ("rows-3", include_str!("../qml/assets/icons/primary/rows-3.svg")),
    ("scissors", include_str!("../qml/assets/icons/primary/scissors.svg")),
    ("search", include_str!("../qml/assets/icons/primary/search.svg")),
    // The GEAR. Distinct from settings-2 (two sliders) — the kiosk NavRail's
    // seventh tile uses this one (`shell/NavRail.slint:185`).
    ("server", include_str!("../qml/assets/icons/primary/server.svg")),
    ("settings", include_str!("../qml/assets/icons/primary/settings.svg")),
    ("settings-2", include_str!("../qml/assets/icons/primary/settings-2.svg")),
    ("shopping-bag", include_str!("../qml/assets/icons/primary/shopping-bag.svg")),
    ("shuffle", include_str!("../qml/assets/icons/primary/shuffle.svg")),
    ("skip-back", include_str!("../qml/assets/icons/primary/skip-back.svg")),
    ("skip-forward", include_str!("../qml/assets/icons/primary/skip-forward.svg")),
    ("sliders-horizontal", include_str!("../qml/assets/icons/primary/sliders-horizontal.svg")),
    ("smartphone", include_str!("../qml/assets/icons/primary/smartphone.svg")),
    ("speaker", include_str!("../qml/assets/icons/primary/speaker.svg")),
    ("square", include_str!("../qml/assets/icons/primary/square.svg")),
    ("square-check-big", include_str!("../qml/assets/icons/primary/square-check-big.svg")),
    ("star", include_str!("../qml/assets/icons/primary/star.svg")),
    ("sun-moon", include_str!("../qml/assets/icons/primary/sun-moon.svg")),
    ("sun", include_str!("../qml/assets/icons/primary/sun.svg")),
    ("tags", include_str!("../qml/assets/icons/primary/tags.svg")),
    ("thumbs-down", include_str!("../qml/assets/icons/primary/thumbs-down.svg")),
    // The Tidal wordmark. The ONE brand mark in this table on purpose: it is
    // monochrome (a black `fill` in the upstream asset), so the "never tint a
    // logo, it becomes an accent silhouette" rule that keeps the Qobuz / Plex /
    // Discogs marks in qml/assets/brand/ does not apply — there is no colour to
    // lose. The reference tints it too (PlaylistImportModal.slint:271-279:
    // "Tidal's SVG is black — tint white so it reads on dark surfaces"); going
    // through the tint table instead of a fixed white rendition is what makes it
    // legible on the 11 light themes as well.
    ("ticket-check", include_str!("../qml/assets/icons/primary/ticket-check.svg")),
    ("tidal-tidal", include_str!("../qml/assets/icons/primary/tidal-tidal.svg")),
    ("translate", include_str!("../qml/assets/icons/primary/translate.svg")),
    ("trash-2", include_str!("../qml/assets/icons/primary/trash-2.svg")),
    ("trash-list", include_str!("../qml/assets/icons/primary/trash-list.svg")),
    ("triangle-alert", include_str!("../qml/assets/icons/primary/triangle-alert.svg")),
    ("turntable", include_str!("../qml/assets/icons/primary/turntable.svg")),
    ("user-plus", include_str!("../qml/assets/icons/primary/user-plus.svg")),
    ("user", include_str!("../qml/assets/icons/primary/user.svg")),
    ("volume-2", include_str!("../qml/assets/icons/primary/volume-2.svg")),
    ("volume-x", include_str!("../qml/assets/icons/primary/volume-x.svg")),
    // The header menu's "What's New" glyph (HeaderBar.slint:1222).
    ("wand-sparkles", include_str!("../qml/assets/icons/primary/wand-sparkles.svg")),
    ("wifi", include_str!("../qml/assets/icons/primary/wifi.svg")),
    ("x", include_str!("../qml/assets/icons/primary/x.svg")),
];

/// The only paint literal in a `primary/` master (verified over all 115 files:
/// 125 occurrences, no `#fff` short form). The uppercase form is accepted too
/// so a hand-added master cannot silently skip the substitution.
const MASTER_HEX_LOWER: &str = "#ffffff";
const MASTER_HEX_UPPER: &str = "#FFFFFF";

/// Written last into a tint directory; its presence means "complete" AND its
/// CONTENT names the icon set that was written.
///
/// It used to hold the hex, which made "complete" mean "complete for whatever
/// the icon list was when this directory was baked". Adding a glyph to
/// `MASTERS` then never reached an already-baked cache — the directory still
/// stamped complete, so the new icon resolved to a `file://` URL that does not
/// exist and `QbzIcon` logged `Cannot open: .../<name>.svg` on EVERY repaint,
/// for every user who had ever run an older build. Found by adding
/// `circle-stop` for the queue's stop-after marker (2026-07-31); the fix is
/// generic, so the next added glyph cannot repeat it.
const STAMP: &str = ".complete";

/// Fingerprint of the icon SET, stored in `STAMP`. A directory whose stamp
/// does not match the running binary's set is stale and is re-baked.
///
/// The names, not the bodies: an EDITED glyph keeps its name, and re-baking
/// every cache dir on an artwork tweak is not worth the I/O — a changed
/// drawing still renders, a missing file does not.
fn master_set_fingerprint() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    MASTERS.len().hash(&mut h);
    for (name, _) in MASTERS {
        name.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// How many tinted sets to keep on disk (~390 KB each). 36 themes x 10 tokens
/// is the theoretical ceiling, but the palettes share most of their text ramp,
/// so this covers ordinary theme-hopping without unbounded growth.
const MAX_TINT_DIRS: usize = 32;

// ---------------------------------------------------------------------------
// Tint name -> ThemeColors token
// ---------------------------------------------------------------------------

/// Every `ThemeTokens` key a tint can resolve to — the set this module bakes.
const TOKENS: &[&str] = &[
    "textPrimary",
    "textSecondary",
    "textMuted",
    "textDisabled",
    "accent",
    "accentText",
    "warning",
    "favorite",
    "success",
    "danger",
];

/// The `ThemeTokens` (theme_qt.rs) key a tint follows, or `None` for the tints
/// that are deliberately theme-independent and keep their qrc bake.
///
/// The short spellings are the ones the port already uses; the `text*` ones
/// name the token exactly and are what new call sites should say.
fn token_for(tint: &str) -> Option<&'static str> {
    Some(match tint {
        "textPrimary" => "textPrimary",
        "secondary" | "textSecondary" => "textSecondary",
        "muted" | "textMuted" => "textMuted",
        "disabled" | "textDisabled" => "textDisabled",
        "accent" => "accent",
        "accentText" => "accentText",
        "warning" => "warning",
        "favorite" => "favorite",
        // The toast's per-kind glyph tints (primitives/Toast.slint:20-38 hands
        // Theme.success / Theme.danger straight to the icon). QbzIcon exposes
        // `tintName` only — never a raw colour — so without these two entries
        // there is no way for a QML call site to reach either token, and both
        // keys already exist in every theme document (theme_qt.rs:159,173;
        // QbzTheme.qml:28,32,70,78).
        "success" => "success",
        "danger" => "danger",
        // Fixed on purpose (see the module header):
        //   "primary" / "white": a light glyph on a host that is dark under
        //                        every theme (scrims, gradients, accent fills)
        //   "black":             a dark glyph on a host painted light
        //   "amber":             #e0b341, the audio stamp's brand accent
        //   "green" / "orange":  #22c55e / #f59e0b, PmLocalBadge's two
        //                        availability states. Literal brand colours,
        //                        not theme tokens — falling through to `None`
        //                        here is what makes QbzIcon serve them from
        //                        the one-glyph qrc dirs green/ and orange/.
        //                        Do NOT "complete" them with a token.
        _ => return None,
    })
}

/// `#aarrggbb` (what theme_qt.rs `argb()` emits), `#rrggbb` or `#rgb` ->
/// lowercase `rrggbb`. The alpha byte is dropped: SVG paint attributes have no
/// ARGB notation, and every tint token in the registry is opaque.
fn rgb6(value: &str) -> Option<String> {
    let s = value.trim().trim_start_matches('#');
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match s.len() {
        8 => Some(s[2..8].to_ascii_lowercase()),
        6 => Some(s.to_ascii_lowercase()),
        3 => Some(
            s.chars()
                .flat_map(|c| [c.to_ascii_lowercase(), c.to_ascii_lowercase()])
                .collect(),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Process-global state
// ---------------------------------------------------------------------------

/// `<cache>/qbz/icon-tints`, resolved once. `None` = no usable cache dir, and
/// every caller falls back to qrc for the life of the process.
fn tint_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = dirs::cache_dir()?.join("qbz").join("icon-tints");
        match fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                log::warn!("[qbz-qt] icon tint cache unavailable ({e}); using the baked icons");
                None
            }
        }
    })
    .as_deref()
}

/// The parsed tint map for the theme document last seen, keyed by its hash so
/// the hundreds of per-icon calls between two theme switches parse nothing.
static TINTS: Mutex<Option<(u64, HashMap<&'static str, String>)>> = Mutex::new(None);

/// Colours whose directory is known-good on disk (checked once per process).
static BAKED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn hash_of(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// token -> `rrggbb` for every theme-following tint in one document.
fn tints_from_json(json: &str) -> HashMap<&'static str, String> {
    let mut out = HashMap::new();
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    for token in TOKENS {
        if let Some(hex) = doc.get(*token).and_then(|v| v.as_str()).and_then(rgb6) {
            out.insert(*token, hex);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Baking
// ---------------------------------------------------------------------------

/// Materialize `<root>/<hex>/` with all 99 glyphs. Returns false on any I/O
/// failure (caller falls back to qrc).
///
/// The set is built in a temp directory and RENAMED into place, so a crash or
/// a full disk mid-bake can never leave a half-populated directory that later
/// looks complete.
fn bake(root: &Path, hex: &str) -> bool {
    let dir = root.join(hex);
    let fingerprint = master_set_fingerprint();
    if stamp_matches(&dir, &fingerprint) {
        return true;
    }
    // Stale (or hex-stamped by an older build): drop it so the rename below
    // lands. Leaving it would make every future run take the rename-lost
    // branch and keep serving the incomplete set.
    if dir.exists() {
        log::info!("[qbz-qt] icon tint {hex}: icon set changed, re-baking");
        let _ = fs::remove_dir_all(&dir);
    }
    let tmp = root.join(format!(".tmp-{hex}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    if let Err(e) = fs::create_dir_all(&tmp) {
        log::warn!(
            "[qbz-qt] icon tint {hex}: cannot create {}: {e}",
            tmp.display()
        );
        return false;
    }
    let color = format!("#{hex}");
    for (name, source) in MASTERS {
        let tinted = source
            .replace(MASTER_HEX_LOWER, &color)
            .replace(MASTER_HEX_UPPER, &color);
        if let Err(e) = fs::write(tmp.join(format!("{name}.svg")), tinted) {
            log::warn!("[qbz-qt] icon tint {hex}: write {name}.svg failed: {e}");
            let _ = fs::remove_dir_all(&tmp);
            return false;
        }
    }
    if fs::write(tmp.join(STAMP), &fingerprint).is_err() {
        let _ = fs::remove_dir_all(&tmp);
        return false;
    }
    match fs::rename(&tmp, &dir) {
        Ok(()) => {
            log::debug!("[qbz-qt] icon tint {hex}: baked {} glyphs", MASTERS.len());
            true
        }
        Err(_) => {
            // Another run won the race; keep whatever is already there — but
            // only if it is the CURRENT set, or we would report success for a
            // directory missing the very glyph that triggered this bake.
            let _ = fs::remove_dir_all(&tmp);
            stamp_matches(&dir, &fingerprint)
        }
    }
}

/// True when `dir` carries a stamp for the running binary's icon set.
fn stamp_matches(dir: &Path, fingerprint: &str) -> bool {
    fs::read_to_string(dir.join(STAMP))
        .map(|s| s.trim() == fingerprint)
        .unwrap_or(false)
}

/// Keep the cache bounded: drop the oldest complete sets past `MAX_TINT_DIRS`.
/// The set just written has the newest mtime, so it is never the victim.
///
/// Returns the colours removed, so the caller can drop them from the
/// known-good set — otherwise a hex pruned mid-session would keep being
/// handed out as a URL to a directory that is no longer there.
fn prune(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with(".tmp-"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    if dirs.len() <= MAX_TINT_DIRS {
        return Vec::new();
    }
    dirs.sort_by_key(|(t, _)| *t);
    let excess = dirs.len() - MAX_TINT_DIRS;
    let mut dropped = Vec::new();
    for (_, path) in dirs.into_iter().take(excess) {
        if fs::remove_dir_all(&path).is_ok() {
            if let Some(hex) = path.file_name().and_then(|n| n.to_str()) {
                dropped.push(hex.to_string());
            }
        }
    }
    dropped
}

// ---------------------------------------------------------------------------
// The invokable
// ---------------------------------------------------------------------------

impl qbz_icon_tint::QbzIconTint {
    pub fn dir_for(&self, tint: QString, theme_json: QString) -> QString {
        let tint = tint.to_string();
        // Fixed tints ("black", "amber") and anything unknown: qrc.
        let Some(token) = token_for(&tint) else {
            return QString::default();
        };
        let Some(root) = tint_root() else {
            return QString::default();
        };
        let json = theme_json.to_string();
        if json.is_empty() {
            return QString::default();
        }

        let hex = {
            let mut guard = TINTS.lock().unwrap_or_else(|e| e.into_inner());
            let key = hash_of(&json);
            if guard.as_ref().map(|(k, _)| *k) != Some(key) {
                *guard = Some((key, tints_from_json(&json)));
            }
            match guard.as_ref().and_then(|(_, m)| m.get(token)) {
                Some(hex) => hex.clone(),
                // The document does not carry this token (a truncated or
                // pre-publish payload): qrc.
                None => return QString::default(),
            }
        };

        {
            let mut baked = BAKED.lock().unwrap_or_else(|e| e.into_inner());
            if !baked.contains(&hex) {
                if !bake(root, &hex) {
                    return QString::default();
                }
                baked.insert(hex.clone());
                for dropped in prune(root) {
                    baked.remove(&dropped);
                }
            }
        }

        // NOT format!("file://{path}"): on Windows that yields
        // file://C:/... where the drive parses as the URL HOST, and the
        // icon renders as an empty rectangle rather than an error.
        QString::from(&qbz_models::fs_url::file_url(
            &root.join(&hex).to_string_lossy(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_master_carries_the_substitutable_paint() {
        for (name, source) in MASTERS {
            assert!(
                source.contains(MASTER_HEX_LOWER) || source.contains(MASTER_HEX_UPPER),
                "{name}.svg has no #ffffff to tint — it would bake identical in every colour"
            );
        }
    }

    #[test]
    fn masters_cover_every_shipped_glyph() {
        // The qrc bake directories are the contract QML call sites resolve
        // against; a name only in `muted/` (mp3) is exactly how the first
        // gap shipped.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("qml/assets/icons");
        let mut shipped: BTreeSet<String> = BTreeSet::new();
        for tint in ["primary", "secondary", "muted", "accent", "warning"] {
            let Ok(entries) = fs::read_dir(dir.join(tint)) else {
                continue;
            };
            for e in entries.flatten() {
                if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                    shipped.insert(stem.to_string());
                }
            }
        }
        let known: BTreeSet<String> = MASTERS.iter().map(|(n, _)| n.to_string()).collect();
        let missing: Vec<&String> = shipped.difference(&known).collect();
        assert!(missing.is_empty(), "masters missing: {missing:?}");
    }

    #[test]
    fn argb_and_rgb_notations_normalize() {
        assert_eq!(rgb6("#ffcccccc").as_deref(), Some("cccccc"));
        assert_eq!(rgb6("#4285F4").as_deref(), Some("4285f4"));
        assert_eq!(rgb6("#abc").as_deref(), Some("aabbcc"));
        assert_eq!(rgb6("transparent"), None);
    }

    #[test]
    fn fixed_tints_never_resolve_to_a_token() {
        // These three are the whole reason a light theme does not break the
        // glyphs that sit on scrims, gradients and white discs.
        assert!(token_for("primary").is_none());
        assert!(token_for("white").is_none());
        assert!(token_for("black").is_none());
        assert!(token_for("amber").is_none());
        assert_eq!(token_for("muted"), Some("textMuted"));
        assert_eq!(token_for("textPrimary"), Some("textPrimary"));
        assert_eq!(token_for("accentText"), Some("accentText"));
    }

    #[test]
    fn every_token_is_bakeable() {
        for token in TOKENS {
            assert!(
                token_for(token).is_some(),
                "{token} is baked but no tint name resolves to it"
            );
        }
    }
}
