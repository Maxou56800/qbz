//! Qt frontend host for the shared transactional QConnect delegation coordinator.
//!
//! Candidate API/QWS work is completed against isolated contexts while the
//! installed runtime remains authoritative. The synchronous commit methods do
//! only a short stamped runtime swap; teardown, queue restoration and cloud
//! bootstrap happen afterwards.
//!
//! Race-sensitive policy is deliberately shared by `qconnect-app`: authority
//! fences and deferred release, QWS preflight/join/rejoin, LAN projection and
//! physical LAN lifecycle. The code left here is an adapter boundary for Qt's
//! playback-task registry, UI projection and Cast ownership lane. Do not mirror
//! new policy into the qbzd adapter; extract it with behavioral tests in
//! `qconnect-app`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use qbz_app::shell::AppRuntime;
use qbz_core::{LoggingAdapter, QueueAuthoritySnapshot};
use qbz_models::Quality;
use qbz_qobuz::{DelegatedApiConfig, DelegatedApiEndpoint, DelegatedQobuzClient, QobuzClient};
use qconnect_app::{
    acquire_transition_guard_and_fence, max_audio_quality_from_quality, AuthorityCell,
    AuthorityOrigin, AuthorityStamp, CommitRejected, DeferredActivationRelease,
    DelegatedRejoinWatchdog, DelegatedRuntimeEventDirective, DelegatedRuntimeEventState,
    DelegationCancellation, DelegationCoordinator, DelegationErrorCode, DelegationHost,
    DelegationPreflight, OwnerActionFence, QconnectApp, QconnectAppEvent, QconnectEventSink,
    QconnectLifecycleState, QconnectRemoteSyncState, QconnectSessionState, RestoreReason,
    SessionLoopHost,
};
use qconnect_lan::{HandoffCandidate, LanProjection};
use qconnect_transport_ws::{NativeWsTransport, TransportEvent, WsTransportConfig};
use tokio::sync::{broadcast, oneshot, watch, Mutex as AsyncMutex, OwnedMutexGuard};
use zeroize::Zeroizing;

use crate::qconnect_engine_qt::QtRendererEngine;
use crate::qconnect_event_sink_qt::{QtQconnectApp, QtQconnectEventSink};
use crate::qconnect_lan_qt::QtLanProjectionSlot;
use crate::qconnect_qt::{
    bootstrap_prepared_owner_presence, lock_inner, QtQconnectInner, QtQconnectRuntime,
    QtSessionLoopHost,
};
use crate::qconnect_transport_qt::{
    default_qconnect_device_info_with_name, resolve_transport_config,
};

type Runtime = Arc<AppRuntime<LoggingAdapter>>;
pub type QtDelegationCoordinator = DelegationCoordinator<QtDelegationHost>;

const SHUTDOWN_RESTORE_OWNER: u8 = 0;
const SHUTDOWN_DISCARD_OWNER: u8 = 1;

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
/// `qconnect-app`; these concrete Qt app/sink handles cannot cross the crate
/// boundary without coupling the shared layer back to the frontend.
struct PreparedCommon {
    app: Arc<QtQconnectApp>,
    sink: Arc<QtQconnectEventSink>,
    config: WsTransportConfig,
    sync_state: Arc<AsyncMutex<QconnectRemoteSyncState>>,
    preflight: DelegationPreflight,
    stamp: AuthorityStamp,
    expected_current: AuthorityStamp,
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
}

/// A restore remains owned by the host until it publishes a terminal result.
/// Waiters clone only the watch receiver, so cancelling or timing out a waiter
/// never drops (and therefore never detaches) the underlying `JoinHandle`.
struct OwnerRestoreTask {
    handle: tokio::task::JoinHandle<()>,
    result: watch::Receiver<Option<Result<(), &'static str>>>,
}

impl OwnerRestoreTask {
    fn is_pending(&self) -> bool {
        self.result.borrow().is_none() && !self.handle.is_finished()
    }
}

fn observe_owner_restore(
    slot: &StdMutex<Option<OwnerRestoreTask>>,
) -> Option<watch::Receiver<Option<Result<(), &'static str>>>> {
    recover_lock(slot).as_ref().map(|task| task.result.clone())
}

impl Drop for OwnerRestoreTask {
    fn drop(&mut self) {
        // Host destruction or explicit replacement must not leave a restore
        // running without an owner. DeferredActivationRelease makes this abort
        // safe for an installed runtime.
        self.handle.abort();
    }
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
    runtime: Option<QtQconnectRuntime>,
    activation: Option<PendingActivation>,
}

/// Runtime-owned host. It contains no delegated credential material; prepared
/// candidates own and scrub that material until a successful stamped commit.
pub struct QtDelegationHost {
    runtime: Runtime,
    inner: Arc<StdMutex<QtQconnectInner>>,
    custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
    authority: Arc<AuthorityCell>,
    coordinator: OnceLock<QtDelegationCoordinator>,
    transition_gate: Arc<AsyncMutex<()>>,
    owner_snapshot: StdMutex<Option<OwnerSnapshot>>,
    owner_restore_task: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    projection: QtLanProjectionSlot,
    shutdown_mode: AtomicU8,
}

impl QtDelegationHost {
    /// Serialize local memory maintenance with authority handoffs. Retain both
    /// guards until restart completes, dropping the fence before the lane.
    pub(crate) async fn playback_memory_fence(
        &self,
    ) -> Result<(OwnedMutexGuard<()>, OwnerActionFence), String> {
        let lane = Arc::clone(&self.transition_gate).lock_owned().await;
        if self.authority.current().is_some_and(|s| s.origin() != AuthorityOrigin::Owner) {
            return Err("Playback memory can only be applied to local playback.".into());
        }
        let fence = OwnerActionFence::acquire_drained(
            Arc::clone(&self.authority),
            crate::playback_qt::cancel_owner_playback_tasks,
        ).await;
        Ok((lane, fence))
    }

    pub fn new(
        runtime: Runtime,
        inner: Arc<StdMutex<QtQconnectInner>>,
        custom_device_name: Arc<tokio::sync::RwLock<Option<String>>>,
        authority: Arc<AuthorityCell>,
    ) -> Self {
        Self {
            runtime,
            inner,
            custom_device_name,
            authority,
            coordinator: OnceLock::new(),
            transition_gate: Arc::new(AsyncMutex::new(())),
            owner_snapshot: StdMutex::new(None),
            owner_restore_task: Arc::new(StdMutex::new(None)),
            projection: QtLanProjectionSlot::default(),
            shutdown_mode: AtomicU8::new(SHUTDOWN_RESTORE_OWNER),
        }
    }

    pub fn install_coordinator(&self, coordinator: QtDelegationCoordinator) -> bool {
        self.coordinator.set(coordinator).is_ok()
    }

    pub fn attach_projection(&self, projection: LanProjection) {
        self.projection.attach(&self.authority, projection);
    }

    pub fn detach_projection(&self) {
        self.projection.detach();
    }

    pub(crate) fn projection_slot(&self) -> QtLanProjectionSlot {
        self.projection.clone()
    }

    pub(crate) fn current_projected_session_id(&self, stamp: AuthorityStamp) -> Option<String> {
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

    fn coordinator(&self) -> Option<QtDelegationCoordinator> {
        self.coordinator.get().cloned()
    }

    pub async fn await_owner_playback_restore(&self) -> Result<(), &'static str> {
        // Clone only the result channel. The host keeps owning the JoinHandle,
        // so cancellation of this future or expiry of this wait cannot detach
        // a rollback that still owns the transition gate/fence.
        let result = observe_owner_restore(&self.owner_restore_task);
        let Some(result) = result else {
            return Ok(());
        };
        match tokio::time::timeout(
            Duration::from_secs(31),
            wait_for_owner_restore_result(result),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err("owner-playback-restore-timed-out"),
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
        engine: QtRendererEngine,
        stamp: AuthorityStamp,
    ) -> (
        Arc<QtQconnectApp>,
        Arc<QtQconnectEventSink>,
        Arc<AsyncMutex<QconnectRemoteSyncState>>,
    ) {
        let transport = Arc::new(NativeWsTransport::new());
        let sync_state = Arc::new(AsyncMutex::new(QconnectRemoteSyncState::default()));
        let sink = Arc::new(QtQconnectEventSink::new(
            engine,
            Arc::clone(&self.runtime),
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

    async fn stop_runtime(&self, runtime: QtQconnectRuntime) {
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
        publish_restored_owner_ui(&self.runtime, &self.authority, None).await;
        recover_lock(&self.owner_snapshot).take();
        Some(snapshot.playback)
    }

    fn publish_origin(&self, origin: AuthorityOrigin) {
        let label = match origin {
            AuthorityOrigin::Owner => "owner",
            AuthorityOrigin::Delegated { .. } => "delegated",
        };
        crate::qconnect_qt::dev_push_event(format!("credential authority switched to {label}"));
    }
}

#[async_trait]
impl DelegationHost for QtDelegationHost {
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
        let engine = QtRendererEngine::delegated(
            Arc::clone(&self.runtime),
            Arc::clone(&delegated_client),
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
        // Fence every installed authority, including delegated -> delegated:
        // runtime action permits can span catalog/stream awaits and must drain
        // before cloud activation. Join is deliberately the final I/O before
        // the stamped swap; once SET_ACTIVE=true arrives, commit stays short.
        // The host gate carries this critical section through commit, retirement
        // and any owner playback rollback. A candidate therefore waits for a
        // valid prior rollback instead of cancelling it.
        let (transition_guard, transition_fence) = acquire_transition_guard_and_fence(
            Arc::clone(&self.transition_gate),
            Arc::clone(&self.authority),
            prepared.common.expected_current,
            crate::playback_qt::cancel_owner_playback_tasks,
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
            effective_delegated_quality(),
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
        crate::integrations_qt::cancel_owner_playback_tasks();

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
            Arc::clone(&self.authority),
            self.coordinator(),
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
        let engine = QtRendererEngine::owner(
            Arc::clone(&self.runtime),
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
            custom_name,
        };
        common
            .preflight
            .wait_for_qws_ready(cancellation.clone())
            .await
            .map_err(|_| DelegationErrorCode::OwnerRestoreFailed)?;
        bootstrap_prepared_owner_presence(&common.app, common.custom_name.clone())
            .await
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
            crate::playback_qt::cancel_owner_playback_tasks,
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
        match self
            .runtime
            .core()
            .try_commit_authority_snapshot(queue, || {
                self.projection.install_authority(
                    &self.authority,
                    stamp,
                    Some(owner_session_id.as_str()),
                )
            }) {
            Ok(_) => {}
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
        }
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
            Arc::clone(&self.authority),
            self.projection.clone(),
        );
        inner.last_error = None;
        inner.last_pushed_queue_ids = None;
        inner.runtime = Some(runtime);
        drop(inner);

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
            }),
        })
    }

    async fn discard_owner(&self, _generation: u64, prepared: PreparedOwner) {
        Self::disconnect_common(prepared.common).await;
    }

    async fn retire_authority(&self, mut retired: RetiredAuthority) {
        let entering_delegated = retired
            .activation
            .as_ref()
            .is_some_and(|activation| matches!(&activation.kind, PendingActivationKind::Delegated));
        if entering_delegated {
            crate::listen_log_qt::handoff().await;
        }
        if let Some(runtime) = retired.runtime.take() {
            self.stop_runtime(runtime).await;
        }

        if let Some(mut activation) = retired.activation.take() {
            activation.release();
        }
    }

    async fn shutdown_authority(&self) {
        // A rollback from the preceding authority transition owns this gate
        // until it stabilizes or fails. Do not abort that valid rollback merely
        // because disable/logout arrived: its queue snapshot was already
        // consumed at commit and cannot be reconstructed after cancellation.
        let transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        let shutdown_fence = OwnerActionFence::acquire_drained(
            Arc::clone(&self.authority),
            crate::playback_qt::cancel_owner_playback_tasks,
        )
        .await;
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
        if was_delegated || has_owner_snapshot {
            // No owner-side integration task may survive the delegated epoch.
            // Keep the authority fence closed until every task and listen-log
            // handoff has observed the transition back to local ownership.
            crate::integrations_qt::cancel_owner_playback_tasks();
            crate::listen_log_qt::handoff().await;
        }
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
        clear_qconnect_ui(&self.authority);
        if let Some(playback) = playback_restore {
            let release =
                DeferredActivationRelease::new(None, Some(shutdown_fence), Some(transition_guard));
            schedule_offline_owner_playback_restore(
                Arc::clone(&self.runtime),
                Arc::clone(&self.authority),
                playback,
                Arc::clone(&self.owner_restore_task),
                release,
            );
        } else {
            let mut release =
                DeferredActivationRelease::new(None, Some(shutdown_fence), Some(transition_guard));
            release.release();
        }
    }
}

fn recover_lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn wait_for_owner_restore_result(
    mut result: watch::Receiver<Option<Result<(), &'static str>>>,
) -> Result<(), &'static str> {
    loop {
        if let Some(result) = *result.borrow_and_update() {
            return result;
        }
        if result.changed().await.is_err() {
            return Err("owner-playback-restore-task-failed");
        }
    }
}

fn spawn_tracked_owner_restore<F>(
    slot: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    restore: F,
    mut release: DeferredActivationRelease,
) where
    F: std::future::Future<Output = Result<(), &'static str>> + Send + 'static,
{
    // The task cannot touch restore state before its JoinHandle and result
    // receiver are installed in the host. This also makes overlap rejection
    // cancellation-safe: dropping the unsignalled task releases its gates.
    let (start_tx, start_rx) = oneshot::channel();
    let (result_tx, result_rx) = watch::channel(None);
    let handle = tokio::spawn(async move {
        if start_rx.await.is_err() {
            return;
        }
        let result = restore.await;
        // Wake the installed loop before publishing completion, but retain the
        // host transition gate until the result is visible to every waiter.
        release.wake_runtime();
        result_tx.send_replace(Some(result));
        release.unlock_transition();
    });
    let mut task = Some(OwnerRestoreTask {
        handle,
        result: result_rx,
    });
    let should_start = {
        let mut restore_slot = recover_lock(&slot);
        if restore_slot
            .as_ref()
            .is_some_and(OwnerRestoreTask::is_pending)
        {
            false
        } else {
            *restore_slot = task.take();
            true
        }
    };
    if should_start {
        let _ = start_tx.send(());
    } else {
        // A live task here would violate the transition-gate invariant. Never
        // cancel that valid rollback to admit the newer one.
        log::error!("[QConnect] overlapping owner playback restoration refused");
    }
}

fn schedule_owner_playback_restore(
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    playback: OwnerPlaybackSnapshot,
    slot: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    release: DeferredActivationRelease,
) {
    let restore = async move {
        if !authority.is_current(stamp) {
            return Err("owner-playback-restore-authority-changed");
        }
        let restore = async {
            let playback_result = if playback.had_loaded_audio {
                crate::playback_qt::restore_owner_playback(
                    &runtime,
                    playback.track_id,
                    playback.position_secs,
                    playback.was_playing,
                )
                .await
            } else {
                Ok(())
            };
            if authority.is_current(stamp) {
                publish_restored_owner_ui(&runtime, &authority, Some(stamp)).await;
            }
            playback_result?;
            if !authority.is_current(stamp) {
                let _ = runtime.core().stop();
                return Err("owner playback authority changed during restore".to_string());
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
    spawn_tracked_owner_restore(slot, restore, release);
}

fn schedule_offline_owner_playback_restore(
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    playback: OwnerPlaybackSnapshot,
    slot: Arc<StdMutex<Option<OwnerRestoreTask>>>,
    release: DeferredActivationRelease,
) {
    let restore = async move {
        if authority.current().is_some() {
            return Err("owner-playback-restore-authority-changed");
        }
        if !playback.had_loaded_audio {
            return Ok(());
        }
        let restore = crate::playback_qt::restore_owner_playback(
            &runtime,
            playback.track_id,
            playback.position_secs,
            playback.was_playing,
        );
        match tokio::time::timeout(Duration::from_secs(30), restore).await {
            Ok(Ok(())) if authority.current().is_none() => Ok(()),
            Ok(Ok(())) => {
                let _ = runtime.core().stop();
                Err("owner-playback-restore-authority-changed")
            }
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
    spawn_tracked_owner_restore(slot, restore, release);
}

fn authority_matches(authority: &AuthorityCell, stamp: Option<AuthorityStamp>) -> bool {
    match stamp {
        Some(stamp) => authority.is_current(stamp),
        None => authority.current().is_none(),
    }
}

async fn publish_restored_owner_ui(
    runtime: &Runtime,
    authority: &AuthorityCell,
    stamp: Option<AuthorityStamp>,
) {
    if !authority_matches(authority, stamp) {
        return;
    }
    crate::now_playing::set_remote(false, "");
    crate::now_playing::set_remote_volume_locked(false);
    crate::playback_qt::refresh_now_playing(runtime).await;
    if !authority_matches(authority, stamp) {
        return;
    }
    crate::queue_qt::publish(runtime).await;
}

fn clear_qconnect_ui(authority: &AuthorityCell) {
    if authority.current().is_some() {
        return;
    }
    crate::qconnect_qt::publish::connected(false);
    crate::qconnect_qt::publish::devices(Vec::new());
    crate::qconnect_qt::publish::active_renderer_id(-1);
    crate::now_playing::set_remote(false, "");
    crate::now_playing::set_remote_volume_locked(false);
}

/// QConnect playback runs on this machine, so it obeys the same effective
/// local-output cap as every other local play funnel.
fn effective_delegated_quality() -> Quality {
    crate::playback_qt::local_playback_quality().0
}

async fn send_delegated_join(
    app: &Arc<QtQconnectApp>,
    session_id: &str,
    become_active: bool,
    custom_name: Option<&str>,
    quality: Quality,
) -> Result<(), DelegationErrorCode> {
    let mut device_info = default_qconnect_device_info_with_name(custom_name);
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

/// Apply a delegated-runtime lifecycle edge only while the exact stamped Qt
/// runtime is still installed. The sink performs the same stamp check around
/// its own asynchronous UI work; this short synchronous gate keeps the service
/// facade's lifecycle projection from being rewritten by a retired loop.
async fn update_lifecycle_state_if_running(
    inner: &Arc<StdMutex<QtQconnectInner>>,
    sink: &QtQconnectEventSink,
    authority: &AuthorityCell,
    stamp: AuthorityStamp,
    next: QconnectLifecycleState,
) {
    if !authority.is_current(stamp) {
        return;
    }
    {
        let mut guard = lock_inner(inner);
        if guard.runtime.as_ref().map(|runtime| runtime.stamp) != Some(stamp)
            || guard.lifecycle_state == next
        {
            return;
        }
        guard.lifecycle_state = next;
    }
    if !authority.is_current(stamp) {
        return;
    }
    sink.on_event(QconnectAppEvent::LifecycleChanged { state: next })
        .await;
}

fn spawn_delegated_runtime(
    common: PreparedCommon,
    session_id: Zeroizing<String>,
    generation: u64,
    inner: Arc<StdMutex<QtQconnectInner>>,
    authority: Arc<AuthorityCell>,
    coordinator: Option<QtDelegationCoordinator>,
) -> (QtQconnectRuntime, oneshot::Sender<()>) {
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
            loop_authority,
            stamp,
            coordinator,
            custom_name,
        )
        .await;
    });
    (
        QtQconnectRuntime {
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
    inner: Arc<StdMutex<QtQconnectInner>>,
    runtime: Runtime,
    authority: Arc<AuthorityCell>,
    projection: QtLanProjectionSlot,
) -> (QtQconnectRuntime, oneshot::Sender<()>) {
    let PreparedCommon {
        app,
        sink,
        config,
        sync_state,
        preflight,
        stamp,
        ..
    } = common;
    let (receiver, buffered) = preflight.into_session_events();
    let idle_retry_active = config.reconnect_idle_retry_ms > 0;
    let host: Arc<dyn SessionLoopHost> = Arc::new(QtSessionLoopHost {
        app: Arc::clone(&app),
        sync_state: Arc::clone(&sync_state),
        inner,
        authority: Arc::clone(&authority),
        stamp,
        sink,
        runtime,
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
        QtQconnectRuntime {
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
    app: Arc<QtQconnectApp>,
    sink: Arc<QtQconnectEventSink>,
    _sync_state: Arc<AsyncMutex<QconnectRemoteSyncState>>,
    mut receiver: broadcast::Receiver<TransportEvent>,
    buffered: VecDeque<TransportEvent>,
    session_id: Zeroizing<String>,
    generation: u64,
    inner: Arc<StdMutex<QtQconnectInner>>,
    authority: Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    coordinator: Option<QtDelegationCoordinator>,
    custom_name: Option<String>,
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
            &authority,
            stamp,
            coordinator.as_ref(),
            custom_name.as_deref(),
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
            &authority,
            stamp,
            coordinator.as_ref(),
            custom_name.as_deref(),
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
    app: &Arc<QtQconnectApp>,
    sink: &Arc<QtQconnectEventSink>,
    event: TransportEvent,
    session_id: &str,
    generation: u64,
    inner: &Arc<StdMutex<QtQconnectInner>>,
    authority: &Arc<AuthorityCell>,
    stamp: AuthorityStamp,
    coordinator: Option<&QtDelegationCoordinator>,
    custom_name: Option<&str>,
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
                authority,
                stamp,
                QconnectLifecycleState::Reconnecting,
            )
            .await;
        }
        DelegatedRuntimeEventDirective::Rejoin => {
            if send_delegated_join(
                app,
                session_id,
                true,
                custom_name,
                effective_delegated_quality(),
            )
            .await
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

async fn request_restore(coordinator: Option<&QtDelegationCoordinator>, generation: u64) {
    if let Some(coordinator) = coordinator {
        let _ = coordinator
            .restore_owner_if_active(generation, RestoreReason::TransportFatal)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_observer_leaves_the_tracked_worker_owned_by_its_slot() {
        let (_result_sender, result) = watch::channel(None);
        let handle = tokio::spawn(std::future::pending::<()>());
        let slot = StdMutex::new(Some(OwnerRestoreTask { handle, result }));

        let observer = observe_owner_restore(&slot).expect("restore observation");
        drop(observer);
        assert!(recover_lock(&slot)
            .as_ref()
            .is_some_and(OwnerRestoreTask::is_pending));
    }
}
