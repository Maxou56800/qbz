//! QbzShell — shell-chrome domain bridge (phase 23 split of the QbzBridge
//! God-object; the pattern is documented in main.rs). Props: sidebar / queue
//! / nav-history / now-playing-bar mode / lyrics panel / window chrome /
//! ambient background / theme / sidebar playlist tree / drag & drop.
//! Invokables: the shell actions (sidebar cycle, nav, npb mode, theme,
//! sidebar tree, drag & drop) — one-line forwards into the crate handlers.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_shell {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Shell chrome (Slint ShellState) ----------------------------
        // Three-state sidebar: 0 = open (240px), 1 = mini (64px), 2 = closed.
        // Seeded from ui_prefs `sidebar_state` and rewritten by cycle_sidebar
        // (1:1 Slint: persist-sidebar-state).
        #[qproperty(i32, sidebar_state)]
        // #771: the open sidebar's width (240..480) and the queue/lyrics
        // column's width (300..600), both persisted in ui_prefs and clamped
        // in Rust (`panel_resize`). QML binds them; the drag handles call the
        // `*_drag` invokables below and never write these directly.
        #[qproperty(i32, sidebar_width)]
        #[qproperty(i32, queue_panel_width)]
        // True from the moment a DRAG snaps the sidebar's state until the next
        // pointer move that merely follows the pointer. Set BEFORE the state
        // so QML's width Behavior (disabled while dragging) sees it and
        // animates the 240<->64 jump instead of teleporting.
        #[qproperty(bool, sidebar_snapping)]
        #[qproperty(bool, queue_open)]
        // --- Section-nav placement (Slint ShellState) ---------------------
        // ON  = the Discover / Library / Local Library / My QBZ rows live in
        //       the SIDEBAR (with a hairline under them); the header shows the
        //       compact icon nav only while the sidebar is fully closed.
        // OFF = the sections live in the HEADER, after the three history
        //       buttons, as full text tabs — or as the icon-only compact form
        //       when `nav_header_compact` is ON (or the sidebar is closed).
        // Both are LIVE (ui_prefs `nav_in_sidebar` / `nav_header_compact`).
        #[qproperty(bool, nav_in_sidebar)]
        #[qproperty(bool, nav_header_compact)]
        // Playlist rows show a 2x2 micro-collage of track covers instead of
        // the generic list-music glyph (Slint SidebarState.playlist-collage,
        // ui_prefs `sidebar_playlist_collage`). Opt-OUT, live.
        #[qproperty(bool, sidebar_playlist_collage)]
        // Content view id — only "home" exists (phase 3 adds the rest).
        #[qproperty(QString, current_view)]
        // Nav history (src/nav_qt.rs):
        #[qproperty(bool, can_back)]
        #[qproperty(bool, can_forward)]
        // --- Scroll-position memory (the Slint NavState.restore-scope /
        // scroll-restore / report-scroll trio, crates/qbz/src/nav.rs:7-14) ---
        //
        // `restore_scope` is the ROUTE ID a back/forward step wants restored,
        // "" when nothing is armed; `scroll_restore` is the `contentY` it
        // should land on. `nav_qt::step` writes BOTH before it writes
        // `current_view`, so the destination view is already armed when the
        // Loader builds it (see the order note on `nav_qt::publish`).
        //
        // WRITABLE FROM QML on purpose: the consuming scroll container clears
        // the scope the moment it applies the offset, which is what stops a
        // second container in the same view — or the same container after a
        // later relayout — from yanking the page back. Same one-shot handshake
        // Slint uses (`NavState.restore-scope = ""` at the apply site).
        #[qproperty(QString, restore_scope)]
        #[qproperty(f32, scroll_restore)]
        // Opaque view-state companion: a routed view reports one JSON
        // snapshot and consumes it on back/forward before it mounts data.
        #[qproperty(QString, restore_state_scope)]
        #[qproperty(QString, state_restore)]
        // --- Flyout landing tab (NavFlyout rows carry view + tab) ----------
        // The tab a nav-flyout click must land on: Discover > For You is
        // nav_tab_view="home" + nav_tab="forYou". Written by
        // `navigate_to_tab`, cleared by a plain `navigate_to`, applied to the
        // mounted view by a Binding in ContentRouter.qml. `nav_tab_seq`
        // bumps per request so re-clicking the SAME entry re-applies the tab
        // (a bare string would not renotify).
        #[qproperty(QString, nav_tab)]
        #[qproperty(QString, nav_tab_view)]
        #[qproperty(i32, nav_tab_seq)]
        // Now-playing bar mode (phase 18): 0 New / 1 Classic / 2 Small /
        // 3 Large — the ui_prefs npb_mode key, live-switchable.
        #[qproperty(i32, npb_mode)]
        // One persisted toggle shared by Appearance, the view-mode flyout and
        // both seekbar mounts.
        #[qproperty(bool, seekbar_waveform)]
        // --- Large-NPB dock (SidebarNowPlayingDock.slint) -----------------
        // The dock is the L's vertical arm: a square cover pinned flush to the
        // window bottom-left, with an optional FFT band above it.
        // Eye toggle on the cover — persists ui_prefs.large_visualizer_on.
        #[qproperty(bool, large_visualizer_on)]
        // Band click cycles 0 Bars / 1 Waveform / 2 Energy
        // (ui_prefs.large_spectrum_mode).
        #[qproperty(i32, large_spectrum_mode)]
        // Total dock height, in lockstep with the toggle. The Sidebar reserves
        // `largeDockHeight - npb height` so the playlist list stops ABOVE the
        // cover instead of running under it, and AppShell pins the dock at
        // `height - largeDockHeight`. Both consumers read THIS — never a
        // literal (Sidebar.slint:1145 is the same contract).
        #[qproperty(f32, large_dock_height)]
        // --- Theme (phase 19) --------------------------------------------
        // ONE JSON token document (theme_qt.rs ThemeTokens: 30 colors + 24
        // alpha tiers + ambient derivations + isDark) — QbzTheme.qml binds
        // to it, so a theme switch repaints the whole app live (the Slint
        // theme::push_colors equivalent).
        #[qproperty(QString, theme_json)]
        // The persisted ui_prefs theme slug ("oled", "auto", "custom", ...).
        #[qproperty(QString, theme_slug)]
        // The dropdown catalog (36 registry themes + Auto/Custom rows).
        #[qproperty(QString, theme_list_json)]
        // Dropdown filter: 0 All / 1 Dark / 2 Light (ui_prefs theme_filter).
        #[qproperty(i32, theme_filter)]
        // Settings > Appearance > Typography & Language > Font, resolved to a
        // family name ("" = System, i.e. leave Qt's own choice alone).
        //
        // The APP's text does not come from here — a plain Text takes the
        // application font at construction, which main() sets before the UI
        // exists (see qml/FontPreload.qml). This property exists so the Qt
        // Quick CONTROLS, which DO follow ApplicationWindow.font, land on the
        // same face instead of staying on Inter while everything else moved.
        //
        // Read once at startup and never written again: the choice cannot be
        // applied to a running UI, so a notify would be a lie.
        #[qproperty(QString, app_font_family)]
        // The custom-theme EDITOR state (custom_theme_qt.rs):
        // {"isDark":bool,"tokens":{"<kebab-key>":"#aarrggbb"}} — the eleven
        // editable base tokens plus the polarity, mirroring the Slint
        // AppearanceState.custom-* swatch properties. The token colours are
        // `#aarrggbb` because QML binds them as `color` fills, like every
        // other theme token; the EDIT/PERSIST side of the same feature is
        // 6-digit `#rrggbb` (see the custom_theme_qt header). Seeded at
        // construction next to `theme_json`, for the same reason.
        //
        // `custom-open-token` (state.slint:3205) is deliberately NOT here:
        // which swatch has the picker open is pure view state and lives in
        // CustomThemeEditor.qml.
        #[qproperty(QString, custom_theme_json)]
        // --- Log viewer (log_viewer_qt.rs) --------------------------------
        // ONE document: {open, rows[], total, shown, filterLevel, search,
        // autoTail, uploading, uploadedUrl}. The ring can hold thousands of
        // lines and the view shows at most 500, so the FILTER runs Rust-side
        // and only the survivors cross the bridge.
        /// The Diagnostics panel's whole document (Settings > Developer).
        /// Seeded with the full shape so every binding reads a real object on
        /// the pre-publish frame.
        #[qproperty(QString, diagnostics_json)]
        #[qproperty(QString, log_viewer_json)]
        // --- Lyrics panel (phase 9) ----------------------------------------
        #[qproperty(bool, lyrics_open)]
        // One JSON document (lyrics_qt.rs LyricsDoc: status/lines/synced/
        // provider/error).
        #[qproperty(QString, lyrics_json)]
        // --- Sidebar playlist tree ---------------------------------------
        // One JSON document: the flattened entries (folders + playlists,
        // expand/sort/search applied Rust-side — sidebar_qt.rs).
        #[qproperty(QString, sidebar_json)]
        #[qproperty(QString, sidebar_sort_by)]
        #[qproperty(bool, sidebar_sort_asc)]
        // Collapsible My QBZ tree feed (opt-in): both grids' rows as
        // `{mixtapes:[{id,name,hidden}], collections:[…]}`. It lives on
        // QbzShell — NOT QbzMyQbz — because the sidebar reads it: QbzMyQbz is
        // not reliably registered in the shell QML context (NavFlyout.qml:89
        // documents the same hazard), so a binding on it errors on first eval,
        // captures no dependency and never updates. QbzShell is always resolved
        // here. Published by `myqbz_qt::publish_sidebar_tree`.
        #[qproperty(QString, myqbz_tree_json)]
        // The MINI-RAIL folder flyout's own document (contract §4.7):
        // `{folderId, folderName, count, rows:[{id,name,isLocal}]}`. It is a
        // separate document from `sidebar_json` because a COLLAPSED folder's
        // children are absent from the flattened entries — listing them used
        // to require force-expanding the folder, a persistent side effect the
        // reference does not have. The default is the FULL SHAPE, never "{}":
        // `JSON.parse("{}").rows.length` throws in the pre-publish frame.
        #[qproperty(QString, sidebar_folder_popup_json)]
        // --- Window chrome (phase 12) --------------------------------------
        // The APPLIED titlebar mode (the ui_prefs `use_system_title_bar`
        // value read at startup — drives the window flags; never mutated at
        // runtime, matching the Slint restart semantics).
        #[qproperty(bool, system_title_bar)]
        // The PERSISTED pref as it stands NOW (the app-menu check state;
        // flips live when the user toggles, applies on the next launch).
        #[qproperty(bool, system_title_bar_pref)]
        // Hide the custom title bar: kills the drawn cluster AND the drag
        // surface (Slint `chrome-drag-enabled`, HeaderBar.slint:594-596).
        // LIVE, unlike `system_title_bar` — nothing negotiates with the
        // compositor here, it is pure in-app layering.
        #[qproperty(bool, hide_title_bar)]
        // The drawn cluster's visibility and side (Slint
        // `show-window-controls` / `wc-position-index`, HeaderBar.slint:
        // 597-609). Both LIVE: the reference re-anchors from these same
        // properties with no restart, which is why its Rust arm is
        // persist-only (main.rs:11265-11271) — Slint's settings view writes
        // the shared state object directly. Qt has no such shared object, so
        // the push has to come from here or the row goes dead.
        #[qproperty(bool, show_window_controls)]
        #[qproperty(bool, wc_on_left)]
        // Put the playing track in the OS window title (app.slint:44). LIVE.
        #[qproperty(bool, window_title_show)]
        // Flip the two-finger swipe mapping for users who run WITHOUT
        // natural scrolling (Slint `invert-swipe-navigation`,
        // AppShell.slint:298-312). LIVE — the gesture layer reads it per
        // gesture, nothing is cached.
        #[qproperty(bool, invert_swipe_navigation)]
        // macOS keeps the NATIVE traffic lights (the overlay window
        // attributes), so the drawn cluster is never mounted there and the
        // header reserves a left inset for the lights instead — Slint
        // `AppearanceState.is-macos`, HeaderBar.slint:598 and :605-607.
        #[qproperty(bool, is_macos)]
        // The RENDERER group is Linux-only in the reference: macOS is always
        // Skia/Metal and Windows negotiates its own backend, so the selector
        // would offer choices that change nothing there
        // (`main.rs:318` seeds `renderer-setting-visible` from the same
        // `cfg!`, and AppearanceSettings.slint:1011-1043 gates the whole
        // group AND the Preferred-GPU row on it).
        #[qproperty(bool, is_linux)]
        // Windows is neither of the two above, and QML must be able to say
        // so: without it every two-way `isMacos ? ... : ...` ternary hands
        // Windows the LINUX arm by default. Compile-time, like both others.
        #[qproperty(bool, is_windows)]
        // The Windows as-is disclaimer. True while the modal should be up.
        //
        // Seeded at CONSTRUCTION, like the window geometry above and for the
        // same reason: Main.qml binds a modal to it and the first frame is
        // already too late for a post-boot push.
        //
        // What it is seeded FROM is a version string, not a boolean. "Don't
        // show again" stores the version that was acknowledged, so a build
        // whose version differs from the stored one shows it again with no
        // separate reset step -- the reset IS the comparison. Dismissing
        // WITHOUT the box ticked only clears this property, so the next launch
        // asks once more.
        #[qproperty(bool, windows_disclaimer_open)]
        // --- Main-window geometry -----------------------------------------
        // The restored LOGICAL size + maximized flag from the shared
        // ui_prefs.json (settings_qt::window_size / window_maximized; the
        // Slint keys). Seeded at CONSTRUCTION, not at boot(): Main.qml binds
        // `width`/`height` to these, and the very first frame is already too
        // late for a post-boot push — same reason `theme_json` is seeded in
        // Default. Never rewritten at runtime; the save path goes the other
        // way, through `save_window_geometry`.
        #[qproperty(f32, window_width)]
        #[qproperty(f32, window_height)]
        #[qproperty(bool, window_maximized)]
        // The app floor (940x600), carried instead of literalised in QML so
        // the number stays single-sourced with the restore clamp and the
        // save gate that use it in settings_qt.
        #[qproperty(f32, window_min_width)]
        #[qproperty(f32, window_min_height)]
        // --- Ambient background (phase 14) --------------------------------
        // 0 = off, 1 = on (the ambient look; the owner's store carries
        // "ambient"). Live — toggling applies immediately (pure QML
        // layering). Combined in QML with npHasTrack for the D4 no-track
        // rule.
        /// Reduce-motion: throttle every continuous animation to a coarse
        /// tick instead of the display rate.
        ///
        /// DERIVED, never a user preference — the reference computes it as
        /// `kiosk_profile || !use_gpu_renderer`
        /// (`qbz-nix/crates/qbz/src/main.rs:8597`) and there is no Settings
        /// row for it in either frontend. Do not add one: the play-indicator
        /// animation toggle is a *preference* and is a different thing.
        ///
        /// Built by the cortinilla-parity contract (§5.g, ruling R10) so the
        /// KIOSK port can consume it rather than having to invent it. See
        /// `set_reduce_motion` for the two halves and why one is dormant.
        #[qproperty(bool, reduce_motion)]
        /// The api QRhi actually gave us: "opengl" | "metal" | "vulkan" |
        /// "d3d11" | "d3d12" | "software" | "null", or "unknown" before the
        /// probe reports. Diagnostics only — features gate on the two
        /// booleans below, never on this string.
        #[qproperty(QString, renderer_api)]
        /// Does the active backend carry the GPU tier? The Qt analogue of
        /// Slint's `use_gpu_renderer` (`crates/qbz/src/main.rs:8448`).
        #[qproperty(bool, gpu_tier)]
        /// May the 6 immersive shader scenes be offered? Off the GPU tier they
        /// render BLACK, which is why the reference hides them from the picker
        /// and the `g` cycle key (`main.rs:8557`).
        ///
        /// **This is the gate the immersive shader scenes were waiting on** —
        /// the immersive contract's D11 records the renderer-tier half as
        /// deliberately absent, and it was the reason the scenes could not
        /// ship.
        #[qproperty(bool, shader_scenes_available)]
        /// May the app-wide dynamic background be offered?
        /// Same tier, same reason (`main.rs:8563`); gates the whole picker row.
        #[qproperty(bool, app_background_available)]
        /// The desktop wallpaper (or the user's own image) as a file:// URL,
        /// and its atmosphere bitmap — the "wallpaper" background modes
        /// (wallpaper_qt.rs). Empty until a mode that paints it resolves one.
        #[qproperty(QString, wallpaper_url)]
        #[qproperty(QString, wallpaper_atmosphere_url)]
        /// This process's windows with their absolute geometry, as a Plasma
        /// Wayland compositor reports them (`[{"x","y","w","h"}, …]`);
        /// "[]" anywhere else. WallpaperField.qml picks the ApplicationWindow
        /// out of it by size (wallpaper_wayland_qt.rs).
        #[qproperty(QString, wallpaper_windows_json)]
        /// Is the kiosk touch shell the one currently mounted?
        ///
        /// The Qt counterpart of `ShellState.kiosk-profile`
        /// (`qbz-ui/ui/state.slint:4054`). Seeded from the resolved profile at
        /// construction and rewritten by the live toggle
        /// (`kiosk_profile_qt::toggle`), which is the ONLY writer.
        ///
        /// QML reads it for the chrome that differs in kiosk: the view menu's
        /// "Kiosk mode"/"Desktop mode" label, and the small bar's flyout,
        /// which in kiosk collapses to the profile row alone
        /// (`shell/PlayerBarSmall.slint:772,780,788,796,811,819,826,847,983`).
        #[qproperty(bool, kiosk_profile)]
        /// Should the window map FULLSCREEN at boot?
        ///
        /// True only when the kiosk profile is active AND
        /// `QBZ_KIOSK_FULLSCREEN` is set — an appliance image sets it so the
        /// panel owns the whole screen (`qbz/src/main.rs:8607-8617`).
        ///
        /// Deliberately NOT derived from the persisted profile alone: a user
        /// who toggles kiosk on the desktop and restarts would be TRAPPED,
        /// because the kiosk shell has no titlebar control and neither Esc nor
        /// F11 leave fullscreen (incident 2026-07-11). Windowed by default
        /// keeps the OS titlebar reachable.
        ///
        /// Seeded at construction and never rewritten: `Main.qml`'s
        /// `visibility` binding must be correct on the FIRST mapped frame, and
        /// a later push would arrive after the window is already on screen.
        #[qproperty(bool, kiosk_fullscreen_boot)]
        /// App-wide dynamic background mode, `AppearanceState
        /// .app-background-mode-index` semantics: 0 = Off, 1 = Ambient (the
        /// album-triad metaball field), 2 = Blurred art (the
        /// ImmersiveAtmosphere cover look). AppShell mounts a DIFFERENT layer
        /// for 1 and 2 — never treat this as a bool.
        #[qproperty(i32, ambient_mode)]
        // Album-art triad (ambient_qt.rs), pushed on track change.
        #[qproperty(QString, ambient_primary)]
        #[qproperty(QString, ambient_secondary)]
        #[qproperty(QString, ambient_accent)]
        // Look knobs (Slint AppearanceState defaults; QBZ_BG_* env seed).
        #[qproperty(f32, ambient_dim)]
        /// The Wallpaper mode's blur, MultiEffect units 0.0-1.0
        /// (settings_qt::wallpaper_blur). Settings writes it while the slider
        /// drags, for a live preview, and persists it on release.
        #[qproperty(f32, wallpaper_blur)]
        /// Debug knob QBZ_BG_SCALE — RETIRED 2026-08-13: the ambient field
        /// renders inline now (no offscreen target to scale), and the knob
        /// measured as a non-lever anyway. Kept to avoid bridge churn.
        #[qproperty(f32, ambient_scale)]
        /// Debug knobs QBZ_VIZ_TICK / QBZ_PANE_LAYER — the two whole-window
        /// redraw levers (settings_qt.rs documents what each measures).
        /// NOTE 2026-08-13: viz_tick_ms is RETIRED (VizSettle ticks off the
        /// pulse now); it stays only to avoid bridge churn.
        #[qproperty(i32, viz_tick_ms)]
        #[qproperty(bool, pane_layer)]
        /// THE shell repaint pulse (2026-08-13 single-clock redesign).
        ///
        /// One Rust thread (`start_shell_pulse`) bumps this every
        /// `settings_qt::shell_pulse_ms()` (default 33 ms, the FFT producer's
        /// TARGET_FPS = 30 period). Every continuous whole-window animator
        /// ticks off its NOTIFY EDGE — the ambient background drift
        /// (ImmersiveAtmosphere) and the visualizer's frame application
        /// (VizSettle) — instead of owning a private Timer. Qt Quick has no
        /// dirty-region rendering, so N unsynchronised ~30 Hz clocks cost
        /// N x 30 full-window presents a second; on one edge they all dirty
        /// the scene in the SAME event-loop turn and the window presents
        /// once per period. That rate is the shell's dominant GPU term
        /// (render = 2 ms, swap = 12-24 ms on the owner's 4070 — the frames
        /// are GPU-bound, so presents/s x window area is the whole bill).
        ///
        /// CONTRACT for QML: tick ONLY off this edge (a Connections on
        /// QbzShell with `onPulseMsChanged`), and a handler that writes no
        /// property costs no frame — the notify alone never schedules a
        /// repaint, so an idle shell stays at zero presents. Never introduce
        /// a component-local Timer/NumberAnimation/FrameAnimation for a
        /// continuous animation; that is the exact regression this pulse
        /// replaced.
        ///
        /// The value is a wrapping millisecond counter (24 h wrap) whose
        /// absolute value means nothing — consumers accumulate their own
        /// local tick on the edge, so a component can freeze/reset its pose
        /// without touching the shared clock. The thread runs unconditionally
        /// (unlike the viz drain, which owns its enable bit): ~30 event-loop
        /// wakeups a second with zero frames when nothing animates, because
        /// Rust cannot see QML mount state and a gate it cannot see would be
        /// a lie.
        ///
        /// Release note: docs/release-2.1.0/CHANGELOG.md (first Qt release).
        #[qproperty(i32, pulse_ms)]
        #[qproperty(f32, ambient_surface_alpha)]
        #[qproperty(f32, ambient_bar_alpha)]
        // --- Rounded-cover render arm (manual override) --------------------
        // `RoundedImage` masks covers with a MultiEffect layer and falls back
        // to its CPU Canvas raster automatically when `GraphicsInfo.api`
        // reports Software/Null. This pins the Canvas arm by hand on a machine
        // where GraphicsInfo reports a GPU and the mask still misbehaves,
        // without a rebuild: env `QBZ_QT_ROUND_MODE=canvas`. Default false.
        // Seeded in `impl Default`, NOT in `boot()` — QML singletons
        // instantiate lazily and the first access is a `RoundedImage`
        // `_useCanvas` read, which can precede `QbzShell.boot()`.
        #[qproperty(bool, force_canvas_art)]
        // --- In-app toasts (shared) ---------------------------------------
        // ONE JSON document (toast_qt.rs ToastDoc: seq / kind / message /
        // persistent), rendered by the single `controls/QbzToast.qml` host
        // mounted in AppShell. Default "{}"; `seq` is monotonic and starts at
        // 1, so the host hides itself while `seq <= 0` and re-shows even when
        // the SAME message repeats.
        //
        // Lives on QbzShell rather than getting a bridge of its own for the
        // same reason the drag ghost does: the producers are everywhere (any
        // controller, any thread) and the consumer is one window-level
        // overlay. Rust owns no timer — the auto-hide delay is keyed off `seq`
        // in QML, so a toast never needs a hide round-trip.
        #[qproperty(QString, toast_json)]
        // --- Drag & drop (phase 17) ----------------------------------------
        // The shared drag state (DragState in state.slint): the ghost reads
        // count/title/subtitle + the window-coord pointer; sidebar playlist
        // rows self-detect the drop via dragX/dragY + over-playlist-id.
        #[qproperty(bool, drag_active)]
        #[qproperty(i32, drag_count)]
        #[qproperty(QString, drag_title)]
        #[qproperty(QString, drag_subtitle)]
        #[qproperty(f32, drag_x)]
        #[qproperty(f32, drag_y)]
        // True while the drag source supplies a full-size inline visual. The
        // window-level compact pill stands down in that region; if the row
        // leaves it (for example toward a sidebar playlist), the source flips
        // this off and the portable ghost resumes.
        #[qproperty(bool, drag_inline_visual)]
        #[qproperty(QString, drag_over_playlist_id)]
        // The QUEUE drop target: the upcoming-list SLOT the dragged row would
        // be inserted at, or -1 when the pointer is not over the queue. Two
        // separate claims rather than one tagged target, because they are
        // mutually exclusive by geometry (the panel and the sidebar cannot
        // both be under the pointer) and a slot is an int, not an id — folding
        // them into one string would mean parsing a discriminator on every
        // pointer move.
        //
        // The panel reads it to draw the insertion line, so it has to be a
        // property and not just Rust-side state.
        #[qproperty(i32, drag_over_queue_index)]
        // Floating album-art preview over the now-playing bar's small cover
        // (SongCard.slint:296 + ArtPreviewOverlay.slint). Anchor is in WINDOW
        // coordinates: x = the cover's horizontal centre, y = its top edge.
        // Carried here for the same reason the drag ghost is: the trigger
        // lives inside the bar, the overlay must paint above every surface, and
        // the two are in different .qml files.
        /// APPLIED-FILTERS TOOLTIP CHANNEL. Written by QML, read by QML — the
        /// bridge is only the wire.
        ///
        /// A filter funnel lives deep inside a view's toolbar; the tooltip
        /// overlay is a sibling of the whole shell. A QML `id` does not cross
        /// documents, so the two cannot see each other, and threading an
        /// `Item` down five levels for every one of eighteen filter surfaces is
        /// the kind of plumbing that gets half-done. The art preview solved the
        /// identical problem the identical way (`art_preview_*` below): the
        /// state rides the bridge.
        ///
        /// `{ key, x, y, w, h, groups: [{group, values[]}] }` in SCENE
        /// coordinates (the overlay fills the window, so they are its own), or
        /// `{}` to hide. Numbers, never an Item reference — a recycled toolbar
        /// must not be able to dangle here.
        #[qproperty(QString, filter_tip_json)]
        #[qproperty(bool, art_preview_show)]
        #[qproperty(f32, art_preview_x)]
        #[qproperty(f32, art_preview_y)]
        // --- Multi-select seam (2026-08-03 hotkeys-port contract §4.6) ----
        // QML-REPORTED — selection state is in-view QML JS
        // (`library_bulk.rs:8`: "select-all / clear never reach Rust"), so the
        // AppShell reporter Bindings write these: a view implementing the
        // duck-typed `selectAll()` is mounted (capable) / a view's
        // `multiSelectOn` flag is up (active). CUSTOM WRITE (the
        // `QbzImmersive.open` funnel precedent) so the Rust-side AtomicBool
        // mirrors stay exact for the cross-singleton hotkeys reads — the
        // Ctrl+A consumption predicate and the §1.2 Escape stack arm 6 run
        // on the QbzHotkeys singleton and cannot reach this QObject's
        // properties.
        #[qproperty(bool, multi_select_active, READ, WRITE = set_multi_select_active, NOTIFY)]
        #[qproperty(bool, multi_select_capable, READ, WRITE = set_multi_select_capable, NOTIFY)]
        type QbzShell = super::QbzShellRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY
        /// domain singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzShell>);

        /// Report the api QRhi actually resolved to. Called ONCE, by the
        /// renderer probe in `Main.qml`, with `GraphicsInfo.api` mapped to a
        /// name in QML — so Rust never depends on the numeric enum, whose
        /// values are a Qt implementation detail.
        ///
        /// Only QML can answer this: `apply_renderer_preference()` runs before
        /// `QGuiApplication` and knows only what we ASKED for, and a driver
        /// that refuses the request lands us somewhere else entirely.
        /// "unknown" (the pre-resolution value) is ignored, not latched.
        #[qinvokable]
        fn report_renderer_api(self: Pin<&mut QbzShell>, api: QString);

        /// Close the Windows disclaimer. `remember` = the "Don't show this
        /// again" box was ticked, which persists the CURRENT version as
        /// acknowledged; the next release therefore shows it again.
        #[qinvokable]
        fn dismiss_windows_disclaimer(self: Pin<&mut QbzShell>, remember: bool);

        /// The frame-liveness watchdog's verdict (PARITY-DEBT #104's other
        /// owed half). Reported once by `Main.qml` after a settling window,
        /// with the number of frames the window actually swapped in it.
        ///
        /// Proof of frames — not merely of a window — is what disarms the
        /// graphics startup sentinels: a backend or GPU can create a window and
        /// then never present, which looks identical to a healthy start until
        /// the user sees a frozen pane.
        #[qinvokable]
        fn report_frame_liveness(self: Pin<&mut QbzShell>, frames: i32);

        // --- Shell chrome -------------------------------------------------
        /// Header panel-left button: cycle the sidebar open -> mini ->
        /// closed -> open (Slint `ShellState.cycle-sidebar()`).
        #[qinvokable]
        fn cycle_sidebar(self: Pin<&mut QbzShell>);
        /// NPB queue button / queue panel close.
        #[qinvokable]
        fn toggle_queue(self: Pin<&mut QbzShell>);
        /// #771 sidebar edge drag: `x` is the pointer's window-left-relative
        /// px. Applies the snap rules (`panel_resize::sidebar_drag_target`)
        /// LIVE: open widths follow the pointer, past the thresholds the
        /// sidebar snaps to mini / closed exactly as the header button would.
        #[qinvokable]
        fn sidebar_drag(self: Pin<&mut QbzShell>, x: f64);
        /// Pointer released: persist the width the drag ended on.
        #[qinvokable]
        fn sidebar_drag_end(self: Pin<&mut QbzShell>);
        /// #771 column edge drag: `width` is window-right minus pointer x.
        /// Past the threshold the whole column closes (queue and lyrics),
        /// as their X buttons would.
        #[qinvokable]
        fn queue_panel_drag(self: Pin<&mut QbzShell>, width: f64);
        #[qinvokable]
        fn queue_panel_drag_end(self: Pin<&mut QbzShell>);
        /// Header history buttons.
        #[qinvokable]
        fn navigate_back(self: Pin<&mut QbzShell>);
        /// Re-resolve the desktop wallpaper (window activation, a mode switch).
        #[qinvokable]
        fn refresh_wallpaper(self: Pin<&mut QbzShell>);
        /// Start following this window's place on a Plasma Wayland desktop
        /// (the Wallpaper mode on Wayland). Idempotent; a no-op elsewhere.
        #[qinvokable]
        fn track_window_position(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn navigate_forward(self: Pin<&mut QbzShell>);

        /// NPB lyrics button / lyrics panel close X.
        #[qinvokable]
        fn toggle_lyrics(self: Pin<&mut QbzShell>);

        /// The mounted page's live scroll offset and the SCOPE it belongs to
        /// ("album", "library:albums", ...), reported on every `contentY`
        /// change by controls/ScrollMemory.qml. Read only when a navigation
        /// stamps the page it is leaving, so this is a store and nothing else
        /// — no notify, no binding re-evaluation, and no allocation once the
        /// page has reported once. (Slint wires the same callback,
        /// `NavState.report-scroll`; the scope is this port's addition — see
        /// the note on `nav_qt::Entry::scope` for why it cannot be derived
        /// from the route here the way Slint derives it from the entry.)
        #[qinvokable]
        fn report_scroll(self: Pin<&mut QbzShell>, scope: QString, y: f32);
        #[qinvokable]
        fn report_nav_state(self: Pin<&mut QbzShell>, scope: QString, state: QString);
        #[qinvokable]
        fn record_local_tab(self: Pin<&mut QbzShell>, tab: QString, state: QString);
        #[qinvokable]
        fn record_kiosk_tab(self: Pin<&mut QbzShell>, view: QString, tab: QString, state: QString);
        #[qinvokable]
        fn local_navigation_state(self: &QbzShell) -> QString;
        #[qinvokable]
        fn navigation_state(self: &QbzShell, view: QString) -> QString;

        /// Sidebar navigation: record a content view ("home" | "library")
        /// and lazy-load its data on first visit.
        #[qinvokable]
        fn navigate_to(self: Pin<&mut QbzShell>, view: QString);
        /// NavFlyout entry activation: navigate AND land on a view-internal
        /// tab ("home" + "forYou"). The tab rides the bridge (navTab /
        /// navTabView / navTabSeq) because a QML id cannot cross documents —
        /// ContentRouter applies it to the mounted view through a Binding.
        #[qinvokable]
        fn navigate_to_tab(self: Pin<&mut QbzShell>, view: QString, tab: QString);
        /// Open a url in the system browser (the Slint
        /// AlbumActions.open-external-link). Spawns xdg-open detached.
        #[qinvokable]
        fn open_external_url(self: Pin<&mut QbzShell>, url: QString);

        /// Now-Playing-view flyout: switch the bar mode (0 New / 1 Classic /
        /// 2 Small / 3 Large) — persists ui_prefs.npb_mode; Large forces the
        /// sidebar open (the Slint "large" arm).
        #[qinvokable]
        fn npb_set_mode(self: Pin<&mut QbzShell>, mode: i32);

        /// Large dock, cover eye button: show/hide the FFT band. Persists the
        /// pref, republishes `largeDockHeight`, and gates the capture tap —
        /// hiding the band stops the FFT producer outright.
        #[qinvokable]
        fn large_toggle_visualizer(self: Pin<&mut QbzShell>);
        /// Large dock, band click: cycle Bars -> Waveform -> Energy.
        #[qinvokable]
        fn large_cycle_spectrum(self: Pin<&mut QbzShell>);

        // --- Log viewer (log_viewer_qt.rs) --------------------------------
        /// Open / close the viewer. `close` also drops auto-tail, so a closed
        /// modal never keeps republishing.
        #[qinvokable]
        fn log_open(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn log_close(self: Pin<&mut QbzShell>);
        /// "all" | "error" | "warn" | "info" | "debug" | "trace".
        #[qinvokable]
        fn log_set_level(self: Pin<&mut QbzShell>, level: QString);
        /// Case-insensitive substring over target + message.
        #[qinvokable]
        fn log_set_search(self: Pin<&mut QbzShell>, search: QString);
        /// Re-snapshot the ring now.
        #[qinvokable]
        fn log_refresh(self: Pin<&mut QbzShell>);
        /// Republish once a second while open.
        #[qinvokable]
        fn log_set_auto_tail(self: Pin<&mut QbzShell>, on: bool);
        /// Empty the ring (the in-memory history only — the log FILE is
        /// untouched, same as the reference).
        #[qinvokable]
        fn log_clear(self: Pin<&mut QbzShell>);
        /// The filtered view, as plain redacted lines.
        #[qinvokable]
        fn log_copy_all(self: Pin<&mut QbzShell>);
        /// The GitHub-ready `<details>` bundle.
        #[qinvokable]
        fn log_copy_bundle(self: Pin<&mut QbzShell>);
        /// Diagnostics panel (Settings > Developer). The open state reaches
        /// Rust because it is what gates the publisher — a collapsed panel is
        /// not reading and a publish can carry a hundred rows.
        #[qinvokable]
        fn diag_set_open(self: Pin<&mut QbzShell>, open: bool);
        #[qinvokable]
        fn diag_refresh(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn diag_export_clipboard(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn diag_cast_scan(self: Pin<&mut QbzShell>);
        /// The header menu's "Report an Issue" row — the GitHub bug template.
        #[qinvokable]
        fn report_issue_open(self: Pin<&mut QbzShell>);
        /// Upload the bundle to a public paste; the url lands in the document.
        #[qinvokable]
        fn log_upload(self: Pin<&mut QbzShell>);
        /// Copy the uploaded paste url.
        #[qinvokable]
        fn log_copy_url(self: Pin<&mut QbzShell>);
        /// Hand the log FILE to the desktop (what "Share logs" used to do on
        /// its own).
        #[qinvokable]
        fn log_open_file(self: Pin<&mut QbzShell>);

        /// macOS only (a no-op elsewhere): apply the overlay window
        /// attributes and vertically centre the native traffic lights in the
        /// header. Called from `Main.qml` on the first rendered frame —
        /// AppKit has no main window before that, and the call is idempotent
        /// so a retry costs nothing. See `macos_chrome.rs`.
        /// Returns TRUE once the chrome is actually applied; the caller
        /// retries while it is false (AppKit has no main window on the first
        /// rendered frame).
        #[qinvokable]
        fn apply_mac_chrome(self: Pin<&mut QbzShell>) -> bool;

        // --- Theme (phase 19) ---------------------------------------------
        /// Appearance > Theme row: persist the picked slug + republish
        /// `themeJson` (live switch).
        #[qinvokable]
        fn theme_set(self: Pin<&mut QbzShell>, slug: QString);
        /// Appearance > theme filter cycle button (0 All / 1 Dark / 2 Light).
        #[qinvokable]
        fn theme_set_filter(self: Pin<&mut QbzShell>, index: i32);

        // --- Custom-theme editor (custom_theme_qt.rs) ----------------------
        /// Set one editable base token from a `#rrggbb` string — BOTH the live
        /// ColorPicker drag and the HEX-field commit (the reference splits
        /// these into `custom-set-token` / `custom-set-token-hex`, which is a
        /// Slint artifact: there a colour and a string are different types).
        /// Re-derives and republishes the whole palette live; the disk write
        /// is debounced. Malformed hex and unknown keys are ignored.
        #[qinvokable]
        fn custom_set_token(self: Pin<&mut QbzShell>, key: QString, hex: QString);
        /// Persist NOW if a live edit is pending — called on colour-drag end
        /// and on a HEX commit, so a settled edit never waits out the debounce.
        #[qinvokable]
        fn custom_flush(self: Pin<&mut QbzShell>);
        /// Flip the custom theme's light/dark polarity (derived shades,
        /// borders and overlays follow; the base colours do not change).
        #[qinvokable]
        fn custom_toggle_dark(self: Pin<&mut QbzShell>, is_dark: bool);
        /// "Use current colors": snapshot the applied palette into the
        /// editable base, persist, apply and republish the swatches.
        #[qinvokable]
        fn custom_seed_from_current(self: Pin<&mut QbzShell>);

        /// Main.qml, debounced off every settled resize / visibility flip and
        /// fired once more on close: persist the FLOATING size plus the
        /// maximized flag. The whole rule set (floating-only sizes, the app
        /// minimum, the >0.5px dirty check) lives in
        /// `settings_qt::save_window_geometry`, mirroring the Slint
        /// `WindowEvent::Resized` handler — QML only supplies the numbers.
        #[qinvokable]
        fn save_window_geometry(
            self: Pin<&mut QbzShell>,
            width: f32,
            height: f32,
            maximized: bool,
            fullscreen: bool,
        );

        /// App-menu chrome toggle: flip the persisted `use_system_title_bar`
        /// pref (applies on the next launch — the window flags are fixed at
        /// creation, 1:1 Slint). Updates `systemTitleBarPref` only.
        #[qinvokable]
        fn toggle_system_title_bar(self: Pin<&mut QbzShell>);

        /// App-menu ambient toggle: flip the persisted `app_background` pref
        /// off <-> the last picked mode ("ambient" or "blurred") and apply
        /// LIVE (pure QML layering — no restart, unlike the titlebar).
        #[qinvokable]
        fn toggle_ambient_background(self: Pin<&mut QbzShell>);

        /// Sidebar tree: rebuild + republish (the `…` menu's Refresh row).
        ///
        /// Routes to `crate::reload_sidebar_including_local()`, NOT
        /// `crate::reload_sidebar()`. The latter early-returns while offline,
        /// which made the ONE recovery affordance for a broken tree unusable in
        /// exactly the state where the user needs it — and an account-less user
        /// has nothing else. The local-safe verb is not a no-op offline: it
        /// re-reads folders, folder membership, the hidden set and the LOCAL
        /// playlists from library.db and republishes, while `sidebar_qt::load`
        /// preserves the cached Qobuz set rather than wiping it (see its
        /// header). Online the two verbs do the same thing.
        #[qinvokable]
        fn reload_sidebar(self: Pin<&mut QbzShell>);
        /// Collapsible My QBZ tree actions, routed through QbzShell so the
        /// sidebar never touches QbzMyQbz (not reliably registered here).
        #[qinvokable]
        fn myqbz_open_card(self: Pin<&mut QbzShell>, id: QString);
        #[qinvokable]
        fn myqbz_set_hidden(self: Pin<&mut QbzShell>, id: QString, hidden: bool);
        /// Open the create modal (app-global MyQbzModals) for "mixtape" |
        /// "collection", and force a tree republish, both from the sidebar.
        #[qinvokable]
        fn myqbz_create_open(self: Pin<&mut QbzShell>, kind: QString);
        #[qinvokable]
        fn myqbz_refresh_tree(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn sidebar_set_sort(self: Pin<&mut QbzShell>, option: QString);
        #[qinvokable]
        fn sidebar_search(self: Pin<&mut QbzShell>, query: QString);
        #[qinvokable]
        fn sidebar_toggle_folder(self: Pin<&mut QbzShell>, id: QString);
        /// Mini-rail folder click: publish that folder's playlists into
        /// `sidebar_folder_popup_json`. Reads the sidebar CACHE only — no DB,
        /// no network, no expand side effect — so it behaves identically
        /// offline (contract §4.7 / block 6).
        #[qinvokable]
        fn sidebar_open_folder_popup(self: Pin<&mut QbzShell>, folder_id: QString);
        /// Sidebar cover dispatch: JSON array of cover URLS (the tree's
        /// collage is url-keyed, unlike the feed's artKey).
        #[qinvokable]
        fn sidebar_artwork_window(self: Pin<&mut QbzShell>, urls_json: QString);

        // --- Drag & drop (phase 17) ----------------------------------------
        /// Track-row press-drag start (row id, ghost texts, window coords).
        #[qinvokable]
        fn drag_start(
            self: Pin<&mut QbzShell>,
            track_id: QString,
            title: QString,
            subtitle: QString,
            x: f32,
            y: f32,
            inline_visual: bool,
        );
        /// LOCAL-mode twin: a Local Library / Explorer row enters the shared
        /// drag by ROW ID (resolved to source-aware refs at drop time).
        #[qinvokable]
        fn drag_start_local(
            self: Pin<&mut QbzShell>,
            row_id: QString,
            title: QString,
            subtitle: QString,
            x: f32,
            y: f32,
            inline_visual: bool,
        );
        #[qinvokable]
        fn drag_move(self: Pin<&mut QbzShell>, x: f32, y: f32);
        /// A sidebar playlist row claims / releases the drop target.
        #[qinvokable]
        fn drag_set_over(self: Pin<&mut QbzShell>, playlist_id: QString);
        /// Claim (>= 0) or release (-1) the queue as the drop target, with the
        /// upcoming SLOT the row would land on.
        #[qinvokable]
        fn drag_set_over_queue(self: Pin<&mut QbzShell>, slot: i32);
        /// Release: add the dragged track(s) to the over-playlist target.
        #[qinvokable]
        fn drag_end(self: Pin<&mut QbzShell>);

        // --- Multi-select seam (§4.6) --------------------------------------
        /// The hotkeys (C2) Ctrl+A arm / the §1.2 Escape-stack arm 6. Rust
        /// cannot reach the content-view Loader (AppShell.qml), so both
        /// bounce to QML as a signal; the AppShell router forwards to the
        /// mounted view's duck-typed `selectAll()` /
        /// `exitMultiSelectMode()`.
        #[qinvokable]
        fn select_all_active(self: Pin<&mut QbzShell>);
        #[qinvokable]
        fn exit_multi_select(self: Pin<&mut QbzShell>);
        #[qsignal]
        fn select_all_requested(self: Pin<&mut QbzShell>);
        #[qsignal]
        fn exit_multi_select_requested(self: Pin<&mut QbzShell>);
        /// Offline-cache row status fan-out (offline_cache_qt::row_sink):
        /// 0 none · 1 queued · 2 downloading · 3 ready · 4 failed. Views
        /// patch the matching track row inside their own document copy.
        #[qsignal]
        fn track_cache_status_changed(
            self: Pin<&mut QbzShell>,
            track_id: QString,
            status: i32,
            progress: f64,
        );
    }

    // The custom WRITE targets of the §4.6 seam properties. NOT qinvokables —
    // QML reaches them by writing `QbzShell.multiSelectActive` /
    // `multiSelectCapable`, never by name (the `QbzImmersive.open` cxx-qt
    // custom-setter pattern: the property's auto setter is replaced by this
    // method, which owns the store + notify + mirror).
    unsafe extern "RustQt" {
        fn set_multi_select_active(self: Pin<&mut QbzShell>, value: bool);
        fn set_multi_select_capable(self: Pin<&mut QbzShell>, value: bool);
    }

    impl cxx_qt::Threading for QbzShell {}
}

use qbz_shell::QbzShell;

/// Rust side of the shell bridge (plain storage, phase-1 pattern).
pub struct QbzShellRust {
    sidebar_state: i32,
    sidebar_width: i32,
    queue_panel_width: i32,
    sidebar_snapping: bool,
    nav_in_sidebar: bool,
    nav_header_compact: bool,
    sidebar_playlist_collage: bool,
    queue_open: bool,
    current_view: QString,
    can_back: bool,
    can_forward: bool,
    restore_scope: QString,
    scroll_restore: f32,
    restore_state_scope: QString,
    state_restore: QString,
    nav_tab: QString,
    nav_tab_view: QString,
    nav_tab_seq: i32,
    npb_mode: i32,
    seekbar_waveform: bool,
    large_visualizer_on: bool,
    large_spectrum_mode: i32,
    large_dock_height: f32,
    theme_json: QString,
    theme_slug: QString,
    theme_list_json: QString,
    theme_filter: i32,
    app_font_family: QString,
    custom_theme_json: QString,
    diagnostics_json: QString,
    log_viewer_json: QString,
    lyrics_open: bool,
    lyrics_json: QString,
    sidebar_json: QString,
    sidebar_sort_by: QString,
    sidebar_sort_asc: bool,
    myqbz_tree_json: QString,
    sidebar_folder_popup_json: QString,
    system_title_bar: bool,
    system_title_bar_pref: bool,
    hide_title_bar: bool,
    show_window_controls: bool,
    wc_on_left: bool,
    window_title_show: bool,
    invert_swipe_navigation: bool,
    is_macos: bool,
    is_linux: bool,
    is_windows: bool,
    windows_disclaimer_open: bool,
    window_width: f32,
    window_height: f32,
    window_maximized: bool,
    window_min_width: f32,
    window_min_height: f32,
    reduce_motion: bool,
    renderer_api: QString,
    gpu_tier: bool,
    shader_scenes_available: bool,
    app_background_available: bool,
    wallpaper_url: QString,
    wallpaper_atmosphere_url: QString,
    wallpaper_windows_json: QString,
    kiosk_profile: bool,
    kiosk_fullscreen_boot: bool,
    ambient_mode: i32,
    ambient_primary: QString,
    ambient_secondary: QString,
    ambient_accent: QString,
    ambient_dim: f32,
    wallpaper_blur: f32,
    ambient_scale: f32,
    viz_tick_ms: i32,
    pane_layer: bool,
    pulse_ms: i32,
    ambient_surface_alpha: f32,
    ambient_bar_alpha: f32,
    force_canvas_art: bool,
    toast_json: QString,
    drag_active: bool,
    drag_count: i32,
    drag_title: QString,
    drag_subtitle: QString,
    drag_x: f32,
    drag_y: f32,
    drag_inline_visual: bool,
    drag_over_playlist_id: QString,
    drag_over_queue_index: i32,
    filter_tip_json: QString,
    art_preview_show: bool,
    art_preview_x: f32,
    art_preview_y: f32,
    multi_select_active: bool,
    multi_select_capable: bool,
}

impl Default for QbzShellRust {
    fn default() -> Self {
        // One file read for the restored size (the pair is all-or-nothing —
        // see settings_qt::window_size).
        let (window_width, window_height) = crate::settings_qt::window_size();
        let seekbar_waveform = crate::settings_qt::seekbar_waveform();
        qbz_audio::set_seek_waveform_enabled(seekbar_waveform);
        Self {
            sidebar_state: crate::settings_qt::sidebar_state(),
            sidebar_width: crate::settings_qt::sidebar_width(),
            queue_panel_width: crate::settings_qt::queue_panel_width(),
            sidebar_snapping: false,
            nav_in_sidebar: crate::settings_qt::nav_in_sidebar(),
            nav_header_compact: crate::settings_qt::nav_header_compact(),
            sidebar_playlist_collage: crate::settings_qt::sidebar_playlist_collage(),
            queue_open: false,
            // The persisted startup page (Settings > Appearance > Startup
            // page). Resolved at CONSTRUCTION so the first mounted view is
            // already the right one — routing after the fact would flash Home
            // and then swap, which is worse than not restoring at all.
            // `startup_view()` returns "home" unless the pref says "remember"
            // AND the stored view is in the safe set.
            current_view: QString::from(crate::nav_qt::startup_view().as_str()),
            can_back: false,
            restore_scope: QString::default(),
            scroll_restore: 0.0,
            restore_state_scope: QString::default(),
            state_restore: QString::default(),
            nav_tab: QString::default(),
            nav_tab_view: QString::default(),
            nav_tab_seq: 0,
            can_forward: false,
            npb_mode: crate::settings_qt::npb_mode_index(),
            seekbar_waveform,
            large_visualizer_on: crate::settings_qt::large_visualizer_on(),
            large_spectrum_mode: crate::settings_qt::large_spectrum_mode(),
            large_dock_height: large_dock_height(crate::settings_qt::large_visualizer_on()),
            theme_json: QString::from(crate::theme_qt::theme_json().as_str()),
            theme_slug: QString::from(crate::theme_qt::current_slug().as_str()),
            theme_list_json: QString::from(crate::theme_qt::theme_list_json().as_str()),
            theme_filter: crate::theme_qt::theme_filter(),
            app_font_family: QString::from(crate::settings_qt::app_font_family().as_str()),
            custom_theme_json: QString::from(crate::custom_theme_qt::state_json().as_str()),
            diagnostics_json: QString::from(crate::diagnostics_qt::empty_doc_json().as_str()),
            log_viewer_json: QString::from("{}"),
            lyrics_open: false,
            lyrics_json: QString::from("{}"),
            sidebar_json: QString::from("[]"),
            sidebar_sort_by: QString::from("name"),
            sidebar_sort_asc: true,
            myqbz_tree_json: QString::from("{\"mixtapes\":[],\"collections\":[]}"),
            // FULL SHAPE, not "{}" — see the qproperty comment.
            sidebar_folder_popup_json: QString::from(
                r#"{"folderId":"","folderName":"","count":0,"rows":[]}"#,
            ),
            system_title_bar: crate::settings_qt::use_system_title_bar(),
            system_title_bar_pref: crate::settings_qt::use_system_title_bar(),
            hide_title_bar: crate::settings_qt::hide_title_bar(),
            show_window_controls: crate::settings_qt::show_window_controls(),
            wc_on_left: crate::settings_qt::wc_on_left(),
            window_title_show: crate::settings_qt::window_title_show(),
            invert_swipe_navigation: crate::settings_qt::invert_swipe_navigation(),
            // Compile-time, not a pref: the reference sets `is-macos` from
            // the same `cfg!` (main.rs seeds AppearanceState at startup).
            is_macos: cfg!(target_os = "macos"),
            is_linux: cfg!(target_os = "linux"),
            is_windows: cfg!(target_os = "windows"),
            windows_disclaimer_open: crate::shell_bridge::windows_disclaimer_pending(),
            window_width,
            window_height,
            window_maximized: crate::settings_qt::window_maximized(),
            window_min_width: crate::settings_qt::window_min_size().0,
            window_min_height: crate::settings_qt::window_min_size().1,
            reduce_motion: reduce_motion_at_boot(),
            // The tier starts GPU-capable: both platforms were MEASURED on the
            // GPU (OpenGL RHI / Metal, 2026-07-29), so it is the honest prior,
            // and it keeps the pre-probe frames behaving exactly as they did
            // before this seam existed. `renderer_qt` explains why a `false`
            // default would be the worse guess.
            renderer_api: QString::from("unknown"),
            gpu_tier: crate::renderer_qt::gpu_tier(),
            shader_scenes_available: crate::renderer_qt::gpu_tier(),
            app_background_available: crate::renderer_qt::gpu_tier(),
            wallpaper_url: QString::default(),
            wallpaper_atmosphere_url: QString::default(),
            wallpaper_windows_json: QString::from("[]"),
            kiosk_profile: crate::kiosk_profile_qt::active(),
            kiosk_fullscreen_boot: crate::kiosk_profile_qt::active()
                && crate::kiosk_profile_qt::fullscreen_at_boot(),
            ambient_mode: crate::settings_qt::app_background_mode(),
            // The Slint ImmersiveState default triad (pre-artwork colors).
            ambient_primary: QString::from("#00dcc8"),
            ambient_secondary: QString::from("#9632ff"),
            ambient_accent: QString::from("#3fd9c8"),
            ambient_dim: crate::settings_qt::ambient_dim(),
            wallpaper_blur: crate::settings_qt::wallpaper_blur(),
            ambient_scale: crate::settings_qt::ambient_scale(),
            viz_tick_ms: crate::settings_qt::viz_tick_ms(),
            pane_layer: crate::settings_qt::pane_layer(),
            pulse_ms: 0,
            ambient_surface_alpha: crate::settings_qt::ambient_surface_alpha(),
            ambient_bar_alpha: crate::settings_qt::ambient_bar_alpha(),
            // Read here, not in boot(): the first QML access to this singleton
            // is a RoundedImage `_useCanvas` read, which can happen before
            // QbzShell.boot() runs, and a boot() seed would leave the arm
            // decided against a stale `false`.
            force_canvas_art: std::env::var("QBZ_QT_ROUND_MODE").as_deref() == Ok("canvas"),
            // seq 0 = "nothing has been published"; the host stays hidden.
            toast_json: QString::from("{}"),
            drag_active: false,
            drag_count: 0,
            drag_title: QString::default(),
            drag_subtitle: QString::default(),
            drag_x: 0.0,
            drag_y: 0.0,
            drag_inline_visual: false,
            drag_over_playlist_id: QString::default(),
            // -1 = the pointer is not over the queue. NOT 0, which is a valid
            // slot (drop at the very top of upcoming).
            drag_over_queue_index: -1,
            // "{}" so the overlay's JSON.parse never throws on the first frame.
            filter_tip_json: QString::from("{}"),
            art_preview_show: false,
            art_preview_x: 0.0,
            art_preview_y: 0.0,
            multi_select_active: false,
            multi_select_capable: false,
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Large-NPB dock geometry
// ---------------------------------------------------------------------------
// The cover is square and as wide as the sidebar's content column (240
// sidebar - 16 left - 16 right gutters = 208). Legacy spectrum modes use a
// compact 42px band; scope modes use the same 208px square as the artwork and
// grow upward from their bottom edge. SidebarNowPlayingDock.qml derives its
// band height from the same mode boundary.
pub(crate) const DOCK_ART_SIZE: f32 = 208.0;
pub(crate) const DOCK_BAND_HEIGHT: f32 = 42.0;
pub(crate) const DOCK_PAD_TOP: f32 = 9.0;
pub(crate) const DOCK_PAD_BOTTOM: f32 = 4.0;
pub(crate) const DOCK_BAND_GAP: f32 = 10.0;

/// Total dock height for a visualizer state.
pub(crate) fn large_dock_height(visualizer_on: bool) -> f32 {
    let base = DOCK_PAD_TOP + DOCK_ART_SIZE + DOCK_PAD_BOTTOM;
    if visualizer_on {
        let mode = crate::settings_qt::large_spectrum_mode_for_tier(
            crate::settings_qt::large_spectrum_mode(),
            crate::renderer_qt::gpu_tier(),
        );
        let band_height = if mode >= 3 {
            DOCK_ART_SIZE
        } else {
            DOCK_BAND_HEIGHT
        };
        base + band_height + DOCK_BAND_GAP
    } else {
        base
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzShell>> = OnceLock::new();

/// Queue a shell-bridge mutation onto the Qt event loop (no-op before
/// boot registers the thread).
/// The boot value of [`reduce_motion`].
///
/// The reference is `kiosk_profile || !use_gpu_renderer`. Only the FIRST half
/// is wired here, deliberately:
///
/// The renderer-tier half has no honest source in this port. The tier is a
/// QML-side fact (`GraphicsInfo.api`), not a Rust one, and this port runs on
/// the GPU in every real session (OpenGL RHI, measured 2026-07-29). The one
/// place where a tier probe WOULD report software is the offscreen smoke,
/// which forces it by definition — so wiring it would turn reduce-motion on
/// exactly where it means nothing and stay off everywhere it would matter.
/// If a genuine software-tier session ever appears (a remote desktop, a
/// llvmpipe box), that is when the second half earns its probe.
///
/// The kiosk half is now live (2026-08-02 kiosk-port contract §8.3):
/// `kiosk_profile_qt::active()` is latched by `init_at_boot()` before the
/// bridge is constructed, and the live toggle republishes both this and
/// `kiosk_profile` together.
fn reduce_motion_at_boot() -> bool {
    crate::kiosk_profile_qt::active()
}

pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzShell>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

// ---------------------------------------------------------------------------
// The shell repaint pulse (2026-08-13 single-clock redesign)
// ---------------------------------------------------------------------------

/// 24 h wrap for the pulse counter. Consumers only use the notify EDGE (they
/// accumulate their own local tick), so the wrap is invisible; it exists to
/// keep the counter an i32 forever.
const PULSE_WRAP_MS: i32 = 86_400_000;

/// Spawn the one shared repaint clock. Started from `boot()` (the first boot
/// only — a duplicate registration warns and skips), ticks every
/// `settings_qt::shell_pulse_ms()`, and hops to the Qt loop to bump
/// `pulse_ms`. Runs unconditionally: a bump no QML handler reacts to costs an
/// event-loop wakeup and no frame (see the qproperty contract).
fn start_shell_pulse() {
    let period = crate::settings_qt::shell_pulse_ms();
    std::thread::Builder::new()
        .name("qbz-qt-shell-pulse".to_string())
        .spawn(move || {
            let mut tick: i32 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(period as u64));
                tick = (tick + period) % PULSE_WRAP_MS;
                ui(move |mut shell| shell.as_mut().set_pulse_ms(tick));
            }
        })
        .expect("spawn shell pulse thread");
}

/// Rust-side mirror of the `queue_open` property — `toggle_queue` below is
/// the ONLY writer (the NPB button and the queue panel's close both call it;
/// QML never writes the property directly). Read by the hotkeys §1.2 Escape
/// stack arm 7 (2026-08-03 hotkeys-port contract), which runs on the
/// QbzHotkeys singleton and cannot reach this QObject's properties.
static QUEUE_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn queue_open() -> bool {
    QUEUE_OPEN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Rust-side mirrors of the §4.6 multi-select properties, written inside the
/// custom-WRITE setters below — the funnel EVERY write passes through (QML
/// reporter Bindings included), so they are exact (the QUEUE_OPEN
/// precedent). Read by the hotkeys pipeline: the (C2) Ctrl+A consumption
/// predicate and the §1.2 Escape stack arm 6 (2026-08-03 hotkeys-port
/// contract), which run on ANOTHER singleton and cannot reach this QObject's
/// properties.
static MULTI_SELECT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static MULTI_SELECT_CAPABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn multi_select_active() -> bool {
    MULTI_SELECT_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn multi_select_capable() -> bool {
    MULTI_SELECT_CAPABLE.load(std::sync::atomic::Ordering::SeqCst)
}

/// The Windows as-is disclaimer, stored in `ui_prefs.json` as TWO fields
/// (owner's call, 2026-08-27):
///
/// - `windows_disclaimer_hidden` -- the "Don't show this again" box;
/// - `windows_disclaimer_ack_version` -- the version it was ticked on.
///
/// Two rather than one so the boolean means something on its own: a future
/// "show it again" control can flip it without inventing a version to write,
/// and reading the file tells you both what the user chose and when.
///
/// KNOWN AND ACCEPTED: reinstalling the SAME version does not bring the modal
/// back, because `ui_prefs.json` lives in %APPDATA% and outlives the
/// installer. A version CHANGE does bring it back, which is the requirement.
/// If the same-version case ever matters, the MSI is the place to clear these
/// two keys.
/// Should the Windows as-is disclaimer be shown for THIS build?
///
/// Shown unless the user hid it AND hid it on this exact version. A newer
/// build therefore shows it again with nothing to reset by hand.
///
/// Both fields come out of ONE parsed document; the keys and the write live in
/// `settings_qt`, which owns `ui_prefs.json`'s read-modify-write discipline.
///
/// Windows only. On every other platform the modal does not exist, and this
/// answers false without touching the prefs file.
pub(crate) fn windows_disclaimer_pending() -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    let (hidden, acked) = crate::settings_qt::windows_disclaimer_state();
    !hidden || acked != env!("CARGO_PKG_VERSION")
}

impl qbz_shell::QbzShell {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] shell Qt thread already registered");
        } else {
            start_shell_pulse();
        }
        if crate::renderer_qt::take_preflight_fallback_notice() {
            crate::toast_qt::warning(qbz_i18n::t(
                "Selected GPU is unavailable in this display session — using Auto",
            ));
        }
        #[cfg(target_os = "linux")]
        if crate::renderer_qt::auto_preflight::take_software_notice() {
            crate::toast_qt::warning(qbz_i18n::t(
                "Renderer switched automatically — the previous renderer failed to start",
            ));
        }
    }

    /// The renderer probe's one-shot report (see the declaration).
    ///
    /// Writes all four derived properties from the SAME latch, so a consumer
    /// can never see `gpuTier` disagree with `shaderScenesAvailable`. The
    /// reduce-motion write composes both halves through
    /// `renderer_qt::reduce_motion` rather than assigning the tier directly —
    /// the kiosk toggle writes the same property, and a bare assignment from
    /// either side would erase the other's contribution.
    /// Close the Windows disclaimer, optionally for this version.
    ///
    /// `remember == false` clears the property only: the modal is gone for
    /// this run and comes back on the next launch, which is what a plain
    /// "Close" should mean.
    pub fn dismiss_windows_disclaimer(mut self: Pin<&mut Self>, remember: bool) {
        self.as_mut().set_windows_disclaimer_open(false);
        if !remember {
            return;
        }
        // One transaction, and the log follows the RESULT: `save_pref` reports
        // nothing, so an unconditional "acknowledged" line would claim a write
        // that never happened whenever the document was unreadable.
        if crate::settings_qt::save_windows_disclaimer_ack(env!("CARGO_PKG_VERSION")) {
            log::info!(
                "[qbz-qt] Windows disclaimer acknowledged for v{}",
                env!("CARGO_PKG_VERSION")
            );
        } else {
            log::warn!("[qbz-qt] could not persist the Windows disclaimer acknowledgement");
        }
    }

    pub fn report_renderer_api(mut self: Pin<&mut Self>, api: QString) {
        let api = api.to_string();
        if !crate::renderer_qt::set_active_api(&api) {
            return;
        }
        let tier = crate::renderer_qt::gpu_tier();
        self.as_mut()
            .set_renderer_api(QString::from(&crate::renderer_qt::active_api()));
        self.as_mut().set_gpu_tier(tier);
        self.as_mut().set_shader_scenes_available(tier);
        self.as_mut().set_app_background_available(tier);
        // The two scope modes are QSG-native traces. On Software their QML
        // guides still draw, which used to leave an empty instrument in the
        // Large NPB. Publish a LIVE Bars fallback without overwriting the
        // stored scope choice; a later GPU session restores it.
        let live_mode = crate::settings_qt::large_spectrum_mode_for_tier(
            crate::settings_qt::large_spectrum_mode(),
            tier,
        );
        self.as_mut().set_large_spectrum_mode(live_mode);
        crate::viz_qt::set_mode(live_mode);
        self.as_mut()
            .set_large_dock_height(large_dock_height(crate::settings_qt::large_visualizer_on()));
        let kiosk = crate::kiosk_profile_qt::active();
        self.as_mut()
            .set_reduce_motion(crate::renderer_qt::reduce_motion(kiosk));
    }

    /// See the declaration. A window that swapped no frames is NOT liveness,
    /// so it leaves the sentinel armed and the next launch reverts to auto.
    pub fn report_frame_liveness(self: Pin<&mut Self>, frames: i32) {
        if frames <= 0 {
            // Only alarming when something is actually being protected. The
            // offscreen smoke presents no frames BY DESIGN, and warning on
            // every gate run would train the reader to ignore the one time it
            // matters.
            if crate::renderer_qt::sentinel_armed() {
                log::warn!(
                    "[qbz-qt] graphics: the liveness window closed with NO frames rendered — \
                     leaving the startup sentinel armed so the next launch reverts to Auto"
                );
            } else {
                log::debug!("[qbz-qt] graphics: no frames in the liveness window (nothing armed)");
            }
            return;
        }
        crate::renderer_qt::disarm_on_liveness(frames);
    }

    pub fn cycle_sidebar(mut self: Pin<&mut Self>) {
        let next = (self.sidebar_state() + 1) % 3;
        self.as_mut().set_sidebar_state(next);
        // 1:1 Slint (`ShellState.cycle-sidebar` -> persist-sidebar-state): the
        // state survives the relaunch instead of snapping back to open.
        crate::settings_qt::set_sidebar_state(next);
        // state.slint:4094-4096 — the Large dock lives in the open sidebar, so
        // collapsing falls back to New. LIVE only: this is the generated
        // property setter, never npb_set_mode, which would persist the fallback
        // and lose the user's Large preference.
        if next != 0 && *self.npb_mode() == 3 {
            self.as_mut().set_npb_mode(0);
        }
    }

    pub fn toggle_queue(mut self: Pin<&mut Self>) {
        let next = !self.queue_open();
        QUEUE_OPEN.store(next, std::sync::atomic::Ordering::SeqCst);
        self.as_mut().set_queue_open(next);
    }

    /// Snap the sidebar to `state` from a drag: the same persistence and the
    /// same Large-NPB fallback as `cycle_sidebar`, without the cycle.
    fn snap_sidebar_state(mut self: Pin<&mut Self>, state: i32) {
        if *self.sidebar_state() == state {
            return;
        }
        self.as_mut().set_sidebar_state(state);
        crate::settings_qt::set_sidebar_state(state);
        if state != 0 && *self.npb_mode() == 3 {
            self.as_mut().set_npb_mode(0);
        }
    }

    pub fn sidebar_drag(mut self: Pin<&mut Self>, x: f64) {
        use crate::panel_resize::{sidebar_drag_target, SidebarDrag};
        let x = x.round() as i32;
        match sidebar_drag_target(*self.sidebar_state(), x) {
            SidebarDrag::Open(width) => {
                if *self.sidebar_state() != crate::panel_resize::SIDEBAR_OPEN {
                    // A state snap: animate it (flag first, then state).
                    self.as_mut().set_sidebar_snapping(true);
                    self.as_mut().snap_sidebar_state(crate::panel_resize::SIDEBAR_OPEN);
                } else if *self.sidebar_snapping() {
                    // Back to pointer-following: the width tracks the mouse.
                    self.as_mut().set_sidebar_snapping(false);
                }
                if *self.sidebar_width() != width {
                    self.as_mut().set_sidebar_width(width);
                }
            }
            SidebarDrag::Mini => {
                if *self.sidebar_state() != crate::panel_resize::SIDEBAR_MINI {
                    self.as_mut().set_sidebar_snapping(true);
                    self.as_mut().snap_sidebar_state(crate::panel_resize::SIDEBAR_MINI);
                }
            }
            SidebarDrag::Closed => {
                if *self.sidebar_state() != crate::panel_resize::SIDEBAR_CLOSED {
                    self.as_mut().set_sidebar_snapping(true);
                    self.as_mut().snap_sidebar_state(crate::panel_resize::SIDEBAR_CLOSED);
                }
            }
        }
    }

    pub fn sidebar_drag_end(mut self: Pin<&mut Self>) {
        // The width never drops below the minimum (Mini/Closed are STATES),
        // so reopening later restores the last open width.
        crate::settings_qt::set_sidebar_width(*self.sidebar_width());
        if *self.sidebar_snapping() {
            self.as_mut().set_sidebar_snapping(false);
        }
    }

    pub fn queue_panel_drag(mut self: Pin<&mut Self>, width: f64) {
        use crate::panel_resize::{queue_drag_target, ColumnDrag};
        let queue_in_column =
            *self.queue_open() && self.current_view().to_string() != "queue-view";
        match queue_drag_target(width.round() as i32, queue_in_column) {
            ColumnDrag::Open(width) => {
                if *self.queue_panel_width() != width {
                    self.as_mut().set_queue_panel_width(width);
                }
            }
            ColumnDrag::Closed => {
                if *self.queue_open() {
                    self.as_mut().toggle_queue();
                }
                if *self.lyrics_open() {
                    self.as_mut().toggle_lyrics();
                }
            }
            // Owner spec 2026-09-11: pushed past the maximum with the queue in
            // the column, the queue moves to the Listen List view; lyrics (if
            // open) keep the column. The logical queue toggle is untouched, so
            // leaving the view restores the drawer, exactly as the panel's own
            // expand button does (`QbzShell.navigateTo("queue-view")`).
            ColumnDrag::PushThrough => {
                if *self.queue_panel_width() != crate::panel_resize::QUEUE_MAX_WIDTH {
                    self.as_mut().set_queue_panel_width(crate::panel_resize::QUEUE_MAX_WIDTH);
                }
                crate::navigate_to("queue-view");
            }
        }
    }

    pub fn queue_panel_drag_end(self: Pin<&mut Self>) {
        crate::settings_qt::set_queue_panel_width(*self.queue_panel_width());
    }

    pub fn navigate_back(self: Pin<&mut Self>) {
        crate::nav_qt::back();
    }

    pub fn refresh_wallpaper(self: Pin<&mut Self>) {
        crate::wallpaper_qt::refresh();
    }

    pub fn track_window_position(self: Pin<&mut Self>) {
        crate::wallpaper_qt::track_window_position();
    }

    pub fn navigate_forward(self: Pin<&mut Self>) {
        crate::nav_qt::forward();
    }

    pub fn toggle_lyrics(self: Pin<&mut Self>) {
        crate::toggle_lyrics();
    }

    pub fn report_scroll(self: Pin<&mut Self>, scope: QString, y: f32) {
        crate::nav_qt::set_live_scroll(&scope.to_string(), y);
    }

    pub fn report_nav_state(self: Pin<&mut Self>, scope: QString, state: QString) {
        let scope = scope.to_string();
        let state = state.to_string();
        crate::nav_qt::set_live_state(&scope, &state);
        if scope == "local" && !crate::kiosk_profile_qt::active() {
            crate::local_restore_qt::save_browser(&state);
        }
    }

    pub fn navigation_state(&self, view: QString) -> QString {
        QString::from(crate::nav_qt::state_for_view(&view.to_string()))
    }

    pub fn local_navigation_state(&self) -> QString {
        QString::from(crate::local_restore_qt::browser_json())
    }

    pub fn record_local_tab(self: Pin<&mut Self>, tab: QString, state: QString) {
        crate::nav_qt::record_local_tab(&tab.to_string(), &state.to_string());
    }

    pub fn record_kiosk_tab(self: Pin<&mut Self>, view: QString, tab: QString, state: QString) {
        crate::nav_qt::record_kiosk_tab(&view.to_string(), &tab.to_string(), &state.to_string());
    }

    pub fn navigate_to(self: Pin<&mut Self>, view: QString) {
        crate::navigate_to(&view.to_string());
    }

    pub fn navigate_to_tab(self: Pin<&mut Self>, view: QString, tab: QString) {
        crate::navigate_to_tab(&view.to_string(), &tab.to_string());
    }

    pub fn npb_set_mode(self: Pin<&mut Self>, mode: i32) {
        crate::npb_set_mode(mode);
    }

    pub fn large_toggle_visualizer(self: Pin<&mut Self>) {
        crate::large_toggle_visualizer();
    }

    pub fn large_cycle_spectrum(self: Pin<&mut Self>) {
        crate::large_cycle_spectrum();
    }

    pub fn log_open(self: Pin<&mut Self>) {
        crate::log_viewer_qt::open();
    }
    pub fn log_close(self: Pin<&mut Self>) {
        crate::log_viewer_qt::close();
    }
    pub fn log_set_level(self: Pin<&mut Self>, level: QString) {
        crate::log_viewer_qt::set_level(level.to_string());
    }
    pub fn log_set_search(self: Pin<&mut Self>, search: QString) {
        crate::log_viewer_qt::set_search(search.to_string());
    }
    pub fn log_refresh(self: Pin<&mut Self>) {
        crate::log_viewer_qt::publish();
    }
    pub fn log_set_auto_tail(self: Pin<&mut Self>, on: bool) {
        crate::log_viewer_qt::set_auto_tail(on);
    }
    pub fn log_clear(self: Pin<&mut Self>) {
        crate::log_viewer_qt::clear();
    }
    pub fn log_copy_all(self: Pin<&mut Self>) {
        crate::log_viewer_qt::copy_all();
    }
    pub fn log_copy_bundle(self: Pin<&mut Self>) {
        // The bundle now carries the full diagnostics report, which re-runs the
        // settings reads and the `pactl` shell-outs — so it is async, and this
        // takes the `log_upload` shape. The clipboard lands one tick later.
        crate::spawn(async move { crate::log_viewer_qt::copy_bundle().await });
    }
    pub fn diag_set_open(self: Pin<&mut Self>, open: bool) {
        crate::diagnostics_qt::set_open(open);
    }
    pub fn diag_refresh(self: Pin<&mut Self>) {
        crate::diagnostics_qt::refresh();
    }
    pub fn diag_export_clipboard(self: Pin<&mut Self>) {
        crate::diagnostics_qt::export_clipboard();
    }
    pub fn diag_cast_scan(self: Pin<&mut Self>) {
        crate::diagnostics_qt::cast_scan();
    }
    pub fn report_issue_open(self: Pin<&mut Self>) {
        // Same URL the reference opens (crates/qbz/src/main.rs:14619-14624):
        // the repo's bug-report template, no prefilled body.
        if let Err(e) =
            open::that("https://github.com/vicrodh/qbz/issues/new?template=bug_report.yml")
        {
            log::warn!("[qbz-qt] could not open the issue template: {e}");
        }
    }
    pub fn log_upload(self: Pin<&mut Self>) {
        crate::spawn(async move { crate::log_viewer_qt::upload().await });
    }
    pub fn log_copy_url(self: Pin<&mut Self>) {
        crate::log_viewer_qt::copy_url();
    }
    pub fn log_open_file(self: Pin<&mut Self>) {
        crate::log_viewer_qt::open_log_file();
    }

    pub fn apply_mac_chrome(self: Pin<&mut Self>) -> bool {
        crate::macos_chrome::apply_and_center()
    }

    pub fn theme_set(self: Pin<&mut Self>, slug: QString) {
        crate::theme_set(slug.to_string());
    }

    pub fn theme_set_filter(self: Pin<&mut Self>, index: i32) {
        crate::theme_set_filter(index);
    }

    pub fn custom_set_token(self: Pin<&mut Self>, key: QString, hex: QString) {
        crate::custom_theme_qt::set_token(&key.to_string(), &hex.to_string());
    }

    pub fn custom_flush(self: Pin<&mut Self>) {
        crate::custom_theme_qt::flush();
    }

    pub fn custom_toggle_dark(self: Pin<&mut Self>, is_dark: bool) {
        crate::custom_theme_qt::toggle_dark(is_dark);
    }

    pub fn custom_seed_from_current(self: Pin<&mut Self>) {
        crate::custom_theme_qt::seed_from_applied();
    }

    pub fn save_window_geometry(
        self: Pin<&mut Self>,
        width: f32,
        height: f32,
        maximized: bool,
        fullscreen: bool,
    ) {
        crate::settings_qt::save_window_geometry(width, height, maximized, fullscreen);
    }

    pub fn toggle_system_title_bar(self: Pin<&mut Self>) {
        crate::toggle_system_title_bar();
    }

    pub fn toggle_ambient_background(self: Pin<&mut Self>) {
        crate::toggle_ambient_background();
    }

    pub fn reload_sidebar(self: Pin<&mut Self>) {
        // The OFFLINE-SAFE verb — see the declaration's doc comment.
        crate::reload_sidebar_including_local();
    }

    pub fn myqbz_open_card(self: Pin<&mut Self>, id: QString) {
        crate::myqbz_detail_qt::open(id.to_string());
    }

    pub fn myqbz_set_hidden(self: Pin<&mut Self>, id: QString, hidden: bool) {
        crate::myqbz_qt::set_collection_hidden(id.to_string(), hidden);
    }

    pub fn myqbz_create_open(self: Pin<&mut Self>, kind: QString) {
        crate::myqbz_qt::create_open(&kind.to_string());
    }

    pub fn myqbz_refresh_tree(self: Pin<&mut Self>) {
        crate::myqbz_qt::publish_sidebar_tree();
    }

    pub fn sidebar_set_sort(self: Pin<&mut Self>, option: QString) {
        crate::sidebar_set_sort(&option.to_string());
    }

    pub fn sidebar_search(self: Pin<&mut Self>, query: QString) {
        crate::sidebar_set_search(&query.to_string());
    }

    pub fn sidebar_toggle_folder(self: Pin<&mut Self>, id: QString) {
        crate::sidebar_toggle_folder(&id.to_string());
    }

    pub fn sidebar_open_folder_popup(self: Pin<&mut Self>, folder_id: QString) {
        crate::sidebar_open_folder_popup(&folder_id.to_string());
    }

    pub fn sidebar_artwork_window(self: Pin<&mut Self>, urls_json: QString) {
        crate::sidebar_artwork_window(urls_json.to_string());
    }

    pub fn drag_start(
        self: Pin<&mut Self>,
        track_id: QString,
        title: QString,
        subtitle: QString,
        x: f32,
        y: f32,
        inline_visual: bool,
    ) {
        crate::drag_start(
            track_id.to_string(),
            title.to_string(),
            subtitle.to_string(),
            x,
            y,
            inline_visual,
        );
    }

    pub fn drag_start_local(
        self: Pin<&mut Self>,
        row_id: QString,
        title: QString,
        subtitle: QString,
        x: f32,
        y: f32,
        inline_visual: bool,
    ) {
        crate::drag_start_local(
            row_id.to_string(),
            title.to_string(),
            subtitle.to_string(),
            x,
            y,
            inline_visual,
        );
    }

    pub fn drag_move(self: Pin<&mut Self>, x: f32, y: f32) {
        crate::drag_move(x, y);
    }

    pub fn drag_set_over(self: Pin<&mut Self>, playlist_id: QString) {
        crate::drag_set_over(playlist_id.to_string());
    }

    pub fn drag_set_over_queue(self: Pin<&mut Self>, slot: i32) {
        crate::drag_set_over_queue(slot);
    }

    pub fn drag_end(self: Pin<&mut Self>) {
        crate::drag_end();
    }

    /// The custom WRITE targets of the §4.6 seam properties — the funnel
    /// every QML reporter-Binding write passes through (the
    /// `QbzImmersive.open` precedent), keeping the cross-singleton mirrors
    /// exact.
    pub fn set_multi_select_active(mut self: Pin<&mut Self>, value: bool) {
        use cxx_qt::CxxQtType as _;
        if self.multi_select_active == value {
            return;
        }
        self.as_mut().rust_mut().multi_select_active = value;
        self.as_mut().multi_select_active_changed();
        MULTI_SELECT_ACTIVE.store(value, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_multi_select_capable(mut self: Pin<&mut Self>, value: bool) {
        use cxx_qt::CxxQtType as _;
        if self.multi_select_capable == value {
            return;
        }
        self.as_mut().rust_mut().multi_select_capable = value;
        self.as_mut().multi_select_capable_changed();
        MULTI_SELECT_CAPABLE.store(value, std::sync::atomic::Ordering::SeqCst);
    }

    /// §4.6: Ctrl+A select-all — bounce to the AppShell router, which
    /// forwards to the mounted view's duck-typed `selectAll()`.
    pub fn select_all_active(mut self: Pin<&mut Self>) {
        self.as_mut().select_all_requested();
    }

    /// §4.6 / §1.2 Escape stack arm 6: exit the mounted view's multi-select
    /// mode via its duck-typed `exitMultiSelectMode()`.
    pub fn exit_multi_select(mut self: Pin<&mut Self>) {
        self.as_mut().exit_multi_select_requested();
    }
    pub fn open_external_url(self: Pin<&mut Self>, url: QString) {
        let url = url.to_string();
        if let Err(e) = open::that(&url) {
            log::warn!("[qbz-qt] failed to open '{url}': {e}");
        }
    }
}
