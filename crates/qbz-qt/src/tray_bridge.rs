//! QbzTray — the tray's QML seam (contract rows A-21, A-21b, A-22).
//!
//! Same mechanics as `mini_bridge.rs` / `immersive_bridge.rs`:
//! `OnceLock<CxxQtThread>`, a `ui()` that is a silent no-op before `boot()`,
//! and `impl Default` for construction-seeded values.
//!
//! ## Why this bridge exists at all
//!
//! Rust cannot reach a `QQuickWindow` (contract §2.3), so the tray cannot show,
//! hide, raise or close a window by itself. The hop is:
//!
//! ```text
//! ksni thread -> tray_qt::<verb>() -> tray_bridge::ui(..) -> #[qsignal]
//!             -> Connections in Main.qml -> the window function
//! ```
//!
//! That path is well-trodden, not a first: eleven `#[qsignal]`s across nine
//! bridges already use it, the nearest analogue being
//! `hotkeys_bridge.rs:137-138`'s `focus_search_requested`, consumed at
//! `qml/shell/AppShell.qml:104-107` as
//! `Connections { target: QbzHotkeys; function onFocusSearchRequested() … }`.
//! `#[auto_cxx_name]` is what turns `window_show_requested` into QML's
//! `onWindowShowRequested`.
//!
//! ## Both properties need WRITERS — this is the silent-failure row (A-21b)
//!
//! cxx-qt derives `Default`, so a `bool` `#[qproperty]` nobody writes is
//! `false` FOREVER. `Main.qml`'s close choreography is
//! `QbzTray.trayLive && QbzTray.closeToTray`; with either operand unwritten the
//! expression is permanently false, **every close quits, and the failure looks
//! exactly like "close-to-tray was not implemented"** (§15 trap 31). The three
//! writers, none optional:
//!
//!  - **(a) the seed below**, at construction;
//!  - **(b) `tray_qt::init`'s thread pushes `tray_live`** on BOTH its `Ok` and
//!    its `Err` arm — the two arms the reference sets `TRAY` in
//!    (`crates/qbz/src/tray/mod.rs:151-158`) — and it re-pushes `close_to_tray`
//!    from the per-user store once a session is entered (the seed below runs at
//!    the login screen, where that store is not bound yet);
//!  - **(c) the `"tray-close-to-tray"` Settings write arm** pushes
//!    `close_to_tray` beside its persist, so the toggle changes the very next
//!    close with no restart.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;

#[cxx_qt::bridge]
pub mod qbz_tray {
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // Whether a tray icon is actually live in a panel. FALSE when the tray
        // is disabled by the user, suppressed under gamescope, or failed to
        // initialise (an icon-decode Err aborts creation). Half of the
        // close-to-tray predicate: **tray off => close QUITS**, because gating
        // on the setting alone would strand the process with no window and no
        // way back on a desktop with no SNI host (§5.7, §15 trap 22).
        #[qproperty(bool, tray_live)]
        // The user's "Close to tray" setting, mirrored for QML. The shared
        // default is `true` (`qbz-app/src/settings/tray.rs:61`).
        #[qproperty(bool, close_to_tray)]
        type QbzTray = super::QbzTrayRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY domain
        /// singleton). **Without it every `tray_bridge::ui()` is a SILENT
        /// no-op** — the bridge never errors, the signals simply never arrive,
        /// so the tray's clicks would do nothing at all (§15 trap 27).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzTray>);

        /// QML reports the main window's REAL visibility back after every
        /// change. **D18 — there is ONE visibility owner:** `hideToTray()` and
        /// `showFromTray()` are the only functions that call this, and they are
        /// the only places the main window is hidden or shown, including from
        /// the miniplayer's enter/exit. That is what keeps the tray toggle
        /// honest (§5.8.1, AC-24).
        #[qinvokable]
        fn set_window_shown(self: Pin<&mut QbzTray>, shown: bool);

        /// `closeOrHide`'s evidence line (2026-08-04): logs the two predicate
        /// operands and the arm taken, RUST-side, so it lands in qbz.log —
        /// QML's console.log only reaches stderr. One line that settles every
        /// future "toggling Close to tray changes nothing" report: it shows
        /// exactly what `trayLive` and `closeToTray` held at close time.
        #[qinvokable]
        fn close_decision(self: Pin<&mut QbzTray>, hiding: bool);

        /// Arms the hard-exit watchdog from the QML quit arm — the quit paths
        /// that do NOT pass through `tray_qt::quit` (window close / app-menu
        /// Close / kiosk X / mini closeApp, all with close-to-tray off).
        /// Idempotent; see `main.rs::arm_hard_exit_watchdog`.
        #[qinvokable]
        fn arm_quit_watchdog(self: Pin<&mut QbzTray>);

        #[qinvokable]
        fn confirm_quit_enabled(self: &QbzTray) -> bool;
        #[qinvokable]
        fn disable_quit_confirmation(self: Pin<&mut QbzTray>);
        #[qsignal]
        fn close_requested(self: Pin<&mut QbzTray>);

        /// Show + raise + focus the main window -> `Main.qml`'s
        /// `showFromTray()`.
        #[qsignal]
        fn window_show_requested(self: Pin<&mut QbzTray>);
        /// Hide the main window to the tray -> `Main.qml`'s `hideToTray()`.
        #[qsignal]
        fn window_hide_requested(self: Pin<&mut QbzTray>);
        /// Raise the MINIPLAYER window (it is open; the mini IS the app's
        /// window right now). Emitted by `tray_qt::present()`, which the tray's
        /// left-click degenerates to while the mini is up — §5.8.2 / §13-D21.
        #[qsignal]
        fn mini_present_requested(self: Pin<&mut QbzTray>);
        /// Quit the app -> `Main.qml`'s
        /// `persistWindowGeometryOnExit(); Qt.exit(0)`. `Qt.exit`, not
        /// `Qt.quit` — a `Qt.quit()` is vetoable by any window's close
        /// handler and that veto is the 2026-08-04 "Quit closes the window,
        /// not the app" bug (see `tray_qt::quit`).
        #[qsignal]
        fn quit_requested(self: Pin<&mut QbzTray>);
    }

    impl cxx_qt::Threading for QbzTray {}
}

use qbz_tray::QbzTray;

/// Rust side of the tray bridge (plain storage, phase-1 pattern).
pub struct QbzTrayRust {
    tray_live: bool,
    close_to_tray: bool,
}

impl Default for QbzTrayRust {
    fn default() -> Self {
        Self {
            // The tray is initialised at session entry, so at construction
            // (Main.qml's Component.onCompleted, i.e. the login screen) this is
            // normally false; `tray_qt::init` pushes the real value from both
            // arms of its init thread — A-21b(b).
            tray_live: crate::tray_qt::handle().is_some(),
            // The literal shared default (`qbz-app/src/settings/tray.rs:61` —
            // `close_to_tray: true`), matching what the Settings document
            // reports after the S1 fix. Seeding `false` here would be a second
            // copy of that bug and would make the user's very first close QUIT
            // instead of hide (AC-29).
            //
            // **It is a literal on purpose: this must NOT read the store.** A
            // `#[qml_singleton]` is constructed on first access, and
            // `QbzTray.boot()` sits in `Main.qml`'s `Component.onCompleted` —
            // which runs at the LOGIN screen, where `sidebar_qt::user_dir()` is
            // still `None`. `settings_qt::tray()` is a `OnceLock::get_or_init`
            // whose closure is the only place `init_at` is ever called, so a
            // read from here would latch an UNBOUND store for the life of the
            // process: every later `get_settings()` returns "No active session",
            // and Settings' whole SYSTEM TRAY group — which works today —
            // silently stops persisting. `tray_qt::init` pushes the user's real
            // value at session entry, after the store binds and long before any
            // close can happen.
            close_to_tray: true,
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (immersive_bridge.rs / mini_bridge.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzTray>> = OnceLock::new();

unsafe extern "C" {
    fn qbz_install_lifecycle_filter(request: extern "C" fn(bool));
    #[cfg(target_os = "macos")]
    fn qbz_install_macos_reopen_handler(present: extern "C" fn());
}

#[cfg(target_os = "macos")]
extern "C" fn present_from_dock() {
    if crate::HARD_EXIT_ARMED.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    log::info!("[macos] Dock/Finder reopen requested");
    crate::tray_qt::present();
}

extern "C" fn request_lifecycle(quit: bool) {
    ui(move |mut tray| {
        if quit {
            tray.as_mut().quit_requested();
        } else {
            tray.as_mut().close_requested();
        }
    });
}

/// Queue a tray-bridge mutation onto the Qt event loop (no-op before boot
/// registers the thread). **Every tray verb reaches QML through here** — the
/// ksni item runs on its own thread and must never touch a QObject directly.
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzTray>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_tray::QbzTray {
    pub fn boot(self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] tray Qt thread already registered");
        }
        // SAFETY: boot runs on the GUI thread after QGuiApplication exists;
        // callback has static lifetime and queues all QML work.
        unsafe { qbz_install_lifecycle_filter(request_lifecycle) };
        #[cfg(target_os = "macos")]
        unsafe { qbz_install_macos_reopen_handler(present_from_dock) };
        // If `tray_qt::init` ran before this point its queued closure was
        // dropped on the floor -- `ui()` above is a no-op until the line
        // right here. Pick the work up now; we are already on the GUI thread.
        #[cfg(target_os = "windows")]
        crate::tray_qt::create_now();
        #[cfg(target_os = "linux")]
        crate::single_instance_qt::bind_ui();
        #[cfg(target_os = "windows")]
        crate::single_instance_win::bind_ui();
    }

    /// D18's report-back. Flag only — it never touches a window.
    ///
    /// It IS, however, the one place that knows the main window just became
    /// hidden or visible, and it is called from exactly two sites
    /// (`hideToTray` / `showFromTray`), so the macOS Dock-icon policy hangs
    /// here rather than growing a third visibility owner.
    ///
    /// Hide the Dock only by opt-in, with a live menu-bar recovery path and
    /// no miniplayer on screen. Showing always restores it.
    pub fn set_window_shown(self: Pin<&mut Self>, shown: bool) {
        crate::tray_qt::set_window_shown(shown);
        crate::tray_qt::refresh_mac_dock_policy(*self.as_ref().tray_live());
    }

    /// The close-choreography evidence line — see the declaration above. The
    /// two operands are read off THIS object, i.e. the exact values QML's
    /// predicate evaluated, so the line cannot disagree with the decision.
    pub fn close_decision(self: Pin<&mut Self>, hiding: bool) {
        log::info!(
            "[tray] closeOrHide: tray_live={} close_to_tray={} -> {}",
            self.as_ref().tray_live(),
            self.as_ref().close_to_tray(),
            if hiding { "hide to tray" } else { "quit" }
        );
    }

    /// See the declaration above and `main.rs::arm_hard_exit_watchdog`.
    pub fn arm_quit_watchdog(self: Pin<&mut Self>) {
        crate::arm_hard_exit_watchdog("QML quit arm");
    }

    pub fn confirm_quit_enabled(&self) -> bool {
        crate::settings_qt::pref_bool("confirm_quit", true)
    }

    pub fn disable_quit_confirmation(self: Pin<&mut Self>) {
        crate::settings_qt::save_pref("confirm_quit", serde_json::json!(false));
    }
}
