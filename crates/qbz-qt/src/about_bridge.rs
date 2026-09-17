//! QbzAbout — About QBZ, release notes and local application updates.
//!
//! Controllers: `about_qt.rs`, `whats_new_qt.rs`, and `updates_qt.rs`.
//! This file is glue and nothing else, exactly like
//! `musician_bridge.rs`.
//!
//! # Why a NEW bridge instead of more members on QbzShell
//!
//! `QbzShell` is already the port's 1200-line catch-all and several lanes edit
//! it at once. This port has 23 bridges and a small domain object is the normal
//! shape, so About/What's New gets its own rather than widening the shared
//! file. External links are the one thing that does NOT move here:
//! `QbzShell.openExternalUrl` (`shell_bridge.rs:440` / `:1202`) already exists
//! and both modals call it — a second opener would be a second thing to keep
//! in sync.
//!
//! # A new bridge is SEVEN artefacts, not one file
//!
//! This file · its `build.rs` `rust_files` entry · the `main.rs` `mod` line ·
//! the QML singleton registration (which IS the build.rs entry) · the two
//! modal mounts in `AppShell.qml` · the two `HeaderBar.qml` rows. A bridge
//! missing from `build.rs` does not fail the build: its QML singleton simply
//! does not exist and every `QbzAbout.foo()` becomes a runtime ReferenceError
//! `cargo check` cannot see. `qml_singleton_xref.py` is the gate for that.
//!
//! # The Qt-thread hop registers ITSELF
//!
//! Every other domain singleton is booted from `Main.qml`'s `boot()` sweep.
//! [`QbzAbout::boot`] exists and does the same job, but the four invokables
//! ALSO register the hop on first use, because both modals are opened by a
//! user action long after startup and nothing here publishes before that. That
//! makes this bridge work whether or not the (integration-owned) `Main.qml`
//! sweep ever gains its line — a bridge whose publishes silently no-op because
//! one QML line is missing is exactly the class of failure `cargo check`
//! cannot see.

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_about_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// The About QBZ document — `open`, the version/platform/build stamp,
        /// the release URL, the codename, the author chip and the row-grouped
        /// contributor chips (avatars as `file://` paths). Produced by
        /// `about_qt.rs`.
        ///
        /// Defaults to `"{}"`, NOT `""`: the QML host `JSON.parse`s this on
        /// its very first frame and an empty string throws into the catch arm.
        #[qproperty(QString, about_json)]
        /// The What's New document — `open`, `loading`, `version`, `date`,
        /// `hasBody`, the TOC and the flat block model. Produced by
        /// `whats_new_qt.rs`. Same `"{}"` default, same reason.
        #[qproperty(QString, whats_new_json)]
        #[qproperty(QString, updates_json)]
        type QbzAbout = super::QbzAboutRust;

        /// Registers this object's Qt-thread hop. Harmless to call twice, and
        /// unnecessary in practice — every invokable below registers it too
        /// (see the module header).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzAbout>);

        #[qinvokable]
        fn updates_launch(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_check(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_close(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_ignore(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_install(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_cancel(self: Pin<&mut QbzAbout>);
        #[qinvokable]
        fn updates_set_launch(self: Pin<&mut QbzAbout>, enabled: bool);

        /// The header menu's "About QBZ" row. Publishes the document open and
        /// kicks the one-shot GitHub avatar fetch.
        #[qinvokable]
        fn about_open(self: Pin<&mut QbzAbout>);

        /// All About dismissal paths (Escape, the X, the backdrop).
        #[qinvokable]
        fn about_close(self: Pin<&mut QbzAbout>);

        /// The header menu's "What's New" row. Publishes the loading state
        /// immediately, then fetches + renders the matching GitHub release off
        /// the UI thread. No cache: every open re-fetches, like the reference.
        #[qinvokable]
        fn whats_new_open(self: Pin<&mut QbzAbout>);

        /// All What's New dismissal paths (Escape, the X, the backdrop, the
        /// footer Close button). Dismissing marks this version's deck as seen.
        #[qinvokable]
        fn whats_new_close(self: Pin<&mut QbzAbout>);

        /// Offer the release deck once per version, from the shell's first
        /// settled frame. No-op when the version has no deck or its deck was
        /// already dismissed, so it is safe to call on every start.
        #[qinvokable]
        fn whats_new_auto_open(self: Pin<&mut QbzAbout>);
    }

    impl cxx_qt::Threading for QbzAbout {}
}

use qbz_about_bridge::QbzAbout;

/// Rust side of the About bridge (plain storage, phase-1 pattern).
pub struct QbzAboutRust {
    about_json: QString,
    whats_new_json: QString,
    updates_json: QString,
}

impl Default for QbzAboutRust {
    fn default() -> Self {
        Self {
            // "{}" and not "": both hosts JSON.parse these straight, and an
            // empty string throws on the very first frame.
            about_json: QString::from("{}"),
            whats_new_json: QString::from("{}"),
            updates_json: QString::from("{}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzAbout>> = OnceLock::new();

/// Queue an about-bridge mutation onto the Qt event loop (no-op before the
/// hop is registered).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzAbout>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_about_bridge::QbzAbout {
    /// Idempotent hop registration. Invokables run ON the Qt thread, so this
    /// is safe from any of them.
    fn register_thread(&self) {
        if QT_THREAD.get().is_none() {
            let _ = QT_THREAD.set(self.qt_thread());
        }
    }

    pub fn boot(self: Pin<&mut Self>) {
        self.register_thread();
    }

    pub fn about_open(self: Pin<&mut Self>) {
        self.register_thread();
        crate::about_qt::open();
    }

    pub fn about_close(self: Pin<&mut Self>) {
        self.register_thread();
        crate::about_qt::close();
    }

    pub fn whats_new_open(self: Pin<&mut Self>) {
        self.register_thread();
        crate::whats_new_qt::open();
    }

    pub fn whats_new_close(self: Pin<&mut Self>) {
        self.register_thread();
        crate::whats_new_qt::close();
    }

    pub fn whats_new_auto_open(self: Pin<&mut Self>) {
        self.register_thread();
        crate::whats_new_qt::auto_open_if_new();
    }
}

impl qbz_about_bridge::QbzAbout {
    pub fn updates_launch(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::launch();
    }
    pub fn updates_check(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::check();
    }
    pub fn updates_close(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::close();
    }
    pub fn updates_ignore(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::ignore();
    }
    pub fn updates_install(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::install();
    }
    pub fn updates_cancel(self: Pin<&mut Self>) {
        self.register_thread();
        crate::updates_qt::cancel();
    }
    pub fn updates_set_launch(self: Pin<&mut Self>, enabled: bool) {
        self.register_thread();
        crate::updates_qt::set_launch(enabled);
    }
}
