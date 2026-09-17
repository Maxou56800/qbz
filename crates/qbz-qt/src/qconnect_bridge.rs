//! QbzQConnect — Qobuz Connect domain bridge (block B4-Rust of the
//! 2026-08-01 QConnect Qt-port contract, §8). The QML surface of the Slint
//! `NowPlayingState.qconnect-connected` toggle + the `QconnectDevState`
//! global (device picker rows, active renderer, diagnostics modal); the
//! service behind it is the facade/sink in `qconnect_qt.rs` /
//! `qconnect_event_sink_qt.rs` over the shared `qconnect-app` crate.
//!
//! Pattern, verbatim from `src/queue_bridge.rs`: ONE `#[cxx_qt::bridge]`
//! module, a `#[qml_element] #[qml_singleton]` QObject, its own
//! `OnceLock<CxxQtThread>`, a `boot()` invokable that registers the
//! Qt-thread hop and a `pub(crate) fn ui()` that queues mutations onto it.
//! The invokables are one-line forwards into `qconnect_qt.rs`; async work
//! goes through `crate::spawn`.
//!
//! DEVICES_JSON SHAPE (what `QconnectFlyout.qml` / `QconnectDevModal.qml`
//! `JSON.parse()`): a JSON array of objects with EXACTLY these keys —
//!   { renderer_id: int, name: string, is_local: bool, is_active: bool,
//!     icon: "mobile" | "web" | "computer" | "speaker" }
//! One JSON document rather than a QVariantList of QVariantMaps for the
//! reason `cast_bridge.rs`'s own `devices_json` gives (cxx-qt-lib 0.7.3
//! has no `QVariantValue` impl for `QMap`/`QList`). Rebuilt and pushed by
//! the sink on every inbound event (push-driven — QML never polls).
//!
//! The 8 s "Looking for devices…" discovery window and the silent connect
//! timeout stay QML-side timers in the flyout (1:1 with the Slint
//! `qc-connecting` flag + timer) — there is deliberately NO Rust busy
//! flag on this bridge (contract §8 state-mapping table).

use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::CxxQtThread;
use cxx_qt::Threading as _;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qbz_qconnect {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // --- Bar badge ---------------------------------------------------
        // The golden ConnectButton state (`NowPlayingState.qconnect-connected`).
        // Flipped by the manual toggle's success tail, the startup
        // auto-connect success tail and the offline force-disconnect watcher —
        // the facade itself never writes it (1:1 with the reference).
        #[qproperty(bool, qconnect_connected)]
        // --- Device picker -------------------------------------------------
        // JSON array of device rows (see the module header for the exact
        // shape). Rebuilt on every inbound event by the sink.
        #[qproperty(QString, devices_json)]
        // -1 = none.
        #[qproperty(i32, active_renderer_id)]
        // --- Playback-conflict modal --------------------------------------
        // Raised when local audio is already playing and SESSION_STATE names a
        // peer renderer. No remote queue/renderer command is applied until QML
        // resolves one of the four explicit choices.
        #[qproperty(bool, playback_conflict_open)]
        #[qproperty(QString, playback_conflict_renderer_name)]
        // 0=ask every time, 1..=3 mirror the modal's non-cancel choices.
        // Both the flyout and Settings bind this same persisted value.
        #[qproperty(i32, playback_conflict_policy_index)]
        // --- Diagnostics modal (DeveloperSettings > QOBUZ CONNECT) ---------
        // QML-driven open/close (DeveloperSettings sets it true; the modal's
        // own close sets it false).
        #[qproperty(bool, diag_open)]
        // The live status block (session topology / renderer roles / queue).
        #[qproperty(QString, diag_status)]
        // The rolling 150-line event log, newest first.
        #[qproperty(QString, diag_log_text)]
        type QbzQConnect = super::QbzQConnectRust;

        /// Registers this object's Qt-thread hop (Main.qml boots EVERY
        /// domain singleton; only QbzSession.boot also fires crate::on_boot).
        #[qinvokable]
        fn boot(self: Pin<&mut QbzQConnect>);

        /// The bar/flyout connect toggle: connects when off, disconnects when
        /// on. Flips `qconnect_connected` from its success tail and does the
        /// RememberLast write-through (see the impl for the 1:1 semantics).
        #[qinvokable]
        fn connect_toggle(self: Pin<&mut QbzQConnect>);

        /// Picker row: make `renderer_id` the session's active renderer.
        #[qinvokable]
        fn set_active(self: Pin<&mut QbzQConnect>, renderer_id: i32);

        /// Picker row "On this network": hand the renderer this account's
        /// delegated credentials so it joins the session (LAN pairing, the
        /// same step the official apps take). `device_uuid` is the row's
        /// `lan_uuid`.
        #[qinvokable]
        fn pair_lan(self: Pin<&mut QbzQConnect>, device_uuid: QString);

        /// Resolve the playback-conflict modal. Values 1..=4 match the visual
        /// order and are validated again by the Rust facade.
        #[qinvokable]
        fn resolve_playback_conflict(self: Pin<&mut QbzQConnect>, choice: i32);

        /// Diagnostics modal footer: clear the rolling event log.
        #[qinvokable]
        fn diag_clear(self: Pin<&mut QbzQConnect>);

        /// Diagnostics modal visibility (QML owns both directions). NOT named
        /// `set_diag_open`: the qproperty codegen already emits that setter,
        /// so the invokable would redefine it (same class of collision
        /// cast_bridge.rs documents for `connect`/`disconnect`). QML may also
        /// assign the `diagOpen` property directly.
        #[qinvokable]
        fn diag_set_open(self: Pin<&mut QbzQConnect>, open: bool);
    }

    impl cxx_qt::Threading for QbzQConnect {}
}

use qbz_qconnect::QbzQConnect;

/// Rust side of the QConnect bridge (plain storage, phase-1 pattern).
pub struct QbzQConnectRust {
    qconnect_connected: bool,
    devices_json: QString,
    active_renderer_id: i32,
    playback_conflict_open: bool,
    playback_conflict_renderer_name: QString,
    playback_conflict_policy_index: i32,
    diag_open: bool,
    diag_status: QString,
    diag_log_text: QString,
}

impl Default for QbzQConnectRust {
    fn default() -> Self {
        Self {
            qconnect_connected: false,
            // "[]" so the picker's JSON.parse never throws on the first
            // frame (the home_bridge / cast_bridge convention).
            devices_json: QString::from("[]"),
            active_renderer_id: -1,
            playback_conflict_open: false,
            playback_conflict_renderer_name: QString::default(),
            playback_conflict_policy_index: 0,
            diag_open: false,
            diag_status: QString::default(),
            diag_log_text: QString::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// The UI hop (bridges/mod.rs pattern)
// ---------------------------------------------------------------------------

static QT_THREAD: OnceLock<CxxQtThread<QbzQConnect>> = OnceLock::new();

/// Queue a QConnect-bridge mutation onto the Qt event loop (no-op before
/// boot registers the thread).
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzQConnect>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

impl qbz_qconnect::QbzQConnect {
    pub fn boot(mut self: Pin<&mut Self>) {
        if QT_THREAD.set(self.qt_thread()).is_err() {
            log::warn!("[qbz-qt] qconnect Qt thread already registered");
        }
        let policy_index = crate::qconnect_transport_qt::load_playback_conflict_policy().index();
        self.as_mut()
            .set_playback_conflict_policy_index(policy_index);
    }

    pub fn connect_toggle(self: Pin<&mut Self>) {
        let Some(service) = crate::qconnect_qt::service() else {
            return;
        };
        crate::spawn(async move {
            // 1:1 with the Slint `on_qconnect_toggle` (reference
            // main.rs:14043-14087). `record` is the RememberLast write-through
            // value — Some ONLY when the operation succeeded, so a failed
            // manual connect never downgrades a remembered "connected".
            let (connected, record) = if service.is_running().await {
                match service.disconnect_safely().await {
                    Ok(outcome) if outcome.authority_safe => {
                        if !outcome.owner_restored {
                            log::warn!(
                                "[QConnect] disconnected safely, but owner playback was not restored"
                            );
                        }
                        (false, Some(false))
                    }
                    Ok(_) => (true, None),
                    Err(err) => {
                        log::warn!("[QConnect] disconnect failed: {err}");
                        (true, None)
                    }
                }
            } else {
                match service.connect().await {
                    Ok(()) => (true, Some(true)),
                    Err(err) => {
                        log::warn!("[QConnect] connect failed: {err}");
                        // The offline refusal already toasted from INSIDE
                        // connect() (qconnect_qt.rs, the D5 msgid) — don't
                        // stack a second toast with the same text. Every other
                        // failure is silent in the reference: it only lands in
                        // the service's `last_error`. Qt also retains that in
                        // diagnostics, but the immediate toast keeps a failed
                        // click from appearing inert.
                        if err != "Qobuz Connect is unavailable while offline" {
                            crate::toast_qt::error(err);
                        }
                        (false, None)
                    }
                }
            };
            // Record the USER-chosen on/off here — the authoritative intent
            // point (the bar toggle is the only manual path) — so a crash
            // can't lose it and internal teardowns (offline force-disconnect,
            // bootstrap cleanup) never overwrite it. Only while mode ==
            // RememberLast, mirroring the reference. Blocking SQLite, so
            // spawn_blocking exactly like the reference does.
            if let Some(state) = record {
                let _ = tokio::task::spawn_blocking(move || {
                    if crate::qconnect_transport_qt::load_startup_mode()
                        == qconnect_app::QconnectStartupMode::RememberLast
                    {
                        crate::qconnect_transport_qt::save_last_known_state(state);
                    }
                })
                .await;
            }
            // THE MANUAL-TOGGLE TAIL: the facade deliberately does NOT flip
            // the badge itself (1:1 with the Slint reference) — the toggle,
            // the startup auto-connect success tail and the offline
            // force-disconnect watcher each publish their own flip.
            crate::qconnect_qt::publish::connected(connected);
        });
    }

    pub fn set_active(self: Pin<&mut Self>, renderer_id: i32) {
        let Some(service) = crate::qconnect_qt::service() else {
            // Contract §9 D3: hardcoded English plain &str, NO msgid (1:1
            // with the reference's `main.rs:14201`).
            crate::toast_qt::error("Failed to switch renderer");
            return;
        };
        crate::spawn(async move {
            if let Err(err) = service.set_active_renderer(renderer_id).await {
                log::warn!("[QConnect] set_active_renderer({renderer_id}) failed: {err}");
                crate::toast_qt::error("Failed to switch renderer");
            }
        });
    }

    pub fn pair_lan(self: Pin<&mut Self>, device_uuid: QString) {
        let uuid = device_uuid.to_string();
        let Some(service) = crate::qconnect_qt::service() else {
            crate::toast_qt::error("Qobuz Connect is not running");
            return;
        };
        crate::spawn(async move {
            match service.pair_lan(&uuid).await {
                Ok(name) => crate::toast_qt::info(format!("Pairing with {name}…")),
                Err(err) => {
                    log::warn!("[QConnect LAN] pair_lan({uuid}) failed: {err}");
                    crate::toast_qt::error("Could not pair with that device");
                }
            }
        });
    }

    pub fn resolve_playback_conflict(self: Pin<&mut Self>, choice: i32) {
        crate::qconnect_qt::resolve_playback_conflict_choice(choice);
    }

    pub fn diag_clear(self: Pin<&mut Self>) {
        crate::qconnect_qt::dev_clear();
    }

    pub fn diag_set_open(mut self: Pin<&mut Self>, open: bool) {
        self.as_mut().set_diag_open(open);
    }
}
