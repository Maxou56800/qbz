//! System tray — the platform-agnostic façade.
//!
//! Port of `crates/qbz/src/tray/mod.rs` (391 L) per the 2026-08-03
//! miniplayer/tray contract (rows A-19, A-23, A-24; §5.1, §5.6, §5.8).
//! The ksni half lives in `tray_linux.rs` (A-20); the QObject QML talks to is
//! `tray_bridge.rs` (A-21).
//!
//! Linux uses ksni (`tray_linux.rs`), macOS uses NSStatusItem
//! (`tray_macos.rs`), and Windows uses Shell_NotifyIcon (`tray_windows.rs`).
//!
//! RETIRED 2026-08-05 — the note that used to sit here said macOS "would be a
//! NEW implementation, not a port", because the reference only hand-rolls its
//! menu-bar item to dodge the `muda` that Slint's winit backend bundles, and
//! Qt has no such conflict. The premise was right and the conclusion was
//! backwards: `tray/macos.rs` avoids muda entirely, depends on nothing winit
//! provides, and ported here almost verbatim (it got SHORTER — its
//! `CTX`/`slint::Weak` plumbing has no counterpart on this side). The cost of
//! believing it: the owner's macOS smoke found the tray icon missing and
//! close-to-tray quitting the app, which is the one absence wearing two
//! faces.
//!
//! ## Three things about this module are not style choices
//!
//! 1. **Nothing here touches a window.** Rust cannot reach a `QQuickWindow`
//!    (§2.3), so every window verb emits a `#[qsignal]` on `QbzTray` and QML's
//!    `Connections` in `Main.qml` answers it — the same hop
//!    `hotkeys_bridge.rs:137-138` → `qml/shell/AppShell.qml:104-107` already
//!    uses for `focus_search_requested`.
//! 2. **Nothing here calls `ksni`.** `ksni`'s blocking API wraps every call in
//!    `Runtime::block_on`, which panics inside a tokio runtime **and the panic
//!    is swallowed, so updates appear to succeed and never apply**
//!    (`ksni-0.3.4/src/blocking.rs:206-208`; the reference states it twice,
//!    `tray/mod.rs:141-150` and `tray/linux.rs:320-327`). `init` runs the ksni
//!    setup on a dedicated `qbz-tray-init` thread and every mutation crosses an
//!    `mpsc` into a second `qbz-tray-updater` thread. §15 traps 24-25.
//! 3. **The tray does zero work per tick** (§7-M12). Every update below is
//!    edge-triggered by its publisher; there is no tray poll and no tray timer.
//!
//! On non-Linux targets the whole surface is inert, so the per-item
//! `allow(dead_code)` noise is collapsed into one module attribute rather than
//! sprinkled over a dozen items.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Tooltip model + composition (§5.4)
// ---------------------------------------------------------------------------

/// Now-playing info shown in the tooltip. Cleared when no track is loaded.
/// 1:1 with `crates/qbz/src/tray/linux.rs:157-162`; it lives HERE rather than
/// beside the ksni item so the composition is unit-testable without a D-Bus
/// connection (UT-4, contract §11).
#[derive(Clone, Debug, Default)]
pub(crate) struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// Compose the SNI tooltip's `(title, description)` pair.
///
/// Verbatim port of `QbzTray::tool_tip`'s body
/// (`crates/qbz/src/tray/linux.rs:201-228`), including every omission rule:
///
///  - no track → title `"QBZ"` (hard-coded, NOT translated) and the single
///    description msgid `"Music Player\nNothing playing"` (the `\n` is inside
///    the msgid);
///  - track loaded → title = the track title, falling back to the literal
///    `"QBZ"` when empty, and the description is these lines joined with `\n`:
///      1. `by {artist}` — **only when the artist is non-empty**,
///      2. the album — **only when non-empty**,
///      3. the middle-click hint, keyed on `is_playing`,
///      4. the scroll hint (always).
///
/// All five msgids already exist in all seven translated catalogs (§6.1); this
/// port adds none.
pub(crate) fn compose_tooltip(np: Option<&NowPlaying>, is_playing: bool) -> (String, String) {
    match np {
        Some(np) => {
            let header = if np.title.is_empty() {
                "QBZ".to_string()
            } else {
                np.title.clone()
            };
            let mut lines: Vec<String> = Vec::with_capacity(4);
            if !np.artist.is_empty() {
                lines.push(qbz_i18n::t_args("by {}", &[np.artist.as_str()]));
            }
            if !np.album.is_empty() {
                lines.push(np.album.clone());
            }
            lines.push(if is_playing {
                qbz_i18n::t("Middle-click to pause")
            } else {
                qbz_i18n::t("Middle-click to play")
            });
            lines.push(qbz_i18n::t("Scroll to adjust volume"));
            (header, lines.join("\n"))
        }
        None => (
            "QBZ".to_string(),
            qbz_i18n::t("Music Player\nNothing playing"),
        ),
    }
}

// ---------------------------------------------------------------------------
// The live handle (A-19)
// ---------------------------------------------------------------------------

/// Cross-thread handle to the live tray. Cloneable; mutators forward to the
/// platform backend (ksni on Linux) and are no-ops when the tray is disabled or
/// on a platform without a live-update path.
/// 1:1 with `crates/qbz/src/tray/mod.rs:52-102`, macOS arm included since
/// 2026-08-05.
///
/// The macOS side carries no per-handle state: its `NSStatusItem` lives in a
/// main-thread `thread_local` (it is `!Send`), so the flag only records that a
/// tray EXISTS. That is the load-bearing part — `handle().is_some()` is what
/// `should_hide_on_close` consults, and it is why an absent macOS tray made
/// close-to-tray quit instead of hiding.
#[derive(Clone, Default)]
pub(crate) struct TrayHandle {
    #[cfg(target_os = "linux")]
    linux: Option<crate::tray_linux::LinuxTrayHandle>,
    #[cfg(target_os = "macos")]
    macos: bool,
    // Same shape as the macOS flag and for the same reason: the icon lives in
    // C++ process-global state, so this only records that a tray EXISTS. It is
    // what `handle().is_some()` -- and therefore close-to-tray -- consults.
    #[cfg(target_os = "windows")]
    windows: bool,
}

impl TrayHandle {
    pub(crate) fn set_track(&self, title: String, artist: String, album: String) {
        #[cfg(target_os = "linux")]
        if let Some(h) = &self.linux {
            h.set_track(title, artist, album);
            return;
        }
        #[cfg(target_os = "windows")]
        if self.windows {
            // The tray's hover text is the Windows analogue of the Linux
            // menu header: title on the first line, artist/album below.
            //
            // Through the GUI hop, exactly as the macOS theme arm below. This
            // publisher runs on whatever thread playback pushed from -- the
            // 1 Hz poll lives in `crate::spawn` -- while the Windows tray keeps
            // process-global C++ state and talks to a window owned by the GUI
            // thread. Calling straight through was a data race, not merely a
            // documentation mismatch. Linux needs no hop because ksni's handle
            // is a thread-safe channel.
            let desc = match (artist.is_empty(), album.is_empty()) {
                (true, true) => String::new(),
                (true, false) => album.clone(),
                (false, true) => artist.clone(),
                (false, false) => format!("{artist} - {album}"),
            };
            crate::tray_bridge::ui(move |_b| {
                crate::tray_windows::set_tooltip(&title, &desc);
            });
            return;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (title, artist, album);
    }

    pub(crate) fn clear_track(&self) {
        #[cfg(target_os = "linux")]
        if let Some(h) = &self.linux {
            h.clear_track();
        }
        #[cfg(target_os = "windows")]
        if self.windows {
            crate::tray_bridge::ui(|_b| crate::tray_windows::set_tooltip("QBZ", ""));
        }
    }

    pub(crate) fn set_playing(&self, is_playing: bool) {
        #[cfg(target_os = "linux")]
        if let Some(h) = &self.linux {
            h.set_playing(is_playing);
            return;
        }
        #[cfg(target_os = "windows")]
        if self.windows {
            crate::tray_bridge::ui(move |_b| crate::tray_windows::set_playing(is_playing));
            return;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = is_playing;
    }

    pub(crate) fn set_icon_theme(&self, theme: String) {
        #[cfg(target_os = "linux")]
        if let Some(h) = &self.linux {
            h.set_icon_theme(theme);
            return;
        }
        #[cfg(target_os = "macos")]
        if self.macos {
            // Main-thread only — the status item is a thread_local there.
            crate::tray_bridge::ui(move |_b| crate::tray_macos::set_icon_theme(&theme));
            return;
        }
        // Windows has no arm on purpose: the notification area draws the
        // executable's own RT_GROUP_ICON and Windows owns light/dark itself,
        // so there is nothing for a theme choice to change. The settings row
        // stays hidden there rather than offering a control that does nothing.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = theme;
    }
}

/// Process-global tray handle, set once by `init`. `None` until the tray is
/// created — **or forever, if it was disabled, suppressed, or failed to
/// initialise** (an icon-decode `Err` aborts creation, §5.6). That `None` is
/// exactly what makes the close-to-tray predicate honest: tray off ⇒ close
/// QUITS (§5.7, AC-32).
static TRAY: OnceLock<TrayHandle> = OnceLock::new();

/// The live tray handle, if the tray was initialized. Callers (the now-playing
/// publisher, the poll loop, Settings) use this to push tooltip / theme
/// updates, and every one of them is a silent no-op when it returns `None`.
/// 1:1 with `tray/mod.rs:110-112`.
pub(crate) fn handle() -> Option<&'static TrayHandle> {
    TRAY.get()
}

/// Apply the macOS Dock-icon activation policy: `.accessory` hides the icon
/// (menu-bar-only), `.regular` keeps it. No-op off macOS.
///
/// This is the Spotify behaviour the owner asked for on 2026-08-04: closing
/// puts the app in the menu bar and DROPS the Dock icon when the setting is
/// on, while a plain minimize still goes to the Dock like any app. Only the
/// close path calls this; minimize is untouched by design.
///
/// **Main thread only.** Visibility and preference changes reach this through
/// `refresh_mac_dock_policy` on the Qt GUI/AppKit thread. The marker returns
/// `None` and no-ops anywhere else rather than panicking.
///
/// Port of `crates/qbz/src/tray/macos.rs:320-331`, reached through the
/// reference's own `tray::set_mac_dock_hidden` (`tray/mod.rs:285-291`).
#[cfg(target_os = "macos")]
pub(crate) fn set_mac_dock_hidden(hidden: bool) {
    // `MainThreadMarker` lives in objc2-FOUNDATION in the 0.2/0.5 generation,
    // not at the objc2 root — that is where it moved later. The reference
    // imports it from exactly here (crates/qbz/src/tray/macos.rs:38), and a
    // Mac build caught the wrong path on the first try.
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("[tray] dock policy skipped: not on the AppKit main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(if hidden {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    });
    log::info!(
        "[tray] dock icon {}",
        if hidden { "hidden" } else { "shown" }
    );
}

/// The close-to-tray predicate, ported verbatim from the reference's
/// `tray_settings::get().close_to_tray && tray::handle().is_some()`
/// (`crates/qbz/src/main.rs:23253`, WM arm `:23276`). QML evaluates the same
/// `&&` as `QbzTray.trayLive && QbzTray.closeToTray` (§5.7), so this function
/// has no production caller — it is the Rust statement of the rule and what
/// UT-8 / AC-32 assert on. **Gating on the setting alone would strand the
/// process with no window on a desktop with no SNI host** (§15 trap 22).
#[allow(dead_code)]
pub(crate) fn should_hide_on_close(close_to_tray: bool) -> bool {
    close_to_tray && handle().is_some()
}

// ---------------------------------------------------------------------------
// Init — the THREE gates, in order (§5.6, §15 trap 20)
// ---------------------------------------------------------------------------

/// The synchronous one-shot that closes the race the `OnceLock` cannot: on
/// Linux `TRAY` is set *asynchronously* inside the init thread, so a second
/// shell entry arriving before that thread finishes would pass gate 2 and spawn
/// a DUPLICATE ksni tray. The Qt port has the same re-entry
/// (`QbzSession.recoveryLogin`, `src/session_bridge.rs:72`, and
/// `on_session_entered` is explicitly MULTI-ENTRY).
static INIT_STARTED: AtomicBool = AtomicBool::new(false);

/// The three gates, extracted so their ORDER is unit-testable without spawning
/// a D-Bus service (UT-6, AC-23).
///
/// Returns `true` when `init` may proceed. The order is load-bearing:
///
/// 1. `enabled` — the user setting (and gamescope suppression) — logs and stops;
/// 2. the `OnceLock` — already initialised;
/// 3. the one-shot — **checked LAST on purpose, so a disabled first call does
///    NOT burn it** and a later enabled call can still initialise
///    (`tray/mod.rs:131-135`). Reintroducing the obvious "check the cheap flag
///    first" ordering is the bug §15 trap 20 names.
fn init_gates(enabled: bool, tray_present: bool, started: &AtomicBool) -> bool {
    if !enabled {
        log::info!("[tray] disabled by user setting");
        return false;
    }
    if tray_present {
        return false;
    }
    if started.swap(true, Ordering::SeqCst) {
        return false;
    }
    true
}

/// Initialize the system tray, gated by the user's `enable_tray` setting.
/// `theme_override` is the persisted `tray_icon_theme`
/// ("auto"/"mono-light"/"mono-dark"/"color"). No-op when disabled or already
/// initialized.
///
/// **Signature note (A-19).** The reference takes FIVE parameters
/// (`tray/mod.rs:117-123`); the Qt shape drops three: `runtime` and `handle`
/// come from `crate::app()` / `crate::spawn` (both driven by process-global
/// `OnceLock`s, so they are reachable from the non-tokio ksni thread), and
/// `weak` has no analogue at all — the hop to the window is a `#[qsignal]`
/// (§5.8).
///
/// **There is NO teardown and logout does not touch the tray** — `TRAY` is a
/// never-cleared `OnceLock`, `INIT_STARTED` a one-shot, and `ksni`'s
/// `shutdown()` is never called. That is the reference's behaviour, reproduced
/// 1:1 (§12-P12): after logout the icon stays in the panel and its menu still
/// drives playback. **Do not add a teardown.**
pub(crate) fn init(theme_override: String, enabled: bool) {
    if !init_gates(enabled, TRAY.get().is_some(), &INIT_STARTED) {
        return;
    }

    // Re-seed the bridge's `close_to_tray` from the per-user store now that a
    // session is entered.
    //
    // A-21b(a) seeds it at CONSTRUCTION, but a QML singleton is constructed at
    // `Main.qml`'s `Component.onCompleted` — i.e. at the LOGIN SCREEN, where
    // `sidebar_qt::user_dir()` is not bound yet, so the store answers `Err` and
    // the seed is structurally the `unwrap_or(true)` default rather than the
    // user's value. Reading it here (on the caller's thread, NOT on the ksni
    // thread) is what makes the seed the user's own setting before the first
    // close can happen. The Settings write arm keeps it live afterwards
    // (A-21b(c)).
    push_close_to_tray_seed();

    #[cfg(target_os = "linux")]
    {
        // ksni's blocking `spawn()` calls `Runtime::block_on` internally, which
        // panics inside an existing tokio runtime — and `init` is called from
        // `on_session_entered`, which runs ON the tokio runtime. Run the ksni
        // setup on a dedicated std::thread, outside any tokio context. The ksni
        // service + updater thread persist independently; this thread exits.
        std::thread::Builder::new()
            .name("qbz-tray-init".into())
            .spawn(move || match crate::tray_linux::init(&theme_override) {
                Ok(linux_handle) => {
                    let _ = TRAY.set(TrayHandle {
                        linux: Some(linux_handle),
                    });
                    // A-21b(b): the `tray_live` push lives in the SAME two arms
                    // the reference sets `TRAY` in (`tray/mod.rs:151-158`).
                    // Without it the property keeps cxx-qt's derived `false`
                    // and `trayLive && closeToTray` is permanently false —
                    // close-to-tray never fires and the failure looks exactly
                    // like "not implemented" (§15 trap 31).
                    push_tray_live(true);
                }
                Err(e) => {
                    log::error!("[tray] Linux tray init failed: {e}");
                    // An icon-decode Err aborts tray creation entirely and
                    // `TRAY` stays None, so close-to-tray must quit instead of
                    // hiding. AC-32 is unprovable without this arm.
                    push_tray_live(false);
                }
            })
            .expect("spawn tray init thread");
    }

    // macOS: the NSStatusItem must be built on the AppKit main thread, and
    // `init` runs on the tokio runtime — so it is queued through the tray
    // bridge, whose Qt GUI thread IS the AppKit main thread. (The Linux arm
    // above needs the opposite: a thread OUTSIDE tokio, because ksni blocks.)
    #[cfg(target_os = "macos")]
    {
        crate::tray_bridge::ui(move |_b| {
            let ok = crate::tray_macos::create(&theme_override);
            // Same two arms as Linux: `TRAY` and `tray_live` are set together,
            // and a failure must publish `false` or close-to-tray hides the
            // window into a tray that is not there.
            if ok {
                let _ = TRAY.set(TrayHandle { macos: true });
                push_tray_live(true);
            } else {
                log::error!("[tray] macOS menu-bar item could not be created");
                push_tray_live(false);
            }
        });
    }

    // Windows: Shell_NotifyIconW owns a hidden window with a message loop,
    // so the icon must be created on the thread that pumps it -- the Qt GUI
    // thread.
    //
    // The hop ALONE is not enough, and the first version of this shipped with
    // exactly that bug: `tray_bridge::ui` silently discards work until
    // `QbzTray::boot` registers the Qt thread, and this runs from
    // `on_session_entered`, which reaches it first. The symptom was a tray that
    // never appeared and a log with no `[tray]` line at all -- not even the
    // "disabled by user setting" the gates print when they refuse, which is
    // what pinned it down.
    //
    // So: record the intent, then try. Whichever of the two arrives SECOND --
    // this call or `boot` -- does the work, and `create_now` is idempotent.
    // No polling, and nothing to time out.
    #[cfg(target_os = "windows")]
    {
        let _ = theme_override;
        TRAY_WANTED.store(true, Ordering::SeqCst);
        crate::tray_bridge::ui(|_b| create_now());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = theme_override;
        log::info!("[tray] no tray backend on this platform");
    }
}

/// Set once `init` has decided a tray SHOULD exist. Read by
/// `QbzTray::boot`, which creates it if `init` ran before the Qt thread was
/// registered and its queued closure went nowhere.
#[cfg(target_os = "windows")]
static TRAY_WANTED: AtomicBool = AtomicBool::new(false);

/// Create the Windows tray. GUI THREAD ONLY, and idempotent: both `init`'s
/// hop and `QbzTray::boot` call it, and whichever runs first wins.
#[cfg(target_os = "windows")]
pub(crate) fn create_now() {
    if !TRAY_WANTED.load(Ordering::SeqCst) || TRAY.get().is_some() {
        return;
    }
    let ok = crate::tray_windows::create();
    // Both arms publish, exactly as Linux and macOS do: a failure MUST report
    // false, or close-to-tray hides the window into a tray that is not there.
    if ok {
        let _ = TRAY.set(TrayHandle { windows: true });
        push_tray_live(true);
        log::info!("[tray] Windows notification-area icon created");
    } else {
        log::error!("[tray] Windows notification-area icon could not be created");
        push_tray_live(false);
    }
}

/// True when running under gamescope (Steam Deck Game Mode). Stateless, and a
/// verbatim port of `crates/qbz/src/main.rs:8333-8336` — the raw
/// `GAMESCOPE_WAYLAND_DISPLAY` env is inherited by a re-spawn, and
/// `QBZ_GAMESCOPE` is the marker `apply_gamescope_quirks` sets at boot.
///
/// ONE predicate serves both halves of the contract: the tray's `enabled`
/// argument (`crates/qbz/src/main.rs:262`,
/// `tray.enable_tray && !in_gamescope()`) and the miniplayer's two entry points
/// (§4.10.1). An extra mapped window or tray surface can steal gamescope's
/// focused-window pick.
pub(crate) fn in_gamescope() -> bool {
    std::env::var_os("QBZ_GAMESCOPE").is_some()
        || std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some()
}

/// Push `QbzTray.tray_live`. Separate from the closure so both init arms read
/// identically and neither can be edited without the other being visible.
fn push_tray_live(live: bool) {
    crate::tray_bridge::ui(move |mut t| {
        t.as_mut().set_tray_live(live);
    });
}

/// Push `QbzTray.close_to_tray` from the per-user store, applying the SAME
/// `.unwrap_or(true)` the Settings document uses after the S1 fix — the shared
/// default is `close_to_tray: true` (`qbz-app/src/settings/tray.rs:61`), and a
/// second copy of the old `unwrap_or(false)` bug here would make the user's
/// very first close quit instead of hide (AC-29).
fn push_close_to_tray_seed() {
    let close_to_tray = crate::settings_qt::tray()
        .get_settings()
        .map(|t| t.close_to_tray)
        .unwrap_or(true);
    crate::tray_bridge::ui(move |mut t| {
        t.as_mut().set_close_to_tray(close_to_tray);
    });
}

// ---------------------------------------------------------------------------
// Window verbs (A-23, §5.8)
//
// Rust owns no window. Each verb emits the matching `QbzTray` signal and
// `Main.qml`'s `Connections` calls the window function (A-25/A-26, B6).
// ---------------------------------------------------------------------------

/// Whether the main window is currently shown.
///
/// **D18 — there is ONE visibility owner.** Every path that changes the main
/// window's visibility goes through `hideToTray()` / `showFromTray()`, and both
/// report back through `QbzTray.setWindowShown(bool)` → `set_window_shown`
/// below. The reference has a real desync here: the miniplayer's `enter()` /
/// `exit()` bypass `tray::hide_window` / `show_window`, so this flag goes
/// stale and two tray clicks can raise the MAIN window beside a live mini
/// (§5.8.1). The Qt port defines the invariant instead of inheriting the hole.
static WINDOW_SHOWN: AtomicBool = AtomicBool::new(true);

/// Last tray-toggle timestamp, for the double-click debounce (`toggle_window`).
static LAST_TOGGLE: Mutex<Option<Instant>> = Mutex::new(None);

/// Ignore a 2nd tray activation within this window. A tray double-click
/// otherwise fires two `Activate` in milliseconds; in the reference each toggle
/// destroys/recreates the winit surface AND the wgpu shader underlay and a
/// render in flight then use-after-frees a `TextureView` (wgpu-core panic).
/// **Qt's renderer differs, so the crash may not reproduce — but "one toggle
/// per double-click" is the user-visible semantics and it is part of 1:1**
/// (§5.8, AC-26).
const TOGGLE_DEBOUNCE_MS: u128 = 400;

/// The debounce decision, extracted so it is unit-testable without sleeping
/// (UT-7). `last` is read-modify-written: **the timestamp is stored only when
/// the click PASSES**, so a rejected click does not extend the window — a burst
/// lets one through every 400 ms rather than never — and **the first click
/// always passes** (`last` starts `None`).
fn debounce_pass(last: &mut Option<Instant>, now: Instant) -> bool {
    if let Some(prev) = *last {
        if now.duration_since(prev).as_millis() < TOGGLE_DEBOUNCE_MS {
            return false;
        }
    }
    *last = Some(now);
    true
}

/// Toggle the main window: hide if shown, else show + focus — **unless the
/// miniplayer is open, in which case this degenerates to `present()` and
/// raises the MINI** (D21, §5.8.2).
///
/// The mini-awareness is not a nicety: with D18's honest `WINDOW_SHOWN`, a
/// mini-blind toggle would show the main window beside a live mini on the FIRST
/// click — the "second instance" symptom of #559/#618, reached one click sooner
/// than the reference's own bug. While the mini is up, the mini IS the app's
/// window; exiting it is not the tray's job (the capsule's expand button,
/// `Shift+M` and `Escape` all do that).
pub(crate) fn toggle_window() {
    {
        // Poison-tolerant on purpose (`tray/mod.rs:196`): a panic elsewhere
        // while holding this lock must not wedge the toggle permanently. The
        // guard is dropped by this bare block before anything else runs.
        let now = Instant::now();
        let mut last = LAST_TOGGLE.lock().unwrap_or_else(|p| p.into_inner());
        // `&mut *last`: `last` is the guard; the state is the `Option<Instant>`
        // behind it.
        if !debounce_pass(&mut *last, now) {
            return;
        }
    }
    if crate::mini_bridge::is_open() {
        present();
        return;
    }
    if WINDOW_SHOWN.load(Ordering::Relaxed) {
        hide_window();
    } else {
        show_window();
    }
}

/// Show the main window and focus it → `Main.qml`'s `showFromTray()`.
pub(crate) fn show_window() {
    // The flag moves HERE, synchronously, exactly as the reference does
    // (`tray/mod.rs:212-213` sets it before the queued window call). QML's
    // `setWindowShown` is the RECONCILER, not the only writer: the hop through
    // `ui()` is queued, so a second tray click that arrives after the 400 ms
    // debounce but before the Qt loop drains would otherwise read the stale
    // value and queue a SECOND hide — the window hides once and the user's
    // second click does nothing, one click out of phase. D18 requires one
    // visibility OWNER, not one writer of the flag.
    WINDOW_SHOWN.store(true, Ordering::Relaxed);
    crate::tray_bridge::ui(|mut t| {
        t.as_mut().window_show_requested();
    });
}

/// Hide the main window to the tray → `Main.qml`'s `hideToTray()`.
///
/// The reference clears the dynamic-background shader texture BEFORE the
/// surface dies (`tray/mod.rs:258-265`). That step is Slint/wgpu-specific — the
/// texture wraps a wgpu texture from that surface's instance — and has no Qt
/// analogue (§12-P13). The SHAPE still transfers (drop GPU resources tied to a
/// surface before destroying it); nothing in the Qt path holds one.
pub(crate) fn hide_window() {
    // See `show_window` — the optimistic store is the reference's shape
    // (`tray/mod.rs:255`) and is what keeps rapid clicks in phase.
    WINDOW_SHOWN.store(false, Ordering::Relaxed);
    crate::tray_bridge::ui(|mut t| {
        t.as_mut().window_hide_requested();
    });
}

/// Raise whichever window is CURRENT: the miniplayer when it is open, else the
/// main window. `show_window` would force the MAIN window up next to a live
/// mini — visually a "second instance", #559/#618 (`tray/mod.rs:236-241`).
///
/// Its caller in this diff is `toggle_window` (§2.4). Owner ruling K3 keeps
/// MPRIS out of scope, and this verb is deliberately left in the shape MPRIS
/// `Raise` and the single-instance `Present()` will call **unchanged** when
/// they land — one decision serving all three tray entry points.
///
/// Unlike the reference this needs no event-loop hop: `mini_bridge::is_open()`
/// reads an `AtomicBool` mirror (`src/mini_bridge.rs:140-144`), where Slint's
/// `miniplayer::is_open` reads `thread_local` state.
pub(crate) fn present() {
    if crate::mini_bridge::is_open() {
        crate::tray_bridge::ui(|mut t| {
            t.as_mut().mini_present_requested();
        });
    } else {
        show_window();
    }
}

/// Sync the shown-state flag without touching any window — the QML side reports
/// the REAL visibility back through `QbzTray.setWindowShown(bool)`, which is
/// what keeps the toggle honest (D18). 1:1 with `tray/mod.rs:281-283`.
pub(crate) fn set_window_shown(shown: bool) {
    WINDOW_SHOWN.store(shown, Ordering::Relaxed);
}

pub(crate) fn refresh_mac_dock_policy(tray_live: bool) {
    #[cfg(target_os = "macos")]
    {
        let hide_dock = crate::settings_qt::tray()
            .get()
            .map(|settings| settings.mac_hide_dock)
            .unwrap_or(false);
        set_mac_dock_hidden(mac_dock_should_hide(
            WINDOW_SHOWN.load(Ordering::Relaxed),
            crate::mini_bridge::is_open(),
            tray_live,
            hide_dock,
        ));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = tray_live;
}

#[cfg(any(target_os = "macos", test))]
fn mac_dock_should_hide(main_shown: bool, mini_open: bool, tray_live: bool, opt_in: bool) -> bool {
    opt_in && tray_live && !main_shown && !mini_open
}

#[test]
fn mac_dock_remains_a_recovery_path_unless_menu_bar_only_is_selected() {
    assert!(!mac_dock_should_hide(false, false, true, false));
    assert!(mac_dock_should_hide(false, false, true, true));
    assert!(!mac_dock_should_hide(false, false, false, true));
    assert!(!mac_dock_should_hide(true, false, true, true));
    assert!(!mac_dock_should_hide(false, true, true, true));
}

/// Quit the whole app from a tray action (any thread) → `Main.qml`'s
/// `persistWindowGeometryOnExit(); Qt.exit(0)`.
///
/// `Qt.exit(0)`, NEVER `Qt.quit()` — the difference IS the 2026-08-04 bug the
/// owner reported three times ("Quit QBZ closes the window, not the app"):
/// `Qt.quit()` delivers `QEvent::Quit`, whose `QGuiApplication` handler first
/// tries to close every top-level window and silently CANCELS the quit when
/// any window refuses its close event — which Main.qml's close-to-tray arm
/// does whenever `trayLive && closeToTray`, and MiniWindow.qml does
/// unconditionally. So with close-to-tray on, "Quit" degenerated into
/// hide-to-tray (window gone, process alive — reproduced under
/// `QT_QPA_PLATFORM=vnc`, see `arm_hard_exit_watchdog`'s header). `Qt.exit`
/// stops the event loops directly; no window gets a vote.
///
/// The watchdog is armed HERE, before the hop, because this is the moment of
/// user intent: if the queued signal is dropped (bridge unbooted, loop
/// wedged) or anything downstream hangs, the process still dies within 5s.
///
/// The reference flushes the session from each quit HANDLER
/// (`session_persist::save_on_exit()`, `tray/mod.rs:300` and three more in
/// `main.rs`); this port flushes ONCE after the event loop instead
/// (`main.rs:2913`), behind the same watchdog armed here — window close, tray
/// Quit and the hotkey all converge there, so tray quit persists the session
/// without this path owing anything beyond the geometry flush (§5.7).
pub(crate) fn quit() {
    log::info!("[tray] quit requested");
    // QML confirms first; only the accepted exit arms the watchdog.
    crate::tray_bridge::ui(|mut t| {
        t.as_mut().quit_requested();
    });
}

// ---------------------------------------------------------------------------
// Player-action dispatch (A-24, §5.5)
//
// In Slint the remote-first ladder is open-coded in each dispatcher
// (`tray/mod.rs:311-372`). In Qt it lives INSIDE the verb, so these are thin.
//
// §13-D19, and it is a divergence IN THE PORT'S FAVOUR: the Qt verbs ride the
// now-playing bar's FULL `cast -> QConnect -> local` ladder
// (`src/playback_qt.rs:1384-1409` tries `cast_qt::service().toggle_play_if_cast()`
// FIRST), where the Slint tray's dispatchers are cast-blind. While a Cast
// session is active the Qt tray drives the cast device. **Do NOT "fix" this
// back to cast-blind.**
// ---------------------------------------------------------------------------

pub(crate) fn dispatch_play_pause() {
    crate::transport_toggle_play();
}

pub(crate) fn dispatch_next() {
    crate::transport_next();
}

pub(crate) fn dispatch_previous() {
    crate::transport_previous();
}

/// Step the local volume by `ticks` notches of 5 % (positive = up).
///
/// **Local-only, REPRODUCE-1:1** (§12-P11): the reference's own header says
/// *"Local-only for now (remote-renderer volume forwarding is a later
/// refinement)"* (`tray/mod.rs:374-378`). Linux-only, because scroll-to-volume
/// is a StatusNotifierItem feature the macOS/Windows trays do not expose.
///
/// The 5 % step lives HERE, not in the ksni `scroll` handler — same split as the
/// reference. The current level is read through
/// `runtime.core().player().get_playback_event().volume` (an `f32` in `0..1`;
/// worked precedent `src/playback_qt.rs:1671`), and the read happens on the
/// tokio runtime rather than on the ksni thread, exactly as the reference does
/// it inside `handle.spawn`.
#[cfg(target_os = "linux")]
pub(crate) fn dispatch_volume_delta(ticks: i32) {
    let runtime = crate::app();
    crate::spawn(async move {
        let current = runtime.core().player().get_playback_event().volume;
        let next = (current + ticks as f32 * 0.05).clamp(0.0, 1.0);
        // `transport_set_volume` updates the local model first (instant UI),
        // then the engine (`src/main.rs:1667-1672`).
        crate::transport_set_volume(next);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UT-4 — the tooltip's four description lines, in order, with the
    /// empty-artist and empty-album omissions, the `is_playing` branch and the
    /// `"QBZ"` title fallback (AC-19).
    ///
    /// The msgids resolve to themselves under the test binary's untranslated
    /// locale, which is what makes the assertions readable AND is the `en`
    /// behaviour (§6.1: the 42 msgids resolve 7/7 in the translated catalogs
    /// and 0/7 in `en`).
    #[test]
    fn tray_tooltip_composition() {
        // No track: hard-coded title, one description msgid.
        let (title, desc) = compose_tooltip(None, false);
        assert_eq!(title, "QBZ");
        assert_eq!(desc, "Music Player\nNothing playing");

        // Full track, playing: title, "by {artist}", album, pause hint, scroll.
        let np = NowPlaying {
            title: "Pull Me Under".to_string(),
            artist: "Dream Theater".to_string(),
            album: "Images and Words".to_string(),
        };
        let (title, desc) = compose_tooltip(Some(&np), true);
        assert_eq!(title, "Pull Me Under");
        let lines: Vec<&str> = desc.split('\n').collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("Dream Theater"), "line 0: {}", lines[0]);
        assert_eq!(lines[1], "Images and Words");
        assert_eq!(lines[2], "Middle-click to pause");
        assert_eq!(lines[3], "Scroll to adjust volume");

        // Paused flips ONLY line 3 of the block.
        let (_, desc) = compose_tooltip(Some(&np), false);
        let lines: Vec<&str> = desc.split('\n').collect();
        assert_eq!(lines[2], "Middle-click to play");

        // Empty artist is OMITTED, not rendered as "by ".
        let np = NowPlaying {
            title: "Untitled".to_string(),
            artist: String::new(),
            album: "Bootleg".to_string(),
        };
        let (title, desc) = compose_tooltip(Some(&np), true);
        assert_eq!(title, "Untitled");
        let lines: Vec<&str> = desc.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "Bootleg");

        // Empty album is OMITTED too, and an empty TITLE falls back to "QBZ".
        let np = NowPlaying {
            title: String::new(),
            artist: "Nobody".to_string(),
            album: String::new(),
        };
        let (title, desc) = compose_tooltip(Some(&np), false);
        assert_eq!(title, "QBZ");
        let lines: Vec<&str> = desc.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Nobody"));
        assert_eq!(lines[1], "Middle-click to play");
        assert_eq!(lines[2], "Scroll to adjust volume");

        // Everything empty: the two hints survive on their own.
        let (title, desc) = compose_tooltip(Some(&NowPlaying::default()), false);
        assert_eq!(title, "QBZ");
        assert_eq!(desc.split('\n').count(), 2);
    }

    /// UT-6 — the gate ORDER (AC-23). The one-shot must survive a disabled
    /// first call, or a user who launches with the tray off can never get one
    /// in that process.
    #[test]
    fn tray_init_gate_order() {
        let started = AtomicBool::new(false);

        // 1. Disabled first call: refused, and it does NOT burn the one-shot.
        assert!(!init_gates(false, false, &started));
        assert!(!started.load(Ordering::SeqCst));

        // 2. A later ENABLED call proceeds — and now the one-shot is burnt.
        assert!(init_gates(true, false, &started));
        assert!(started.load(Ordering::SeqCst));

        // 3. A third call is refused by the one-shot even though `TRAY` is not
        //    set yet — that is the whole point: on Linux `TRAY` is filled
        //    asynchronously, so the OnceLock check alone leaves a race window.
        assert!(!init_gates(true, false, &started));

        // The OnceLock gate refuses on its own once the tray exists.
        let already = AtomicBool::new(false);
        assert!(!init_gates(true, true, &already));
        assert!(!already.load(Ordering::SeqCst));
    }

    /// UT-7 — the 400 ms toggle debounce (AC-26): the first click always
    /// passes, a second 100 ms later is swallowed, and a second 500 ms later
    /// passes.
    #[test]
    fn tray_toggle_debounce() {
        use std::time::Duration;

        let t0 = Instant::now();
        let mut last: Option<Instant> = None;

        // First click always passes.
        assert!(debounce_pass(&mut last, t0));

        // 100 ms later: swallowed — a double-click is ONE toggle.
        assert!(!debounce_pass(&mut last, t0 + Duration::from_millis(100)));

        // A rejected click does NOT extend the window: 401 ms after the click
        // that PASSED is enough, even though a click landed at 100 ms.
        assert!(debounce_pass(&mut last, t0 + Duration::from_millis(401)));

        // 500 ms apart produce two effects.
        let mut last: Option<Instant> = None;
        assert!(debounce_pass(&mut last, t0));
        assert!(debounce_pass(&mut last, t0 + Duration::from_millis(500)));

        // Exactly at the boundary the click passes (`<` is the reject test).
        let mut last: Option<Instant> = None;
        assert!(debounce_pass(&mut last, t0));
        assert!(debounce_pass(&mut last, t0 + Duration::from_millis(400)));
    }

    /// UT-8 — with no tray, close QUITS regardless of the setting (AC-32).
    /// The unit test runs in a process where `init` never ran, so `handle()` is
    /// `None` — which is also the state an icon-decode `Err` leaves behind.
    #[test]
    fn tray_absent_makes_close_quit() {
        assert!(handle().is_none());
        assert!(!should_hide_on_close(true));
        assert!(!should_hide_on_close(false));
    }
}
