// TODO(converge: qconnect-glue) — derived from crates/qbz/src/qconnect_service.rs @ 5d50158e
// (the connect/disconnect facade + startup auto-connect, UI stripped);
// do not fix bugs here without fixing the source, and vice versa.
//
//! Daemon QConnect service facade + boot-step-12 entry point.
//!
//! Composes the copied glue (engine / sink / session / report / transport /
//! remote_stream) into a headless connect flow that reproduces the desktop
//! `SlintQconnectService::connect` recipe (build transport -> one shared
//! sync-state Mutex -> sink -> `QconnectApp::new` -> `set_app` -> `connect` ->
//! subscribe transport events BEFORE the spawn -> spawn `run_session_loop` ->
//! `bootstrap_remote_presence`), minus every UI surface. Lifecycle transitions
//! latch into `DaemonShared.qconnect` so `/api/status` stays diagnosable.
//!
//! `start()` mints the daemon's OWN device identity in the daemon-root KV, reads
//! the effective startup mode (cli_override = None — never shadow the KV that
//! T11/T13 write), and, when auto-connect is on, spawns a connect-on-Ready task
//! with the bounded [2s, 5s, 15s, 30s] retry schedule. QConnect reads NOTHING
//! from qbzd.toml — only the daemon-root `qconnect_settings.db`.

pub mod authority;
pub mod delegation;
pub mod engine;
pub mod lan;
pub mod publish;
pub mod remote_stream;
pub mod report;
pub mod session;
pub mod sink;
pub mod transport;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use qbz_app::shell::AppRuntime;
use qbz_models::CoreEvent;
use qconnect_app::{
    compute_effective_startup, lan_callback_is_current, CredentialOrigin, DelegationCoordinator,
    DelegationCoordinatorConfig, DelegationErrorCode, DelegationHost, DelegationPhase,
    DelegationSnapshot, LanRuntimeLifecycle, QconnectApp, QconnectAppEvent, QconnectEnableIntent,
    QconnectEnableToken, QconnectEventSink, QconnectLifecycleState, QconnectRemoteSyncState,
    SessionLoopHost,
};
use qconnect_lan::EndpointPolicy;
use qconnect_transport_ws::{NativeWsTransport, WsTransportConfig};
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use crate::adapter::DaemonAdapter;
use crate::paths::ProfileRoots;
use crate::state::{AuthState, DaemonShared};

use self::authority::{AuthorityCell, AuthorityOrigin, AuthorityStamp};
use self::delegation::{DaemonDelegationCoordinator, DaemonDelegationHost};
use self::engine::DaemonRendererEngine;
use self::lan::DaemonLanRuntime;
use self::session::{bootstrap_remote_presence, DaemonSessionLoopHost};
use self::sink::{DaemonEventSink, DaemonQconnectApp};

type Runtime = Arc<AppRuntime<DaemonAdapter>>;
type SharedState = Arc<std::sync::Mutex<DaemonShared>>;

const QCONNECT_COORDINATOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const QCONNECT_AUTHORITY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// The live QConnect runtime for one connected session (app + its config + the
/// spawned event loop + the shared sync accumulator).
pub(crate) struct DaemonQconnectRuntime {
    pub stamp: AuthorityStamp,
    pub app: Arc<DaemonQconnectApp>,
    /// Re-latched by `bootstrap_after_reconnect` on a credential re-resolve;
    /// consumed by the full-reconnect path (T10) + status/endpoint reporting.
    #[allow(dead_code)]
    pub config: WsTransportConfig,
    pub event_loop: JoinHandle<()>,
    pub sync_state: Arc<AsyncMutex<QconnectRemoteSyncState>>,
}

/// Connect-flow state, mirrored on the desktop `SlintQconnectInner`. `pub(crate)`
/// fields so `session::DaemonSessionLoopHost` can gate lifecycle + re-latch the
/// config + drop the runtime on reconnect-exhausted.
#[derive(Default)]
pub(crate) struct DaemonQconnectInner {
    pub runtime: Option<DaemonQconnectRuntime>,
    /// Latched connect/loop error; surfaced by `qbzd status` QConnect block (T11).
    #[allow(dead_code)]
    pub last_error: Option<String>,
    pub lifecycle_state: QconnectLifecycleState,
    /// Exact enabled intent that owns the pre-runtime `Connecting` claim.
    pub connecting_token: Option<QconnectEnableToken>,
    /// Last local queue ids this session pushed to the cloud (the publish.rs
    /// echo latch). Cleared on every connect (parity with the desktop
    /// `qconnect_service.rs` connect reset).
    pub last_pushed_queue_ids: Option<Vec<u64>>,
}

pub(crate) fn lock_inner(
    inner: &StdMutex<DaemonQconnectInner>,
) -> MutexGuard<'_, DaemonQconnectInner> {
    inner.lock().unwrap_or_else(|poisoned| {
        log::error!("[QConnect] runtime state recovered from a poisoned lock");
        poisoned.into_inner()
    })
}

/// Map a lifecycle state to the `/api/status` `qconnect.state` label + the
/// session-active flag, and latch it into `DaemonShared`.
fn latch_lifecycle_into_shared(shared: &SharedState, state: QconnectLifecycleState) {
    let (label, active) = match state {
        QconnectLifecycleState::Off => ("off", false),
        QconnectLifecycleState::Connecting => ("connecting", false),
        QconnectLifecycleState::Connected => ("connected", true),
        QconnectLifecycleState::Reconnecting => ("retrying", false),
        QconnectLifecycleState::Exhausted => ("exhausted", false),
    };
    if let Ok(mut s) = shared.lock() {
        s.qconnect.state = label.to_string();
        s.qconnect.session_active = active;
        if matches!(state, QconnectLifecycleState::Reconnecting) {
            s.qconnect.last_transport_reconnect = Some(unix_seconds_string());
        }
        // 01 §9.3: a live QConnect session (fresh connect OR post-reconnect
        // recovery) is a real network-reachable outcome — latch it true. The
        // reconnect-EXHAUSTED failure side latches false in
        // `session::DaemonSessionLoopHost::on_reconnect_exhausted` (it does
        // not route through this helper — see there).
        if matches!(state, QconnectLifecycleState::Connected) {
            s.set_network_online(true);
        }
        // Surface the transition on the CoreEvent bus (SSE, `qbzd watch`,
        // the event hook) alongside the /api/status latch.
        s.emit_qconnect_session_changed();
    }
}

fn unix_seconds_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Dedup + gate a lifecycle transition: only emit while a runtime is alive and
/// the state actually changes. Mirrors the desktop
/// `update_lifecycle_state_if_running`, plus the `DaemonShared` latch.
pub(crate) async fn update_lifecycle_state_if_running(
    inner: &Arc<StdMutex<DaemonQconnectInner>>,
    sink: &DaemonEventSink,
    shared: &SharedState,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
    next: QconnectLifecycleState,
) {
    if !authority.is_current(stamp) {
        return;
    }
    {
        let mut guard = lock_inner(inner);
        if guard.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp) {
            return;
        }
        if guard.lifecycle_state == next {
            return;
        }
        guard.lifecycle_state = next;
    }
    if !authority.is_current(stamp) {
        return;
    }
    latch_lifecycle_into_shared(shared, next);
    if !authority.is_current(stamp) {
        return;
    }
    sink.on_event(QconnectAppEvent::LifecycleChanged { state: next })
        .await;
}

/// The headless QConnect connect service.
pub struct DaemonQconnectService {
    inner: Arc<StdMutex<DaemonQconnectInner>>,
    authority: Arc<AuthorityCell>,
    enable_intent: Arc<QconnectEnableIntent>,
    runtime: Runtime,
    shared: SharedState,
    #[allow(dead_code)] // T11 (settings reload) re-reads the KV through this path.
    settings_db: PathBuf,
    custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Shared with the playback driver and settings reload route; the engine
    /// reads it for every remote stream start so QConnect cannot bypass the
    /// daemon's configured maximum quality (#693).
    quality_cap: Arc<std::sync::Mutex<qbz_models::Quality>>,
    delegation_host: Arc<DaemonDelegationHost>,
    coordinator: DaemonDelegationCoordinator,
    lan: AsyncMutex<Option<DaemonLanRuntime>>,
    lan_lifecycle: LanRuntimeLifecycle<DaemonLanRuntime>,
    lifecycle_gate: AsyncMutex<()>,
    teardown_incomplete: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QconnectDisconnectOutcome {
    pub authority_safe: bool,
    pub owner_restored: bool,
    pub lan_withdrawn: bool,
}

impl DaemonQconnectService {
    async fn await_while_enabled<T>(
        &self,
        enable_token: QconnectEnableToken,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        let value = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                return Err("Qobuz Connect was disabled".to_string());
            }
            value = future => value,
        };
        self.enable_intent
            .is_current(enable_token)
            .then_some(value)
            .ok_or_else(|| "Qobuz Connect was disabled".to_string())
    }

    fn set_lan_status(&self, state: &str, port: Option<u16>, error: Option<&str>) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.qconnect.lan_state = state.to_string();
            shared.qconnect.lan_port = port;
            shared.qconnect.last_lan_error = error.map(str::to_string);
            shared.emit_qconnect_session_changed();
        }
    }

    fn set_enabled_status(&self, enabled: bool) {
        if let Ok(mut shared) = self.shared.lock() {
            if shared.qconnect.enabled != enabled {
                shared.qconnect.enabled = enabled;
                shared.emit_qconnect_session_changed();
            }
        }
    }

    async fn owner_app_id(&self) -> Result<String, String> {
        let client = self
            .runtime
            .core()
            .client()
            .read()
            .await
            .clone()
            .ok_or_else(|| "qconnect-lan-owner-client-unavailable".to_string())?;
        client
            .app_id()
            .await
            .map_err(|_| "qconnect-lan-app-id-unavailable".to_string())
    }

    async fn start_lan(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
        qws_endpoint: &str,
    ) -> Result<(), String> {
        // Settings -> Network safety switch, independent of the enable intent
        // and of the delegated authority: checked first so mDNS registration
        // and the local HTTP receiver never bind even transiently while it's
        // on. Daemon counterpart of qbz-qt's qconnect_qt.rs::start_lan gate,
        // but reads the daemon's OWN root KV (`qconnect.block_lan`, settable
        // via `qbzd settings set`) rather than the desktop's global settings.
        if transport::load_block_lan_at(&self.settings_db) {
            return Err("qconnect-lan-blocked-by-network-settings".to_string());
        }
        if !self.lan_lifecycle.teardown_safe() {
            return Err("qconnect-lan-physical-teardown-unsafe".to_string());
        }
        if !self.enable_intent.is_current(enable_token) {
            return Err("qconnect-lan-disabled".to_string());
        }
        if self.lan.lock().await.is_some() {
            return self
                .enable_intent
                .is_current(enable_token)
                .then_some(())
                .ok_or_else(|| "qconnect-lan-disabled".to_string());
        }
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            return Err("qconnect-lan-owner-superseded".to_string());
        }

        if self
            .enable_intent
            .commit_if_current(enable_token, || self.set_lan_status("binding", None, None))
            .is_none()
        {
            return Err("qconnect-lan-disabled".to_string());
        }
        let endpoint_policy =
            EndpointPolicy::from_trusted_endpoints(qbz_qobuz::endpoints::BASE_URL, qws_endpoint)
                .map_err(|_| "qconnect-lan-endpoint-policy-invalid".to_string())?;
        let app_id = self
            .await_while_enabled(enable_token, self.owner_app_id())
            .await??;
        if !self.enable_intent.is_current(enable_token) {
            return Err("qconnect-lan-disabled".to_string());
        }
        let quality = *self
            .quality_cap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current_session_id = self
            .delegation_host
            .current_projected_session_id(stamp)
            .ok_or_else(|| "qconnect-lan-owner-session-unavailable".to_string())?;
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            return Err("qconnect-lan-owner-superseded".to_string());
        }
        let coordinator = self.coordinator.clone();
        let callback_intent = Arc::clone(&self.enable_intent);
        let callback_authority = Arc::clone(&self.authority);
        let runtime_handle = tokio::runtime::Handle::current();
        let callback = Arc::new(move |candidate| {
            if !lan_callback_is_current(
                callback_intent.as_ref(),
                callback_authority.as_ref(),
                stamp,
            ) {
                return;
            }
            let result = runtime_handle.block_on(coordinator.admit(candidate));
            match result {
                Ok(generation) => {
                    log::info!("[QConnect LAN] handoff admitted generation={generation}");
                }
                Err(error) => {
                    log::warn!("[QConnect LAN] handoff admission rejected: {error}");
                }
            }
        });

        let started = self
            .lan_lifecycle
            .start(
                move || {
                    DaemonLanRuntime::start(
                        endpoint_policy,
                        app_id,
                        quality,
                        Some(current_session_id),
                        callback,
                    )
                },
                self.enable_intent.cancelled(enable_token),
            )
            .await
            .map_err(|error| error.to_string())?;

        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            if let Err(error) = self.lan_lifecycle.shutdown(started).await {
                log::warn!("[QConnect LAN] stale listener teardown failed: {error}");
            }
            return Err("qconnect-lan-owner-superseded".to_string());
        }

        let port = started.port();
        let projection = started.projection();
        let mut pending = Some(started);
        let mut lan = self.lan.lock().await;
        let installed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                if !self.authority.is_current(stamp) || lan.is_some() {
                    return false;
                }
                self.delegation_host.attach_projection(projection);
                *lan = pending.take();
                self.set_lan_status("listening", port, None);
                true
            })
            .unwrap_or(false);
        drop(lan);
        if installed {
            return Ok(());
        }

        if let Some(stale) = pending {
            if let Err(error) = self.lan_lifecycle.shutdown(stale).await {
                log::warn!("[QConnect LAN] rejected listener teardown failed: {error}");
            }
        }
        Err("qconnect-lan-disabled-or-owner-superseded".to_string())
    }

    async fn stop_lan(&self) -> Result<(), String> {
        self.set_lan_status("off", None, None);
        self.delegation_host.detach_projection();
        let runtime = self.lan.lock().await.take();
        if let Some(runtime) = runtime {
            self.lan_lifecycle
                .shutdown(runtime)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.lan_lifecycle
            .settle()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn wait_for_owner_session_ready(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
    ) -> Result<(), String> {
        const OWNER_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
        let wait = async {
            loop {
                if !self.enable_intent.is_current(enable_token) {
                    return Err("qconnect disabled before session acceptance".into());
                }
                if !self.authority.is_current(stamp) {
                    return Err("qconnect owner authority changed before session acceptance".into());
                }
                let lifecycle = lock_inner(&self.inner).lifecycle_state;
                match lifecycle {
                    QconnectLifecycleState::Connected => return Ok(()),
                    QconnectLifecycleState::Off | QconnectLifecycleState::Exhausted => {
                        return Err(format!(
                            "qconnect owner session failed before acceptance ({lifecycle:?})"
                        ));
                    }
                    _ => {
                        if !self.enable_intent.is_current(enable_token) {
                            return Err("qconnect disabled before session acceptance".into());
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        };
        tokio::time::timeout(OWNER_SESSION_TIMEOUT, wait)
            .await
            .map_err(|_| "qconnect owner session acceptance timed out".to_string())?
    }

    /// Retire only the initial owner runtime installed by one connect attempt.
    /// A late failed attempt must not invalidate enabled intent or tear down a
    /// newer owner/delegated runtime.
    async fn retire_initial_owner_if_current(
        &self,
        enable_token: QconnectEnableToken,
        stamp: AuthorityStamp,
        error: Option<String>,
    ) {
        let mut error = error;
        let current_retirement = self.enable_intent.commit_if_current(enable_token, || {
            let runtime = {
                let mut inner = lock_inner(&self.inner);
                if inner.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp) {
                    return None;
                }
                self.delegation_host
                    .projection_slot()
                    .clear_if_current(&self.authority, stamp);
                inner.lifecycle_state = QconnectLifecycleState::Off;
                inner.last_pushed_queue_ids = None;
                if let Some(error) = error.take() {
                    inner.last_error = Some(error);
                }
                inner.runtime.take()
            };
            if runtime.is_some() {
                latch_lifecycle_into_shared(&self.shared, QconnectLifecycleState::Off);
            }
            runtime
        });
        let runtime = match current_retirement {
            Some(runtime) => runtime,
            None => {
                // Disabled intent may already be tearing this stamp down. Help
                // retire it exactly, but never publish its stale error/status.
                let mut inner = lock_inner(&self.inner);
                if inner.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp) {
                    return;
                }
                self.delegation_host
                    .projection_slot()
                    .clear_if_current(&self.authority, stamp);
                inner.lifecycle_state = QconnectLifecycleState::Off;
                inner.last_pushed_queue_ids = None;
                inner.runtime.take()
            }
        };

        if let Some(runtime) = runtime {
            runtime.event_loop.abort();
            {
                let mut sync = runtime.sync_state.lock().await;
                sync.watchdog_generation = sync.watchdog_generation.wrapping_add(1);
                sync.session = qconnect_app::QconnectSessionState::default();
                sync.session_renderer_states.clear();
            }
            let _ = runtime.app.disconnect().await;
            let _ = runtime.event_loop.await;
        }
    }

    fn record_connect_error_if_current(&self, enable_token: QconnectEnableToken, message: String) {
        let _ = self.enable_intent.commit_if_current(enable_token, || {
            let recorded = {
                let mut inner = lock_inner(&self.inner);
                if inner.runtime.is_some() {
                    return false;
                }
                inner.lifecycle_state = QconnectLifecycleState::Off;
                inner.connecting_token = None;
                inner.last_error = Some(message);
                true
            };
            if recorded {
                latch_lifecycle_into_shared(&self.shared, QconnectLifecycleState::Off);
            }
            recorded
        });
    }

    async fn disconnect_with_owner_policy(
        &self,
        restore_owner: bool,
    ) -> Result<QconnectDisconnectOutcome, String> {
        // Cancellation must be observable by a connect that currently owns
        // the lifecycle gate. Waiting for that connect before invalidating its
        // token would turn disable into a full bootstrap/session timeout.
        self.enable_intent.disable();
        let _ = self
            .enable_intent
            .commit_if_disabled(|| self.set_enabled_status(false));
        {
            let mut inner = lock_inner(&self.inner);
            inner.connecting_token = None;
            if inner.runtime.is_none() {
                inner.lifecycle_state = QconnectLifecycleState::Off;
            }
        }
        let _lifecycle = self.lifecycle_gate.lock().await;
        if self.enable_intent.current_token().is_some() {
            return Err("qconnect-disable-superseded-by-enable".to_string());
        }
        self.disconnect_with_owner_policy_locked(restore_owner)
            .await
    }

    async fn disconnect_with_owner_policy_locked(
        &self,
        restore_owner: bool,
    ) -> Result<QconnectDisconnectOutcome, String> {
        // The public entry invalidated enabled intent before waiting for this
        // lane. Do not invalidate again here: an enable requested while that
        // wait was pending is newer and must win.
        {
            let mut inner = lock_inner(&self.inner);
            inner.connecting_token = None;
            if inner.runtime.is_none() {
                inner.lifecycle_state = QconnectLifecycleState::Off;
            }
        }
        let lan_withdrawn = if let Err(error) = self.stop_lan().await {
            log::warn!("[QConnect] LAN teardown failed: {error}");
            false
        } else {
            !self.lan_lifecycle.start_pending() && self.lan_lifecycle.teardown_safe()
        };
        // A tracked restore from the previous authority transition owns the
        // same gate coordinator shutdown needs. Observe its bounded result
        // first so the shorter shutdown timeout cannot detach a late owner
        // runtime that starts after enabled intent was cleared.
        let prior_owner_restore = self.delegation_host.await_owner_playback_restore().await;
        self.delegation_host
            .set_shutdown_restore_owner(restore_owner);
        let coordinator_stopped = if tokio::time::timeout(
            QCONNECT_COORDINATOR_SHUTDOWN_TIMEOUT,
            self.coordinator.shutdown(),
        )
        .await
        .is_err()
        {
            log::warn!("[QConnect] qconnect-coordinator-shutdown-timed-out");
            false
        } else {
            true
        };
        // `shutdown()` is intentionally a no-op while the coordinator is still
        // Disabled (for example, bootstrap failed before OwnerReady). The host
        // teardown is idempotent and closes that pre-OwnerReady window too.
        let direct_authority_shutdown_needed = !coordinator_stopped
            || self.authority.current().is_some()
            || lock_inner(&self.inner).runtime.is_some()
            || self.delegation_host.owner_snapshot_pending();
        let authority_stopped = if direct_authority_shutdown_needed {
            if tokio::time::timeout(
                QCONNECT_AUTHORITY_SHUTDOWN_TIMEOUT,
                self.delegation_host.shutdown_authority(),
            )
            .await
            .is_err()
            {
                log::warn!("[QConnect] qconnect-authority-shutdown-timed-out");
                false
            } else {
                true
            }
        } else {
            true
        };

        let shutdown_owner_restore = self.delegation_host.await_owner_playback_restore().await;
        let owner_restored = prior_owner_restore.is_ok()
            && shutdown_owner_restore.is_ok()
            && !self.delegation_host.owner_restore_pending();
        let runtime_removed = lock_inner(&self.inner).runtime.is_none();
        let authority_safe = lan_withdrawn
            && coordinator_stopped
            && authority_stopped
            && runtime_removed
            && self.authority.current().is_none();
        self.teardown_incomplete
            .store(!authority_safe, Ordering::Release);

        if authority_safe {
            if let Ok(mut shared) = self.shared.lock() {
                shared.qconnect.state = "off".to_string();
                shared.qconnect.session_active = false;
                shared.qconnect.credential_origin = "owner".to_string();
                shared.qconnect.candidate_generation = None;
                shared.emit_qconnect_session_changed();
            }
        }
        let outcome = QconnectDisconnectOutcome {
            authority_safe,
            owner_restored,
            lan_withdrawn,
        };
        if authority_safe {
            Ok(outcome)
        } else {
            Err("qconnect-authority-teardown-incomplete".to_string())
        }
    }

    /// Establish the QConnect session. Gated on an initialized API client (the
    /// qws/createToken discovery needs it). Idempotent while a runtime is live.
    pub async fn connect(&self) -> Result<(), String> {
        let enable_token = self.enable_intent.enable();
        let _ = self
            .enable_intent
            .commit_if_current(enable_token, || self.set_enabled_status(true));
        self.connect_with_token(enable_token).await
    }

    async fn connect_with_token(&self, enable_token: QconnectEnableToken) -> Result<(), String> {
        let _lifecycle = self
            .await_while_enabled(enable_token, self.lifecycle_gate.lock())
            .await?;
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }
        if self.teardown_incomplete.load(Ordering::Acquire) {
            return Err("qconnect-authority-teardown-incomplete".to_string());
        }
        let prior_restore = self
            .await_while_enabled(
                enable_token,
                self.delegation_host.await_owner_playback_restore(),
            )
            .await?;
        if let Err(error) = prior_restore {
            let message = format!("qconnect owner rollback incomplete: {error}");
            self.record_connect_error_if_current(enable_token, message.clone());
            return Err(message);
        }
        let api_initialized = self
            .await_while_enabled(enable_token, self.runtime.core().is_api_initialized())
            .await?;
        if !api_initialized {
            return Err("Qobuz API is not initialized; cannot start Qobuz Connect".to_string());
        }

        let claimed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                let mut guard = lock_inner(&self.inner);
                if guard.runtime.is_some() {
                    return Ok(false);
                }
                if guard.connecting_token == Some(enable_token) {
                    return Err("QConnect connect is already in progress".to_string());
                }
                guard.connecting_token = Some(enable_token);
                guard.lifecycle_state = QconnectLifecycleState::Connecting;
                guard.last_error = None;
                Ok(true)
            })
            .ok_or_else(|| "Qobuz Connect was disabled".to_string())??;
        if !claimed {
            return Ok(());
        }
        let _ = self.enable_intent.commit_if_current(enable_token, || {
            latch_lifecycle_into_shared(&self.shared, QconnectLifecycleState::Connecting)
        });

        let config = match self
            .await_while_enabled(
                enable_token,
                transport::resolve_transport_config(&self.runtime),
            )
            .await
        {
            Ok(Ok(config)) => config,
            Ok(Err(err)) => {
                self.record_connect_error_if_current(enable_token, err.clone());
                return Err(err);
            }
            Err(err) => return Err(err),
        };
        if !self.enable_intent.is_current(enable_token) {
            return Err("Qobuz Connect was disabled".to_string());
        }

        let qws_endpoint = config.endpoint_url.clone();
        let stamp = self.authority.reserve(AuthorityOrigin::Owner);
        let projection = self.delegation_host.projection_slot();
        let transport = Arc::new(NativeWsTransport::new());
        let sync_state = Arc::new(AsyncMutex::new(QconnectRemoteSyncState::default()));
        // T10 (OD4, §7.4): resolve the volume policy from the daemon-root KV at
        // connect, so a later `qbzd settings reload` (T11) is picked up on the
        // next connect. Unset/unknown -> Software (the OD4 default).
        let volume_mode = engine::VolumeMode::from_kv(
            transport::load_volume_mode_at(&self.settings_db).as_deref(),
        );
        let engine = DaemonRendererEngine::new(
            Arc::clone(&self.runtime),
            volume_mode,
            Arc::clone(&self.quality_cap),
            Arc::clone(&self.authority),
            stamp,
        );
        let sink = Arc::new(DaemonEventSink::new(
            engine,
            Arc::clone(&sync_state),
            Arc::clone(&self.authority),
            stamp,
            projection.clone(),
        ));
        let app = Arc::new(QconnectApp::new(
            Arc::clone(&transport),
            Arc::clone(&sink),
            Arc::clone(&sync_state),
        ));
        sink.set_app(&app);

        // The transport broadcast has no replay; subscribe before connect so
        // Connected/Auth/Subscribed cannot race past this runtime.
        let transport_rx = app.subscribe_transport_events();

        let app_connect = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                let _ = app.disconnect().await;
                return Err("Qobuz Connect was disabled during transport connect".to_string());
            }
            result = app.connect(config.clone()) => result,
        };
        if let Err(err) = app_connect {
            let message = format!("qconnect transport connect failed: {err}");
            self.record_connect_error_if_current(enable_token, message.clone());
            return Err(message);
        }
        if !self.enable_intent.is_current(enable_token) {
            let _ = app.disconnect().await;
            return Err("Qobuz Connect was disabled during transport connect".to_string());
        }

        // T10 (OD4, §7.4): if locked volume mode, pin the player to 100% (1.0)
        // at connect time. Harmless no-op on bit-perfect backends (ALSA-direct
        // hw_volume=false, JACK, DoP/DSD), corrects Rodio backends (PipeWire/
        // Pulse) where Player default is 0.75 (not 1.0).
        if volume_mode == engine::VolumeMode::Locked {
            if let Err(err) = self.runtime.core().set_volume(1.0) {
                log::warn!("[QConnect] failed to pin volume to 100% at connect: {err}");
            }
        }
        if !self.enable_intent.is_current(enable_token) {
            let _ = app.disconnect().await;
            return Err("Qobuz Connect was disabled before owner install".to_string());
        }

        let idle_retry_active = config.reconnect_idle_retry_ms > 0;
        let host: Arc<dyn SessionLoopHost> = Arc::new(DaemonSessionLoopHost {
            app: Arc::clone(&app),
            sync_state: Arc::clone(&sync_state),
            inner: Arc::clone(&self.inner),
            authority: Arc::clone(&self.authority),
            stamp,
            sink: Arc::clone(&sink),
            runtime: Arc::clone(&self.runtime),
            shared: Arc::clone(&self.shared),
            volume_mode,
            projection: projection.clone(),
        });
        let mut host = Some(host);
        let mut transport_rx = Some(transport_rx);
        let installed = self
            .enable_intent
            .commit_if_current(enable_token, || {
                let mut guard = lock_inner(&self.inner);
                if guard.runtime.is_some() || guard.connecting_token != Some(enable_token) {
                    return false;
                }
                if !projection.install_authority(&self.authority, stamp, None) {
                    guard.connecting_token = None;
                    guard.lifecycle_state = QconnectLifecycleState::Off;
                    return false;
                }
                let app_for_loop = Arc::clone(&app);
                let host = host.take().expect("connect host consumed once");
                let transport_rx = transport_rx.take().expect("connect receiver consumed once");
                let event_loop = tokio::spawn(async move {
                    app_for_loop
                        .run_session_loop(host, transport_rx, idle_retry_active)
                        .await;
                });
                guard.last_error = None;
                guard.last_pushed_queue_ids = None;
                guard.connecting_token = None;
                guard.runtime = Some(DaemonQconnectRuntime {
                    stamp,
                    app: Arc::clone(&app),
                    config: config.clone(),
                    event_loop,
                    sync_state: Arc::clone(&sync_state),
                });
                true
            })
            .unwrap_or(false);
        if !installed {
            let error = "qconnect owner install was disabled or superseded".to_string();
            self.record_connect_error_if_current(enable_token, error.clone());
            let _ = app.disconnect().await;
            return Err(error);
        }

        let custom_name = self.custom_device_name.read().await.clone();
        let bootstrap = tokio::select! {
            biased;
            _ = self.enable_intent.cancelled(enable_token) => {
                let error = "Qobuz Connect was disabled during bootstrap".to_string();
                self.retire_initial_owner_if_current(enable_token, stamp, None).await;
                return Err(error);
            }
            result = bootstrap_remote_presence(
                &app,
                custom_name.clone(),
                &self.authority,
                stamp,
            ) => result,
        };
        if let Err(err) = bootstrap {
            let error = format!("qconnect bootstrap failed: {err}");
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
            let error = "qconnect owner authority changed during bootstrap".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if let Err(error) = self.wait_for_owner_session_ready(enable_token, stamp).await {
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }

        // Reflect the resolved device name in `/api/status` while this exact
        // enabled intent is still current.
        let effective_name = transport::resolve_qconnect_friendly_name(custom_name.as_deref());
        let _ = self.enable_intent.commit_if_current(enable_token, || {
            if let Ok(mut shared) = self.shared.lock() {
                shared.qconnect.device_name = effective_name;
                shared.qconnect.credential_origin = "owner".to_string();
            }
        });

        let owner_ready = self
            .await_while_enabled(
                enable_token,
                self.coordinator
                    .declare_owner_ready_if(|| self.enable_intent.is_current(enable_token)),
            )
            .await
            .unwrap_or(false);
        if !owner_ready {
            if self.enable_intent.is_current(enable_token) {
                self.coordinator.shutdown().await;
            }
            let error = "qconnect delegation coordinator did not enter OwnerReady".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if !self.enable_intent.is_current(enable_token) {
            let error = "Qobuz Connect was disabled before LAN startup".to_string();
            self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                .await;
            return Err(error);
        }
        if let Err(error) = self.start_lan(enable_token, stamp, &qws_endpoint).await {
            if !self.enable_intent.is_current(enable_token) || !self.authority.is_current(stamp) {
                self.retire_initial_owner_if_current(enable_token, stamp, Some(error.clone()))
                    .await;
                return Err(error);
            }
            let _ = self.enable_intent.commit_if_current(enable_token, || {
                log::warn!("[QConnect LAN] listener unavailable: {error}");
                self.set_lan_status("error", None, Some(&error));
            });
        }

        self.enable_intent
            .is_current(enable_token)
            .then_some(())
            .ok_or_else(|| "Qobuz Connect was disabled during connect".to_string())
    }

    /// Tear the QConnect session down. Always forces Off. Aborts AND joins the
    /// event loop so its `Arc<AppRuntime>` clone drops before the daemon's
    /// shutdown releases the audio device (§8.2 / #521 ordering).
    pub async fn disconnect(&self) -> Result<(), String> {
        self.disconnect_safely().await.map(|_| ())
    }

    pub async fn disconnect_safely(&self) -> Result<QconnectDisconnectOutcome, String> {
        self.disconnect_with_owner_policy(true).await
    }

    /// Credential replacement/logout tears delegated material down before the
    /// owner session mutates. The saved owner queue is restored locally (no
    /// owner cloud runtime is rebuilt), so a guest queue can never reach the
    /// next account or the final session save.
    pub async fn disconnect_for_credentials_change(&self) -> Result<(), String> {
        self.disconnect_safely().await.map(|_| ())
    }

    /// T11 (`POST /api/settings/reload`): re-cache the device-name override from
    /// the daemon-root KV so the NEXT `connect()` (whenever that happens) uses
    /// whatever `qbzd qconnect name` / `settings set qconnect.device_name` most
    /// recently wrote — 03-setup-tui.md §3.4's "applies on the next connection"
    /// rule. Does NOT force a reconnect by itself (a rename alone must not
    /// bounce an active session).
    async fn refresh_device_name(&self, settings_db: &std::path::Path) {
        let name = transport::load_device_name_at(settings_db);
        *self.custom_device_name.write().await = name;
    }

    /// Wait until the daemon is Ready (logged in + API initialized), then attempt
    /// `connect()` with the bounded [2s, 5s, 15s, 30s] retry schedule. Each
    /// `connect()` re-resolves the transport config internally, so a transient
    /// credential/network failure can clear on a later attempt.
    async fn connect_on_ready(self: Arc<Self>, enable_token: QconnectEnableToken) {
        loop {
            if !self.enable_intent.is_current(enable_token) {
                log::info!("[QConnect] auto-connect cancelled before Ready");
                return;
            }
            let logged_in = self
                .shared
                .lock()
                .map(|s| s.auth == AuthState::LoggedIn)
                .unwrap_or(false);
            let api_initialized = logged_in && self.runtime.core().is_api_initialized().await;
            if !self.enable_intent.is_current(enable_token) {
                return;
            }
            if api_initialized {
                break;
            }
            if !self.enable_intent.is_current(enable_token) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !self.enable_intent.is_current(enable_token) {
                return;
            }
        }

        let schedule: [u64; 4] = [2_000, 5_000, 15_000, 30_000];
        for attempt in 0..=schedule.len() {
            if !self.enable_intent.is_current(enable_token) {
                log::info!("[QConnect] auto-connect cancelled by disable");
                return;
            }
            match self.connect_with_token(enable_token).await {
                Ok(()) => {
                    if !self.enable_intent.is_current(enable_token) {
                        return;
                    }
                    log::info!("[QConnect] auto-connect succeeded");
                    return;
                }
                Err(err) => {
                    if !self.enable_intent.is_current(enable_token) {
                        return;
                    }
                    log::warn!(
                        "[QConnect] auto-connect attempt {} failed: {err}",
                        attempt + 1
                    );
                }
            }
            match schedule.get(attempt) {
                Some(delay_ms) => {
                    if !self.enable_intent.is_current(enable_token) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                    if !self.enable_intent.is_current(enable_token) {
                        return;
                    }
                }
                None => {
                    log::warn!(
                        "[QConnect] auto-connect gave up for this session after {} attempts",
                        attempt + 1
                    );
                    return;
                }
            }
        }
    }
}

/// Owner handle held by the daemon boot for the process lifetime. Drives runtime
/// enable/disable (T11) and the ordered shutdown (§8.2-1).
pub struct QconnectHandle {
    service: Arc<DaemonQconnectService>,
    watcher: Option<JoinHandle<()>>,
    /// T10: the report-tick scheduler task. Held so shutdown can abort+join it
    /// (it clones `Arc<AppRuntime>`, so it must drop before `drop(booted)` per the
    /// #521 clock-release ordering, exactly like the watcher).
    report_task: Option<JoinHandle<()>>,
    /// The queue-publish subscriber (publish.rs). Same #521 ordering contract as
    /// `report_task` — it clones `Arc<AppRuntime>` + the qconnect inner.
    publish_task: Option<JoinHandle<()>>,
    /// Mirrors the coordinator's sanitized phase/generation into daemon status.
    delegation_task: Option<JoinHandle<()>>,
}

/// T11: a `Clone`-able, `Send + Sync` handle onto the running
/// [`DaemonQconnectService`] — what the reload route reaches through
/// `ApiState.qconnect_control` (`Arc<std::sync::OnceLock<QconnectControl>>`,
/// populated by `QconnectHandle::control()` right after `start()` returns).
/// `connect`/`disconnect` are already idempotent on the service (a no-op when
/// already in the target state), so the reload path can call either
/// unconditionally without checking current status first.
#[derive(Clone)]
pub struct QconnectControl(Arc<DaemonQconnectService>);

impl QconnectControl {
    pub async fn connect(&self) -> Result<(), String> {
        self.0.connect().await
    }

    pub async fn disconnect(&self) -> Result<(), String> {
        self.0.disconnect().await
    }

    pub async fn disconnect_for_credentials_change(&self) -> Result<(), String> {
        self.0.disconnect_for_credentials_change().await
    }

    pub fn owner_actions_allowed(&self) -> bool {
        self.0.authority.owner_actions_allowed()
    }

    pub fn try_owner_action_permit(&self) -> Option<authority::AuthorityActionPermit> {
        self.0.authority.try_owner_action_permit()
    }

    pub fn try_transport_action_permit(&self) -> Option<authority::AuthorityActionPermit> {
        self.0.authority.try_transport_action_permit()
    }

    /// Re-cache the device-name override from the daemon-root KV (§ see
    /// [`DaemonQconnectService::refresh_device_name`] — applies on the NEXT
    /// connect, never forces a reconnect for a rename alone).
    pub async fn refresh_device_name(&self, settings_db: &std::path::Path) {
        self.0.refresh_device_name(settings_db).await;
    }
}

impl QconnectHandle {
    /// Connect on demand (T11 `qbzd qconnect enable`).
    #[allow(dead_code)]
    pub async fn connect(&self) -> Result<(), String> {
        self.service.connect().await
    }

    /// Disconnect on demand (T11 `qbzd qconnect disable`).
    #[allow(dead_code)]
    pub async fn disconnect(&self) -> Result<(), String> {
        self.service.disconnect().await
    }

    /// A cheap, `Clone`-able handle onto the running service — what
    /// `daemon.rs`'s `POST /api/settings/reload` route holds (via an
    /// `Arc<OnceLock<QconnectControl>>` populated right after `start()`
    /// returns, since QConnect boots AFTER the HTTP server per the normative
    /// order, 01-architecture.md §8.1 steps 11/12). Carries none of the
    /// `JoinHandle`s `QconnectHandle` owns — those stay daemon-shutdown-only.
    pub fn control(&self) -> QconnectControl {
        QconnectControl(Arc::clone(&self.service))
    }

    /// §8.2-1: stop the auto-connect watcher and disconnect the session BEFORE
    /// the daemon stops playback. Aborts + joins the watcher and the event loop so
    /// every `Arc<AppRuntime>` clone this handle owns drops ahead of
    /// `drop(booted)` (the #521 clock-release ordering).
    pub async fn shutdown(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
            let _ = watcher.await;
        }
        // Normative shutdown order: withdraw LAN admission and cancel the
        // coordinator before stopping report/publish tasks or playback.
        let _ = self.service.disconnect().await;
        if let Some(delegation_task) = self.delegation_task.take() {
            delegation_task.abort();
            let _ = delegation_task.await;
        }
        // T10: stop the report scheduler too, so its `Arc<AppRuntime>` clone drops
        // ahead of `drop(booted)`.
        if let Some(report_task) = self.report_task.take() {
            report_task.abort();
            let _ = report_task.await;
        }
        // Same for the queue-publish subscriber (publish.rs).
        if let Some(publish_task) = self.publish_task.take() {
            publish_task.abort();
            let _ = publish_task.await;
        }
    }
}

fn delegation_error_label(error: DelegationErrorCode) -> &'static str {
    match error {
        DelegationErrorCode::CandidateExpired => "candidate-expired",
        DelegationErrorCode::CandidateCancelled => "candidate-cancelled",
        DelegationErrorCode::ValidationTimedOut => "validation-timeout",
        DelegationErrorCode::ApiRejected => "api-rejected",
        DelegationErrorCode::QwsRejected => "qws-rejected",
        DelegationErrorCode::ActivationRejected => "activation-rejected",
        DelegationErrorCode::CommitRejected => "commit-rejected",
        DelegationErrorCode::OwnerRestoreFailed => "owner-restore-failed",
        DelegationErrorCode::GenerationExhausted => "generation-exhausted",
        DelegationErrorCode::Internal => "internal",
    }
}

fn latch_delegation_snapshot(shared: &SharedState, snapshot: DelegationSnapshot) {
    if let Ok(mut state) = shared.lock() {
        state.qconnect.credential_origin = match snapshot.credential_origin {
            CredentialOrigin::Owner => "owner",
            CredentialOrigin::Delegated { .. } => "delegated",
        }
        .to_string();
        state.qconnect.candidate_generation = snapshot.candidate_generation;
        match snapshot.phase {
            DelegationPhase::Disabled | DelegationPhase::ShuttingDown => {
                state.qconnect.lan_state = "off".to_string();
                state.qconnect.lan_port = None;
            }
            DelegationPhase::CandidateValidating { .. }
            | DelegationPhase::CandidateValidated { .. }
            | DelegationPhase::Activating { .. } => {
                state.qconnect.lan_state = "validating".to_string();
                state.qconnect.last_lan_error = None;
            }
            DelegationPhase::DelegatedActive { .. } => {
                state.qconnect.lan_state = "delegated".to_string();
                state.qconnect.last_lan_error = None;
            }
            DelegationPhase::RestoringOwner { .. } => {
                state.qconnect.lan_state = "restoring".to_string();
                state.qconnect.last_lan_error = None;
            }
            DelegationPhase::OwnerReady => {
                if state.qconnect.lan_port.is_some() {
                    state.qconnect.lan_state = "listening".to_string();
                    if snapshot.last_error.is_none() {
                        state.qconnect.last_lan_error = None;
                    }
                }
            }
        }
        if let Some(error) = snapshot.last_error {
            state.qconnect.last_lan_error = Some(delegation_error_label(error).to_string());
        }
        state.emit_qconnect_session_changed();
    }
}

/// Boot step 12: wire QConnect. Mints the daemon's OWN device identity in the
/// daemon-root KV, decides auto-connect from the persisted startup mode
/// (`cli_override = None`, `last_known = None` — P0), latches the initial status,
/// and, when enabled, spawns the connect-on-Ready retry task.
pub fn start(
    runtime: Runtime,
    shared: SharedState,
    authority: Arc<AuthorityCell>,
    roots: &ProfileRoots,
    quality_cap: Arc<std::sync::Mutex<qbz_models::Quality>>,
    report_notify: Arc<tokio::sync::Notify>,
    core_events: broadcast::Receiver<CoreEvent>,
) -> QconnectHandle {
    let settings_db = roots.data.join("qconnect_settings.db");
    // Re-point device identity + KV at the daemon root (NEVER the desktop global).
    transport::init_settings_db_path(settings_db.clone());

    // Effective startup decision (Ready-state only). `cli_override` stays None: a
    // `Some` would permanently shadow the KV store that `qbzd qconnect
    // enable|disable` (T11) + the TUI (T13) write, making both dead controls.
    // `last_known` is None in P0 (RememberLast resolves to off).
    let mode = transport::load_startup_mode_at(&settings_db);
    let should_auto_connect = compute_effective_startup(mode, None, None);
    let custom_name = transport::load_device_name_at(&settings_db);
    let effective_name = transport::resolve_qconnect_friendly_name(custom_name.as_deref());

    // Latch the initial status so `/api/status` reflects the config before Ready.
    if let Ok(mut s) = shared.lock() {
        s.qconnect.enabled = should_auto_connect;
        s.qconnect.device_name = effective_name;
        s.qconnect.state = "off".to_string();
        s.qconnect.session_active = false;
    }

    let inner = Arc::new(StdMutex::new(DaemonQconnectInner::default()));
    let enable_intent = Arc::new(QconnectEnableIntent::new(should_auto_connect));
    let custom_device_name = Arc::new(tokio::sync::RwLock::new(custom_name));
    let delegation_host = Arc::new(DaemonDelegationHost::new(
        Arc::clone(&runtime),
        Arc::clone(&inner),
        Arc::clone(&shared),
        settings_db.clone(),
        Arc::clone(&custom_device_name),
        Arc::clone(&quality_cap),
        Arc::clone(&authority),
    ));
    let coordinator = DelegationCoordinator::disabled(
        Arc::clone(&delegation_host),
        DelegationCoordinatorConfig::default(),
    );
    assert!(
        delegation_host.install_coordinator(coordinator.clone()),
        "qconnect delegation coordinator may only be installed once"
    );
    let service = Arc::new(DaemonQconnectService {
        inner,
        authority,
        enable_intent,
        runtime,
        shared,
        settings_db,
        custom_device_name,
        quality_cap,
        delegation_host,
        coordinator,
        lan: AsyncMutex::new(None),
        lan_lifecycle: LanRuntimeLifecycle::new(|runtime: &mut DaemonLanRuntime| {
            runtime.shutdown()
        }),
        lifecycle_gate: AsyncMutex::new(()),
        teardown_incomplete: AtomicBool::new(false),
    });

    let watcher = if should_auto_connect {
        log::info!(
            "[QConnect] auto-connect enabled (startup mode = {}); waiting for Ready",
            mode.as_str()
        );
        let svc = Arc::clone(&service);
        let enable_token = service
            .enable_intent
            .current_token()
            .expect("enabled startup intent must have a token");
        Some(tokio::spawn(async move {
            svc.connect_on_ready(enable_token).await
        }))
    } else {
        log::info!(
            "[QConnect] auto-connect disabled (startup mode = {})",
            mode.as_str()
        );
        None
    };

    // T10 (§7.2): spawn the report-tick scheduler. It runs for the daemon
    // lifetime, waking on the driver's ReportEdge signal (via `report_notify`)
    // and its own ~2 s floor, and reports on the LIVE session (a no-op until a
    // connect installs a runtime).
    let scheduler_inner = Arc::clone(&service.inner);
    let scheduler_runtime = Arc::clone(&service.runtime);
    let scheduler_authority = Arc::clone(&service.authority);
    let report_task = Some(tokio::spawn(async move {
        report::run_report_scheduler(
            report_notify,
            scheduler_inner,
            scheduler_runtime,
            scheduler_authority,
        )
        .await;
    }));

    // Queue-publish subscriber (publish.rs): debounced CoreEvent::QueueUpdated ->
    // push the local queue to the cloud when it changed. Runs for the daemon
    // lifetime; a no-op until a connect installs a runtime (and gated to
    // active-local-renderer sessions inside the publish body).
    let publish_task = Some(publish::spawn_queue_cloud_publish(
        Arc::clone(&service.inner),
        Arc::clone(&service.runtime),
        Arc::clone(&service.authority),
        core_events,
    ));

    let mut delegation_rx = service.coordinator.subscribe();
    let delegation_shared = Arc::clone(&service.shared);
    let delegation_task = Some(tokio::spawn(async move {
        loop {
            let snapshot = *delegation_rx.borrow_and_update();
            latch_delegation_snapshot(&delegation_shared, snapshot);
            if delegation_rx.changed().await.is_err() {
                break;
            }
        }
    }));

    QconnectHandle {
        service,
        watcher,
        report_task,
        publish_task,
        delegation_task,
    }
}
