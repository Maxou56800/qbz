//! qbzd host for the shared transactional QConnect delegation coordinator.
//!
//! Candidate API/QWS work is completed against isolated contexts while the
//! installed runtime remains authoritative. The synchronous commit methods do
//! only a short stamped runtime swap; teardown, queue restoration and cloud
//! bootstrap happen afterwards.
//!
//! Race-sensitive policy is deliberately shared by `qconnect-app`: authority
//! fences and deferred release, QWS preflight/join/rejoin, LAN projection and
//! physical LAN lifecycle. The code left here is an adapter boundary for
//! qbzd's shared state, volume modes and abortable task registry. Do not mirror
//! new policy into the Qt adapter; extract it with behavioral tests in
//! `qconnect-app`.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use qbz_app::shell::AppRuntime;
use qbz_core::QueueAuthoritySnapshot;
use qbz_models::{CoreEvent, Quality};
use qbz_qobuz::{DelegatedApiConfig, DelegatedApiEndpoint, DelegatedQobuzClient, QobuzClient};
use qconnect_app::{
    acquire_transition_guard_and_fence, max_audio_quality_from_quality, CommitRejected,
    DeferredActivationRelease, DelegatedRejoinWatchdog, DelegatedRuntimeEventDirective,
    DelegatedRuntimeEventState, DelegationCancellation, DelegationCoordinator, DelegationErrorCode,
    DelegationHost, DelegationPreflight, OwnerActionFence, QconnectApp, QconnectAppEvent,
    QconnectEventSink, QconnectLifecycleState, QconnectRemoteSyncState, QconnectSessionState,
    RestoreReason, SessionLoopHost,
};
use qconnect_lan::{HandoffCandidate, LanProjection};
use qconnect_transport_ws::{NativeWsTransport, TransportEvent, WsTransportConfig};
use tokio::sync::{broadcast, oneshot, watch, Mutex as AsyncMutex, OwnedMutexGuard};
use zeroize::Zeroizing;

use crate::adapter::DaemonAdapter;
use crate::state::DaemonShared;

use super::authority::{AuthorityCell, AuthorityOrigin, AuthorityStamp};
use super::engine::{DaemonRendererEngine, VolumeMode};
use super::lan::DaemonLanProjectionSlot;
use super::session::{bootstrap_prepared_owner_presence, DaemonSessionLoopHost};
use super::sink::{DaemonEventSink, DaemonQconnectApp};
use super::transport::{device_info_with_volume_mode, resolve_transport_config};
use super::{
    lock_inner, update_lifecycle_state_if_running, DaemonQconnectInner, DaemonQconnectRuntime,
};

type Runtime = Arc<AppRuntime<DaemonAdapter>>;
type SharedState = Arc<StdMutex<DaemonShared>>;
type OwnerRestoreResult = Result<(), &'static str>;
pub type DaemonDelegationCoordinator = DelegationCoordinator<DaemonDelegationHost>;

const SHUTDOWN_RESTORE_OWNER: u8 = 0;
const SHUTDOWN_DISCARD_OWNER: u8 = 1;
static NEXT_OWNER_RESTORE_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct OwnerRestoreTask {
    id: u64,
    abort: tokio::task::AbortHandle,
    mutation_finished: Arc<AtomicBool>,
    completion: watch::Receiver<Option<OwnerRestoreResult>>,
}

impl OwnerRestoreTask {
    fn is_pending(&self) -> bool {
        self.completion.borrow().is_none()
    }
}

#[derive(Clone)]
struct OwnerSnapshot {
    queue: QueueAuthoritySnapshot,
    volume: f32,
    playback: OwnerPlaybackSnapshot,
}

#[derive(Clone, Copy)]
struct OwnerPlaybackSnapshot {
    track_id: u64,
    position_secs: u64,
    was_playing: bool,
    had_loaded_audio: bool,
}

/// Adapter payload only: the shared preparation/activation policy lives in
/// `qconnect-app`; these concrete daemon app/sink handles and `VolumeMode`
/// cannot cross the crate boundary without coupling the shared layer to qbzd.
struct PreparedCommon {
    app: Arc<DaemonQconnectApp>,
    sink: Arc<DaemonEventSink>,
    config: WsTransportConfig,
    sync_state: Arc<AsyncMutex<QconnectRemoteSyncState>>,
    preflight: DelegationPreflight,
    stamp: AuthorityStamp,
    expected_current: AuthorityStamp,
    volume_mode: VolumeMode,
    custom_name: Option<String>,
}

pub struct PreparedDelegation {
    common: PreparedCommon,
    session_id: Zeroizing<String>,
    become_active: bool,
    owner_snapshot: Option<OwnerSnapshot>,
    transition_guard: Option<OwnedMutexGuard<()>>,
    transition_fence: Option<OwnerActionFence>,
}

pub struct PreparedOwner {
    common: PreparedCommon,
    transition_guard: OwnedMutexGuard<()>,
    transition_fence: OwnerActionFence,
}

enum PendingActivationKind {
    Delegated,
    Owner {
        volume: f32,
        playback: OwnerPlaybackSnapshot,
    },
}

/// Cancellation-safe release of a freshly installed runtime. Normal retirement
/// stops the old transport first; if the coordinator's cleanup timeout drops
/// that future, `Drop` still stops stale audio and releases the current loop.
struct PendingActivation {
    start: Option<oneshot::Sender<()>>,
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    kind: PendingActivationKind,
    transition_guard: Option<OwnedMutexGuard<()>>,
    transition_fence: Option<OwnerActionFence>,
    owner_restore_task: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    owner_quality: Quality,
}

impl PendingActivation {
    fn release(&mut self) {
        let Some(start) = self.start.take() else {
            return;
        };
        if !self.authority.is_current(self.stamp) {
            self.transition_fence.take();
            self.transition_guard.take();
            return;
        }
        let _ = self.runtime.core().stop();
        if let PendingActivationKind::Owner { volume, .. } = &self.kind {
            if let Err(error) = self.runtime.core().set_volume(*volume) {
                log::warn!("[QConnect] failed to restore owner volume: {error}");
            }
        }
        if let PendingActivationKind::Owner { playback, .. } = &self.kind {
            let release = DeferredActivationRelease::new(
                Some(start),
                self.transition_fence.take(),
                self.transition_guard.take(),
            );
            schedule_owner_playback_restore(
                Arc::clone(&self.runtime),
                Arc::clone(&self.authority),
                self.stamp,
                *playback,
                self.owner_quality,
                Arc::clone(&self.owner_restore_task),
                release,
            );
        } else {
            let mut release = DeferredActivationRelease::new(
                Some(start),
                self.transition_fence.take(),
                self.transition_guard.take(),
            );
            release.release();
        }
    }
}

impl Drop for PendingActivation {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct RetiredAuthority {
    runtime: Option<DaemonQconnectRuntime>,
    activation: Option<PendingActivation>,
}

/// Runtime-owned host. It contains no delegated credential material; prepared
/// candidates own and scrub that material until a successful stamped commit.
pub struct DaemonDelegationHost {
    runtime: Runtime,
    inner: Arc<StdMutex<DaemonQconnectInner>>,
    shared: SharedState,
    settings_db: std::path::PathBuf,
    custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
    quality_cap: Arc<StdMutex<Quality>>,
    authority: Arc<AuthorityCell>,
    coordinator: OnceLock<DaemonDelegationCoordinator>,
    transition_gate: Arc<AsyncMutex<()>>,
    owner_snapshot: StdMutex<Option<OwnerSnapshot>>,
    owner_restore_task: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    projection: DaemonLanProjectionSlot,
    shutdown_mode: AtomicU8,
}

impl DaemonDelegationHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Runtime,
        inner: Arc<StdMutex<DaemonQconnectInner>>,
        shared: SharedState,
        settings_db: std::path::PathBuf,
        custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
        quality_cap: Arc<StdMutex<Quality>>,
        authority: Arc<AuthorityCell>,
    ) -> Self {
        Self {
            runtime,
            inner,
            shared,
            settings_db,
            custom_device_name,
            quality_cap,
            authority,
            coordinator: OnceLock::new(),
            transition_gate: Arc::new(AsyncMutex::new(())),
            owner_snapshot: StdMutex::new(None),
            owner_restore_task: Arc::new(StdMutex::new(None)),
            projection: DaemonLanProjectionSlot::default(),
            shutdown_mode: AtomicU8::new(SHUTDOWN_RESTORE_OWNER),
        }
    }

    pub fn install_coordinator(&self, coordinator: DaemonDelegationCoordinator) -> bool {
        self.coordinator.set(coordinator).is_ok()
    }

    pub fn attach_projection(&self, projection: LanProjection) {
        self.projection.attach(&self.authority, projection);
    }

    pub fn detach_projection(&self) {
        self.projection.detach();
    }

    pub fn projection_slot(&self) -> DaemonLanProjectionSlot {
        self.projection.clone()
    }

    pub fn current_projected_session_id(&self, stamp: AuthorityStamp) -> Option<String> {
        self.projection.current_session_id(&self.authority, stamp)
    }

    pub fn set_shutdown_restore_owner(&self, restore: bool) {
        self.shutdown_mode.store(
            if restore {
                SHUTDOWN_RESTORE_OWNER
            } else {
                SHUTDOWN_DISCARD_OWNER
            },
            Ordering::Release,
        );
    }

    fn coordinator(&self) -> Option<DaemonDelegationCoordinator> {
        self.coordinator.get().cloned()
    }

    pub async fn await_owner_playback_restore(&self) -> Result<(), &'static str> {
        // Clone the observation handles but leave the task in the host slot.
        // If this future is itself cancelled, a later caller can still await
        // the same restoration instead of silently detaching its JoinHandle.
        let task = recover_lock(&self.owner_restore_task).as_ref().cloned();
        let Some(mut task) = task else {
            return Ok(());
        };
        match tokio::time::timeout(
            Duration::from_secs(31),
            wait_for_owner_restore_completion(&mut task),
        )
        .await
        {
            Ok(result) => {
                clear_owner_restore_task_if(&self.owner_restore_task, task.id);
                result
            }
            Err(_) => {
                // The restoration has its own 30-second deadline, so this is
                // a stuck task/scheduler boundary. Abort the actual worker and
                // wait for its supervisor to join it. A pathological delayed
                // join remains in the slot for a later teardown retry.
                task.abort.abort();
                if tokio::time::timeout(
                    Duration::from_secs(1),
                    wait_for_owner_restore_completion(&mut task),
                )
                .await
                .is_ok()
                {
                    clear_owner_restore_task_if(&self.owner_restore_task, task.id);
                }
                Err("owner-playback-restore-timed-out")
            }
        }
    }

    pub fn owner_restore_pending(&self) -> bool {
        self.owner_snapshot_pending()
            || recover_lock(&self.owner_restore_task)
                .as_ref()
                .is_some_and(OwnerRestoreTask::is_pending)
    }

    pub fn owner_snapshot_pending(&self) -> bool {
        recover_lock(&self.owner_snapshot).is_some()
    }

    fn volume_mode(&self) -> VolumeMode {
        VolumeMode::from_kv(super::transport::load_volume_mode_at(&self.settings_db).as_deref())
    }

    async fn owner_client(&self) -> Result<QobuzClient, DelegationErrorCode> {
        self.runtime
            .core()
            .client()
            .read()
            .await
            .clone()
            .ok_or(DelegationErrorCode::ApiRejected)
    }

    fn make_app(
        &self,
        engine: DaemonRendererEngine,
        stamp: AuthorityStamp,
    ) -> (
        Arc<DaemonQconnectApp>,
        Arc<DaemonEventSink>,
        Arc<AsyncMutex<QconnectRemoteSyncState>>,
    ) {
        let transport = Arc::new(NativeWsTransport::new());
        let sync_state = Arc::new(AsyncMutex::new(QconnectRemoteSyncState::default()));
        let sink = Arc::new(DaemonEventSink::new(
            engine,
            Arc::clone(&sync_state),
            Arc::clone(&self.authority),
            stamp,
            self.projection.clone(),
        ));
        let app = Arc::new(QconnectApp::new(
            transport,
            Arc::clone(&sink),
            Arc::clone(&sync_state),
        ));
        sink.set_app(&app);
        (app, sink, sync_state)
    }

    async fn disconnect_common(common: PreparedCommon) {
        let _ = common.app.disconnect().await;
    }

    async fn stop_runtime(&self, runtime: DaemonQconnectRuntime) {
        runtime.event_loop.abort();
        {
            let mut sync = runtime.sync_state.lock().await;
            sync.watchdog_generation = sync.watchdog_generation.wrapping_add(1);
            sync.session = QconnectSessionState::default();
            sync.session_renderer_states.clear();
        }
        let _ = runtime.app.disconnect().await;
        let _ = runtime.event_loop.await;
    }

    async fn restore_owner_snapshot(&self) -> Option<OwnerPlaybackSnapshot> {
        // Clone first and clear only after the atomic replacement completes.
        // If shutdown cancellation drops this future, the original snapshot
        // remains available to the idempotent teardown retry.
        let snapshot = recover_lock(&self.owner_snapshot).as_ref().cloned();
        let Some(snapshot) = snapshot else {
            return None;
        };
        let _ = self.runtime.core().stop();
        self.runtime
            .core()
            .restore_authority_snapshot(snapshot.queue)
            .await;
        let _ = self.runtime.core().set_volume(snapshot.volume);
        let state = self.runtime.core().get_queue_state().await;
        let bus = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.bus.clone());
        if let Some(bus) = bus {
            let _ = bus.send(CoreEvent::QueueUpdated { state });
        }
        recover_lock(&self.owner_snapshot).take();
        Some(snapshot.playback)
    }

    fn publish_origin(&self, origin: AuthorityOrigin) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.qconnect.credential_origin = match origin {
                AuthorityOrigin::Owner => "owner",
                AuthorityOrigin::Delegated { .. } => "delegated",
            }
            .to_string();
            shared.emit_qconnect_session_changed();
        }
    }
}

#[async_trait]
impl DelegationHost for DaemonDelegationHost {
    type Candidate = HandoffCandidate;
    type PreparedDelegation = PreparedDelegation;
    type PreparedOwner = PreparedOwner;
    type RetiredAuthority = RetiredAuthority;

    async fn prepare_delegation(
        &self,
        generation: u64,
        candidate: HandoffCandidate,
        cancellation: DelegationCancellation,
    ) -> Result<PreparedDelegation, DelegationErrorCode> {
        let expected_current = self
            .authority
            .current()
            .ok_or(DelegationErrorCode::CommitRejected)?;
        let app_credentials = self
            .owner_client()
            .await?
            .delegated_app_credentials()
            .await
            .map_err(|_| DelegationErrorCode::ApiRejected)?;

        let (session_id, api_token, qws_token, become_active) = candidate.into_parts();
        let session_id = Zeroizing::new(session_id);
        let (api_endpoint, api_exp, api_jwt) = api_token.into_parts();
        let api_endpoint = Zeroizing::new(api_endpoint);
        let mut api_jwt = Zeroizing::new(api_jwt);
        let (qws_endpoint, _qws_exp, qws_jwt) = qws_token.into_parts();
        let mut qws_endpoint = Zeroizing::new(qws_endpoint);
        let mut qws_jwt = Zeroizing::new(qws_jwt);
        let api_exp = u64::try_from(api_exp).map_err(|_| DelegationErrorCode::CandidateExpired)?;
        let api_host = reqwest::Url::parse(&api_endpoint)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .ok_or(DelegationErrorCode::ApiRejected)?;
        let endpoint = DelegatedApiEndpoint::new(&api_endpoint, &[api_host.as_str()])
            .map_err(|_| DelegationErrorCode::ApiRejected)?;
        let api_config = DelegatedApiConfig::new(
            endpoint,
            api_exp,
            std::mem::take(&mut *api_jwt),
            app_credentials,
        )
        .map_err(|_| DelegationErrorCode::ApiRejected)?;
        let delegated_client = Arc::new(
            DelegatedQobuzClient::new(api_config).map_err(|_| DelegationErrorCode::ApiRejected)?,
        );

        let stamp = self
            .authority
            .reserve(AuthorityOrigin::Delegated { generation });
        let volume_mode = self.volume_mode();
        let engine = DaemonRendererEngine::delegated(
            Arc::clone(&self.runtime),
            Arc::clone(&delegated_client),
            volume_mode,
            Arc::clone(&self.quality_cap),
            Arc::clone(&self.authority),
            stamp,
        );
        let (app, sink, sync_state) = self.make_app(engine, stamp);
        let receiver = app.subscribe_transport_events();
        let mut config = WsTransportConfig::default();
        config.endpoint_url = std::mem::take(&mut *qws_endpoint);
        config.jwt_qws = Some(std::mem::take(&mut *qws_jwt));
        config.require_jwt = true;
        config.subscribe_channels = vec![vec![0x01], vec![0x02], vec![0x03]];
        config.reconnect_idle_retry_ms = 60_000;
        app.connect(config.clone())
            .await
            .map_err(|_| DelegationErrorCode::QwsRejected)?;

        let custom_name = self.custom_device_name.read().await.clone();
        let mut common = PreparedCommon {
            app,
            sink,
            config,
            sync_state,
            preflight: DelegationPreflight::new(receiver),
            stamp,
            expected_current,
            volume_mode,
            custom_name,
        };
        let api_validation = async {
            delegated_client
                .validate_access()
                .await
                .map_err(|_| DelegationErrorCode::ApiRejected)
        };
        tokio::try_join!(
            api_validation,
            common.preflight.wait_for_qws_ready(cancellation)
        )?;

        Ok(PreparedDelegation {
            common,
            session_id,
            become_active,
            owner_snapshot: None,
            transition_guard: None,
            transition_fence: None,
        })
    }

    async fn activate_delegation(
        &self,
        _generation: u64,
        prepared: &mut PreparedDelegation,
        cancellation: DelegationCancellation,
    ) -> Result<(), DelegationErrorCode> {
        // Serialize the fallible activation/commit/retirement tail before
        // closing ordinary action admission. The guard remains in `prepared`
        // through commit and moves into `PendingActivation`; owner rollback
        // keeps it until playback is restored. A second fence is not a mutex,
        // so this explicit gate is what prevents overlapping transitions.
        let (transition_guard, transition_fence) = acquire_transition_guard_and_fence(
            Arc::clone(&self.transition_gate),
            Arc::clone(&self.authority),
            prepared.common.expected_current,
            || {},
        )
        .await?;
        prepared.transition_guard = Some(transition_guard);
        prepared.transition_fence = Some(transition_fence);
        if prepared.common.expected_current.origin() == AuthorityOrigin::Owner {
            let queue = self.runtime.core().capture_authority_snapshot().await;
            if !self.authority.is_current(prepared.common.expected_current) {
                return Err(DelegationErrorCode::CandidateCancelled);
            }
            let playback_state = self.runtime.core().get_playback_state();
            let queue_track_id = self
                .runtime
                .core()
                .current_track()
                .await
                .map(|track| track.id);
            if !self.authority.is_current(prepared.common.expected_current) {
                return Err(DelegationErrorCode::CandidateCancelled);
            }
            prepared.owner_snapshot = Some(OwnerSnapshot {
                queue,
                volume: playback_state.volume,
                playback: OwnerPlaybackSnapshot {
                    track_id: playback_state.track_id,
                    position_secs: playback_state.position,
                    was_playing: playback_state.is_playing,
                    had_loaded_audio: self.runtime.core().player().has_loaded_audio()
                        && playback_state.track_id != 0
                        && queue_track_id == Some(playback_state.track_id),
                },
            });
        }

        send_delegated_join(
            &prepared.common.app,
            prepared.session_id.as_str(),
            prepared.become_active,
            prepared.common.custom_name.as_deref(),
            prepared.common.volume_mode,
            current_quality(&self.quality_cap),
        )
        .await?;
        prepared
            .common
            .preflight
            .wait_for_activation(cancellation)
            .await?;
        if !self.authority.is_current(prepared.common.expected_current) {
            return Err(DelegationErrorCode::CandidateCancelled);
        }
        Ok(())
    }

    fn commit_delegation(
        &self,
        generation: u64,
        prepared: PreparedDelegation,
    ) -> Result<RetiredAuthority, CommitRejected<PreparedDelegation>> {
        let mut inner = lock_inner(&self.inner);
        if prepared.common.stamp.origin() != (AuthorityOrigin::Delegated { generation })
            || !self.authority.is_current(prepared.common.expected_current)
            || inner.runtime.as_ref().map(|runtime| runtime.stamp)
                != Some(prepared.common.expected_current)
            || prepared.transition_guard.is_none()
            || prepared.transition_fence.is_none()
        {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        }
        if prepared.common.expected_current.origin() == AuthorityOrigin::Owner
            && prepared.owner_snapshot.is_none()
        {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        }
        if !self.projection.install_authority(
            &self.authority,
            prepared.common.stamp,
            Some(prepared.session_id.as_str()),
        ) {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        }

        let retired = inner.runtime.take().expect("runtime stamp was checked");
        retired.event_loop.abort();
        let PreparedDelegation {
            common,
            session_id,
            become_active: _,
            owner_snapshot,
            transition_guard,
            transition_fence,
        } = prepared;
        let (runtime, start) = spawn_delegated_runtime(
            common,
            session_id,
            generation,
            Arc::clone(&self.inner),
            Arc::clone(&self.shared),
            Arc::clone(&self.authority),
            self.coordinator(),
            current_quality(&self.quality_cap),
        );
        inner.last_error = None;
        inner.last_pushed_queue_ids = None;
        let installed_stamp = runtime.stamp;
        inner.runtime = Some(runtime);
        drop(inner);

        if let Some(snapshot) = owner_snapshot {
            *recover_lock(&self.owner_snapshot) = Some(snapshot);
        }
        self.publish_origin(AuthorityOrigin::Delegated { generation });
        Ok(RetiredAuthority {
            runtime: Some(retired),
            activation: Some(PendingActivation {
                start: Some(start),
                runtime: Arc::clone(&self.runtime),
                authority: Arc::clone(&self.authority),
                stamp: installed_stamp,
                kind: PendingActivationKind::Delegated,
                transition_guard,
                transition_fence,
                owner_restore_task: Arc::clone(&self.owner_restore_task),
                owner_quality: current_quality(&self.quality_cap),
            }),
        })
    }

    async fn discard_delegation(&self, _generation: u64, prepared: PreparedDelegation) {
        Self::disconnect_common(prepared.common).await;
    }

    async fn prepare_owner(
        &self,
        _generation: u64,
        _reason: RestoreReason,
        cancellation: DelegationCancellation,
    ) -> Result<PreparedOwner, DelegationErrorCode> {
        let expected_current = self
            .authority
            .current()
            .filter(|stamp| matches!(stamp.origin(), AuthorityOrigin::Delegated { .. }))
            .ok_or(DelegationErrorCode::CommitRejected)?;
        let config = resolve_transport_config(&self.runtime)
            .await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        let stamp = self.authority.reserve(AuthorityOrigin::Owner);
        let volume_mode = self.volume_mode();
        let engine = DaemonRendererEngine::owner(
            Arc::clone(&self.runtime),
            volume_mode,
            Arc::clone(&self.quality_cap),
            Arc::clone(&self.authority),
            stamp,
        );
        let (app, sink, sync_state) = self.make_app(engine, stamp);
        let receiver = app.subscribe_transport_events();
        app.connect(config.clone())
            .await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        let custom_name = self.custom_device_name.read().await.clone();
        let mut common = PreparedCommon {
            app,
            sink,
            config,
            sync_state,
            preflight: DelegationPreflight::new(receiver),
            stamp,
            expected_current,
            volume_mode,
            custom_name,
        };
        common
            .preflight
            .wait_for_qws_ready(cancellation.clone())
            .await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        bootstrap_prepared_owner_presence(
            &common.app, common.custom_name.clone(), common.volume_mode,
        ).await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        common
            .preflight
            .wait_for_owner_session(cancellation)
            .await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        let (transition_guard, transition_fence) = acquire_transition_guard_and_fence(
            Arc::clone(&self.transition_gate),
            Arc::clone(&self.authority),
            expected_current,
            || {},
        )
        .await?;
        Ok(PreparedOwner {
            common,
            transition_guard,
            transition_fence,
        })
    }

    fn commit_owner(
        &self,
        _generation: u64,
        prepared: PreparedOwner,
    ) -> Result<RetiredAuthority, CommitRejected<PreparedOwner>> {
        let mut inner = lock_inner(&self.inner);
        if !self.authority.is_current(prepared.common.expected_current)
            || inner.runtime.as_ref().map(|runtime| runtime.stamp)
                != Some(prepared.common.expected_current)
        {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        }
        let stamp = prepared.common.stamp;
        let Some(owner_session_id) = prepared
            .common
            .preflight
            .confirmed_owner_session_id()
            .map(|session_id| Zeroizing::new(session_id.to_string()))
        else {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        };
        let Some(owner_snapshot) = recover_lock(&self.owner_snapshot).take() else {
            return Err(CommitRejected::new(
                prepared,
                DelegationErrorCode::CommitRejected,
            ));
        };
        let OwnerSnapshot {
            queue,
            volume,
            playback,
        } = owner_snapshot;
        let queue_state = match self
            .runtime
            .core()
            .try_commit_authority_snapshot(queue, || {
                self.projection.install_authority(
                    &self.authority,
                    stamp,
                    Some(owner_session_id.as_str()),
                )
            }) {
            Ok(state) => state,
            Err(queue) => {
                *recover_lock(&self.owner_snapshot) = Some(OwnerSnapshot {
                    queue,
                    volume,
                    playback,
                });
                return Err(CommitRejected::new(
                    prepared,
                    DelegationErrorCode::CommitRejected,
                ));
            }
        };
        let retired = inner.runtime.take().expect("runtime stamp was checked");
        retired.event_loop.abort();
        let PreparedOwner {
            common,
            transition_guard,
            transition_fence,
        } = prepared;
        let (runtime, start) = spawn_owner_runtime(
            common,
            Arc::clone(&self.inner),
            Arc::clone(&self.runtime),
            Arc::clone(&self.shared),
            Arc::clone(&self.authority),
            self.projection.clone(),
        );
        inner.last_error = None;
        inner.last_pushed_queue_ids = None;
        inner.runtime = Some(runtime);
        drop(inner);

        let bus = self
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.bus.clone());
        if let Some(bus) = bus {
            let _ = bus.send(CoreEvent::QueueUpdated { state: queue_state });
        }

        self.publish_origin(AuthorityOrigin::Owner);
        Ok(RetiredAuthority {
            runtime: Some(retired),
            activation: Some(PendingActivation {
                start: Some(start),
                runtime: Arc::clone(&self.runtime),
                authority: Arc::clone(&self.authority),
                stamp,
                kind: PendingActivationKind::Owner { volume, playback },
                transition_guard: Some(transition_guard),
                transition_fence: Some(transition_fence),
                owner_restore_task: Arc::clone(&self.owner_restore_task),
                owner_quality: current_quality(&self.quality_cap),
            }),
        })
    }

    async fn discard_owner(&self, _generation: u64, prepared: PreparedOwner) {
        Self::disconnect_common(prepared.common).await;
    }

    async fn retire_authority(&self, mut retired: RetiredAuthority) {
        if let Some(runtime) = retired.runtime.take() {
            self.stop_runtime(runtime).await;
        }

        if let Some(mut activation) = retired.activation.take() {
            activation.release();
        }
    }

    async fn shutdown_authority(&self) {
        // A restore from the preceding authority transition owns this gate
        // until it has either stabilized playback or failed. Waiting here is
        // intentional: cancelling that valid restore would lose the original
        // owner stream just because a later candidate/shutdown arrived.
        let transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        let shutdown_fence =
            OwnerActionFence::acquire_drained(Arc::clone(&self.authority), || {}).await;
        let previous = self.projection.clear_authority(&self.authority);
        let was_delegated = previous
            .is_some_and(|stamp| matches!(stamp.origin(), AuthorityOrigin::Delegated { .. }));
        let runtime = {
            let mut inner = lock_inner(&self.inner);
            inner.lifecycle_state = QconnectLifecycleState::Off;
            inner.last_pushed_queue_ids = None;
            inner.runtime.take()
        };
        if let Some(runtime) = runtime {
            runtime.event_loop.abort();
            self.stop_runtime(runtime).await;
        }
        let has_owner_snapshot = recover_lock(&self.owner_snapshot).is_some();
        let playback_restore = if (was_delegated || has_owner_snapshot)
            && self.shutdown_mode.load(Ordering::Acquire) == SHUTDOWN_RESTORE_OWNER
        {
            self.restore_owner_snapshot().await
        } else if was_delegated || has_owner_snapshot {
            let _ = self.runtime.core().stop();
            recover_lock(&self.owner_snapshot).take();
            None
        } else {
            // A pending candidate may have captured a snapshot, but disabling an
            // installed owner must not stop or replace local playback.
            recover_lock(&self.owner_snapshot).take();
            None
        };
        self.publish_origin(AuthorityOrigin::Owner);
        discard_completed_owner_restore_task(&self.owner_restore_task);
        let mut release =
            DeferredActivationRelease::new(None, Some(shutdown_fence), Some(transition_guard));
        if let Some(playback) = playback_restore {
            schedule_offline_owner_playback_restore(
                Arc::clone(&self.runtime),
                Arc::clone(&self.authority),
                playback,
                current_quality(&self.quality_cap),
                Arc::clone(&self.owner_restore_task),
                release,
            );
        } else {
            release.release();
        }
    }
}

fn recover_lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_owner_restore_task_id() -> u64 {
    NEXT_OWNER_RESTORE_TASK_ID
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .expect("owner playback restore task id space exhausted")
}

async fn wait_for_owner_restore_completion(task: &mut OwnerRestoreTask) -> OwnerRestoreResult {
    loop {
        if let Some(result) = *task.completion.borrow_and_update() {
            return result;
        }
        if task.completion.changed().await.is_err() {
            return (*task.completion.borrow())
                .unwrap_or(Err("owner-playback-restore-task-failed"));
        }
    }
}

fn clear_owner_restore_task_if(slot: &StdMutex<Option<OwnerRestoreTask>>, expected_id: u64) {
    let mut task = recover_lock(slot);
    if task.as_ref().map(|task| task.id) == Some(expected_id) {
        task.take();
    }
}

fn discard_completed_owner_restore_task(slot: &StdMutex<Option<OwnerRestoreTask>>) {
    let mut task = recover_lock(slot);
    if task
        .as_ref()
        .is_some_and(|task| task.mutation_finished.load(Ordering::Acquire))
    {
        task.take();
    }
}

/// Installs a restoration before its start barrier is opened. A live task can
/// only own this slot while it also owns `transition_gate`, so seeing one here
/// is an invariant failure. Preserve that valid older task and cancel only the
/// impossible overlapping newcomer.
fn install_owner_restore_task(
    slot: &StdMutex<Option<OwnerRestoreTask>>,
    task: OwnerRestoreTask,
) -> bool {
    let mut current = recover_lock(slot);
    if current
        .as_ref()
        .is_some_and(|current| !current.mutation_finished.load(Ordering::Acquire))
    {
        log::error!("[QConnect] overlapping owner playback restoration rejected");
        task.abort.abort();
        return false;
    }
    *current = Some(task);
    true
}

/// Spawn the mutating worker behind a start barrier and a non-cancellable
/// supervisor. The supervisor owns the transition release, joins the worker,
/// then publishes completion. Callers can therefore abandon their wait without
/// detaching either the mutation or its authority gates.
fn spawn_tracked_owner_restore<F>(
    restore: F,
    mut release: DeferredActivationRelease,
) -> (OwnerRestoreTask, oneshot::Sender<()>)
where
    F: Future<Output = OwnerRestoreResult> + Send + 'static,
{
    let id = next_owner_restore_task_id();
    let (start, started) = oneshot::channel();
    let worker = tokio::spawn(async move {
        if started.await.is_err() {
            return Err("owner-playback-restore-cancelled");
        }
        restore.await
    });
    let abort = worker.abort_handle();
    let mutation_finished = Arc::new(AtomicBool::new(false));
    let supervisor_finished = Arc::clone(&mutation_finished);
    let (completion_tx, completion) = watch::channel(None);
    tokio::spawn(async move {
        let result = match worker.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Err("owner-playback-restore-cancelled"),
            Err(_) => Err("owner-playback-restore-task-failed"),
        };

        // Mark the mutation finished before opening the transition mutex. A
        // newly admitted transition may run on another executor thread before
        // the watch publication below, but it can now replace this task slot
        // without aborting or losing a live restoration.
        supervisor_finished.store(true, Ordering::Release);
        release.release();
        completion_tx.send_replace(Some(result));
    });
    (
        OwnerRestoreTask {
            id,
            abort,
            mutation_finished,
            completion,
        },
        start,
    )
}

fn schedule_owner_playback_restore(
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    playback: OwnerPlaybackSnapshot,
    quality: Quality,
    slot: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    mut release: DeferredActivationRelease,
) {
    discard_completed_owner_restore_task(&slot);
    if !playback.had_loaded_audio {
        release.release();
        return;
    }
    let restore = async move {
        if !authority.is_current(stamp) {
            return Err("owner-playback-restore-authority-changed");
        }
        let restore = async {
            let current = runtime.core().current_track().await;
            if current.as_ref().map(|track| track.id) != Some(playback.track_id)
                || !authority.is_current(stamp)
            {
                return Err("restored owner queue does not match playback snapshot".to_string());
            }
            runtime
                .core()
                .play_track_resolved(
                    playback.track_id,
                    quality,
                    None,
                    None,
                    playback.position_secs,
                )
                .await?;
            if !authority.is_current(stamp) {
                return Err("owner playback authority changed".to_string());
            }
            if !playback.was_playing {
                runtime.core().pause().map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        };
        match tokio::time::timeout(Duration::from_secs(30), restore).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => {
                if authority.is_current(stamp) {
                    let _ = runtime.core().stop();
                }
                log::warn!("[QConnect] owner playback restoration failed");
                Err("owner-playback-restore-failed")
            }
            Err(_) => {
                if authority.is_current(stamp) {
                    let _ = runtime.core().stop();
                }
                log::warn!("[QConnect] owner playback restoration timed out");
                Err("owner-playback-restore-timed-out")
            }
        }
    };
    let (task, start) = spawn_tracked_owner_restore(restore, release);
    if install_owner_restore_task(&slot, task) {
        let _ = start.send(());
    }
}

fn schedule_offline_owner_playback_restore(
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    playback: OwnerPlaybackSnapshot,
    quality: Quality,
    slot: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    mut release: DeferredActivationRelease,
) {
    discard_completed_owner_restore_task(&slot);
    if !playback.had_loaded_audio {
        release.release();
        return;
    }
    let restore = async move {
        if authority.current().is_some() {
            return Err("owner-playback-restore-authority-changed");
        }
        let restore = async {
            let current = runtime.core().current_track().await;
            if current.as_ref().map(|track| track.id) != Some(playback.track_id)
                || authority.current().is_some()
            {
                return Err("restored owner queue does not match playback snapshot".to_string());
            }
            runtime
                .core()
                .play_track_resolved(
                    playback.track_id,
                    quality,
                    None,
                    None,
                    playback.position_secs,
                )
                .await?;
            if authority.current().is_some() {
                return Err("owner-playback-restore-authority-changed".to_string());
            }
            if !playback.was_playing {
                runtime.core().pause().map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        };
        match tokio::time::timeout(Duration::from_secs(30), restore).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => {
                if authority.current().is_none() {
                    let _ = runtime.core().stop();
                }
                log::warn!("[QConnect] offline owner playback restoration failed");
                Err("owner-playback-restore-failed")
            }
            Err(_) => {
                if authority.current().is_none() {
                    let _ = runtime.core().stop();
                }
                log::warn!("[QConnect] offline owner playback restoration timed out");
                Err("owner-playback-restore-timed-out")
            }
        }
    };
    let (task, start) = spawn_tracked_owner_restore(restore, release);
    if install_owner_restore_task(&slot, task) {
        let _ = start.send(());
    }
}

fn current_quality(quality: &StdMutex<Quality>) -> Quality {
    *recover_lock(quality)
}

async fn send_delegated_join(
    app: &Arc<DaemonQconnectApp>,
    session_id: &str,
    become_active: bool,
    custom_name: Option<&str>,
    volume_mode: VolumeMode,
    quality: Quality,
) -> Result<(), DelegationErrorCode> {
    let mut device_info = device_info_with_volume_mode(custom_name, volume_mode);
    if let Some(capabilities) = device_info.capabilities.as_mut() {
        capabilities.max_audio_quality = Some(max_audio_quality_from_quality(quality));
    }
    app.send_delegated_join(
        session_id,
        become_active,
        serde_json::to_value(device_info).unwrap_or_default(),
    )
    .await
    .map_err(|_| DelegationErrorCode::ActivationRejected)
}

#[allow(clippy::too_many_arguments)]
fn spawn_delegated_runtime(
    common: PreparedCommon,
    session_id: Zeroizing<String>,
    generation: u64,
    inner: Arc<StdMutex<DaemonQconnectInner>>,
    shared: SharedState,
    authority: Arc<AuthorityCell>,
    coordinator: Option<DaemonDelegationCoordinator>,
    quality: Quality,
) -> (DaemonQconnectRuntime, oneshot::Sender<()>) {
    let PreparedCommon {
        app,
        sink,
        config,
        sync_state,
        preflight,
        stamp,
        custom_name,
        ..
    } = common;
    let (receiver, buffered) = preflight.into_session_events();
    let (start, started) = oneshot::channel();
    let loop_app = Arc::clone(&app);
    let loop_sink = Arc::clone(&sink);
    let loop_sync = Arc::clone(&sync_state);
    let loop_authority = Arc::clone(&authority);
    let event_loop = tokio::spawn(async move {
        if started.await.is_err() || !loop_authority.is_current(stamp) {
            return;
        }
        loop_sink
            .on_event(QconnectAppEvent::TransportConnected)
            .await;
        run_delegated_loop(
            loop_app,
            loop_sink,
            loop_sync,
            receiver,
            buffered,
            session_id,
            generation,
            inner,
            shared,
            loop_authority,
            stamp,
            coordinator,
            custom_name,
            quality,
        )
        .await;
    });
    (
        DaemonQconnectRuntime {
            stamp,
            app,
            config,
            event_loop,
            sync_state,
        },
        start,
    )
}

fn spawn_owner_runtime(
    common: PreparedCommon,
    inner: Arc<StdMutex<DaemonQconnectInner>>,
    runtime: Runtime,
    shared: SharedState,
    authority: Arc<AuthorityCell>,
    projection: DaemonLanProjectionSlot,
) -> (DaemonQconnectRuntime, oneshot::Sender<()>) {
    let PreparedCommon {
        app,
        sink,
        config,
        sync_state,
        preflight,
        stamp,
        volume_mode,
        ..
    } = common;
    let (receiver, buffered) = preflight.into_session_events();
    let idle_retry_active = config.reconnect_idle_retry_ms > 0;
    let host: Arc<dyn SessionLoopHost> = Arc::new(DaemonSessionLoopHost {
        app: Arc::clone(&app),
        sync_state: Arc::clone(&sync_state),
        inner,
        authority: Arc::clone(&authority),
        stamp,
        sink,
        runtime,
        shared,
        volume_mode,
        projection,
    });
    let (start, started) = oneshot::channel();
    let loop_app = Arc::clone(&app);
    let loop_authority = Arc::clone(&authority);
    let event_loop = tokio::spawn(async move {
        if started.await.is_err() || !loop_authority.is_current(stamp) {
            return;
        }
        loop_app
            .run_session_loop_with_prefetched(host, receiver, idle_retry_active, buffered)
            .await;
    });
    (
        DaemonQconnectRuntime {
            stamp,
            app,
            config,
            event_loop,
            sync_state,
        },
        start,
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_delegated_loop(
    app: Arc<DaemonQconnectApp>,
    sink: Arc<DaemonEventSink>,
    _sync_state: Arc<AsyncMutex<QconnectRemoteSyncState>>,
    mut receiver: broadcast::Receiver<TransportEvent>,
    buffered: VecDeque<TransportEvent>,
    session_id: Zeroizing<String>,
    generation: u64,
    inner: Arc<StdMutex<DaemonQconnectInner>>,
    shared: SharedState,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    coordinator: Option<DaemonDelegationCoordinator>,
    custom_name: Option<String>,
    quality: Quality,
) {
    let mut reconnect_state = DelegatedRuntimeEventState::default();
    let mut rejoin_watchdog = DelegatedRejoinWatchdog::new();
    for event in buffered {
        if !handle_delegated_event(
            &app,
            &sink,
            event,
            &session_id,
            generation,
            &inner,
            &shared,
            &authority,
            stamp,
            coordinator.as_ref(),
            custom_name.as_deref(),
            quality,
            &mut reconnect_state,
            &mut rejoin_watchdog,
        )
        .await
        {
            return;
        }
    }
    loop {
        let event = match receiver.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                request_restore(coordinator.as_ref(), generation).await;
                return;
            }
            Err(broadcast::error::RecvError::Closed) => {
                request_restore(coordinator.as_ref(), generation).await;
                return;
            }
        };
        if !handle_delegated_event(
            &app,
            &sink,
            event,
            &session_id,
            generation,
            &inner,
            &shared,
            &authority,
            stamp,
            coordinator.as_ref(),
            custom_name.as_deref(),
            quality,
            &mut reconnect_state,
            &mut rejoin_watchdog,
        )
        .await
        {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_delegated_event(
    app: &Arc<DaemonQconnectApp>,
    sink: &Arc<DaemonEventSink>,
    event: TransportEvent,
    session_id: &str,
    generation: u64,
    inner: &Arc<StdMutex<DaemonQconnectInner>>,
    shared: &SharedState,
    authority: &Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    coordinator: Option<&DaemonDelegationCoordinator>,
    custom_name: Option<&str>,
    quality: Quality,
    reconnect_state: &mut DelegatedRuntimeEventState,
    rejoin_watchdog: &mut DelegatedRejoinWatchdog,
) -> bool {
    if !authority.is_current(stamp) {
        return false;
    }
    match reconnect_state.observe(&event) {
        DelegatedRuntimeEventDirective::Reconnecting => {
            rejoin_watchdog.cancel();
            update_lifecycle_state_if_running(
                inner,
                sink,
                shared,
                authority,
                stamp,
                QconnectLifecycleState::Reconnecting,
            )
            .await;
        }
        DelegatedRuntimeEventDirective::Rejoin => {
            if send_delegated_join(
                app, session_id, true, custom_name, sink.volume_mode(), quality,
            ).await
                .is_err()
            {
                request_restore(coordinator, generation).await;
                return false;
            }
            let coordinator = coordinator.cloned();
            rejoin_watchdog.arm(Arc::clone(authority), stamp, generation, move |generation| {
                async move { request_restore(coordinator.as_ref(), generation).await }
            });
        }
        DelegatedRuntimeEventDirective::Connected => {
            rejoin_watchdog.cancel();
            update_lifecycle_state_if_running(
                inner,
                sink,
                shared,
                authority,
                stamp,
                QconnectLifecycleState::Connected,
            )
            .await;
        }
        DelegatedRuntimeEventDirective::RestoreOwner => {
            request_restore(coordinator, generation).await;
            return false;
        }
        DelegatedRuntimeEventDirective::Forward => {}
    }
    if !authority.is_current(stamp) {
        return false;
    }
    if let Err(error) = app.handle_transport_event(event).await {
        if authority.is_current(stamp) {
            log::warn!("[QConnect] delegated event application failed: {error}");
            request_restore(coordinator, generation).await;
        }
        return false;
    }
    authority.is_current(stamp)
}

async fn request_restore(coordinator: Option<&DaemonDelegationCoordinator>, generation: u64) {
    if let Some(coordinator) = coordinator {
        let _ = coordinator
            .restore_owner_if_active(generation, RestoreReason::TransportFatal)
            .await;
    }
}
