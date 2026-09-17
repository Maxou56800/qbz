//! Cast (Chromecast / DLNA) service — the Qt port of
//! `crates/qbz/src/cast_service.rs`, on top of the SHARED, frontend-agnostic
//! `qbz-cast` crate (ADR-006). Nothing about the protocols, the discovery or
//! the media server is reimplemented here: this module is the driver
//! (lifecycle + routing + state publishing), exactly like its Slint twin.
//!
//! Shape, 1:1 with the Slint service:
//!   - ONE process-wide singleton holding the two discovery handles, the
//!     ACTIVE connection (exactly one protocol at a time), one lazy shared
//!     `MediaServer` and a 1 s position poll;
//!   - discovery runs ONLY while the picker is open (`open_picker` /
//!     `close_picker`) — nothing scans in the background, nothing touches the
//!     network until the user asks;
//!   - casting BYPASSES the local audio backend entirely (the renderer's own
//!     DAC decodes the bytes we serve). The PROTECTED local playback path is
//!     never modified — it is only STOPPED while a renderer owns transport.
//!
//! Bytes + MIME are resolved through the shared core API
//! `fetch_for_external_stream_resolved` at the user's streaming-quality
//! preference clamped by the manual per-renderer cap (#638 fix 4). Caps govern
//! what we REQUEST only: bytes already in the L1/L2 cache go out as-is, never
//! resampled. The LOCAL output device's cap never applies here (the local DAC
//! is not in a cast's signal path — #638 precedence rule).
//!
//! SIZE (project rule): this file is one cohesive state machine — the inner
//! state, its lock discipline and the poll are not separable without leaking
//! `CastInner` across a module boundary. The Slint original is 1606 lines in
//! ONE file for the same reason; this is the trimmed port of it. The Qt
//! boundary (properties + invokables) IS split out, into `cast_bridge.rs`.
//!
//! QConnect coexistence (contract §11.4, 1:1 with cast_service.rs:280-294 /
//! :405-437 / :1140-1161): cast connect halts local playback, then SUSPENDS a
//! live QConnect session and proceeds only after that handoff is authority-safe;
//! cast disconnect restores it. The golden bar badge is republished around the
//! suspend/restore because the facade deliberately leaves badge flips to its
//! callers (the toggle / startup auto-connect / offline watcher pattern).
//!
//! DELIBERATE CUTS vs the Slint service, each named:
//!   - no offline-cache tier: the Qt port never brings up `OfflineCacheState`
//!     (`settings_qt/offline.rs` says so), so the resolver is called with
//!     `None` — cache -> network still works, a download-only track does not;
//!   - no Plex casting: the Slint has the same TODO (needs the Plex bytes
//!     resolver);
//!   - no CAST-side lyrics anchor feed: the Slint cast poll publishes its
//!     position into the lyrics remote anchor (cast_service.rs:1086-1097) so
//!     lyrics auto-follow while casting; here `lyrics_qt::position_ms` reads
//!     the local player while casting (the QConnect anchor, §11.1, gates on
//!     `now_playing::is_remote()`, which is false during a cast — see GLUE in
//!     the report). The cast-disconnect `clear_remote_anchor`
//!     (cast_service.rs:439-440) is likewise subsumed: the QConnect suspend
//!     already cleared the anchor through `now_playing::set_remote(false)`.
//! ADDITION (asked for): a renderer that goes silent mid-session is torn down
//! after `LOST_POLL_MAX` consecutive failed reads instead of leaving the UI
//! claiming a live connection forever.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use qbz_app::shell::AppRuntime;
use qbz_cast::{
    CastError, CastPositionInfo, ChromecastHandle, DeviceDiscovery, DiscoveredDevice,
    DiscoveredDlnaDevice, DlnaConnection, DlnaDiscovery, DlnaMetadata, DlnaPositionInfo,
    MediaMetadata, MediaServer, RangeSource,
};
use qbz_core::LoggingAdapter;
use qbz_models::{probe_streaminfo, AssetOrigin, AudioParams, Quality, QualityLimit, QueueTrack};
use qconnect_app::QconnectDisabledToken;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::cast_bridge;

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

/// Cast position poll cadence (Tauri's `POSITION_POLL_INTERVAL_MS`).
const POSITION_POLL_INTERVAL_MS: u64 = 1000;
/// Picker device-list poll cadence + scan-spinner window (mirror Tauri).
const DEVICE_POLL_INTERVAL_MS: u64 = 2000;
const SCAN_DURATION_MS: u64 = 15000;

/// How close (seconds) the renderer must get to a track's end before a DLNA
/// `STOPPED` counts as a genuine track end rather than a renderer hiccup.
const CAST_END_GUARD_SECS: f64 = 5.0;
/// Below this observed max position the renderer's RelTime is unreliable
/// (plenty never implement GetPositionInfo) and the near-end guard is skipped.
const CAST_POSITION_SIGNAL_MIN_SECS: f64 = 1.0;
/// A guard must never wedge the queue: honor a persistent STOPPED anyway.
const CAST_PREMATURE_STOP_POLLS_MAX: u32 = 4;
/// Consecutive polls a DLNA renderer may sit inside the end guard with a
/// frozen RelTime and no STOPPED before the track is treated as ended (#733:
/// a KEF LSX reached the end of every track and never advanced).
const CAST_END_STALL_POLLS_MAX: u32 = 5;

/// Per-track state of the end-stall rule (see `end_stall_tick`).
#[derive(Default, Clone, Copy, Debug)]
struct DlnaEndStall {
    last_position: f64,
    polls: u32,
}

/// One poll of the DLNA end-stall rule. Counts consecutive polls whose
/// RelTime did not move while the track is inside the end guard, ignoring a
/// user pause (`PAUSED_PLAYBACK` is a legitimate non-advancing state). Returns
/// true once the count reaches `CAST_END_STALL_POLLS_MAX`; any movement,
/// leaving the guard window, or a pause resets it.
fn end_stall_tick(stall: &mut DlnaEndStall, state: &str, position: f64, near_end: bool) -> bool {
    let frozen = (position - stall.last_position).abs() < 0.5;
    stall.last_position = position;
    if !near_end || !frozen || state == "PAUSED_PLAYBACK" {
        stall.polls = 0;
        return false;
    }
    stall.polls = stall.polls.saturating_add(1);
    stall.polls >= CAST_END_STALL_POLLS_MAX
}
/// Consecutive failed position reads before the session is declared lost.
const LOST_POLL_MAX: u32 = 5;
/// Minimum spacing between two volume commands sent to a renderer while a
/// slider is being dragged (one SOAP / Cast round trip each).
const VOLUME_COALESCE_MS: u64 = 120;
/// After logical detach, the complete media-lane cleanup and physical renderer
/// teardown may not hold its async caller longer than this budget. Waiting for
/// an earlier transition lane or the initial state detach is deliberately
/// outside this post-detach budget.
const BLOCKING_TEARDOWN_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);
const CHROMECAST_COMMAND_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
const CHROMECAST_CONNECT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
const DLNA_CONNECT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
const CAST_TRANSITION_CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const PHYSICAL_TEARDOWN_WAIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Protocol tag
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CastProtocol {
    Chromecast,
    Dlna,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CastConnectionStamp {
    connection_epoch: u64,
    transition_epoch: CastTransitionEpoch,
    protocol: CastProtocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CastMediaIntentStamp {
    connection: CastConnectionStamp,
    media_intent_epoch: u64,
    track_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CastTransportIntentStamp {
    media: CastMediaIntentStamp,
    transport_intent_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CastMediaCommandReceipt {
    transport: CastTransportIntentStamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CastPollSnapshot {
    connection: CastConnectionStamp,
    media: Option<CastMediaIntentStamp>,
    transport: Option<CastTransportIntentStamp>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PendingVolume {
    connection: CastConnectionStamp,
    value: f32,
}

struct StampedStreamCancel {
    media: CastMediaIntentStamp,
    sender: tokio::sync::watch::Sender<bool>,
}

/// Latest-wins identity for Cast/QConnect renderer handoffs. A newer Cast
/// connect/disconnect or an explicit QConnect start invalidates work that is
/// still waiting on the shared transition lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CastTransitionEpoch(u64);

impl CastProtocol {
    fn as_str(self) -> &'static str {
        match self {
            CastProtocol::Chromecast => "chromecast",
            CastProtocol::Dlna => "dlna",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "chromecast" => Some(CastProtocol::Chromecast),
            "dlna" => Some(CastProtocol::Dlna),
            _ => None,
        }
    }
}

/// What `register_*` learned about the asset it just registered (#638 fix 1):
/// the MIME, the measured STREAMINFO probe of the bytes actually served
/// (None = non-FLAC / local file), where they came from, and the tier the
/// resolver was asked for (None = source not governed by the preference).
struct CastAssetInfo {
    content_type: String,
    probe: Option<AudioParams>,
    origin: Option<AssetOrigin>,
    requested: Option<Quality>,
    /// Request-time cause paired with `requested` (#638 fix 4).
    request_cause: QualityLimit,
    /// The same bytes, for the visualizer's shadow decoder (None when the
    /// renderer is fed by proxy — nothing local to decode).
    shadow: Option<crate::cast_viz::ShadowSource>,
}

enum PendingRenderer {
    Chromecast(ChromecastHandle),
    Dlna(DlnaConnection),
}

struct PendingRendererConnection {
    renderer: PendingRenderer,
    device_ip: String,
    device_name: String,
    device_id: String,
    cap_key: Option<String>,
}

impl PendingRendererConnection {
    fn into_detached(self) -> DetachedRenderer {
        match self.renderer {
            PendingRenderer::Chromecast(handle) => DetachedRenderer {
                chromecast: Some(handle),
                ..DetachedRenderer::default()
            },
            PendingRenderer::Dlna(connection) => DetachedRenderer {
                dlna: Some(Arc::new(Mutex::new(connection))),
                ..DetachedRenderer::default()
            },
        }
    }
}

#[derive(Default)]
struct DetachedRenderer {
    chromecast: Option<ChromecastHandle>,
    dlna: Option<Arc<Mutex<DlnaConnection>>>,
}

#[derive(Default)]
struct BlockingTeardown {
    chromecast: Option<ChromecastHandle>,
    chromecast_discovery: Option<DeviceDiscovery>,
    dlna_discovery: Option<DlnaDiscovery>,
    media_server: Option<MediaServer>,
}

#[derive(Clone, Copy, Debug)]
struct RendererTeardownOutcome {
    restore_qconnect: Option<QconnectDisabledToken>,
    physical_safe: bool,
}

// ---------------------------------------------------------------------------
// Module singleton
// ---------------------------------------------------------------------------

static SERVICE: OnceLock<Arc<CastService>> = OnceLock::new();

/// The process-wide cast service. Constructed on first use — every entry
/// point is user-driven (a picker invokable) or lives on the playback path,
/// both of which run after `APP` is set by the boot sequence.
pub(crate) fn service() -> Arc<CastService> {
    SERVICE
        .get_or_init(|| Arc::new(CastService::new(crate::app())))
        .clone()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CastInner {
    // Discovery (alive only while the picker is open).
    chromecast_discovery: Option<DeviceDiscovery>,
    dlna_discovery: Option<DlnaDiscovery>,
    // Active connection (exactly one protocol at a time).
    chromecast: Option<ChromecastHandle>,
    dlna: Option<Arc<Mutex<DlnaConnection>>>,
    protocol: Option<CastProtocol>,
    connected_device_ip: Option<String>,
    connected_device_name: Option<String>,
    // Stable identity + the ui_prefs cap key derived from it (#638 fix 4).
    // `connected_cap_key` is None when no persistable identity exists (a
    // Chromecast without the mDNS TXT `id` record — a fullname-keyed cap
    // would silently detach on rename); the picker hides the row for it.
    connected_device_id: Option<String>,
    connected_cap_key: Option<String>,
    // Monotonic identity of the active renderer connection. Delayed work
    // captures this value so it cannot mutate a later cast session.
    connection_epoch: u64,
    // Transition intent that committed the active connection. Conditional
    // cleanup may claim a newer transition only from this exact predecessor;
    // it must never supersede a renderer/QConnect intent that arrived later.
    connection_transition_epoch: u64,
    // Latest-wins identity of a LOAD transaction. Track id alone is not an
    // identity: two rapid requests for the same queue row must still expire
    // the first resolver/registration continuation.
    media_intent_epoch: u64,
    media_intent_track_id: Option<u64>,
    // Latest-wins identity inside one committed media epoch. This separates
    // play/pause/seek and delayed poll/seek continuations on the same track.
    transport_intent_epoch: u64,
    // A transport intent is visible before it queues on the total media lane.
    // Polls are inadmissible until that exact command settles, so they cannot
    // attribute pre-command renderer state to the newly published epoch.
    transport_in_flight_epoch: Option<u64>,
    // ONE shared lazy media server for both protocols.
    media_server: Option<MediaServer>,
    // Playback mirror. `current_track_id` is session bookkeeping (which track
    // the renderer currently holds) kept 1:1 with the Slint service; nothing
    // reads it yet; it is transport session bookkeeping.
    current_track_id: Option<u64>,
    is_playing: bool,
    // Track-end one-shot latch (reset on PLAYING).
    track_end_detected: bool,
    // DLNA track-end guard state (all three reset per new track).
    cast_saw_playing: bool,
    cast_max_position: f64,
    cast_premature_stop_polls: u32,
    // DLNA end-stall rule state (reset per new track with the guard above).
    end_stall: DlnaEndStall,
    // Consecutive failed position reads (device-disappeared detection).
    lost_polls: u32,
    // Volume coalescer: the latest requested level and whether the single
    // sender task is alive. A slider drag hands over ~30 values/s; the
    // renderer gets the newest one every VOLUME_COALESCE_MS, last value
    // always delivered.
    pending_volume: Option<PendingVolume>,
    volume_worker_connection: Option<CastConnectionStamp>,
    // QConnect coexistence (§11.4): exact disabled intent created by this Cast
    // lifetime. A later user enable/disable invalidates it atomically.
    qconnect_restore_token: Option<QconnectDisabledToken>,
    // Cancel handle of the progressive Qobuz download feeding the media
    // server for the current cast track (None for cached / local / proxied
    // tracks). Fired when the track changes, on disconnect and on shutdown.
    stream_cancel: Option<StampedStreamCancel>,
    // Device-refresh task (2 s loop while the picker is open).
    discovery_task: Option<tokio::task::JoinHandle<()>>,
}

fn current_connection_stamp(inner: &CastInner) -> Option<CastConnectionStamp> {
    Some(CastConnectionStamp {
        connection_epoch: inner.connection_epoch,
        transition_epoch: CastTransitionEpoch(inner.connection_transition_epoch),
        protocol: inner.protocol?,
    })
}

fn connection_stamp_matches(inner: &CastInner, stamp: CastConnectionStamp) -> bool {
    current_connection_stamp(inner) == Some(stamp)
}

fn current_media_intent_stamp(inner: &CastInner) -> Option<CastMediaIntentStamp> {
    Some(CastMediaIntentStamp {
        connection: current_connection_stamp(inner)?,
        media_intent_epoch: inner.media_intent_epoch,
        track_id: inner.media_intent_track_id?,
    })
}

fn media_intent_stamp_matches(inner: &CastInner, stamp: CastMediaIntentStamp) -> bool {
    current_media_intent_stamp(inner) == Some(stamp)
}

fn committed_media_stamp(inner: &CastInner) -> Option<CastMediaIntentStamp> {
    let stamp = current_media_intent_stamp(inner)?;
    (inner.current_track_id == Some(stamp.track_id)).then_some(stamp)
}

fn current_transport_stamp(inner: &CastInner) -> Option<CastTransportIntentStamp> {
    Some(CastTransportIntentStamp {
        media: committed_media_stamp(inner)?,
        transport_intent_epoch: inner.transport_intent_epoch,
    })
}

fn transport_stamp_matches(inner: &CastInner, stamp: CastTransportIntentStamp) -> bool {
    current_transport_stamp(inner) == Some(stamp)
}

fn issue_transport_intent(
    inner: &mut CastInner,
    expected_media: CastMediaIntentStamp,
) -> Option<CastTransportIntentStamp> {
    if committed_media_stamp(inner) != Some(expected_media) {
        return None;
    }
    inner.transport_intent_epoch = inner.transport_intent_epoch.wrapping_add(1).max(1);
    inner.transport_in_flight_epoch = Some(inner.transport_intent_epoch);
    Some(CastTransportIntentStamp {
        media: expected_media,
        transport_intent_epoch: inner.transport_intent_epoch,
    })
}

/// Renew a delayed command only while the receipt captured at scheduling time
/// is still the exact transport head. The fresh epoch permanently expires any
/// poll snapshot captured before this command, even after the command settles.
fn issue_deferred_transport_intent(
    inner: &mut CastInner,
    expected: CastTransportIntentStamp,
) -> Option<CastTransportIntentStamp> {
    if current_transport_stamp(inner) != Some(expected) || inner.transport_in_flight_epoch.is_some()
    {
        return None;
    }
    issue_transport_intent(inner, expected.media)
}

fn activate_transport_intent(inner: &mut CastInner, stamp: CastTransportIntentStamp) -> bool {
    transport_stamp_matches(inner, stamp)
        && inner.transport_in_flight_epoch == Some(stamp.transport_intent_epoch)
}

fn settle_transport_intent(
    inner: &mut CastInner,
    stamp: CastTransportIntentStamp,
    renderer_replied: bool,
) -> bool {
    if !transport_stamp_matches(inner, stamp)
        || inner.transport_in_flight_epoch != Some(stamp.transport_intent_epoch)
    {
        return false;
    }
    inner.transport_in_flight_epoch = None;
    // A successful receiver response is also a liveness observation. Do not
    // let failures accumulated before a working seek/play/pause make the next
    // poll declare this same session lost immediately.
    if renderer_replied {
        inner.lost_polls = 0;
    }
    true
}

fn invalidate_media_intent(inner: &mut CastInner) {
    inner.media_intent_epoch = inner.media_intent_epoch.wrapping_add(1).max(1);
    inner.transport_intent_epoch = inner.transport_intent_epoch.wrapping_add(1).max(1);
    inner.transport_in_flight_epoch = None;
    inner.media_intent_track_id = None;
    inner.current_track_id = None;
}

fn volume_worker_matches(inner: &CastInner, stamp: CastConnectionStamp) -> bool {
    inner.volume_worker_connection == Some(stamp)
}

fn current_poll_snapshot(
    inner: &CastInner,
    connection: CastConnectionStamp,
) -> Option<CastPollSnapshot> {
    if !connection_stamp_matches(inner, connection) {
        return None;
    }
    let media = committed_media_stamp(inner);
    if inner.media_intent_track_id.is_some() && media.is_none() {
        return None;
    }
    if inner.transport_in_flight_epoch.is_some() {
        return None;
    }
    Some(CastPollSnapshot {
        connection,
        media,
        transport: current_transport_stamp(inner),
    })
}

fn poll_snapshot_matches(inner: &CastInner, snapshot: CastPollSnapshot) -> bool {
    current_poll_snapshot(inner, snapshot.connection) == Some(snapshot)
}

fn lost_poll_snapshot_matches(inner: &CastInner, snapshot: CastPollSnapshot) -> bool {
    poll_snapshot_matches(inner, snapshot) && inner.lost_polls >= LOST_POLL_MAX
}

pub(crate) struct CastService {
    inner: Arc<Mutex<CastInner>>,
    runtime: Runtime,
    // One process-wide renderer handoff lane. Cast holds it from before it
    // suspends QConnect through renderer commit; QConnect receives an owned
    // lease and keeps it through its initial owner-runtime commit.
    transition_gate: Arc<Mutex<()>>,
    // Total order for media replacement. The guard lives from cancellation of
    // the previous source through resolve, registry mutation, renderer LOAD,
    // committed state and UI publication. New intents publish their epoch
    // before waiting here, so a slow predecessor can observe supersession.
    media_command_gate: Arc<Mutex<()>>,
    transition_epoch: AtomicU64,
    // Latest normal transition that actually acquired the total lane. When it
    // trails `transition_epoch`, a newer user/QConnect intent is queued and an
    // autonomous poll cleanup may help teardown but must not supersede it.
    transition_lane_epoch: AtomicU64,
    /// A logical detach is not enough to admit another renderer when the
    /// physical Stop/Disconnect worker timed out. Pending teardown workers keep
    /// this fence raised; a terminal failure keeps `teardown_unsafe` raised.
    teardown_pending: AtomicU64,
    teardown_unsafe: AtomicBool,
    // Position-poll task. OUTSIDE the async `inner` lock on purpose so terminal
    // teardown can abort it before touching connection state. DLNA transport
    // lives behind its own Arc<Mutex<_>>; no network await retains `inner`.
    poll_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub(crate) struct CastQconnectTransitionLease {
    service: Arc<CastService>,
    epoch: CastTransitionEpoch,
    _guard: OwnedMutexGuard<()>,
}

pub(crate) struct CastQconnectTransitionReceipt {
    service: Arc<CastService>,
    epoch: CastTransitionEpoch,
}

impl CastQconnectTransitionLease {
    pub(crate) fn is_current(&self) -> bool {
        self.service.transition_is_current(self.epoch)
    }

    /// Observe a newer Cast/QConnect renderer intent without releasing the
    /// owned transition guard. QConnect selects this alongside pre-install
    /// network/lifecycle awaits so a newer Cast request does not sit behind a
    /// stale handoff indefinitely.
    pub(crate) async fn cancelled(&self) {
        while self.is_current() {
            tokio::time::sleep(CAST_TRANSITION_CANCEL_POLL).await;
        }
    }

    pub(crate) async fn commit_qconnect_started(
        &self,
        expected_restore: Option<QconnectDisabledToken>,
    ) {
        let mut inner = self.service.inner.lock().await;
        let token_matches = expected_restore
            .map(|expected| inner.qconnect_restore_token == Some(expected))
            .unwrap_or(true);
        if self.is_current() && self.service.physical_teardown_safe() && token_matches {
            inner.qconnect_restore_token = None;
        }
    }

    /// Release the Cast transition lane after the initial owner runtime commit,
    /// but retain the exact epoch needed to clear a carried restore latch only
    /// when the complete QConnect startup eventually succeeds.
    pub(crate) fn release(self) -> CastQconnectTransitionReceipt {
        let Self {
            service,
            epoch,
            _guard,
        } = self;
        drop(_guard);
        CastQconnectTransitionReceipt { service, epoch }
    }

    /// The gate makes this check stable through QConnect's following
    /// synchronous owner commit: no Cast connect can enter meanwhile.
    pub(crate) async fn revalidate_no_cast(&self) -> Result<(), String> {
        if !self.is_current() {
            Err("Qobuz Connect start was superseded by a newer Cast request".to_string())
        } else if !self.service.physical_teardown_safe() {
            Err("A previous cast renderer teardown is still incomplete".to_string())
        } else if self.service.is_casting().await {
            Err("Cannot start Qobuz Connect while a cast renderer is active".to_string())
        } else {
            Ok(())
        }
    }
}

impl CastQconnectTransitionReceipt {
    pub(crate) async fn commit_qconnect_started(
        self,
        expected_restore: Option<QconnectDisabledToken>,
    ) {
        let mut inner = self.service.inner.lock().await;
        let token_matches = expected_restore
            .map(|expected| inner.qconnect_restore_token == Some(expected))
            .unwrap_or(true);
        if self.service.transition_is_current(self.epoch)
            && self.service.physical_teardown_safe()
            && token_matches
        {
            inner.qconnect_restore_token = None;
        }
    }
}

impl CastService {
    fn new(runtime: Runtime) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CastInner::default())),
            runtime,
            transition_gate: Arc::new(Mutex::new(())),
            media_command_gate: Arc::new(Mutex::new(())),
            transition_epoch: AtomicU64::new(0),
            transition_lane_epoch: AtomicU64::new(0),
            teardown_pending: AtomicU64::new(0),
            teardown_unsafe: AtomicBool::new(false),
            poll_task: std::sync::Mutex::new(None),
        }
    }

    fn begin_transition_intent(&self) -> CastTransitionEpoch {
        let epoch = self
            .transition_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.wrapping_add(1).max(1))
            })
            .unwrap_or_else(|current| current)
            .wrapping_add(1)
            .max(1);
        CastTransitionEpoch(epoch)
    }

    /// Claim a conditional cleanup transition only when no newer renderer or
    /// QConnect intent has appeared since `expected` committed. Unlike an
    /// unconditional fetch-add, this cannot make stale poll work latest.
    fn begin_transition_intent_if_current(
        &self,
        expected: CastTransitionEpoch,
    ) -> Option<CastTransitionEpoch> {
        let next = expected.0.wrapping_add(1).max(1);
        self.transition_epoch
            .compare_exchange(expected.0, next, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| CastTransitionEpoch(next))
    }

    fn transition_is_current(&self, epoch: CastTransitionEpoch) -> bool {
        self.transition_epoch.load(Ordering::Acquire) == epoch.0
    }

    fn current_transition_epoch(&self) -> CastTransitionEpoch {
        CastTransitionEpoch(self.transition_epoch.load(Ordering::Acquire))
    }

    fn mark_transition_lane_if_current(&self, epoch: CastTransitionEpoch) {
        if self.transition_is_current(epoch) {
            self.transition_lane_epoch.store(epoch.0, Ordering::Release);
        }
    }

    fn transition_lane_is_caught_up(&self, epoch: CastTransitionEpoch) -> bool {
        self.transition_lane_epoch.load(Ordering::Acquire) == epoch.0
    }

    fn physical_teardown_safe(&self) -> bool {
        self.teardown_pending.load(Ordering::Acquire) == 0
            && !self.teardown_unsafe.load(Ordering::Acquire)
    }

    fn finish_physical_teardown(&self, safe: bool) {
        if !safe {
            self.teardown_unsafe.store(true, Ordering::Release);
        }
        self.teardown_pending.fetch_sub(1, Ordering::AcqRel);
    }

    /// Let a newer renderer intent reuse the same request after a superseded
    /// provisional worker finishes. The exact epoch check keeps the wait
    /// latest-wins, while the deadline keeps a genuinely stuck physical worker
    /// fail-closed instead of blocking the shared transition lane forever.
    async fn await_physical_teardown(&self, transition_epoch: CastTransitionEpoch) -> bool {
        if self.physical_teardown_safe() {
            return self.transition_is_current(transition_epoch);
        }
        let settled = tokio::time::timeout(PHYSICAL_TEARDOWN_WAIT_BUDGET, async {
            loop {
                if !self.transition_is_current(transition_epoch)
                    || self.teardown_unsafe.load(Ordering::Acquire)
                {
                    return false;
                }
                if self.teardown_pending.load(Ordering::Acquire) == 0 {
                    return true;
                }
                tokio::time::sleep(CAST_TRANSITION_CANCEL_POLL).await;
            }
        })
        .await
        .unwrap_or(false);
        settled && self.transition_is_current(transition_epoch) && self.physical_teardown_safe()
    }

    /// Publish a media request before waiting on the media lane. This is the
    /// cancellation edge for a resolver/LOAD already in flight, including a
    /// second request for the same `track_id`.
    async fn begin_media_intent(
        &self,
        track_id: u64,
        expected_connection: Option<CastConnectionStamp>,
    ) -> Result<Option<(CastMediaIntentStamp, bool)>, String> {
        let mut inner = self.inner.lock().await;
        let connection =
            current_connection_stamp(&inner).ok_or_else(|| "Not connected".to_string())?;
        if expected_connection.is_some_and(|expected| expected != connection) {
            return Ok(None);
        }
        let replaced_loaded_media = inner.current_track_id.is_some();
        inner.media_intent_epoch = inner.media_intent_epoch.wrapping_add(1).max(1);
        inner.transport_intent_epoch = inner.transport_intent_epoch.wrapping_add(1).max(1);
        inner.transport_in_flight_epoch = None;
        inner.media_intent_track_id = Some(track_id);
        // A prior renderer item is not the committed state of this new intent.
        // Keeping it here lets an old poll/control continuation claim the new
        // request before its LOAD has committed.
        inner.current_track_id = None;
        inner.is_playing = false;
        inner.track_end_detected = false;
        Ok(Some((
            CastMediaIntentStamp {
                connection,
                media_intent_epoch: inner.media_intent_epoch,
                track_id,
            },
            replaced_loaded_media,
        )))
    }

    async fn media_intent_is_current(&self, stamp: CastMediaIntentStamp) -> bool {
        media_intent_stamp_matches(&*self.inner.lock().await, stamp)
    }

    async fn activate_transport_command(&self, stamp: CastTransportIntentStamp) -> bool {
        activate_transport_intent(&mut *self.inner.lock().await, stamp)
    }

    async fn issue_deferred_transport_command(
        &self,
        expected: CastTransportIntentStamp,
    ) -> Option<CastTransportIntentStamp> {
        issue_deferred_transport_intent(&mut *self.inner.lock().await, expected)
    }

    /// Settle only the exact command that owns the in-flight marker. A stale T1
    /// completion cannot reopen polling while T2 is still queued or executing.
    async fn settle_transport_command(
        &self,
        stamp: CastTransportIntentStamp,
        renderer_replied: bool,
    ) -> bool {
        settle_transport_intent(&mut *self.inner.lock().await, stamp, renderer_replied)
    }

    /// First mutation in the total media lane: cancel and unregister the
    /// previous source only if this exact request still owns the connection.
    async fn prepare_media_command(&self, stamp: CastMediaIntentStamp) -> bool {
        let cancel = {
            let mut inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, stamp) {
                return false;
            }
            let cancel = inner.stream_cancel.take().map(|cancel| cancel.sender);
            if let Some(server) = inner.media_server.as_ref() {
                server.clear_entries();
            }
            cancel
        };
        crate::cast_viz::stop();
        if let Some(cancel) = cancel {
            let _ = cancel.send(true);
        }
        true
    }

    /// Roll back only registry/cancel state installed by this media intent.
    /// The caller owns `media_command_gate`, so no successor can have
    /// registered its same-id route while this cleanup runs.
    async fn discard_media_attempt(&self, stamp: CastMediaIntentStamp) {
        let cancel = {
            let mut inner = self.inner.lock().await;
            let cancel = match inner.stream_cancel.as_ref() {
                Some(cancel) if cancel.media == stamp => {
                    inner.stream_cancel.take().map(|cancel| cancel.sender)
                }
                _ => None,
            };
            if let Some(server) = inner.media_server.as_ref() {
                server.clear_entries();
            }
            cancel
        };
        if let Some(cancel) = cancel {
            let _ = cancel.send(true);
        }
    }

    async fn retire_failed_media_intent(&self, stamp: CastMediaIntentStamp, error: &str) {
        let mut inner = self.inner.lock().await;
        if media_intent_stamp_matches(&inner, stamp) {
            // Error publication is part of the exact media transaction. A T2
            // intent cannot interleave and inherit T1's failure line.
            set_error(error.to_string());
            invalidate_media_intent(&mut inner);
            inner.is_playing = false;
            inner.track_end_detected = false;
            Self::publish_connection_state_locked(&inner);
        }
    }

    async fn stop_loaded_media_if_connection(&self, stamp: CastConnectionStamp) {
        match stamp.protocol {
            CastProtocol::Chromecast => {
                let handle = {
                    let inner = self.inner.lock().await;
                    if !connection_stamp_matches(&inner, stamp) {
                        return;
                    }
                    inner.chromecast.clone()
                };
                if let Some(handle) = handle {
                    let _ = Self::run_chromecast_call(
                        handle,
                        CHROMECAST_COMMAND_BUDGET,
                        "chromecast-stale-load-stop",
                        |handle| handle.stop(),
                    )
                    .await;
                }
            }
            CastProtocol::Dlna => {
                let connection = {
                    let inner = self.inner.lock().await;
                    connection_stamp_matches(&inner, stamp)
                        .then(|| inner.dlna.clone())
                        .flatten()
                };
                if let Some(connection) = connection {
                    let mut connection = connection.lock().await;
                    if connection_stamp_matches(&*self.inner.lock().await, stamp) {
                        let _ = connection.stop().await;
                    }
                }
            }
        }
    }

    /// Abort the position poll synchronously (no lock on `inner` needed).
    fn abort_poll(&self) {
        if let Some(task) = self.poll_task.lock().ok().and_then(|mut p| p.take()) {
            task.abort();
        }
    }

    async fn run_chromecast_call<T, F>(
        handle: ChromecastHandle,
        budget: std::time::Duration,
        operation: &'static str,
        call: F,
    ) -> Result<T, CastError>
    where
        T: Send + 'static,
        F: FnOnce(ChromecastHandle) -> Result<T, CastError> + Send + 'static,
    {
        let timeout_fence = handle.clone();
        let task = tokio::task::spawn_blocking(move || call(handle));
        match tokio::time::timeout(budget, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                timeout_fence.invalidate();
                Err(CastError::Connection(
                    "Chromecast blocking worker unavailable".to_string(),
                ))
            }
            Err(_) => {
                timeout_fence.invalidate();
                Err(CastError::Timeout(operation.to_string()))
            }
        }
    }

    /// A failed provisional connect never loaded media, so disconnecting its
    /// control channel is sufficient. This is deliberately separate from the
    /// active-renderer teardown, where an unconfirmed Stop must fail closed.
    async fn disconnect_failed_chromecast_connect(handle: ChromecastHandle) -> bool {
        let task = tokio::task::spawn_blocking(move || handle.disconnect());
        matches!(
            tokio::time::timeout(BLOCKING_TEARDOWN_BUDGET, task).await,
            Ok(Ok(Ok(())))
        )
    }

    /// Retain both the blocking connect and a usable handle after the async
    /// caller's budget/epoch expires. QConnect stays fenced until the worker
    /// reaches a terminal result and any connection it created has received a
    /// confirmed Stop + Disconnect.
    fn supervise_late_chromecast_connect(
        self: &Arc<Self>,
        task: tokio::task::JoinHandle<Result<(), CastError>>,
        handle: ChromecastHandle,
        transition_epoch: CastTransitionEpoch,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let (connected, worker_safe) = match task.await {
                Ok(Ok(())) => (true, true),
                Ok(Err(error)) => {
                    log::warn!(
                        "[qbz-qt][Cast] late Chromecast connect ended with an error: {error}"
                    );
                    (false, true)
                }
                Err(error) => {
                    log::warn!("[qbz-qt][Cast] late Chromecast connect worker failed: {error}");
                    (false, false)
                }
            };
            let cleanup_safe = if connected {
                service
                    .teardown_detached_renderer(DetachedRenderer {
                        chromecast: Some(handle),
                        ..DetachedRenderer::default()
                    })
                    .await
            } else {
                Self::disconnect_failed_chromecast_connect(handle).await
            };
            let physical_safe = worker_safe && cleanup_safe;
            service.finish_physical_teardown(physical_safe);
            if physical_safe {
                service.restore_latched_qconnect(transition_epoch).await;
            }
        });
    }

    /// Bound the caller's wait from before the total media lane through
    /// physical renderer teardown without abandoning work that already owns
    /// renderer handles. A timed-out task remains supervised and keeps the
    /// physical fence raised until its exact continuation reaches a terminal
    /// result.
    async fn run_media_teardown_bounded<F>(self: &Arc<Self>, label: &'static str, future: F) -> bool
    where
        F: std::future::Future<Output = bool> + Send + 'static,
    {
        self.teardown_pending.fetch_add(1, Ordering::AcqRel);
        let mut task = tokio::spawn(future);
        match tokio::time::timeout(BLOCKING_TEARDOWN_BUDGET, &mut task).await {
            Ok(Ok(reported_safe)) => {
                // This is only the outer media-lane fence. An inner physical
                // timeout owns its own pending worker and may still recover,
                // so `reported_safe == false` must not latch unsafe here.
                self.finish_physical_teardown(true);
                reported_safe && self.physical_teardown_safe()
            }
            Ok(Err(error)) => {
                log::warn!("[qbz-qt][Cast] {label} teardown worker failed: {error}");
                self.finish_physical_teardown(false);
                false
            }
            Err(_) => {
                log::warn!("[qbz-qt][Cast] {label} teardown exceeded its caller time budget");
                let service = Arc::clone(self);
                tokio::spawn(async move {
                    match task.await {
                        Ok(_reported_safe) => service.finish_physical_teardown(true),
                        Err(error) => {
                            log::warn!(
                                "[qbz-qt][Cast] late {label} teardown worker failed: {error}"
                            );
                            service.finish_physical_teardown(false);
                        }
                    }
                });
                false
            }
        }
    }

    async fn run_blocking_teardown(self: &Arc<Self>, mut resources: BlockingTeardown) -> bool {
        let fences_renderer = resources.chromecast.is_some();
        let chromecast_fence = resources.chromecast.clone();
        if fences_renderer {
            self.teardown_pending.fetch_add(1, Ordering::AcqRel);
        }
        let mut task = tokio::task::spawn_blocking(move || {
            let mut renderer_safe = true;
            if let Some(handle) = resources.chromecast.take() {
                // Stop before disconnect: disconnect alone can leave the
                // receiver playing. Both replies are independently bounded by
                // qbz-cast; this outer budget also covers worker scheduling.
                match handle.stop() {
                    Ok(()) | Err(CastError::NoMediaSession) => {}
                    Err(error) => {
                        renderer_safe = false;
                        log::warn!("[qbz-qt][Cast] Chromecast Stop was not confirmed: {error}");
                    }
                }
                if let Err(error) = handle.disconnect() {
                    renderer_safe = false;
                    log::warn!("[qbz-qt][Cast] Chromecast disconnect was not confirmed: {error}");
                }
            }
            if let Some(mut discovery) = resources.chromecast_discovery.take() {
                let _ = discovery.stop_discovery();
            }
            if let Some(mut discovery) = resources.dlna_discovery.take() {
                let _ = discovery.stop_discovery();
            }
            if let Some(mut server) = resources.media_server.take() {
                server.stop();
            }
            renderer_safe
        });

        match tokio::time::timeout(BLOCKING_TEARDOWN_BUDGET, &mut task).await {
            Ok(Ok(safe)) => {
                if fences_renderer {
                    self.finish_physical_teardown(safe);
                }
                safe
            }
            Ok(Err(_)) => {
                if let Some(handle) = chromecast_fence {
                    handle.invalidate();
                }
                log::warn!("[qbz-qt][Cast] blocking teardown worker failed");
                if fences_renderer {
                    self.finish_physical_teardown(false);
                }
                false
            }
            Err(_) => {
                // The blocking task cannot be force-cancelled. Remove its
                // command capability before detaching it so neither that task
                // nor another clone can enqueue after this timeout.
                if let Some(handle) = chromecast_fence {
                    handle.invalidate();
                }
                log::warn!("[qbz-qt][Cast] blocking teardown exceeded its time budget");
                // Keep QConnect fenced until the physical worker actually
                // finishes. A successful late completion clears only this
                // pending fence; a failed completion latches the unsafe state.
                if fences_renderer {
                    let service = Arc::clone(self);
                    tokio::spawn(async move {
                        let safe = matches!(task.await, Ok(true));
                        service.finish_physical_teardown(safe);
                    });
                }
                false
            }
        }
    }

    async fn teardown_detached_renderer(self: &Arc<Self>, mut renderer: DetachedRenderer) -> bool {
        let blocking = self.run_blocking_teardown(BlockingTeardown {
            chromecast: renderer.chromecast.take(),
            ..BlockingTeardown::default()
        });
        let dlna = async move {
            if let Some(connection) = renderer.dlna.take() {
                match tokio::time::timeout(BLOCKING_TEARDOWN_BUDGET, async {
                    let mut connection = connection.lock().await;
                    let stopped = connection.stop().await.is_ok();
                    let disconnected = connection.disconnect().is_ok();
                    stopped && disconnected
                })
                .await
                {
                    Ok(safe) => safe,
                    Err(_) => {
                        log::warn!("[qbz-qt][Cast] DLNA teardown exceeded its time budget");
                        false
                    }
                }
            } else {
                true
            }
        };
        let (blocking_safe, dlna_safe) = tokio::join!(blocking, dlna);
        if !dlna_safe {
            self.teardown_unsafe.store(true, Ordering::Release);
        }
        blocking_safe && dlna_safe
    }

    async fn qconnect_is_running() -> bool {
        match crate::qconnect_qt::service() {
            Some(service) => service.is_running().await,
            None => false,
        }
    }

    /// True while a renderer is connected and owns transport.
    pub(crate) async fn is_casting(&self) -> bool {
        self.inner.lock().await.protocol.is_some()
    }

    /// The tier the next connected-renderer request would use: the raw
    /// streaming preference clamped by that renderer's manual cap, never by
    /// the local DAC. Immediate queue warming consumes this because the cast
    /// resolver is cache-first and the playback cache is quality-blind.
    pub(crate) async fn casting_prefetch_quality(&self) -> Option<Quality> {
        let cap_key = {
            let inner = self.inner.lock().await;
            if inner.protocol.is_none() {
                return None;
            }
            inner.connected_cap_key.clone()
        };
        Some(effective_cast_quality(cap_key.as_deref()).0)
    }

    // ---- Discovery ---------------------------------------------------------

    /// Start mDNS (Chromecast) + SSDP (DLNA) discovery, the 2 s device-refresh
    /// loop and the 15 s scan-spinner window. Picker-owned: this is the ONLY
    /// place either discovery is armed.
    pub(crate) async fn start_discovery(self: &Arc<Self>) {
        if crate::settings_qt::network()
            .get_settings()
            .map(|settings| settings.block_cast)
            .unwrap_or(false)
        {
            log::info!("[qbz-qt][Cast] discovery blocked by Settings -> Network");
            return;
        }
        {
            let mut inner = self.inner.lock().await;
            if inner.chromecast_discovery.is_none() {
                let mut disco = DeviceDiscovery::new();
                if let Err(e) = disco.start_discovery() {
                    log::warn!("[qbz-qt][Cast] chromecast discovery start failed: {e}");
                }
                inner.chromecast_discovery = Some(disco);
            }
            if inner.dlna_discovery.is_none() {
                let mut disco = DlnaDiscovery::new();
                if let Err(e) = disco.start_discovery().await {
                    log::warn!("[qbz-qt][Cast] dlna discovery start failed: {e}");
                }
                inner.dlna_discovery = Some(disco);
            }
        }

        // Arm the scan-spinner window.
        set_scanning(true);
        crate::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(SCAN_DURATION_MS)).await;
            set_scanning(false);
        });

        // 2 s device-refresh loop (replaces any prior).
        let svc = self.clone();
        let task = tokio::spawn(async move {
            loop {
                svc.refresh_devices().await;
                tokio::time::sleep(std::time::Duration::from_millis(DEVICE_POLL_INTERVAL_MS)).await;
            }
        });
        let mut inner = self.inner.lock().await;
        if let Some(old) = inner.discovery_task.replace(task) {
            old.abort();
        }
    }

    /// Stop both discoveries + the refresh loop (picker closed). The active
    /// connection is untouched — a cast survives the picker.
    pub(crate) async fn stop_discovery(self: &Arc<Self>) {
        let resources = {
            let mut inner = self.inner.lock().await;
            if let Some(task) = inner.discovery_task.take() {
                task.abort();
            }
            BlockingTeardown {
                chromecast_discovery: inner.chromecast_discovery.take(),
                dlna_discovery: inner.dlna_discovery.take(),
                ..BlockingTeardown::default()
            }
        };
        let _ = self.run_blocking_teardown(resources).await;
        set_scanning(false);
    }

    /// `(chromecast_count, dlna_count, [(protocol, name)])` for the Settings >
    /// Developer Diagnostics panel's Cast Discovery section.
    ///
    /// The reference reads the `CastState` Slint global on the UI thread
    /// (`crates/qbz/src/diagnostics.rs:253-270`) because that is where its
    /// picker's device list lives. Here the lists live in `inner`, so the
    /// panel reads them straight from the service instead of round-tripping
    /// through the bridge — same numbers, one fewer hop, and no oneshot.
    /// PURE read: it never starts or stops discovery.
    pub(crate) async fn diag_devices(&self) -> (i32, i32, Vec<(String, String)>) {
        let inner = self.inner.lock().await;
        let cc = inner
            .chromecast_discovery
            .as_ref()
            .map(|d| d.get_discovered_devices())
            .unwrap_or_default();
        let dl = inner
            .dlna_discovery
            .as_ref()
            .map(|d| d.get_discovered_devices())
            .unwrap_or_default();
        let mut rows: Vec<(String, String)> = Vec::with_capacity(cc.len() + dl.len());
        for d in &cc {
            rows.push(("chromecast".to_string(), d.name.clone()));
        }
        for d in &dl {
            rows.push(("dlna".to_string(), d.name.clone()));
        }
        (cc.len() as i32, dl.len() as i32, rows)
    }

    /// Snapshot both device lists and push them to the picker.
    async fn refresh_devices(&self) {
        let (chromecast, dlna) = {
            let inner = self.inner.lock().await;
            let cc = inner
                .chromecast_discovery
                .as_ref()
                .map(|d| d.get_discovered_devices())
                .unwrap_or_default();
            let dl = inner
                .dlna_discovery
                .as_ref()
                .map(|d| d.get_discovered_devices())
                .unwrap_or_default();
            (cc, dl)
        };
        push_devices(chromecast, dlna);
    }

    // ---- Connect / disconnect ----------------------------------------------

    /// Connect to a device: halt local playback, suspend QConnect if it was
    /// on (§11.4), then re-cast the current track at its position if one was
    /// playing (`castStore.connectToDevice`).
    async fn connect_exact(
        self: &Arc<Self>,
        device_id: String,
        proto: CastProtocol,
        transition_epoch: CastTransitionEpoch,
    ) -> Result<(), String> {
        let transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        self.mark_transition_lane_if_current(transition_epoch);
        if !self.transition_is_current(transition_epoch) {
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }
        if !self.await_physical_teardown(transition_epoch).await {
            return Err("A previous cast renderer teardown is still incomplete".to_string());
        }
        let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
            return Ok(());
        };

        // Snapshot local playback BEFORE we tear it down.
        let snapshot_track = self.runtime.core().current_track().await;
        let pb = self.runtime.core().get_playback_state();
        let cast_was_playing = self.inner.lock().await.is_playing;
        let was_playing = pb.is_playing || cast_was_playing;
        let resume_pos = pb.position;
        if !self.transition_is_current(transition_epoch) {
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }

        // QConnect shutdown acquires a drained authority fence. Release this
        // action first so that teardown can drain it, then reacquire after the
        // fenced transition before touching the cast renderer.
        drop(_owner_action);

        // A -> B is a physical replacement, never two provisional renderer
        // connections followed by an overwrite in `inner`. Tear A down under
        // this transition lane, carry its exact QConnect restore obligation,
        // and fail closed before even constructing B when Stop/Disconnect was
        // not confirmed.
        if self.is_casting().await {
            let Some(outcome) = self.teardown_renderer().await else {
                return Err("Cast renderer authority could not be fenced".to_string());
            };
            self.publish_disconnected_state().await;
            if let Some(token) = outcome.restore_qconnect {
                self.inner.lock().await.qconnect_restore_token = Some(token);
            }
            if !outcome.physical_safe {
                if let Some(token) = outcome.restore_qconnect {
                    self.supervise_late_qconnect_restore(transition_epoch, token);
                }
                return Err("Previous cast renderer physical teardown is incomplete".to_string());
            }
            if !self.transition_is_current(transition_epoch) {
                return Err(
                    "Cast connection was superseded by a newer renderer request".to_string()
                );
            }
        }

        // Suspend QConnect if it was on (§11.4), before the renderer connect.
        // A non-authority-safe teardown fails closed.
        if !self.transition_is_current(transition_epoch) {
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }
        self.suspend_qconnect_if_on(transition_epoch).await?;
        if !self.transition_is_current(transition_epoch) {
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }
        let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
            let restore_qconnect = self.inner.lock().await.qconnect_restore_token.is_some();
            drop(transition_guard);
            if restore_qconnect {
                self.restore_latched_qconnect(transition_epoch).await;
            }
            return Err("Cast ownership handoff is no longer available".to_string());
        };

        // Halt the local audio backend only after this Cast intent survived
        // the QConnect handoff. A newer QConnect request can therefore cancel
        // a provisional Cast connect without silencing its current renderer.
        let _ = self.runtime.core().stop();

        // Build the renderer connection provisionally. It is not visible in
        // `inner` until the inverse-owner check immediately below succeeds.
        let pending_result = match proto {
            CastProtocol::Chromecast => self.connect_chromecast(&device_id, transition_epoch).await,
            CastProtocol::Dlna => self.connect_dlna(&device_id, transition_epoch).await,
        };
        let pending = match pending_result {
            Ok(pending) => pending,
            Err(error) => {
                if !self.transition_is_current(transition_epoch) {
                    return Err(
                        "Cast connection was superseded by a newer renderer request".to_string()
                    );
                }
                let restore_qconnect_now = self.physical_teardown_safe()
                    && self.inner.lock().await.qconnect_restore_token.is_some();
                drop(_owner_action);
                drop(transition_guard);
                if restore_qconnect_now {
                    self.restore_latched_qconnect(transition_epoch).await;
                }
                return Err(error);
            }
        };

        if !self.transition_is_current(transition_epoch) {
            drop(_owner_action);
            let physical_safe = self
                .teardown_detached_renderer(pending.into_detached())
                .await;
            if !physical_safe {
                log::warn!(
                    "[qbz-qt][Cast] superseded provisional renderer teardown was not confirmed"
                );
            }
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }

        // Renderer ownership is fail-closed if QConnect remained live. The
        // transition lease prevents a fresh QConnect connect between this
        // await and the synchronous commit.
        if Self::qconnect_is_running().await {
            let _ = self
                .teardown_detached_renderer(pending.into_detached())
                .await;
            self.inner.lock().await.qconnect_restore_token = None;
            return Err("Cannot start Cast while Qobuz Connect is still active".to_string());
        }

        if !self.transition_is_current(transition_epoch) {
            drop(_owner_action);
            let physical_safe = self
                .teardown_detached_renderer(pending.into_detached())
                .await;
            if !physical_safe {
                log::warn!(
                    "[qbz-qt][Cast] superseded provisional renderer teardown was not confirmed"
                );
            }
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        }

        let mut pending = Some(pending);
        let connection_stamp = {
            let mut inner = self.inner.lock().await;
            if !self.transition_is_current(transition_epoch) {
                None
            } else {
                // This read under CastInner is the renderer-commit
                // linearization point. An intent published while T1 waited for
                // the lock prevents publication; one published after this
                // point is ordered after the committed T1 and tears it down.
                let PendingRendererConnection {
                    renderer,
                    device_ip,
                    device_name,
                    device_id,
                    cap_key,
                } = pending
                    .take()
                    .expect("pending renderer is consumed exactly once");
                match renderer {
                    PendingRenderer::Chromecast(handle) => inner.chromecast = Some(handle),
                    PendingRenderer::Dlna(connection) => {
                        inner.dlna = Some(Arc::new(Mutex::new(connection)))
                    }
                }
                inner.connection_epoch = inner.connection_epoch.wrapping_add(1).max(1);
                inner.connection_transition_epoch = transition_epoch.0;
                inner.protocol = Some(proto);
                invalidate_media_intent(&mut inner);
                inner.pending_volume = None;
                inner.volume_worker_connection = None;
                inner.connected_device_ip = Some(device_ip);
                inner.connected_device_name = Some(device_name);
                inner.connected_device_id = Some(device_id);
                inner.connected_cap_key = cap_key;
                inner.track_end_detected = false;
                inner.cast_saw_playing = false;
                inner.cast_max_position = 0.0;
                inner.cast_premature_stop_polls = 0;
                inner.end_stall = DlnaEndStall::default();
                inner.lost_polls = 0;
                log::info!(
                    "[qbz-qt][Cast] connected to {} ({}; cap key: {})",
                    inner.connected_device_name.as_deref().unwrap_or("?"),
                    inner.connected_device_id.as_deref().unwrap_or("?"),
                    inner
                        .connected_cap_key
                        .as_deref()
                        .unwrap_or("none — unstable id"),
                );
                Some(CastConnectionStamp {
                    connection_epoch: inner.connection_epoch,
                    transition_epoch,
                    protocol: proto,
                })
            }
        };
        let Some(connection_stamp) = connection_stamp else {
            drop(_owner_action);
            let physical_safe = self
                .teardown_detached_renderer(
                    pending
                        .take()
                        .expect("stale provisional renderer remains detached")
                        .into_detached(),
                )
                .await;
            if !physical_safe {
                log::warn!(
                    "[qbz-qt][Cast] superseded provisional renderer teardown was not confirmed"
                );
            }
            return Err("Cast connection was superseded by a newer renderer request".to_string());
        };
        // Cast ownership is now committed. Everything after this point is
        // presentation/bootstrap and cannot make QConnect win the same race.
        set_error(String::new());
        self.push_connection_state().await;
        self.push_device_cap_row().await;
        self.start_position_poll(connection_stamp);
        drop(transition_guard);

        // Re-cast the current track at its position, passing the REAL source.
        if was_playing {
            if let Some(track) = snapshot_track {
                match self
                    .cast_track_for_connection(&track, Some(connection_stamp))
                    .await
                {
                    Err(e) => {
                        log::warn!("[qbz-qt][Cast] resume re-cast failed: {e}");
                    }
                    Ok(Some(receipt)) if resume_pos > 5 => {
                        // Deferred seek (the renderer needs the media loaded first).
                        let svc = self.clone();
                        let pos = resume_pos as f64;
                        crate::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            let Some(_owner_action) = crate::playback_qt::begin_owner_action()
                            else {
                                log::debug!(
                                    "[qbz-qt][Cast] deferred resume seek expired during authority handoff"
                                );
                                return;
                            };
                            let Some(transport) = svc
                                .issue_deferred_transport_command(receipt.transport)
                                .await
                            else {
                                log::debug!(
                                    "[qbz-qt][Cast] deferred resume seek was superseded before dispatch"
                                );
                                return;
                            };
                            match svc.seek_secs_if_session(transport, pos).await {
                                Ok(true) => {}
                                Ok(false) => log::debug!(
                                    "[qbz-qt][Cast] deferred resume seek expired with its cast session"
                                ),
                                Err(e) => {
                                    log::warn!("[qbz-qt][Cast] deferred resume seek failed: {e}")
                                }
                            }
                        });
                    }
                    Ok(_) => {}
                }
            }
        }
        Ok(())
    }

    async fn connect_chromecast(
        self: &Arc<Self>,
        device_id: &str,
        transition_epoch: CastTransitionEpoch,
    ) -> Result<PendingRendererConnection, String> {
        let device: DiscoveredDevice = {
            let inner = self.inner.lock().await;
            inner
                .chromecast_discovery
                .as_ref()
                .and_then(|d| d.get_device(device_id))
                .ok_or_else(|| format!("Chromecast device not found: {device_id}"))?
        };
        let handle = ChromecastHandle::new();
        let connect_ip = device.ip.clone();
        let connect_port = device.port;
        let worker_handle = handle.clone();
        self.teardown_pending.fetch_add(1, Ordering::AcqRel);
        let mut task =
            tokio::task::spawn_blocking(move || worker_handle.connect(connect_ip, connect_port));
        let mut deadline = Box::pin(tokio::time::sleep(CHROMECAST_CONNECT_BUDGET));
        let connect_result = loop {
            tokio::select! {
                biased;
                result = &mut task => break Some(result),
                _ = &mut deadline => break None,
                _ = tokio::time::sleep(CAST_TRANSITION_CANCEL_POLL) => {
                    if !self.transition_is_current(transition_epoch) {
                        self.supervise_late_chromecast_connect(
                            task,
                            handle,
                            transition_epoch,
                        );
                        return Err(
                            "Cast connection was superseded by a newer renderer request".to_string()
                        );
                    }
                }
            }
        };
        let Some(connect_result) = connect_result else {
            self.supervise_late_chromecast_connect(task, handle, transition_epoch);
            return Err(CastError::Timeout("chromecast-connect".to_string()).to_string());
        };
        match connect_result {
            Ok(Ok(())) => self.finish_physical_teardown(true),
            Ok(Err(error)) => {
                let cleanup_safe = Self::disconnect_failed_chromecast_connect(handle).await;
                self.finish_physical_teardown(cleanup_safe);
                return Err(error.to_string());
            }
            Err(error) => {
                let cleanup_safe = Self::disconnect_failed_chromecast_connect(handle).await;
                self.finish_physical_teardown(false);
                if !cleanup_safe {
                    log::warn!(
                        "[qbz-qt][Cast] failed Chromecast connect cleanup was not confirmed"
                    );
                }
                return Err(format!(
                    "Chromecast blocking connect worker failed: {error}"
                ));
            }
        }
        // Cap key only when the id is the mDNS TXT `id` record (the Cast
        // UUID). The fullname fallback tracks the friendly name, so a cap
        // keyed on it would silently stop applying on rename.
        let cap_key = device
            .id_is_stable
            .then(|| format!("chromecast:{}", device.id));
        if !device.id_is_stable {
            log::info!(
                "[qbz-qt][Cast] {} broadcasts no mDNS TXT id — per-device quality cap unavailable",
                device.name
            );
        }
        Ok(PendingRendererConnection {
            renderer: PendingRenderer::Chromecast(handle),
            device_ip: device.ip,
            device_name: device.name,
            device_id: device.id,
            cap_key,
        })
    }

    async fn connect_dlna(
        &self,
        device_id: &str,
        transition_epoch: CastTransitionEpoch,
    ) -> Result<PendingRendererConnection, String> {
        // `DlnaConnection::connect` consumes the discovered device by value.
        let device: DiscoveredDlnaDevice = {
            let inner = self.inner.lock().await;
            inner
                .dlna_discovery
                .as_ref()
                .and_then(|d| d.get_device(device_id))
                .ok_or_else(|| format!("DLNA device not found: {device_id}"))?
        };
        let ip = device.ip.clone();
        let name = device.name.clone();
        let udn = device.id.clone();
        let connect = DlnaConnection::connect(device);
        tokio::pin!(connect);
        let mut deadline = Box::pin(tokio::time::sleep(DLNA_CONNECT_BUDGET));
        let conn = loop {
            tokio::select! {
                biased;
                result = &mut connect => break result.map_err(|error| error.to_string())?,
                _ = &mut deadline => {
                    return Err("DLNA connection timed out".to_string());
                }
                _ = tokio::time::sleep(CAST_TRANSITION_CANCEL_POLL) => {
                    if !self.transition_is_current(transition_epoch) {
                        return Err(
                            "Cast connection was superseded by a newer renderer request".to_string()
                        );
                    }
                }
            }
        };
        // The DLNA id IS the UPnP UDN — stable by construction, so a DLNA
        // renderer is always cappable.
        Ok(PendingRendererConnection {
            renderer: PendingRenderer::Dlna(conn),
            device_ip: ip,
            device_name: name,
            cap_key: Some(format!("dlna:{udn}")),
            device_id: udn,
        })
    }

    /// Disconnect: stop the renderer, drop the connection, restore the
    /// QConnect session connect() suspended (§11.4), reset state.
    async fn disconnect_exact(self: &Arc<Self>, transition_epoch: CastTransitionEpoch) {
        let transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        self.mark_transition_lane_if_current(transition_epoch);
        self.finish_disconnect(transition_epoch, transition_guard)
            .await;
    }

    /// Conditional liveness cleanup never publishes an intent before it has
    /// detached the exact lost snapshot. If a newer user/QConnect transition is
    /// already queued, this cleanup may help it by tearing A down but never
    /// supersedes it; the restore latch is inherited by that newer transition.
    async fn disconnect_if_poll_snapshot(self: &Arc<Self>, expected: CastPollSnapshot) {
        let transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        let observed_transition = self.current_transition_epoch();
        let lane_was_caught_up = self.transition_lane_is_caught_up(observed_transition);
        if !lost_poll_snapshot_matches(&*self.inner.lock().await, expected) {
            return;
        }
        if !self.await_physical_teardown(observed_transition).await {
            return;
        }
        if !lost_poll_snapshot_matches(&*self.inner.lock().await, expected) {
            return;
        }
        let Some(outcome) = self.teardown_renderer_if_poll(expected).await else {
            return;
        };
        // Only a stable connection lifetime needs a fresh epoch for its own
        // restore. When another transition is already current, leave it current
        // and let it inherit the exact QConnect latch.
        let restore_epoch = lane_was_caught_up
            .then(|| self.begin_transition_intent_if_current(observed_transition))
            .flatten();
        if let Some(restore_epoch) = restore_epoch {
            self.mark_transition_lane_if_current(restore_epoch);
        }
        self.complete_disconnect(outcome, transition_guard, restore_epoch)
            .await;
    }

    async fn finish_disconnect(
        self: &Arc<Self>,
        transition_epoch: CastTransitionEpoch,
        transition_guard: OwnedMutexGuard<()>,
    ) {
        if !self.transition_is_current(transition_epoch) {
            return;
        }
        if !self.await_physical_teardown(transition_epoch).await {
            log::warn!("[qbz-qt][Cast] disconnect deferred while physical teardown is incomplete");
            return;
        }
        let Some(outcome) = self.teardown_renderer().await else {
            return;
        };
        self.complete_disconnect(outcome, transition_guard, Some(transition_epoch))
            .await;
    }

    async fn complete_disconnect(
        self: &Arc<Self>,
        outcome: RendererTeardownOutcome,
        transition_guard: OwnedMutexGuard<()>,
        restore_epoch: Option<CastTransitionEpoch>,
    ) {
        self.publish_disconnected_state().await;
        if !outcome.physical_safe {
            if let Some(token) = outcome.restore_qconnect {
                self.inner.lock().await.qconnect_restore_token = Some(token);
                if let Some(restore_epoch) = restore_epoch {
                    self.supervise_late_qconnect_restore(restore_epoch, token);
                }
            }
            log::warn!(
                "[qbz-qt][Cast] renderer detached logically, but physical teardown is incomplete"
            );
            return;
        }
        if let Some(token) = outcome.restore_qconnect {
            // Keep the obligation attached to the Cast lifetime until an exact
            // restore succeeds. If a newer renderer supersedes this restore,
            // it inherits the latch and restores QConnect when it later exits.
            self.inner.lock().await.qconnect_restore_token = Some(token);
        }
        drop(transition_guard);
        if let Some(restore_epoch) = restore_epoch {
            if outcome.restore_qconnect.is_some() && self.transition_is_current(restore_epoch) {
                self.restore_latched_qconnect(restore_epoch).await;
            }
        }
    }

    /// Pure cast teardown shared with QConnect's inverse mutual-exclusion seam.
    ///
    /// This helper deliberately cannot restore QConnect. Keeping restoration
    /// in [`Self::disconnect`] breaks the async type cycle
    /// `QConnect::connect -> cast teardown -> QConnect::connect` at compile
    /// time, not merely behind a runtime boolean.
    async fn teardown_renderer(self: &Arc<Self>) -> Option<RendererTeardownOutcome> {
        self.teardown_renderer_exact(None).await
    }

    async fn teardown_renderer_if_poll(
        self: &Arc<Self>,
        expected: CastPollSnapshot,
    ) -> Option<RendererTeardownOutcome> {
        self.teardown_renderer_exact(Some(expected)).await
    }

    async fn teardown_renderer_exact(
        self: &Arc<Self>,
        expected_poll: Option<CastPollSnapshot>,
    ) -> Option<RendererTeardownOutcome> {
        let Some(owner_action) = crate::playback_qt::begin_owner_action() else {
            log::debug!(
                "[qbz-qt][Cast] disconnect deferred while delegated authority or a handoff fence is active"
            );
            return None;
        };
        // Unconditional teardown aborts first so a slow DLNA poll releases the
        // state lock. Conditional poll cleanup instead validates under that
        // lock before aborting: otherwise stale A cleanup could abort B's poll.
        if expected_poll.is_none() {
            self.abort_poll();
            crate::cast_viz::stop();
        }
        let (restore_qconnect, renderer) = {
            let mut inner = self.inner.lock().await;
            if let Some(expected) = expected_poll {
                if !lost_poll_snapshot_matches(&inner, expected) {
                    return None;
                }
                self.abort_poll();
                crate::cast_viz::stop();
            }
            // Logically detach first. This expires every exact control/poll and
            // prevents a fresh media request from entering while teardown is
            // queued behind the current total media transaction.
            inner.connection_epoch = inner.connection_epoch.wrapping_add(1).max(1);
            invalidate_media_intent(&mut inner);
            inner.pending_volume = None;
            inner.volume_worker_connection = None;
            let renderer = DetachedRenderer {
                chromecast: inner.chromecast.take(),
                dlna: inner.dlna.take(),
            };
            inner.protocol = None;
            inner.connected_device_ip = None;
            inner.connected_device_name = None;
            inner.connected_device_id = None;
            inner.connected_cap_key = None;
            inner.is_playing = false;
            inner.track_end_detected = false;
            inner.lost_polls = 0;
            let restore_qconnect = inner.qconnect_restore_token.take();
            (restore_qconnect, renderer)
        };
        // Cancellation and registry clearing participate in the same total
        // media order as resolve/register/LOAD. A stale resolver therefore
        // cannot re-register its same-id route behind this teardown. The
        // complete continuation is supervised so waiting for this lane is
        // included in the caller's budget without abandoning renderer handles.
        let cleanup_service = Arc::clone(self);
        let physical_safe = self
            .run_media_teardown_bounded("renderer", async move {
                let _media_guard = Arc::clone(&cleanup_service.media_command_gate)
                    .lock_owned()
                    .await;
                let cancel = {
                    let mut inner = cleanup_service.inner.lock().await;
                    let cancel = inner.stream_cancel.take().map(|cancel| cancel.sender);
                    if let Some(server) = inner.media_server.as_ref() {
                        server.clear_entries();
                    }
                    cancel
                };
                if let Some(cancel) = cancel {
                    let _ = cancel.send(true);
                }
                // Stop before disconnect without holding the async state lock.
                // Sync Chromecast teardown runs on the blocking pool.
                let physical_safe = cleanup_service.teardown_detached_renderer(renderer).await;
                // Restoring QConnect installs its own drained authority fence.
                // Keep this owner action until the supervised teardown really
                // finishes, including after the caller's timeout.
                drop(owner_action);
                physical_safe
            })
            .await;
        Some(RendererTeardownOutcome {
            restore_qconnect,
            physical_safe,
        })
    }

    async fn publish_disconnected_state(&self) {
        // Clear the per-connection disclosure + cap row.
        cast_bridge::ui(|mut b| {
            b.as_mut().set_quality_limit_cause(0);
            b.as_mut().set_quality_over_cap(false);
            b.as_mut().set_quality_origin(cxx_qt_lib::QString::from(""));
            b.as_mut().set_device_cap_available(false);
            b.as_mut().set_device_cap_key(cxx_qt_lib::QString::from(""));
            b.as_mut().set_device_cap_index(0);
        });
        self.push_connection_state().await;
    }

    // ---- QConnect coexistence (§11.4 — cast_service.rs:1140-1161) -----------

    /// Suspend QConnect while casting (mutual exclusion). A disconnect may
    /// degrade internally, but Cast can proceed only once QConnect confirms
    /// that delegated/owner authority is fenced. The latch is recorded only
    /// for a session this transition actually suspended.
    async fn suspend_qconnect_if_on(
        &self,
        transition_epoch: CastTransitionEpoch,
    ) -> Result<(), String> {
        let Some(qc) = crate::qconnect_qt::service() else {
            return Ok(());
        };
        // Snapshot the carried latch, then release CastInner before touching
        // QConnect. A previous restore may already have consumed this token
        // (`T -> E`) without installing a runtime; the new Cast must still
        // cancel that enabled intent and replace the latch with exact `T2`.
        let carried_restore = self.inner.lock().await.qconnect_restore_token;
        let qconnect_running = qc.is_running().await;
        let consumed_restore_in_flight = carried_restore.is_some() && qc.has_enabled_intent();
        if !qconnect_running && !consumed_restore_in_flight {
            return Ok(());
        }
        let outcome = match qc.disconnect_for_cast(transition_epoch).await {
            Ok(outcome) => outcome,
            Err(_) => {
                // A newer Cast/QConnect epoch inherits the carried marker. It
                // may be a consumed token, but cannot resurrect by itself; the
                // successor uses it plus enabled intent to mint exact T2.
                log::warn!("[qbz-qt][Cast] QConnect suspend did not reach an authority-safe state");
                return Err("Cannot start Cast while Qobuz Connect authority is active".to_string());
            }
        };
        // Some(T2): this Cast owns an automatic restore. None: a later manual
        // disable won and deliberately removed the inherited obligation. Keep
        // an unconsumed carried token through a repeated physical teardown.
        let restore_token = outcome
            .cast_restore_token
            .filter(|token| qc.cast_restore_token_is_current(*token))
            .or_else(|| carried_restore.filter(|token| qc.cast_restore_token_is_current(*token)));
        self.inner.lock().await.qconnect_restore_token = restore_token;
        if !outcome.authority_safe {
            log::warn!("[qbz-qt][Cast] QConnect suspend did not reach an authority-safe state");
            return Err("Cannot start Cast while Qobuz Connect authority is active".to_string());
        }
        // The facade deliberately does NOT flip the bar badge itself (the
        // toggle / startup auto-connect / offline force-disconnect paths each
        // publish their own — the qconnect_bridge.rs connectToggle tail); a
        // suspend must not leave the golden button lit while the session is
        // down. Publish only after the authority-safe outcome above.
        crate::qconnect_qt::publish::connected(false);
        Ok(())
    }

    /// Consume the restore latch only after an exact restore succeeds. Keeping
    /// it set across physical worker waits and superseded Cast epochs prevents
    /// a timeout/new renderer from silently losing the QConnect session Cast
    /// originally suspended.
    fn supervise_late_qconnect_restore(
        self: &Arc<Self>,
        transition_epoch: CastTransitionEpoch,
        restore_token: QconnectDisabledToken,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if !service.transition_is_current(transition_epoch) {
                    return;
                }
                if service.teardown_unsafe.load(Ordering::Acquire) {
                    log::warn!(
                        "[qbz-qt][Cast] late renderer teardown stayed unsafe; QConnect restore remains fenced"
                    );
                    return;
                }
                if service.teardown_pending.load(Ordering::Acquire) == 0 {
                    let restore_is_current = {
                        let inner = service.inner.lock().await;
                        inner.protocol.is_none()
                            && inner.qconnect_restore_token == Some(restore_token)
                    };
                    if restore_is_current && service.physical_teardown_safe() {
                        log::info!(
                            "[qbz-qt][Cast] late renderer teardown recovered; resuming exact QConnect restore"
                        );
                        service.restore_latched_qconnect(transition_epoch).await;
                    }
                    return;
                }
                tokio::time::sleep(CAST_TRANSITION_CANCEL_POLL).await;
            }
        });
    }

    async fn restore_latched_qconnect(&self, transition_epoch: CastTransitionEpoch) {
        let restore_token = {
            let inner = self.inner.lock().await;
            if self.transition_is_current(transition_epoch) && self.physical_teardown_safe() {
                inner.qconnect_restore_token
            } else {
                None
            }
        };
        let Some(restore_token) = restore_token else {
            return;
        };
        let Some(qc) = crate::qconnect_qt::service() else {
            return;
        };
        match qc
            .connect_for_cast_restore(transition_epoch, restore_token)
            .await
        {
            Ok(()) => {
                let publish = {
                    let inner = self.inner.lock().await;
                    self.transition_is_current(transition_epoch)
                        && self.physical_teardown_safe()
                        && inner.qconnect_restore_token.is_none()
                };
                if publish {
                    crate::qconnect_qt::publish::connected(true);
                }
            }
            Err(error) => {
                // Exact re-enable consumes this disabled token forever, even
                // if the following cloud bootstrap fails. Preserve it only
                // when a newer Cast epoch inherited it before consumption.
                if !qc.cast_restore_token_is_current(restore_token) {
                    let mut inner = self.inner.lock().await;
                    if self.transition_is_current(transition_epoch)
                        && inner.qconnect_restore_token == Some(restore_token)
                    {
                        inner.qconnect_restore_token = None;
                    }
                }
                log::warn!("[qbz-qt][Cast] delayed QConnect restore failed: {error}")
            }
        }
    }

    // ---- Casting a track ----------------------------------------------------

    /// Resolve a track's bytes + MIME, register them with the shared media
    /// server, and hand the URL to the active renderer. Routes by source.
    async fn cast_track(
        self: &Arc<Self>,
        track: &QueueTrack,
    ) -> Result<Option<CastMediaCommandReceipt>, String> {
        self.cast_track_for_connection(track, None).await
    }

    async fn cast_track_for_connection(
        self: &Arc<Self>,
        track: &QueueTrack,
        expected_connection: Option<CastConnectionStamp>,
    ) -> Result<Option<CastMediaCommandReceipt>, String> {
        let Some((media_stamp, replaced_loaded_media)) = self
            .begin_media_intent(track.id, expected_connection)
            .await?
        else {
            return Ok(None);
        };
        self.run_media_command(track, media_stamp, replaced_loaded_media)
            .await
    }

    async fn run_media_command(
        self: &Arc<Self>,
        track: &QueueTrack,
        media_stamp: CastMediaIntentStamp,
        replaced_loaded_media: bool,
    ) -> Result<Option<CastMediaCommandReceipt>, String> {
        let _media_guard = Arc::clone(&self.media_command_gate).lock_owned().await;
        if !self.prepare_media_command(media_stamp).await {
            return Ok(None);
        }

        // Resolve + register per source. The fetch happens OUTSIDE the lock.
        // Qobuz has its own tiers (cache / progressive download); every
        // other source goes through the registry's playback ticket: a file on
        // disk is served from disk, a server-streamed item (Plex / Jellyfin /
        // Subsonic) is PROXIED to the renderer with the source's own request
        // contract — the media-server arm the Slint service left as a TODO.
        let info_result = if uses_qobuz_cast_pipeline(track) {
            self.register_qobuz(media_stamp).await
        } else {
            match resolve_castable(track).await {
                Ok(Castable::File(path)) => self.register_local(media_stamp, &path).await,
                Ok(Castable::Stream { url, headers }) => {
                    self.register_proxy(media_stamp, url, headers).await
                }
                Err(error) => Err(error),
            }
        };
        let mut info = match info_result {
            Ok(info) => info,
            Err(error) => {
                let still_current = self.media_intent_is_current(media_stamp).await;
                self.discard_media_attempt(media_stamp).await;
                if still_current {
                    self.retire_failed_media_intent(media_stamp, &error).await;
                    return Err(error);
                }
                return Ok(None);
            }
        };
        if !self.media_intent_is_current(media_stamp).await {
            self.discard_media_attempt(media_stamp).await;
            return Ok(None);
        }
        let content_type = info.content_type.clone();

        // Build the per-device URL and hand it to the renderer.
        let url_result = {
            let inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                Err("Cast media request was superseded".to_string())
            } else {
                let ip = inner.connected_device_ip.clone();
                match inner.media_server.as_ref() {
                    Some(server) => match ip.as_deref() {
                        Some(ip) => server.get_audio_url_for_target(track.id, ip),
                        None => server.get_audio_url(track.id),
                    }
                    .ok_or_else(|| "Failed to build media URL".to_string()),
                    None => Err("Media server not initialized".to_string()),
                }
            }
        };
        let url = match url_result {
            Ok(url) => url,
            Err(error) => {
                let still_current = self.media_intent_is_current(media_stamp).await;
                self.discard_media_attempt(media_stamp).await;
                if still_current {
                    self.retire_failed_media_intent(media_stamp, &error).await;
                    return Err(error);
                }
                return Ok(None);
            }
        };

        let load_result = match media_stamp.connection.protocol {
            CastProtocol::Chromecast => {
                let handle = {
                    let inner = self.inner.lock().await;
                    if !media_intent_stamp_matches(&inner, media_stamp) {
                        None
                    } else {
                        inner.chromecast.clone()
                    }
                };
                match handle {
                    Some(handle) => {
                        let metadata = media_metadata(track);
                        // load_media auto-plays on the Default Media Receiver.
                        Self::run_chromecast_call(
                            handle,
                            CHROMECAST_COMMAND_BUDGET,
                            "chromecast-load",
                            move |handle| handle.load_media(url, content_type, metadata),
                        )
                        .await
                        .map_err(|e| e.to_string())
                    }
                    None => Err("Chromecast connection expired before LOAD".to_string()),
                }
            }
            CastProtocol::Dlna => {
                let conn = {
                    let inner = self.inner.lock().await;
                    if !media_intent_stamp_matches(&inner, media_stamp) {
                        None
                    } else {
                        inner.dlna.clone()
                    }
                };
                if let Some(conn) = conn {
                    let mut conn = conn.lock().await;
                    if !self.media_intent_is_current(media_stamp).await {
                        Err("DLNA connection expired before LOAD".to_string())
                    } else {
                        // DLNA is a TWO-step load -> play. With a track already
                        // loaded the renderer is stopped FIRST: gmediarender
                        // ignored SetAVTransportURI while paused.
                        let result = async {
                            if replaced_loaded_media {
                                let _ = conn.stop().await;
                            }
                            conn.load_media(&url, &dlna_metadata(track), &content_type)
                                .await
                                .map_err(|e| e.to_string())?;
                            conn.play().await.map_err(|e| e.to_string())?;
                            Ok::<(), String>(())
                        }
                        .await;
                        if let Err(e) = result {
                            // Best-effort reset so the renderer doesn't sit
                            // half-loaded on a URI it already faulted (#646).
                            let _ = conn.stop().await;
                            Err(e)
                        } else {
                            Ok(())
                        }
                    }
                } else {
                    Err("DLNA connection expired before LOAD".to_string())
                }
            }
        };
        if let Err(error) = load_result {
            let still_current = self.media_intent_is_current(media_stamp).await;
            self.discard_media_attempt(media_stamp).await;
            if still_current {
                self.retire_failed_media_intent(media_stamp, &error).await;
                return Err(error);
            }
            return Ok(None);
        }

        // Renderer LOAD completed, but a same-track successor may have
        // published a newer media epoch while the network call was in flight.
        // Commit all Cast state/UI under the exact-intent lock edge or stop the
        // stale item before allowing the successor into this total lane.
        let committed = {
            let mut inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                None
            } else {
                inner.current_track_id = Some(track.id);
                inner.transport_in_flight_epoch = None;
                inner.is_playing = true;
                inner.track_end_detected = false;
                inner.cast_saw_playing = false;
                inner.cast_max_position = 0.0;
                inner.cast_premature_stop_polls = 0;
                inner.end_stall = DlnaEndStall::default();
                inner.lost_polls = 0;
                // The visualizer keeps moving: decode the same bytes silently,
                // paced to the renderer's reported position (see `cast_viz`).
                match (info.shadow.take(), self.runtime.visualizer_tap()) {
                    (Some(shadow), Some(tap)) => crate::cast_viz::start(shadow, tap.clone()),
                    _ => crate::cast_viz::stop(),
                }
                // Delivered quality for the picker line (#638 fix 1): MEASURED
                // from served bytes, with catalog fallback for non-FLAC/files.
                let (quality_label, quality_detail) = match info.probe {
                    Some(p) => (
                        if p.bits_per_sample >= 24 {
                            "Hi-Res FLAC"
                        } else {
                            "FLAC"
                        }
                        .to_string(),
                        crate::quality_state::detail(
                            Some(p.bits_per_sample),
                            Some(p.sample_rate as f64),
                        ),
                    ),
                    None => quality_label_from_track(track),
                };
                push_quality(quality_label, quality_detail);
                // One measured-badge publication per committed intent.
                self.publish_measured_badge(&info);
                set_error(String::new());
                Self::publish_connection_state_locked(&inner);
                Some(CastMediaCommandReceipt {
                    transport: current_transport_stamp(&inner)
                        .expect("committed Cast media has a transport stamp"),
                })
            }
        };
        let Some(receipt) = committed else {
            self.stop_loaded_media_if_connection(media_stamp.connection)
                .await;
            self.discard_media_attempt(media_stamp).await;
            return Ok(None);
        };
        Ok(Some(receipt))
    }

    /// qobuz: resolve via the shared core API (cache -> network here; the Qt
    /// port has no offline store), probe the served bytes, register them.
    async fn register_qobuz(
        &self,
        media_stamp: CastMediaIntentStamp,
    ) -> Result<CastAssetInfo, String> {
        let track_id = media_stamp.track_id;
        // The streaming preference — clamped by THIS renderer's manual cap
        // (#638 fix 4) — governs what we REQUEST, resolved fresh per cast
        // track so a Settings or cap change applies to the very next one.
        let cap_key = {
            let inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                return Err("Cast media request was superseded".to_string());
            }
            inner.connected_cap_key.clone()
        };
        let (quality, request_cause) = effective_cast_quality(cap_key.as_deref());

        // COLD track: serve it PROGRESSIVELY. The whole-file fetch below made
        // a Hi-Res click wait for the entire download before the renderer
        // heard anything (5.5 s for 97 MB measured 2026-08-30, on a fast
        // line; every frontend before this one did the same). The
        // progressive source answers the renderer after the first segments
        // and the finished download lands in the cache like a played stream.
        let core = self.runtime.core();
        if !core.player().is_track_cached(track_id) {
            match core.open_external_stream_resolved(track_id, quality).await {
                Ok(handle) => {
                    if !self.media_intent_is_current(media_stamp).await {
                        let _ = handle.cancel.send(true);
                        return Err("Cast media request was superseded".to_string());
                    }
                    let head = handle.source.get_buffered_data().unwrap_or_default();
                    let probe = probe_streaminfo(&head);
                    let content_type = "audio/flac".to_string();
                    if let Err(error) = self.ensure_media_server(media_stamp).await {
                        let _ = handle.cancel.send(true);
                        return Err(error);
                    }
                    let source: Arc<dyn RangeSource> =
                        Arc::new(BufferedRangeSource(Arc::clone(&handle.source)));
                    let shadow_source = Arc::clone(&handle.source);
                    {
                        let mut inner = self.inner.lock().await;
                        if !media_intent_stamp_matches(&inner, media_stamp) {
                            drop(inner);
                            let _ = handle.cancel.send(true);
                            return Err("Cast media request was superseded".to_string());
                        }
                        let server = inner.media_server.as_mut().ok_or("Media server gone")?;
                        server.register_reader(track_id, handle.total_bytes, &content_type, source);
                        inner.stream_cancel = Some(StampedStreamCancel {
                            media: media_stamp,
                            sender: handle.cancel,
                        });
                    }
                    log::info!(
                        "[qbz-qt][Cast] qobuz track {track_id} served progressively ({} B)",
                        handle.total_bytes
                    );
                    return Ok(CastAssetInfo {
                        content_type,
                        probe,
                        origin: Some(AssetOrigin::Network),
                        requested: Some(quality),
                        request_cause,
                        shadow: Some(crate::cast_viz::ShadowSource::Buffered(shadow_source)),
                    });
                }
                Err(e) => {
                    if !self.media_intent_is_current(media_stamp).await {
                        return Err("Cast media request was superseded".to_string());
                    }
                    log::warn!(
                        "[qbz-qt][Cast] progressive stream for {track_id} failed: {e}; falling back to a full fetch"
                    );
                }
            }
        }

        let asset = core
            // No offline tier / cache sink: `OfflineCacheState` is not wired
            // in the Qt port (see the module header).
            .fetch_for_external_stream_resolved(track_id, quality, None, None)
            .await
            .ok_or_else(|| format!("Could not resolve stream for track {track_id}"))?;
        if !self.media_intent_is_current(media_stamp).await {
            return Err("Cast media request was superseded".to_string());
        }

        log::info!(
            "[qbz-qt][Cast] qobuz track {track_id} resolved from {:?}",
            asset.origin
        );
        let content_type = asset.content_type.clone();
        // Measure BEFORE register_audio moves the bytes.
        let probe = probe_streaminfo(&asset.bytes);
        let origin = asset.origin;

        self.ensure_media_server(media_stamp).await?;
        let bytes = Arc::new(asset.bytes);
        {
            let mut inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                return Err("Cast media request was superseded".to_string());
            }
            let server = inner.media_server.as_mut().ok_or("Media server gone")?;
            server.register_audio_shared(track_id, Arc::clone(&bytes), &content_type);
        }
        Ok(CastAssetInfo {
            content_type,
            probe,
            origin: Some(origin),
            requested: Some(quality),
            request_cause,
            shadow: Some(crate::cast_viz::ShadowSource::Bytes(bytes)),
        })
    }

    /// local: stream the file from disk via register_file (no full-RAM read).
    /// No probe/origin/requested tier — local files are not governed by the
    /// streaming preference and keep the catalog-metadata fallback.
    async fn register_local(
        &self,
        media_stamp: CastMediaIntentStamp,
        path: &str,
    ) -> Result<CastAssetInfo, String> {
        let track_id = media_stamp.track_id;
        self.ensure_media_server(media_stamp).await?;
        let content_type = {
            let mut inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                return Err("Cast media request was superseded".to_string());
            }
            let server = inner.media_server.as_mut().ok_or("Media server gone")?;
            server
                .register_file(track_id, path)
                .map_err(|e| e.to_string())?;
            content_type_for_local(path)
        };
        Ok(CastAssetInfo {
            content_type,
            probe: None,
            origin: None,
            requested: None,
            request_cause: QualityLimit::None,
            shadow: Some(crate::cast_viz::ShadowSource::File(path.into())),
        })
    }

    /// Server-streamed sources (Plex / Jellyfin / Subsonic): probe the item
    /// for its exact size + container, then PROXY it — the media server
    /// answers the renderer's Range requests by fetching the same ranges
    /// from the source with the ticket's request headers. The renderer
    /// never sees the source url (it embeds credentials).
    async fn register_proxy(
        &self,
        media_stamp: CastMediaIntentStamp,
        url: String,
        headers: Vec<(String, String)>,
    ) -> Result<CastAssetInfo, String> {
        let track_id = media_stamp.track_id;
        let info = qbz_player::remote_stream::probe_remote_stream_info_with_headers(&url, &headers)
            .await
            .map_err(|e| format!("Cannot probe track {track_id} for casting: {e}"))?;
        if !self.media_intent_is_current(media_stamp).await {
            return Err("Cast media request was superseded".to_string());
        }
        if info.content_length == 0 {
            return Err(format!(
                "Track {track_id} cannot be cast: its server did not report a size"
            ));
        }
        let content_type = match info.format {
            "FLAC" => "audio/flac",
            "MP3" => "audio/mpeg",
            _ => "application/octet-stream",
        }
        .to_string();
        let probe = Some(AudioParams {
            sample_rate: info.sample_rate,
            bits_per_sample: info.bit_depth,
            channels: info.channels,
        });
        self.ensure_media_server(media_stamp).await?;
        let source: Arc<dyn RangeSource> = Arc::new(HttpRangeSource {
            url: url.clone(),
            headers: headers.clone(),
            handle: tokio::runtime::Handle::current(),
        });
        // The renderer pulls its bytes by Range, at its own pace, so QBZ never
        // holds the file — and the visualizer's shadow decoder needs it. Pull
        // a SECOND, sequential copy into a progressive buffer for the shadow
        // (LAN traffic; capped so a huge item cannot eat the box).
        let (shadow, mut cancel) = if info.content_length <= SHADOW_DOWNLOAD_MAX_BYTES {
            let (buffer, cancel) = shadow_download(url, headers, info.content_length);
            (
                Some(crate::cast_viz::ShadowSource::Buffered(buffer)),
                Some(cancel),
            )
        } else {
            (None, None)
        };
        {
            let mut inner = self.inner.lock().await;
            if !media_intent_stamp_matches(&inner, media_stamp) {
                drop(inner);
                if let Some(cancel) = cancel.take() {
                    let _ = cancel.send(true);
                }
                return Err("Cast media request was superseded".to_string());
            }
            let server = inner.media_server.as_mut().ok_or("Media server gone")?;
            server.register_reader(track_id, info.content_length, &content_type, source);
            inner.stream_cancel = cancel.map(|sender| StampedStreamCancel {
                media: media_stamp,
                sender,
            });
        }
        log::info!(
            "[qbz-qt][Cast] track {track_id} proxied from its server ({} B, {})",
            info.content_length,
            info.format
        );
        Ok(CastAssetInfo {
            content_type,
            probe,
            origin: None,
            requested: None,
            request_cause: QualityLimit::None,
            shadow,
        })
    }

    async fn ensure_media_server(&self, media_stamp: CastMediaIntentStamp) -> Result<(), String> {
        if crate::settings_qt::network()
            .get_settings()
            .map(|settings| settings.block_cast)
            .unwrap_or(false)
        {
            return Err("Casting is blocked by Settings -> Network".to_string());
        }
        let mut inner = self.inner.lock().await;
        if !media_intent_stamp_matches(&inner, media_stamp) {
            return Err("Cast media request was superseded".to_string());
        }
        if inner.media_server.is_none() {
            let server = MediaServer::start().map_err(|e| e.to_string())?;
            inner.media_server = Some(server);
        }
        Ok(())
    }

    // ---- Transport (cast-first gating) --------------------------------------
    //
    // Each returns Ok(false) = "not casting, fall through to the local path",
    // Ok(true) = handled. Call sites live on the playback path — see the
    // report's GLUE NEEDED for `playback_qt.rs`.

    pub(crate) async fn request_playing_if_cast(
        self: &Arc<Self>,
        requested: Option<bool>,
    ) -> Result<bool, String> {
        // Serialize both derivation and execution of a toggle. Two rapid
        // toggles must observe each other's committed target (play, then
        // pause), rather than both deriving `play` from the same old state.
        let media_guard = Arc::clone(&self.media_command_gate).lock_owned().await;
        let (connection, playing, transport_stamp) = {
            let mut inner = self.inner.lock().await;
            let Some(connection) = current_connection_stamp(&inner) else {
                return Ok(false);
            };
            let playing = inner.is_playing;
            if requested == Some(playing) {
                return Ok(true);
            }
            let transport = committed_media_stamp(&inner)
                .and_then(|media| issue_transport_intent(&mut inner, media));
            (connection, playing, transport)
        };
        // Connected with nothing handed to the renderer yet (connected while
        // paused, or the last cast failed): play means "cast the current
        // track", not "resume a media session that does not exist".
        let Some(transport_stamp) = transport_stamp else {
            drop(media_guard);
            match self.runtime.core().current_track().await {
                Some(track) => {
                    self.cast_track_for_connection(&track, Some(connection))
                        .await?;
                }
                None => return Err("Nothing to play".to_string()),
            }
            return Ok(true);
        };
        let applied = if playing {
            self.pause_renderer(transport_stamp, &media_guard).await?
        } else {
            self.play_renderer(transport_stamp, &media_guard).await?
        };
        if applied {
            let mut inner = self.inner.lock().await;
            if transport_stamp_matches(&inner, transport_stamp) {
                inner.is_playing = !playing;
                Self::publish_connection_state_locked(&inner);
            }
        }
        Ok(true)
    }

    /// Seek to a 0..1 fraction of the CURRENT cast track. The seekbar cannot
    /// derive the absolute position from the local engine while casting (it
    /// is stopped, so its duration reads 0 and every drag would restart the
    /// track) — resolve the duration from the catalog metadata instead.
    pub(crate) async fn seek_fraction_if_cast(&self, fraction: f64) -> Result<bool, String> {
        let transport_stamp = {
            let mut inner = self.inner.lock().await;
            let Some(connection) = current_connection_stamp(&inner) else {
                return Ok(false);
            };
            let Some(media) = committed_media_stamp(&inner) else {
                return Ok(true);
            };
            debug_assert_eq!(media.connection, connection);
            issue_transport_intent(&mut inner, media)
                .expect("committed Cast media accepts a transport intent")
        };
        let track = self.runtime.core().current_track().await;
        let dur = track
            .filter(|track| track.id == transport_stamp.media.track_id)
            .map(|track| track.duration_secs as f64)
            .unwrap_or(0.0);
        if dur <= 0.0 {
            // No usable duration — swallow the seek rather than jump to 0.
            self.settle_transport_command(transport_stamp, false).await;
            return Ok(true);
        }
        let secs = (fraction.clamp(0.0, 1.0) * dur).max(0.0);
        let _ = self.seek_secs_if_session(transport_stamp, secs).await?;
        Ok(true)
    }

    pub(crate) async fn set_volume_if_cast(self: &Arc<Self>, volume: f32) -> Result<bool, String> {
        let v = volume.clamp(0.0, 1.0);
        let (connection, spawn_worker) = {
            let mut inner = self.inner.lock().await;
            let Some(connection) = current_connection_stamp(&inner) else {
                return Ok(false);
            };
            // COALESCE: a slider drag arrives as one call per pixel and each
            // one used to be a full SOAP / Cast round trip (~80 in 4 s on the
            // 2026-08-29 smoke). Keep only the newest level and let ONE
            // worker drain it every VOLUME_COALESCE_MS; the last value always
            // reaches the renderer.
            inner.pending_volume = Some(PendingVolume {
                connection,
                value: v,
            });
            let spawn = if volume_worker_matches(&inner, connection) {
                false
            } else {
                inner.volume_worker_connection = Some(connection);
                true
            };
            // Publish under the exact connection lock: a disconnect/B commit
            // cannot interleave this A slider value into the new session.
            crate::now_playing::set_volume(v);
            (connection, spawn)
        };
        if spawn_worker {
            let svc = self.clone();
            crate::spawn(async move { svc.drain_volume(connection).await });
        }
        Ok(true)
    }

    /// The single volume sender: takes the newest pending level, sends it,
    /// waits VOLUME_COALESCE_MS, repeats until nothing is pending.
    async fn drain_volume(self: &Arc<Self>, connection: CastConnectionStamp) {
        loop {
            // The enqueue path's lease ends when the UI call returns. Acquire
            // at the real renderer mutation so a handoff fence drains this
            // network command, not merely the enqueue.
            {
                let mut inner = self.inner.lock().await;
                let pending_matches = matches!(
                    inner.pending_volume,
                    Some(pending) if pending.connection == connection
                );
                if !connection_stamp_matches(&inner, connection) || !pending_matches {
                    if volume_worker_matches(&inner, connection) {
                        inner.volume_worker_connection = None;
                    }
                    return;
                }
            }
            let Some(transport_action) = crate::playback_qt::begin_transport_action() else {
                let mut inner = self.inner.lock().await;
                if matches!(
                    inner.pending_volume,
                    Some(pending) if pending.connection == connection
                ) {
                    inner.pending_volume = None;
                }
                if volume_worker_matches(&inner, connection) {
                    inner.volume_worker_connection = None;
                }
                log::debug!(
                    "[qbz-qt][Cast] queued volume expired during an authority handoff fence"
                );
                return;
            };
            // Volume mutates the same physical session as LOAD, controls and
            // teardown. Joining their total lane makes Stop queue behind an
            // already-issued volume SOAP/Cast command, then start its physical
            // budget only after that command releases the renderer handle.
            let _media_guard = Arc::clone(&self.media_command_gate).lock_owned().await;
            let v = {
                let mut inner = self.inner.lock().await;
                if !connection_stamp_matches(&inner, connection) {
                    if volume_worker_matches(&inner, connection) {
                        inner.volume_worker_connection = None;
                    }
                    return;
                }
                match inner.pending_volume {
                    Some(pending) if pending.connection == connection => {
                        inner.pending_volume = None;
                        pending.value
                    }
                    _ => {
                        if volume_worker_matches(&inner, connection) {
                            inner.volume_worker_connection = None;
                        }
                        return;
                    }
                }
            };
            let result: Result<(), String> = match connection.protocol {
                CastProtocol::Chromecast => {
                    let handle = {
                        let inner = self.inner.lock().await;
                        connection_stamp_matches(&inner, connection)
                            .then(|| inner.chromecast.clone())
                            .flatten()
                    };
                    match handle {
                        Some(handle) => Self::run_chromecast_call(
                            handle,
                            CHROMECAST_COMMAND_BUDGET,
                            "chromecast-volume",
                            move |handle| handle.set_volume(v),
                        )
                        .await
                        .map_err(|e| e.to_string()),
                        None => Ok(()),
                    }
                }
                CastProtocol::Dlna => {
                    let handle = {
                        let inner = self.inner.lock().await;
                        connection_stamp_matches(&inner, connection)
                            .then(|| inner.dlna.clone())
                            .flatten()
                    };
                    if let Some(handle) = handle {
                        let mut handle = handle.lock().await;
                        if connection_stamp_matches(&*self.inner.lock().await, connection) {
                            handle.set_volume(v).await.map_err(|e| e.to_string())
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(e) = result {
                log::warn!("[qbz-qt][Cast] set volume failed: {e}");
            }
            drop(transport_action);
            tokio::time::sleep(std::time::Duration::from_millis(VOLUME_COALESCE_MS)).await;
        }
    }

    // NOTE: next/previous are intentionally NOT gated. While casting, the
    // local advance flow still runs (it moves the core cursor + refreshes the
    // card/queue) and the play route casts the new current track. A cast-only
    // advance would desync the UI cursor from the renderer.

    /// Apply a delayed seek only while its original cast connection and
    /// track are still current. The caller holds an owner action; this lock
    /// keeps disconnect/reconnect from changing the session between the
    /// validation and the renderer command.
    async fn seek_secs_if_session(
        &self,
        stamp: CastTransportIntentStamp,
        secs: f64,
    ) -> Result<bool, String> {
        if !self.activate_transport_command(stamp).await {
            return Ok(false);
        }
        // Share the total media lane with LOAD. Otherwise a Chromecast seek
        // sent for T1 can arrive after a concurrent T2 LOAD and move T2.
        let _media_guard = Arc::clone(&self.media_command_gate).lock_owned().await;
        if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
            return Ok(false);
        }
        let result: Result<(), String> = match stamp.media.connection.protocol {
            CastProtocol::Chromecast => {
                let handle = self.inner.lock().await.chromecast.clone();
                match handle {
                    Some(handle) => Self::run_chromecast_call(
                        handle,
                        CHROMECAST_COMMAND_BUDGET,
                        "chromecast-seek",
                        move |handle| handle.seek(secs),
                    )
                    .await
                    .map_err(|e| e.to_string()),
                    None => Ok(()),
                }
            }
            CastProtocol::Dlna => {
                let connection = self.inner.lock().await.dlna.clone();
                if let Some(connection) = connection {
                    let mut connection = connection.lock().await;
                    if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
                        return Ok(false);
                    }
                    connection
                        .seek(secs.max(0.0) as u64)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    Ok(())
                }
            }
        };
        let applied = self.settle_transport_command(stamp, result.is_ok()).await;
        result?;
        Ok(applied)
    }

    async fn play_renderer(
        &self,
        stamp: CastTransportIntentStamp,
        _media_guard: &OwnedMutexGuard<()>,
    ) -> Result<bool, String> {
        // A receiver command mutates whichever media session is current when
        // it lands. Serialize it with LOAD and bind it to the exact media
        // epoch so a delayed T1 command can never alter T2.
        if !self.activate_transport_command(stamp).await {
            return Ok(false);
        }
        if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
            return Ok(false);
        }
        let result: Result<(), String> = match stamp.media.connection.protocol {
            CastProtocol::Chromecast => {
                let handle = self.inner.lock().await.chromecast.clone();
                match handle {
                    Some(handle) => Self::run_chromecast_call(
                        handle,
                        CHROMECAST_COMMAND_BUDGET,
                        "chromecast-play",
                        |handle| handle.play(),
                    )
                    .await
                    .map_err(|e| e.to_string()),
                    None => Ok(()),
                }
            }
            CastProtocol::Dlna => {
                let handle = self.inner.lock().await.dlna.clone();
                if let Some(handle) = handle {
                    let mut handle = handle.lock().await;
                    if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
                        return Ok(false);
                    }
                    handle.play().await.map_err(|e| e.to_string())
                } else {
                    Ok(())
                }
            }
        };
        let applied = self.settle_transport_command(stamp, result.is_ok()).await;
        result?;
        Ok(applied)
    }

    async fn pause_renderer(
        &self,
        stamp: CastTransportIntentStamp,
        _media_guard: &OwnedMutexGuard<()>,
    ) -> Result<bool, String> {
        if !self.activate_transport_command(stamp).await {
            return Ok(false);
        }
        if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
            return Ok(false);
        }
        let result: Result<(), String> = match stamp.media.connection.protocol {
            CastProtocol::Chromecast => {
                let handle = self.inner.lock().await.chromecast.clone();
                match handle {
                    Some(handle) => Self::run_chromecast_call(
                        handle,
                        CHROMECAST_COMMAND_BUDGET,
                        "chromecast-pause",
                        |handle| handle.pause(),
                    )
                    .await
                    .map_err(|e| e.to_string()),
                    None => Ok(()),
                }
            }
            CastProtocol::Dlna => {
                let handle = self.inner.lock().await.dlna.clone();
                if let Some(handle) = handle {
                    let mut handle = handle.lock().await;
                    if !transport_stamp_matches(&*self.inner.lock().await, stamp) {
                        return Ok(false);
                    }
                    handle.pause().await.map_err(|e| e.to_string())
                } else {
                    Ok(())
                }
            }
        };
        let applied = self.settle_transport_command(stamp, result.is_ok()).await;
        result?;
        Ok(applied)
    }

    // ---- Position poll + ended detection ------------------------------------

    fn start_position_poll(self: &Arc<Self>, connection: CastConnectionStamp) {
        let svc = self.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(POSITION_POLL_INTERVAL_MS))
                    .await;
                if !connection_stamp_matches(&*svc.inner.lock().await, connection) {
                    break;
                }
                svc.poll_once(connection).await;
            }
        });
        if let Ok(mut slot) = self.poll_task.lock() {
            if let Some(old) = slot.replace(task) {
                old.abort();
            }
        }
    }

    async fn poll_once(self: &Arc<Self>, connection: CastConnectionStamp) {
        let poll_snapshot = {
            let inner = self.inner.lock().await;
            let Some(snapshot) = current_poll_snapshot(&inner, connection) else {
                return;
            };
            snapshot
        };
        // Capture BEFORE queuing on the total lane. Controls publish an
        // in-flight transport epoch first; therefore an older poll either sees
        // the marker and stays out, or its pre-command snapshot fails this
        // revalidation after it acquires the lane.
        let media_guard = Arc::clone(&self.media_command_gate).lock_owned().await;
        if !poll_snapshot_matches(&*self.inner.lock().await, poll_snapshot) {
            return;
        }

        // Read position/state from the active renderer.
        let read = match connection.protocol {
            CastProtocol::Chromecast => {
                // TYPED read, not `.ok()`: before anything is loaded there is
                // no media session, and `get_media_position` says so with
                // `NoMediaSession` without touching the network. That is an
                // IDLE receiver, not a lost one — the old `.ok()` folded it
                // into "no answer" and every connect made while paused was
                // dropped after exactly LOST_POLL_MAX seconds (2026-08-29
                // smoke, three times in a row). While idle, liveness is
                // proven by the receiver-level `get_status` request/response
                // instead, which needs no media session.
                let handle = {
                    let inner = self.inner.lock().await;
                    poll_snapshot_matches(&inner, poll_snapshot)
                        .then(|| inner.chromecast.clone())
                        .flatten()
                };
                let info: Result<CastPositionInfo, CastError> = match handle {
                    Some(handle) => match Self::run_chromecast_call(
                        handle.clone(),
                        CHROMECAST_COMMAND_BUDGET,
                        "chromecast-position",
                        |handle| handle.get_media_position(),
                    )
                    .await
                    {
                        Err(CastError::NoMediaSession) => {
                            match Self::run_chromecast_call(
                                handle,
                                CHROMECAST_COMMAND_BUDGET,
                                "chromecast-status",
                                |handle| handle.get_status(),
                            )
                            .await
                            {
                                Ok(_) => {
                                    let mut inner = self.inner.lock().await;
                                    if poll_snapshot_matches(&inner, poll_snapshot) {
                                        inner.lost_polls = 0;
                                    }
                                    return;
                                }
                                Err(error) => Err(error),
                            }
                        }
                        other => other,
                    },
                    None => Err(CastError::NotConnected),
                };
                match info {
                    Ok(i) => {
                        let st = i.player_state.to_uppercase();
                        let playing = st == "PLAYING";
                        Some((i.position_secs, i.duration_secs, st, playing))
                    }
                    Err(e) => {
                        log::debug!("[qbz-qt][Cast] chromecast read failed: {e}");
                        None
                    }
                }
            }
            CastProtocol::Dlna => {
                let handle = {
                    let inner = self.inner.lock().await;
                    if !poll_snapshot_matches(&inner, poll_snapshot) {
                        return;
                    }
                    inner.dlna.clone()
                };
                let info: Option<DlnaPositionInfo> = if let Some(handle) = handle {
                    let handle = handle.lock().await;
                    if !poll_snapshot_matches(&*self.inner.lock().await, poll_snapshot) {
                        return;
                    }
                    match handle.get_position_info().await {
                        Ok(info) => Some(info),
                        // Bounded by LOST_POLL_MAX consecutive failures (the
                        // session is dropped after that), so this cannot
                        // flood; before #733 the error was swallowed and an
                        // INFO log could not tell a mute renderer from one
                        // that never reports STOPPED.
                        Err(e) => {
                            log::warn!("[qbz-qt][Cast] DLNA position read failed: {e}");
                            None
                        }
                    }
                } else {
                    None
                };
                info.map(|i| {
                    let st = i.transport_state.to_uppercase();
                    let playing = st == "PLAYING";
                    (i.position_secs as f64, i.duration_secs as f64, st, playing)
                })
            }
        };

        // DEVICE DISAPPEARED (the one addition over the Slint service, which
        // simply skips the tick): a renderer that has been unplugged /
        // rebooted / left the network answers nothing. Skipping forever would
        // leave the picker claiming a live connection and the bar frozen, so
        // after LOST_POLL_MAX consecutive failures the session is torn down
        // and the raw reason is surfaced on the picker's error line.
        let Some((position, duration, state, playing)) = read else {
            let (lost, name) = {
                let mut inner = self.inner.lock().await;
                if !poll_snapshot_matches(&inner, poll_snapshot) {
                    return;
                }
                inner.lost_polls += 1;
                let snapshot = (
                    inner.lost_polls,
                    inner.connected_device_name.clone().unwrap_or_default(),
                );
                if snapshot.0 >= LOST_POLL_MAX {
                    set_error(format!("Lost connection to {}", snapshot.1));
                }
                snapshot
            };
            if lost >= LOST_POLL_MAX {
                log::warn!(
                    "[qbz-qt][Cast] {name} stopped answering after {lost} polls — dropping the session"
                );
                // Tear down from ANOTHER task on purpose: `disconnect` aborts
                // the poll task, and this IS the poll task — awaiting it here
                // would cancel `disconnect` mid-way (at its first await after
                // the abort) and leave the UI still claiming a connection.
                let svc = self.clone();
                crate::spawn(async move {
                    svc.disconnect_if_poll_snapshot(poll_snapshot).await;
                });
            } else {
                log::debug!("[qbz-qt][Cast] position read failed ({lost}/{LOST_POLL_MAX})");
            }
            return;
        };
        {
            let mut inner = self.inner.lock().await;
            if !poll_snapshot_matches(&inner, poll_snapshot) {
                return;
            }
            inner.lost_polls = 0;
        }

        // Many DLNA renderers report TrackDuration as 0 / NOT_IMPLEMENTED,
        // which left the seekbar permanently full. Fall back to the track's
        // catalog duration (the renderer's position stays authoritative).
        let duration = if duration > 0.0 {
            duration
        } else {
            self.runtime
                .core()
                .current_track()
                .await
                .filter(|track| {
                    poll_snapshot
                        .media
                        .is_some_and(|media| media.track_id == track.id)
                })
                .map(|t| t.duration_secs as f64)
                .unwrap_or(0.0)
        };

        // Track-end detection: Chromecast {PLAYING,BUFFERING} -> IDLE; DLNA
        // PLAYING -> {STOPPED, NO_MEDIA_PRESENT}. One-shot latch, reset on
        // PLAYING. For DLNA a bare STOPPED is ambiguous (a renderer that
        // hiccups mid-track also reports STOPPED), so it only counts as
        // end-of-track when the track actually reached near its end —
        // guarded by the max position observed while PLAYING.
        let max_position;
        let ended = {
            let mut inner = self.inner.lock().await;
            if !poll_snapshot_matches(&inner, poll_snapshot) {
                return;
            }
            inner.is_playing = playing;
            if state == "PLAYING" {
                inner.cast_saw_playing = true;
                inner.cast_max_position = inner.cast_max_position.max(position);
            }
            max_position = inner.cast_max_position;
            let ended = match connection.protocol {
                // IDLE counts as "ended" only after this track was seen
                // PLAYING: a receiver app that is idle because nothing was
                // ever loaded (or the load failed) reported IDLE too, and the
                // poll auto-advanced through the whole queue on it.
                CastProtocol::Chromecast => {
                    state == "IDLE" && inner.cast_saw_playing && !inner.track_end_detected
                }
                CastProtocol::Dlna => {
                    let stopped = matches!(state.as_str(), "STOPPED" | "NO_MEDIA_PRESENT");
                    // The guard only makes sense when the position signal is
                    // usable: renderers whose RelTime never moves honor
                    // STOPPED like pre-guard behavior.
                    let position_reliable = inner.cast_max_position > CAST_POSITION_SIGNAL_MIN_SECS;
                    let near_end = duration <= 0.0
                        || !position_reliable
                        || max_position >= duration - CAST_END_GUARD_SECS;
                    if stopped && inner.cast_saw_playing && !near_end {
                        inner.cast_premature_stop_polls += 1;
                        log::warn!(
                            "[qbz-qt][Cast] premature STOPPED {}/{} — not advancing yet \
                             (state={state}, max_position={max_position:.1}, \
                             duration={duration:.1})",
                            inner.cast_premature_stop_polls,
                            CAST_PREMATURE_STOP_POLLS_MAX
                        );
                    } else if !stopped {
                        inner.cast_premature_stop_polls = 0;
                    }
                    let persistent_stop =
                        inner.cast_premature_stop_polls >= CAST_PREMATURE_STOP_POLLS_MAX;
                    // End-stall (#733): a renderer that reaches the last
                    // seconds and then freezes its RelTime without ever
                    // reporting STOPPED (or while its transport query
                    // faults -> UNKNOWN) would park the queue forever. Only
                    // with a real position signal, and never on a pause.
                    let inside_end_guard = position_reliable
                        && duration > 0.0
                        && max_position >= duration - CAST_END_GUARD_SECS;
                    let stalled_at_end =
                        end_stall_tick(&mut inner.end_stall, &state, position, inside_end_guard);
                    if stalled_at_end && inner.cast_saw_playing && !inner.track_end_detected {
                        log::warn!(
                            "[qbz-qt][Cast] position frozen at the end for {} polls without \
                             STOPPED (state={state}, position={position:.1}, \
                             duration={duration:.1}) — treating as track end",
                            inner.end_stall.polls
                        );
                    }
                    ((stopped && (near_end || persistent_stop)) || stalled_at_end)
                        && inner.cast_saw_playing
                        && !inner.track_end_detected
                }
            };
            if state == "PLAYING" {
                inner.track_end_detected = false;
            } else if ended {
                inner.track_end_detected = true;
            }
            // Publish the exact poll result while the validation lock is still
            // held; a newer media/connection intent cannot interleave stale UI.
            crate::viz_qt::set_paused(!playing);
            crate::now_playing::set_position(position as i32, duration as i32, playing, 0.0, true);
            push_position(position as f32, duration as f32, playing);
            crate::cast_viz::anchor(position, playing);
            ended
        };

        log::debug!(
            "[qbz-qt][Cast] poll: state={state} position={position:.1} duration={duration:.1} \
             max_position={max_position:.1}"
        );

        drop(media_guard);
        if ended {
            log::info!(
                "[qbz-qt][Cast] track ended (state={state}, position={position:.1}, \
                 duration={duration:.1}, max_position={max_position:.1}); auto-advancing"
            );
            self.advance(poll_snapshot).await;
        }
    }

    /// End-of-track advance while casting. Moves the core cursor + refreshes
    /// the card/queue exactly like the local advance, then casts the new
    /// current track instead of opening a local stream.
    async fn advance(self: &Arc<Self>, expected_poll: CastPollSnapshot) {
        let Some(expected_media) = expected_poll.media else {
            return;
        };
        let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
            return;
        };
        let runtime = self.runtime.clone();
        // Keep the exact media lock edge across the queue cursor mutation, then
        // publish the successor intent before releasing it. A user T2 arriving
        // after this edge legitimately wins; an old poll can never move the
        // cursor after T2 already exists.
        let (track, media_stamp) = {
            let mut inner = self.inner.lock().await;
            if !poll_snapshot_matches(&inner, expected_poll) {
                return;
            }
            let Some(track) = runtime.core().next_track().await else {
                log::info!("[qbz-qt][Cast] queue finished");
                crate::now_playing::set_playing(false);
                inner.is_playing = false;
                return;
            };
            inner.media_intent_epoch = inner.media_intent_epoch.wrapping_add(1).max(1);
            inner.transport_intent_epoch = inner.transport_intent_epoch.wrapping_add(1).max(1);
            inner.transport_in_flight_epoch = None;
            inner.media_intent_track_id = Some(track.id);
            inner.current_track_id = None;
            inner.is_playing = false;
            inner.track_end_detected = false;
            let media_stamp = CastMediaIntentStamp {
                connection: expected_media.connection,
                media_intent_epoch: inner.media_intent_epoch,
                track_id: track.id,
            };
            (track, media_stamp)
        };
        crate::playback_qt::refresh_now_playing(&runtime).await;
        crate::playback_qt::publish_queue(&runtime).await;
        if let Err(e) = self.run_media_command(&track, media_stamp, true).await {
            log::warn!("[qbz-qt][Cast] advance to {} failed: {e}", track.id);
        }
    }

    // ---- Shutdown (logout / app exit) ---------------------------------------

    /// Tear everything down: stop the renderer, abort the poll, drop discovery
    /// and the media server, so a cast device does not keep playing after
    /// logout or exit (Tauri parity, #32/#33).
    pub(crate) async fn shutdown(self: &Arc<Self>) {
        let shutdown_epoch = self.begin_transition_intent();
        let _transition_guard = Arc::clone(&self.transition_gate).lock_owned().await;
        self.mark_transition_lane_if_current(shutdown_epoch);
        // Poll first, without the lock — see `poll_task`.
        self.abort_poll();
        crate::cast_viz::stop();
        // Terminal teardown is deliberately privileged: invalidate delayed
        // work before Stop, independently of interactive authority.
        let (mut blocking, dlna) = {
            let mut inner = self.inner.lock().await;
            inner.connection_epoch = inner.connection_epoch.wrapping_add(1).max(1);
            invalidate_media_intent(&mut inner);
            inner.pending_volume = None;
            inner.volume_worker_connection = None;
            if let Some(task) = inner.discovery_task.take() {
                task.abort();
            }
            let blocking = BlockingTeardown {
                chromecast: inner.chromecast.take(),
                chromecast_discovery: inner.chromecast_discovery.take(),
                dlna_discovery: inner.dlna_discovery.take(),
                media_server: None,
            };
            let dlna = inner.dlna.take();
            inner.protocol = None;
            inner.connected_device_ip = None;
            inner.connected_device_name = None;
            inner.connected_device_id = None;
            inner.connected_cap_key = None;
            inner.is_playing = false;
            inner.qconnect_restore_token = None;
            (blocking, dlna)
        };
        let cleanup_service = Arc::clone(self);
        let shutdown_safe = self
            .run_media_teardown_bounded("shutdown", async move {
                let _media_guard = Arc::clone(&cleanup_service.media_command_gate)
                    .lock_owned()
                    .await;
                let cancel = {
                    let mut inner = cleanup_service.inner.lock().await;
                    let cancel = inner.stream_cancel.take().map(|cancel| cancel.sender);
                    blocking.media_server = inner.media_server.take();
                    cancel
                };
                if let Some(cancel) = cancel {
                    let _ = cancel.send(true);
                }

                // Blocking resources and the async DLNA Stop run concurrently
                // under the same outer caller budget.
                let (blocking_safe, renderer_safe) = tokio::join!(
                    cleanup_service.run_blocking_teardown(blocking),
                    cleanup_service.teardown_detached_renderer(DetachedRenderer {
                        dlna,
                        ..DetachedRenderer::default()
                    })
                );
                blocking_safe && renderer_safe
            })
            .await;
        if !shutdown_safe {
            log::warn!("[qbz-qt][Cast] shutdown returned with physical teardown fenced");
        }
    }

    // ---- State push to the UI -----------------------------------------------

    async fn push_connection_state(&self) {
        let inner = self.inner.lock().await;
        Self::publish_connection_state_locked(&inner);
    }

    /// Publish while the caller holds the exact state lock. Media commit uses
    /// this form so a newer intent cannot land between validation and queued UI
    /// writes.
    fn publish_connection_state_locked(inner: &CastInner) {
        let connected = inner.protocol.is_some();
        let protocol = inner
            .protocol
            .map(|p| p.as_str().to_string())
            .unwrap_or_default();
        let name = inner.connected_device_name.clone().unwrap_or_default();
        let playing = inner.is_playing;
        if connected {
            crate::viz_qt::set_paused(!playing);
        }
        // The now-playing flags the bars already render: np_cast_active,
        // np_cast_protocol and np_cast_target all come from this ONE call.
        //
        // np_is_remote is deliberately NOT touched — it means "a Qobuz Connect
        // PEER owns the transport", which is a different thing, and the stamps
        // discriminate on exactly that: SongCardStamp.qml:284 and
        // AudioStamp.qml:82 gate the purple CAST/DLNA badge on
        // `npCastActive && !npIsRemote`, so raising is_remote here would HIDE
        // the badge this wiring exists to light. In the Slint the flag is
        // written only by `qconnect_event_sink.rs`; same split here.
        crate::now_playing::set_cast_session(connected, &protocol, &name);
        if connected {
            crate::now_playing::set_playing(playing);
        }

        let (p, n) = (protocol, name);
        cast_bridge::ui(move |mut b| {
            b.as_mut().set_connected(connected);
            b.as_mut()
                .set_protocol(cxx_qt_lib::QString::from(p.as_str()));
            b.as_mut()
                .set_device_name(cxx_qt_lib::QString::from(n.as_str()));
            b.as_mut().set_is_playing(playing);
        });
    }

    /// Publish the measured delivered-quality state the SKIPPED local poll
    /// would have published (#638 fix 1, cast half), plus the over-cap
    /// disclosure for the picker line — bytes that already existed locally
    /// are served AS-IS even above the requested tier, never resampled and
    /// never re-fetched, and the UI says so instead of hiding it.
    fn publish_measured_badge(&self, info: &CastAssetInfo) {
        let (eff_rate_hz, eff_bits) = info
            .probe
            .map(|p| (p.sample_rate, p.bits_per_sample))
            .unwrap_or((0, 0));
        // `quality_state` holds the catalog max seeded by
        // `playback_qt::refresh_now_playing` for this same track.
        let delivered = crate::quality_state::evaluate(eff_rate_hz, eff_bits);
        // The cast's own request-time cause REPLACES the local one: the local
        // DAC cap is never a cast cause (#638 precedence rule), and only this
        // path knows whether the renderer cap shaped the request.
        let requested_id = info.requested.map(|q| q.id()).unwrap_or(0);
        let limit_cause = crate::quality_state::classify_limit_cause(
            delivered.downgraded,
            requested_id,
            info.request_cause as i32,
            eff_bits,
        );
        // Tier token for the stamp's main line. The shared helper's
        // requested-mp3 shortcut is for the LOCAL engine path; here a
        // successful STREAMINFO parse proves the bytes are FLAC, never MP3.
        let effective_tier = if delivered.downgraded && info.probe.is_some() {
            if eff_bits >= 24 { "hires" } else { "cd" }.to_string()
        } else {
            delivered.effective_tier.clone()
        };
        crate::now_playing::set_cast_delivered(crate::quality_state::Delivered {
            downgraded: delivered.downgraded,
            true_detail: delivered.true_detail,
            limit_cause,
            effective_tier,
        });

        // Over-cap: locally-existing bytes ABOVE the requested tier.
        let measured_tier = info.probe.map(|p| {
            if p.bits_per_sample >= 24 && p.sample_rate > 96_000 {
                Quality::UltraHiRes
            } else if p.bits_per_sample >= 24 {
                Quality::HiRes
            } else {
                Quality::Lossless
            }
        });
        let over_cap = matches!(info.origin, Some(o) if o != AssetOrigin::Network)
            && matches!((measured_tier, info.requested), (Some(m), Some(r)) if m > r);
        let origin_str = match info.origin {
            Some(AssetOrigin::Cache) => "cache",
            Some(AssetOrigin::Offline) => "offline",
            _ => "",
        };
        if over_cap {
            log::info!(
                "[qbz-qt][Cast] serving {origin_str} bytes above the requested tier \
                 (measured {measured_tier:?} > requested {:?}) — caps govern requests \
                 only; local bytes go out as-is, never resampled",
                info.requested
            );
        }
        cast_bridge::ui(move |mut b| {
            b.as_mut().set_quality_limit_cause(limit_cause);
            b.as_mut().set_quality_over_cap(over_cap);
            b.as_mut()
                .set_quality_origin(cxx_qt_lib::QString::from(origin_str));
        });
    }

    /// Push the per-renderer cap row for the connected device (#638 fix 4):
    /// whether a cap can be offered at all (stable identity only), the
    /// ui_prefs key the picker hands back, and the current index (0 = follow
    /// the app setting — the absent-entry default).
    async fn push_device_cap_row(&self) {
        let cap_key = self.inner.lock().await.connected_cap_key.clone();
        let index = cap_key.as_deref().map(cap_index_for_key).unwrap_or(0);
        cast_bridge::ui(move |mut b| {
            b.as_mut().set_device_cap_available(cap_key.is_some());
            b.as_mut().set_device_cap_key(cxx_qt_lib::QString::from(
                cap_key.unwrap_or_default().as_str(),
            ));
            b.as_mut().set_device_cap_index(index);
        });
    }

    /// Persist the user's manual cap choice for `cap_key` (#638 fix 4): index
    /// 0 removes the entry (follow the app setting), 1/2/3 store
    /// hires/cd/mp3. Enforcement is REQUEST-TIME only — the next cast resolve
    /// picks the change up; nothing is cleared, re-fetched or restarted (a
    /// per-device cap must never punish the global cache or break an
    /// in-flight cast).
    pub(crate) async fn set_device_cap(&self, cap_key: String, index: i32) {
        if cap_key.is_empty() {
            return;
        }
        // Keep the connected-renderer identity stable through the synchronous
        // preference patch and UI publication. A queued A callback must not
        // write A's key with B's name or alter B's visible selector.
        let inner = self.inner.lock().await;
        if current_connection_stamp(&inner).is_none()
            || inner.connected_cap_key.as_deref() != Some(cap_key.as_str())
        {
            return;
        }
        let tier = match index {
            1 => Some("hires"),
            2 => Some("cd"),
            3 => Some("mp3"),
            _ => None,
        };
        let name = inner.connected_device_name.clone().unwrap_or_default();
        let mut caps = cast_caps();
        match tier {
            Some(t) => {
                caps.insert(
                    cap_key.clone(),
                    serde_json::json!({ "tier": t, "name": name }),
                );
                log::info!("[qbz-qt][Cast] quality cap for {name} ({cap_key}) -> {t}");
            }
            None => {
                caps.remove(&cap_key);
                log::info!(
                    "[qbz-qt][Cast] quality cap for {name} ({cap_key}) removed — follows the app setting"
                );
            }
        }
        // Additive single-key patch of the SHARED ui_prefs.json — the same
        // file and the same `{tier, name}` shape the Slint frontend writes,
        // so a cap set in either frontend applies in both.
        crate::settings_qt::save_pref("cast_quality_caps", serde_json::Value::Object(caps));

        let index = index.clamp(0, 3);
        cast_bridge::ui(move |mut b| {
            b.as_mut().set_device_cap_index(index);
        });
    }
}

// ---------------------------------------------------------------------------
// Entry points for the bridge (each hops onto the tokio runtime)
// ---------------------------------------------------------------------------

/// Whether the cast picker currently owns discovery.
///
/// Rust-side mirror of the `picker_open` qproperty, kept here because the
/// Diagnostics panel's on-demand scan has to decide whether stopping discovery
/// would kill a LIVE picker's device list — the reference reads
/// `CastState.picker-open` on the UI thread for exactly that gate
/// (`crates/qbz/src/diagnostics.rs:284-291`), and a qproperty cannot be read
/// off the Qt thread.
static PICKER_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// See [`PICKER_OPEN`].
pub(crate) fn picker_open() -> bool {
    PICKER_OPEN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Picker opened: arm discovery. This is the ONLY trigger — nothing scans in
/// the background and nothing touches the network before this runs.
pub(crate) fn open_picker() {
    PICKER_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
    set_error(String::new());
    let svc = service();
    crate::spawn(async move {
        svc.start_discovery().await;
    });
}

/// Picker closed: stop both discoveries. An active cast is untouched.
pub(crate) fn close_picker() {
    PICKER_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    let svc = service();
    crate::spawn(async move {
        svc.stop_discovery().await;
    });
}

/// "Search again": re-arm the scan window + the refresh loop.
pub(crate) fn refresh() {
    let svc = service();
    crate::spawn(async move {
        svc.start_discovery().await;
    });
}

pub(crate) fn connect(device_id: String, protocol: String) {
    let svc = service();
    let Some(protocol) = CastProtocol::from_str(&protocol) else {
        set_error(format!("Unknown cast protocol: {protocol}"));
        return;
    };
    // Publish the exact request synchronously with the UI callback. The async
    // result may update presentation only while this epoch is still current.
    let transition_epoch = svc.begin_transition_intent();
    crate::spawn(async move {
        if let Err(e) = svc
            .connect_exact(device_id, protocol, transition_epoch)
            .await
        {
            if !svc.transition_is_current(transition_epoch) {
                log::debug!("[qbz-qt][Cast] stale connect result suppressed: {e}");
            } else {
                log::warn!("[qbz-qt][Cast] connect failed: {e}");
                set_error(e);
            }
        }
    });
}

pub(crate) fn disconnect() {
    let svc = service();
    // Publish at the UI callback boundary, just like connect(). Otherwise a
    // later connect click can publish before this spawned task first polls and
    // be incorrectly superseded by the older disconnect gesture.
    let transition_epoch = svc.begin_transition_intent();
    crate::spawn(async move {
        svc.disconnect_exact(transition_epoch).await;
    });
}

pub(crate) fn set_device_cap(cap_key: String, index: i32) {
    let svc = service();
    crate::spawn(async move {
        svc.set_device_cap(cap_key, index).await;
    });
}

/// Seed/refresh the cap dropdown labels + the live global-quality label.
/// Called from the bridge's `boot` and re-callable on a language or
/// streaming-quality change (option 0 embeds the live global label, and
/// Rust-pushed option models never re-translate on their own).
pub(crate) fn push_cap_options() {
    let key = crate::settings_qt::streaming_quality();
    let idx = crate::settings_qt::STREAMING_QUALITY_KEYS
        .iter()
        .position(|k| *k == key)
        .unwrap_or(3);
    // Tier names ("Hi-Res+", "CD Quality") are product names — untranslated
    // data, the same convention as the Settings streaming dropdown.
    let global_label = crate::settings_qt::STREAMING_QUALITY_LABELS[idx];
    let label_for = |k: &str| {
        crate::settings_qt::STREAMING_QUALITY_KEYS
            .iter()
            .position(|x| *x == k)
            .map(|i| crate::settings_qt::STREAMING_QUALITY_LABELS[i])
            .unwrap_or("")
    };
    // Index order matches `set_device_cap`: 0 follow · 1 hires · 2 cd · 3 mp3.
    let options = serde_json::json!([
        qbz_i18n::t_args("Follow app setting ({})", &[global_label]),
        label_for("hires"),
        label_for("cd"),
        label_for("mp3"),
    ]);
    let json = options.to_string();
    let global = global_label.to_string();
    cast_bridge::ui(move |mut b| {
        b.as_mut()
            .set_device_cap_options(cxx_qt_lib::QString::from(json.as_str()));
        b.as_mut()
            .set_global_quality_label(cxx_qt_lib::QString::from(global.as_str()));
    });
}

// ---------------------------------------------------------------------------
// Playback-path gates (call sites are GLUE in playback_qt.rs)
// ---------------------------------------------------------------------------

/// True while a renderer owns transport.
pub(crate) async fn is_casting() -> bool {
    service().is_casting().await
}

/// Publish a QConnect renderer intent synchronously. Interactive QConnect
/// connects call this before renewing their enabled epoch, giving Cast and
/// QConnect one total cross-domain order before either side performs awaits.
pub(crate) fn begin_qconnect_start_intent() -> CastTransitionEpoch {
    service().begin_transition_intent()
}

/// Revalidate one exact Cast/QConnect transition without acquiring a lock.
/// Cast uses this immediately before its enabled-intent CAS.
pub(crate) fn qconnect_start_intent_is_current(expected: CastTransitionEpoch) -> bool {
    service().transition_is_current(expected)
}

/// Acquire the handoff lane for an already-published QConnect intent.
pub(crate) async fn disconnect_before_qconnect_start_exact(
    expected: CastTransitionEpoch,
) -> Result<CastQconnectTransitionLease, String> {
    let svc = service();
    disconnect_before_qconnect_start_at(svc, expected).await
}

/// Resume the QConnect session suspended by one exact Cast connection. A newer
/// Cast/QConnect intent invalidates `expected`, so a late restore can never
/// tear down the renderer that superseded it.
pub(crate) async fn disconnect_before_qconnect_restore(
    expected: CastTransitionEpoch,
) -> Result<CastQconnectTransitionLease, String> {
    let svc = service();
    disconnect_before_qconnect_start_at(svc, expected).await
}

async fn disconnect_before_qconnect_start_at(
    svc: Arc<CastService>,
    epoch: CastTransitionEpoch,
) -> Result<CastQconnectTransitionLease, String> {
    let guard = Arc::clone(&svc.transition_gate).lock_owned().await;
    svc.mark_transition_lane_if_current(epoch);
    if !svc.transition_is_current(epoch) {
        return Err("Qobuz Connect start was superseded by a newer Cast request".to_string());
    }
    if !svc.await_physical_teardown(epoch).await {
        return Err("A previous cast renderer teardown is still incomplete".to_string());
    }
    if svc.is_casting().await {
        log::info!("[qbz-qt][Cast] QConnect start requested; disconnecting cast renderer first");
        let Some(outcome) = svc.teardown_renderer().await else {
            return Err("Cast renderer authority could not be fenced".to_string());
        };
        // The outer QConnect connect is already the requested restoration.
        // Publish cast teardown without calling the normal restore seam.
        svc.publish_disconnected_state().await;
        if let Some(token) = outcome.restore_qconnect {
            // Retain the obligation until the outer QConnect startup commits.
            // Manual starts may consume any carried latch; automatic restores
            // must present this exact token.
            svc.inner.lock().await.qconnect_restore_token = Some(token);
        }
        if !outcome.physical_safe {
            if let Some(token) = outcome.restore_qconnect {
                svc.supervise_late_qconnect_restore(epoch, token);
            }
            return Err("Cast renderer physical teardown is incomplete".to_string());
        }
    }
    if !svc.transition_is_current(epoch) {
        Err("Qobuz Connect start was superseded by a newer Cast request".to_string())
    } else if !svc.physical_teardown_safe() {
        Err("A previous cast renderer teardown is still incomplete".to_string())
    } else if svc.is_casting().await {
        let message =
            "Cannot start Qobuz Connect while the cast renderer is still active".to_string();
        log::warn!("[qbz-qt][Cast] {message}");
        Err(message)
    } else {
        Ok(CastQconnectTransitionLease {
            service: svc,
            epoch,
            _guard: guard,
        })
    }
}

/// Route THIS track to the connected renderer instead of opening a local
/// stream. false = not casting, take the local path. Used by the funnels
/// that hold the track before the queue cursor moves (the Local Library
/// stages its queue AFTER the audible step succeeds) — routing "the current
/// track" there cast whatever the previous queue was pointing at.
pub(crate) async fn play_track_if_cast(track: &QueueTrack) -> bool {
    let svc = service();
    if !svc.is_casting().await {
        return false;
    }
    if let Err(e) = svc.cast_track(track).await {
        log::warn!("[qbz-qt][Cast] play track {} failed: {e}", track.id);
    }
    true
}

/// Route the queue's CURRENT track to the connected renderer instead of
/// opening a local stream. Ok(false) = not casting, take the local path.
pub(crate) async fn play_current_if_cast(runtime: &Runtime) -> bool {
    let svc = service();
    if !svc.is_casting().await {
        return false;
    }
    match runtime.core().current_track().await {
        Some(track) => {
            if let Err(e) = svc.cast_track(&track).await {
                log::warn!("[qbz-qt][Cast] play new track {} failed: {e}", track.id);
            }
            true
        }
        // Connected but the queue has no current track: still handled — the
        // local path must not open a stream behind the renderer's back.
        None => true,
    }
}

// ---------------------------------------------------------------------------
// UI publishing helpers
// ---------------------------------------------------------------------------

fn set_scanning(scanning: bool) {
    cast_bridge::ui(move |mut b| b.as_mut().set_scanning(scanning));
}

fn set_error(message: String) {
    cast_bridge::ui(move |mut b| {
        b.as_mut()
            .set_error(cxx_qt_lib::QString::from(message.as_str()))
    });
}

fn push_quality(label: String, detail: String) {
    cast_bridge::ui(move |mut b| {
        b.as_mut()
            .set_quality_label(cxx_qt_lib::QString::from(label.as_str()));
        b.as_mut()
            .set_quality_detail(cxx_qt_lib::QString::from(detail.as_str()));
    });
}

fn push_position(position: f32, duration: f32, playing: bool) {
    cast_bridge::ui(move |mut b| {
        b.as_mut().set_position_secs(position);
        b.as_mut().set_duration_secs(duration);
        b.as_mut().set_is_playing(playing);
    });
}

/// Serialize both device lists into the ONE document the picker reads
/// (the home_bridge JSON-document convention: cxx-qt-lib 0.7 cannot express
/// a QVariantList of QVariantMaps).
fn push_devices(chromecast: Vec<DiscoveredDevice>, dlna: Vec<DiscoveredDlnaDevice>) {
    let cc_count = chromecast.len() as i32;
    let dl_count = dlna.len() as i32;
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(chromecast.len() + dlna.len());
    for d in chromecast {
        rows.push(serde_json::json!({
            "id": d.id,
            "name": d.name,
            "ip": d.ip,
            "protocol": "chromecast",
            "model": d.model,
            "canPlay": true,
            "canSetVolume": true,
        }));
    }
    for d in dlna {
        rows.push(serde_json::json!({
            "id": d.id,
            "name": d.name,
            "ip": d.ip,
            "protocol": "dlna",
            "model": d.model,
            "canPlay": d.has_av_transport,
            "canSetVolume": d.has_rendering_control,
        }));
    }
    let json = serde_json::Value::Array(rows).to_string();
    cast_bridge::ui(move |mut b| {
        b.as_mut()
            .set_devices_json(cxx_qt_lib::QString::from(json.as_str()));
        b.as_mut().set_chromecast_count(cc_count);
        b.as_mut().set_dlna_count(dl_count);
    });
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// The tier to REQUEST for a cast to the renderer keyed `cap_key`, plus the
/// request-time cause: the user's streaming preference clamped by the manual
/// per-renderer cap, lowest tier wins (#638 fix 4). On a tie the cause is
/// `RendererCap` — the more specific, more surprising of the two. NEVER
/// consults a local DAC cap: the local DAC is not in a cast's signal path.
fn effective_cast_quality(cap_key: Option<&str>) -> (Quality, QualityLimit) {
    let pref = crate::playback_qt::current_quality();
    let cap = cap_key.and_then(|k| {
        cast_caps()
            .get(k)
            .and_then(|c| c.get("tier"))
            .and_then(|t| t.as_str())
            .map(quality_for_tier)
    });
    match cap {
        // A stored `hires_plus`/unknown tier resolves to UltraHiRes = no
        // effective cap (and never a RendererCap cause).
        Some(cap) if cap < pref => (Quality::min_tier(cap, pref), QualityLimit::RendererCap),
        Some(cap) if cap == pref && cap < Quality::UltraHiRes => (pref, QualityLimit::RendererCap),
        _ => (
            pref,
            if pref < Quality::UltraHiRes {
                QualityLimit::Preference
            } else {
                QualityLimit::None
            },
        ),
    }
}

/// Cap tier key -> request tier (the Slint `streaming_quality_for_key`).
fn quality_for_tier(tier: &str) -> Quality {
    match tier {
        "mp3" => Quality::Mp3,
        "cd" => Quality::Lossless,
        "hires" => Quality::HiRes,
        _ => Quality::UltraHiRes,
    }
}

/// The `cast_quality_caps` map out of the SHARED ui_prefs.json. Read raw
/// because `settings_qt`'s typed accessors only cover scalars and its
/// `prefs_path` is private; writes still go through `settings_qt::save_pref`,
/// which is the additive patch that preserves every other frontend's keys.
fn cast_caps() -> serde_json::Map<String, serde_json::Value> {
    let Some(path) = dirs::data_dir().map(|p| p.join("qbz").join("ui_prefs.json")) else {
        return serde_json::Map::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("cast_quality_caps").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Dropdown index for the stored cap of `cap_key` (0 follow · 1 hires · 2 cd ·
/// 3 mp3). An unknown stored tier reads as "follow", so a hand-edited value
/// degrades to the no-cap default instead of showing a wrong cap.
fn cap_index_for_key(cap_key: &str) -> i32 {
    match cast_caps()
        .get(cap_key)
        .and_then(|c| c.get("tier"))
        .and_then(|t| t.as_str())
    {
        Some("hires") => 1,
        Some("cd") => 2,
        Some("mp3") => 3,
        _ => 0,
    }
}

/// The on-disk file a row can be SERVED from, or a named refusal.
///
/// IC-10. This was `resolve_local_path(track_id)`: an ephemeral-floor test and
/// then a bare `db.get_track(id)`. Its two siblings in `local_playback` both
/// grew guards this one never got — and the id it is handed is a
/// `QueueTrack.id`, which is only sometimes a `library.db` rowid. A Plex row
/// carries a NAMESPACED id (bit 40 set) and an offline row carries the *Qobuz*
/// catalog id, so both looked up a rowid that is not theirs, missed, and
/// reported "Local file not found" — an answer that names the wrong problem.
///
/// It is now a `PlaybackTicket` consumer like every other playback path, which
/// makes the refusals structural rather than remembered:
///
/// - `File` / `DsdFile` — a real path; cast can serve it.
/// - anything else — the row plays through the network, and casting it would
///   need a proxy this service does not have. That is the `TODO(cast-plex)`
///   arm's actual content, and it now applies to every remote source by
///   construction instead of to the one word somebody thought to list.
/// What a non-Qobuz track resolves to for the renderer.
enum Castable {
    /// A file on disk — served straight from disk.
    File(String),
    /// Served by a media server — proxied with the source's request contract.
    Stream {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// Whether this row names a Qobuz catalog track that Cast must fetch through
/// the core's cache/progressive-stream path.
///
/// New QConnect materializations are stamped `qobuz`. The legacy transport tag
/// remains accepted so queues persisted by an older build — or materialized
/// before a manual disconnect in the same process — can still hand off to Cast.
fn uses_qobuz_cast_pipeline(track: &QueueTrack) -> bool {
    !track.is_local
        && matches!(
            track.source.as_deref(),
            None | Some("qobuz") | Some("qobuz_download") | Some("qobuz_connect_remote")
        )
}

async fn resolve_castable(track: &QueueTrack) -> Result<Castable, String> {
    let ticket = qbz_source::registry()
        .playback(track)
        .await
        .map_err(|e| format!("Cannot resolve track {} for casting: {e}", track.id))?;
    match ticket {
        qbz_source::PlaybackTicket::File { path, .. }
        | qbz_source::PlaybackTicket::DsdFile { path, .. } => {
            Ok(Castable::File(path.to_string_lossy().into_owned()))
        }
        qbz_source::PlaybackTicket::Stream {
            url,
            request_headers,
            ..
        } => Ok(Castable::Stream {
            url,
            headers: request_headers,
        }),
        other => Err(format!(
            "Track {} cannot be cast: its source hands it over as {}",
            track.id,
            match other {
                qbz_source::PlaybackTicket::Bytes { .. } => "fetched bytes",
                qbz_source::PlaybackTicket::Catalog { .. } => "a catalog id",
                _ => "an unsupported ticket",
            }
        )),
    }
}

/// Media-server adapter over the player's progressive download buffer:
/// every request gets its own cursor; a read past the buffered edge blocks
/// until the segment lands (the download is far faster than playback).
struct BufferedRangeSource(Arc<qbz_player::BufferedMediaSource>);

impl RangeSource for BufferedRangeSource {
    fn open(&self, start: u64, len: u64) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut reader = self.0.create_reader();
        reader.seek(SeekFrom::Start(start))?;
        Ok(Box::new(reader.take(len)))
    }
}

/// Media-server adapter that re-fetches a range from a source server
/// (Plex / Jellyfin / Subsonic) with the ticket's headers. Runs on the media
/// server's own thread, so it drives the async client through the tokio
/// handle and pulls the body chunk by chunk as the renderer reads.
struct HttpRangeSource {
    url: String,
    headers: Vec<(String, String)>,
    handle: tokio::runtime::Handle,
}

static PROXY_HTTP: OnceLock<reqwest::Client> = OnceLock::new();

/// Largest server-streamed item the shadow decoder will mirror into RAM.
const SHADOW_DOWNLOAD_MAX_BYTES: u64 = 400 * 1024 * 1024;

fn proxy_client() -> &'static reqwest::Client {
    PROXY_HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            // A source server (Plex / Jellyfin / Subsonic) that stops
            // answering mid-body used to park the media server's request
            // thread in `BodyReader::read` forever. Per-read and reset on
            // progress, so a slow-but-alive stream is never cut.
            .read_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

/// Sequentially download `url` into a progressive buffer (exact `total`
/// known) for the shadow decoder. The returned sender cancels it.
fn shadow_download(
    url: String,
    headers: Vec<(String, String)>,
    total: u64,
) -> (
    Arc<qbz_player::BufferedMediaSource>,
    tokio::sync::watch::Sender<bool>,
) {
    let (source, writer) = qbz_player::BufferedMediaSource::new(
        qbz_player::StreamingConfig::fast_start(),
        Some(total),
    );
    let source = Arc::new(source);
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    crate::spawn(async move {
        let mut request = proxy_client()
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .header("Accept-Encoding", "identity");
        for (name, value) in &headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut response = match request.send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let _ = writer.error(format!("shadow download: status {}", r.status()));
                return;
            }
            Err(e) => {
                let _ = writer.error(format!("shadow download: {e}"));
                return;
            }
        };
        loop {
            if *cancel_rx.borrow() {
                let _ = writer.error("shadow download cancelled".to_string());
                return;
            }
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if writer.push_chunk(&chunk).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = writer.complete();
                    return;
                }
                Err(e) => {
                    let _ = writer.error(format!("shadow download: {e}"));
                    return;
                }
            }
        }
    });
    (source, cancel_tx)
}

impl RangeSource for HttpRangeSource {
    fn open(&self, start: u64, len: u64) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        let end = start + len - 1;
        let mut request = proxy_client()
            .get(&self.url)
            .header("User-Agent", "Mozilla/5.0")
            .header("Accept-Encoding", "identity")
            .header("Range", format!("bytes={start}-{end}"));
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = self
            .handle
            .block_on(request.send())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if !response.status().is_success() {
            return Err(std::io::Error::other(format!(
                "source answered {} to a range request",
                response.status()
            )));
        }
        struct BodyReader {
            handle: tokio::runtime::Handle,
            response: reqwest::Response,
            pending: Vec<u8>,
            offset: usize,
            remaining: u64,
        }
        impl std::io::Read for BodyReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Ok(0);
                }
                while self.offset >= self.pending.len() {
                    match self.handle.block_on(self.response.chunk()) {
                        Ok(Some(chunk)) => {
                            self.pending = chunk.to_vec();
                            self.offset = 0;
                        }
                        Ok(None) => return Ok(0),
                        Err(e) => return Err(std::io::Error::other(e.to_string())),
                    }
                }
                let n = buf
                    .len()
                    .min(self.pending.len() - self.offset)
                    .min(self.remaining as usize);
                buf[..n].copy_from_slice(&self.pending[self.offset..self.offset + n]);
                self.offset += n;
                self.remaining -= n as u64;
                Ok(n)
            }
        }
        Ok(Box::new(BodyReader {
            handle: self.handle.clone(),
            response,
            pending: Vec::new(),
            offset: 0,
            remaining: len,
        }))
    }
}

/// Content type for a local file by extension (for the UI label; the SERVED
/// MIME is set by the crate's own map in `register_file`).
fn content_type_for_local(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "m4a" | "alac" | "mp4" => "audio/mp4",
        "aiff" | "aif" => "audio/aiff",
        // DSD containers (MinimServer DLNA convention).
        "dsf" => "audio/x-dsf",
        "dff" => "audio/x-dff",
        "ape" => "audio/x-ape",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "aac" => "audio/aac",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn media_metadata(track: &QueueTrack) -> MediaMetadata {
    MediaMetadata {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        artwork_url: track.artwork_url.clone(),
        duration_secs: Some(track.duration_secs),
    }
}

fn dlna_metadata(track: &QueueTrack) -> DlnaMetadata {
    DlnaMetadata {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        artwork_url: track.artwork_url.clone(),
        duration_secs: Some(track.duration_secs),
    }
}

/// FALLBACK quality label + detail from the track's CATALOG metadata — used
/// only when the STREAMINFO probe cannot read the served bytes (non-FLAC /
/// local files). Returns (label, detail), e.g. ("Hi-Res FLAC", "24-bit / 96 kHz").
fn quality_label_from_track(track: &QueueTrack) -> (String, String) {
    // DSD carries bit_depth=1 + the DSD bit rate as sample_rate — the generic
    // format would print the nonsense "1-bit / 2822.4 kHz".
    if track.bit_depth == Some(1) {
        let dsd = crate::quality_state::dsd_multiple_label(track.sample_rate);
        return (dsd.clone(), dsd);
    }
    let detail = match (track.sample_rate, track.bit_depth) {
        (Some(khz), Some(bits)) => {
            format!("{}-bit / {} kHz", bits, crate::home_qt::format_rate(khz))
        }
        (Some(khz), None) => format!("{} kHz", crate::home_qt::format_rate(khz)),
        (None, Some(bits)) => format!("{bits}-bit"),
        (None, None) => String::new(),
    };
    let label = if track.hires { "Hi-Res FLAC" } else { "FLAC" }.to_string();
    (label, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #733: RelTime frozen inside the end guard, no STOPPED ever -> ended
    /// after CAST_END_STALL_POLLS_MAX polls, whatever the transport says.
    #[test]
    fn end_stall_fires_after_frozen_polls_inside_the_guard() {
        for state in ["PLAYING", "UNKNOWN", "TRANSITIONING"] {
            let mut stall = DlnaEndStall::default();
            // Advancing position never counts.
            assert!(!end_stall_tick(&mut stall, state, 455.0, true));
            assert!(!end_stall_tick(&mut stall, state, 456.0, true));
            let mut fired = false;
            for _ in 0..CAST_END_STALL_POLLS_MAX {
                fired = end_stall_tick(&mut stall, state, 456.0, true);
            }
            assert!(fired, "{state}: expected the stall rule to fire");
            assert_eq!(stall.polls, CAST_END_STALL_POLLS_MAX);
        }
    }

    #[test]
    fn end_stall_ignores_pause_and_resets_on_movement_or_outside_the_guard() {
        let mut stall = DlnaEndStall::default();
        // A user pause at the end is not a stall.
        for _ in 0..CAST_END_STALL_POLLS_MAX + 1 {
            assert!(!end_stall_tick(&mut stall, "PAUSED_PLAYBACK", 456.0, true));
        }
        assert_eq!(stall.polls, 0);
        // Frozen mid-track (outside the guard) is a hiccup, not an end.
        for _ in 0..CAST_END_STALL_POLLS_MAX + 1 {
            assert!(!end_stall_tick(&mut stall, "PLAYING", 120.0, false));
        }
        assert_eq!(stall.polls, 0);
        // Accumulating, then the position moves: the streak restarts.
        assert!(!end_stall_tick(&mut stall, "PLAYING", 456.0, true));
        assert!(!end_stall_tick(&mut stall, "PLAYING", 456.0, true));
        assert_eq!(stall.polls, 1);
        assert!(!end_stall_tick(&mut stall, "PLAYING", 457.0, true));
        assert_eq!(stall.polls, 0);
    }

    #[test]
    fn connection_stamp_expires_with_epoch_protocol_or_disconnect() {
        let stamp = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Dlna,
        };
        let mut inner = CastInner::default();

        assert!(!connection_stamp_matches(&inner, stamp));
        inner.protocol = Some(CastProtocol::Dlna);
        inner.connection_epoch = 7;
        inner.connection_transition_epoch = 5;
        assert!(connection_stamp_matches(&inner, stamp));

        inner.connection_transition_epoch = 6;
        assert!(!connection_stamp_matches(&inner, stamp));
        inner.connection_transition_epoch = 5;
        inner.connection_epoch = 8;
        assert!(!connection_stamp_matches(&inner, stamp));
        inner.connection_epoch = 7;
        inner.protocol = Some(CastProtocol::Chromecast);
        assert!(!connection_stamp_matches(&inner, stamp));
        inner.protocol = None;
        assert!(!connection_stamp_matches(&inner, stamp));
    }

    #[test]
    fn same_track_media_intents_are_distinguished_by_epoch() {
        let connection = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Dlna,
        };
        let stamp = CastMediaIntentStamp {
            connection,
            media_intent_epoch: 11,
            track_id: 42,
        };
        let mut inner = CastInner {
            protocol: Some(CastProtocol::Dlna),
            connection_epoch: 7,
            connection_transition_epoch: 5,
            media_intent_epoch: 11,
            media_intent_track_id: Some(42),
            current_track_id: Some(42),
            ..CastInner::default()
        };

        assert!(media_intent_stamp_matches(&inner, stamp));
        assert_eq!(committed_media_stamp(&inner), Some(stamp));

        // T2 requests the same row: track id is identical, intent identity is not.
        inner.media_intent_epoch = 12;
        inner.current_track_id = None;
        assert!(!media_intent_stamp_matches(&inner, stamp));
        assert_eq!(committed_media_stamp(&inner), None);
    }

    #[test]
    fn poll_snapshot_expires_during_load_and_on_reconnect() {
        let connection = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Dlna,
        };
        let media = CastMediaIntentStamp {
            connection,
            media_intent_epoch: 3,
            track_id: 42,
        };
        let mut inner = CastInner {
            protocol: Some(CastProtocol::Dlna),
            connection_epoch: 7,
            connection_transition_epoch: 5,
            media_intent_epoch: 3,
            media_intent_track_id: Some(42),
            transport_intent_epoch: 9,
            current_track_id: Some(42),
            ..CastInner::default()
        };

        let snapshot = current_poll_snapshot(&inner, connection).expect("committed poll snapshot");
        assert_eq!(snapshot.media, Some(media));
        assert!(poll_snapshot_matches(&inner, snapshot));

        inner.transport_intent_epoch = 10;
        assert!(!poll_snapshot_matches(&inner, snapshot));
        inner.transport_intent_epoch = 9;

        inner.media_intent_epoch = 4;
        inner.media_intent_track_id = Some(43);
        inner.current_track_id = None;
        assert!(!poll_snapshot_matches(&inner, snapshot));

        inner.connection_epoch = 8;
        assert!(!poll_snapshot_matches(&inner, snapshot));
    }

    #[test]
    fn same_media_transport_intents_are_latest_wins() {
        let connection = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Chromecast,
        };
        let media = CastMediaIntentStamp {
            connection,
            media_intent_epoch: 3,
            track_id: 42,
        };
        let mut inner = CastInner {
            protocol: Some(CastProtocol::Chromecast),
            connection_epoch: 7,
            connection_transition_epoch: 5,
            media_intent_epoch: 3,
            media_intent_track_id: Some(42),
            current_track_id: Some(42),
            ..CastInner::default()
        };

        let delayed = issue_transport_intent(&mut inner, media).expect("deferred seek");
        let manual = issue_transport_intent(&mut inner, media).expect("manual seek");
        assert!(!transport_stamp_matches(&inner, delayed));
        assert!(transport_stamp_matches(&inner, manual));
        assert!(current_poll_snapshot(&inner, connection).is_none());
        assert!(!settle_transport_intent(&mut inner, delayed, false));
        assert!(current_poll_snapshot(&inner, connection).is_none());
        inner.lost_polls = 4;
        assert!(activate_transport_intent(&mut inner, manual));
        assert!(settle_transport_intent(&mut inner, manual, true));
        assert_eq!(inner.lost_polls, 0);
        assert!(!activate_transport_intent(&mut inner, manual));
        assert!(!settle_transport_intent(&mut inner, manual, true));
        assert!(current_poll_snapshot(&inner, connection).is_some());
    }

    #[test]
    fn deferred_transport_renews_epoch_and_permanently_expires_old_poll() {
        let connection = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Dlna,
        };
        let media = CastMediaIntentStamp {
            connection,
            media_intent_epoch: 3,
            track_id: 42,
        };
        let mut inner = CastInner {
            protocol: Some(CastProtocol::Dlna),
            connection_epoch: 7,
            connection_transition_epoch: 5,
            media_intent_epoch: 3,
            media_intent_track_id: Some(42),
            transport_intent_epoch: 9,
            current_track_id: Some(42),
            lost_polls: LOST_POLL_MAX,
            ..CastInner::default()
        };
        let old_poll = current_poll_snapshot(&inner, connection).expect("old poll");
        let receipt = current_transport_stamp(&inner).expect("LOAD receipt");

        let renewed = issue_deferred_transport_intent(&mut inner, receipt)
            .expect("exact deferred command renews its transport epoch");
        assert_ne!(
            renewed.transport_intent_epoch,
            receipt.transport_intent_epoch
        );
        assert!(!poll_snapshot_matches(&inner, old_poll));
        assert!(current_poll_snapshot(&inner, connection).is_none());
        assert!(issue_deferred_transport_intent(&mut inner, receipt).is_none());

        assert!(settle_transport_intent(&mut inner, renewed, true));
        assert_eq!(inner.lost_polls, 0);
        assert!(!poll_snapshot_matches(&inner, old_poll));
        assert_eq!(
            current_poll_snapshot(&inner, connection).unwrap().media,
            Some(media)
        );
    }

    #[test]
    fn volume_worker_is_owned_by_one_exact_connection() {
        let a = CastConnectionStamp {
            connection_epoch: 7,
            transition_epoch: CastTransitionEpoch(5),
            protocol: CastProtocol::Chromecast,
        };
        let b = CastConnectionStamp {
            connection_epoch: 8,
            transition_epoch: CastTransitionEpoch(6),
            protocol: CastProtocol::Chromecast,
        };
        let mut inner = CastInner {
            protocol: Some(CastProtocol::Chromecast),
            connection_epoch: 7,
            connection_transition_epoch: 5,
            pending_volume: Some(PendingVolume {
                connection: a,
                value: 0.25,
            }),
            volume_worker_connection: Some(a),
            ..CastInner::default()
        };
        assert!(volume_worker_matches(&inner, a));

        inner.connection_epoch = 8;
        inner.connection_transition_epoch = 6;
        inner.pending_volume = Some(PendingVolume {
            connection: b,
            value: 0.75,
        });
        inner.volume_worker_connection = Some(b);
        assert!(!volume_worker_matches(&inner, a));
        assert!(volume_worker_matches(&inner, b));
        assert_eq!(
            inner.pending_volume.map(|pending| pending.connection),
            Some(b)
        );
    }

    #[test]
    fn volume_sender_joins_the_media_lane_before_consuming_a_value() {
        let source = include_str!("cast_qt.rs");
        let body = source
            .split_once("async fn drain_volume")
            .expect("volume sender")
            .1
            .split_once("// NOTE: next/previous")
            .expect("volume sender end")
            .0;
        let action = body
            .find("let Some(transport_action)")
            .expect("authority action");
        let dispatch = &body[action..];
        let gate = dispatch
            .find("media_command_gate")
            .expect("total media lane");
        let consume = dispatch.find("let v =").expect("pending volume consume");
        let command = dispatch.find("set_volume(v)").expect("renderer mutation");
        assert!(gate < consume && consume < command);
    }

    #[test]
    fn renderer_replacement_tears_a_down_before_connecting_b() {
        let source = include_str!("cast_qt.rs");
        let body = source
            .split_once("async fn connect_exact(")
            .expect("Cast connect")
            .1
            .split_once("async fn connect_chromecast")
            .expect("provisional renderer helpers")
            .0;
        let active = body.find("if self.is_casting().await").expect("A check");
        let teardown = body[active..]
            .find("self.teardown_renderer().await")
            .expect("A teardown")
            + active;
        let physical = body[teardown..]
            .find("if !outcome.physical_safe")
            .expect("physical teardown gate")
            + teardown;
        let provisional = body
            .find("let pending_result = match proto")
            .expect("provisional B connect");
        assert!(active < teardown && teardown < physical && physical < provisional);
    }

    #[test]
    fn provisional_renderer_commit_revalidates_after_inner_lock() {
        let source = include_str!("cast_qt.rs");
        let body = source
            .split_once("async fn connect_exact(")
            .expect("Cast connect")
            .1
            .split_once("async fn connect_chromecast")
            .expect("provisional renderer helpers")
            .0;
        let commit = body
            .rfind("let mut pending = Some(pending)")
            .expect("provisional commit");
        let commit = &body[commit..];
        let inner_lock = commit
            .find("let mut inner = self.inner.lock().await")
            .expect("commit state lock");
        let epoch_check = commit
            .find("if !self.transition_is_current(transition_epoch)")
            .expect("post-lock epoch validation");
        let consume = commit
            .find("pending\n                    .take()")
            .expect("conditional provisional consume");
        let publish = commit
            .find("inner.connection_epoch =")
            .expect("renderer state publication");
        assert!(inner_lock < epoch_check && epoch_check < consume && consume < publish);

        let stale_start = commit
            .find("let Some(connection_stamp) = connection_stamp else")
            .expect("stale commit branch");
        let committed_start = commit
            .find("// Cast ownership is now committed")
            .expect("committed presentation boundary");
        let stale = &commit[stale_start..committed_start];
        assert!(stale.contains("teardown_detached_renderer"));
        assert!(stale.contains("return Err"));
        assert!(!stale.contains("push_connection_state"));
        assert!(!stale.contains("start_position_poll"));
        assert!(commit[committed_start..].contains("push_connection_state"));
        assert!(commit[committed_start..].contains("start_position_poll"));
    }

    #[test]
    fn disconnect_wrapper_publishes_its_exact_intent_before_spawn() {
        let source = include_str!("cast_qt.rs");
        let wrapper = source
            .split_once("pub(crate) fn disconnect()")
            .expect("disconnect wrapper")
            .1
            .split_once("pub(crate) fn set_device_cap")
            .expect("next wrapper")
            .0;
        let publish = wrapper
            .find("begin_transition_intent()")
            .expect("synchronous disconnect intent");
        let spawn = wrapper.find("crate::spawn").expect("disconnect task");
        assert!(publish < spawn);
        assert!(wrapper.contains("disconnect_exact(transition_epoch).await"));

        let exact = source
            .split_once("async fn disconnect_exact(")
            .expect("exact disconnect")
            .1
            .split_once("async fn disconnect_if_poll_snapshot")
            .expect("poll cleanup")
            .0;
        assert!(!exact.contains("begin_transition_intent()"));
    }

    #[test]
    fn media_lane_covers_registry_load_state_and_one_badge_publish() {
        let source = include_str!("cast_qt.rs");
        let body = source
            .split_once("async fn run_media_command")
            .expect("total media command")
            .1
            .split_once("async fn register_qobuz")
            .expect("media registration helpers")
            .0;
        let gate = body.find("media_command_gate").expect("media gate");
        let cancel = body
            .find("prepare_media_command(media_stamp)")
            .expect("previous source cancellation");
        let resolve = body.find("resolve_castable(track).await").expect("resolve");
        let load = body.find("let load_result").expect("renderer LOAD");
        let state = body
            .find("inner.current_track_id = Some(track.id)")
            .expect("committed state");
        assert!(gate < cancel && cancel < resolve && resolve < load && load < state);
        assert_eq!(
            body.matches("self.publish_measured_badge(&info)").count(),
            1
        );
    }

    #[test]
    fn renderer_track_controls_share_the_media_lane_and_exact_stamp() {
        let source = include_str!("cast_qt.rs");
        let seek = source
            .split_once("async fn seek_secs_if_session")
            .expect("seek")
            .1
            .split_once("async fn play_renderer")
            .expect("play")
            .0;
        assert!(seek.contains("media_command_gate"));
        assert!(seek.contains("transport_stamp_matches"));

        let toggle = source
            .split_once("pub(crate) async fn request_playing_if_cast")
            .expect("toggle")
            .1
            .split_once("pub(crate) async fn seek_fraction_if_cast")
            .expect("seek entry")
            .0;
        let gate = toggle.find("media_command_gate").expect("toggle lane");
        let derive = toggle
            .find("let playing = inner.is_playing")
            .expect("toggle derivation");
        let command = toggle.find("pause_renderer").expect("renderer command");
        let publish = toggle
            .find("publish_connection_state_locked")
            .expect("toggle commit");
        assert!(gate < derive && derive < command && command < publish);

        for (start, end) in [
            ("async fn play_renderer", "async fn pause_renderer"),
            ("async fn pause_renderer", "// ---- Position poll"),
        ] {
            let body = source
                .split_once(start)
                .unwrap_or_else(|| panic!("missing {start}"))
                .1
                .split_once(end)
                .unwrap_or_else(|| panic!("missing {end}"))
                .0;
            assert!(
                body.contains("OwnedMutexGuard"),
                "{start} lacks proof of the caller-owned media lane"
            );
            assert!(
                body.contains("transport_stamp_matches"),
                "{start} lacks exact transport validation"
            );
        }
    }

    #[test]
    fn lost_poll_cleanup_uses_exact_snapshot_and_conditional_transition_claim() {
        let source = include_str!("cast_qt.rs");
        let cleanup = source
            .split_once("async fn disconnect_if_poll_snapshot")
            .expect("poll cleanup")
            .1
            .split_once("async fn finish_disconnect")
            .expect("disconnect finisher")
            .0;
        assert!(cleanup.contains("lost_poll_snapshot_matches"));
        let teardown = cleanup
            .find("teardown_renderer_if_poll(expected)")
            .expect("exact teardown");
        let claim = cleanup
            .find("begin_transition_intent_if_current(observed_transition)")
            .expect("conditional post-teardown claim");
        assert!(teardown < claim);

        let poll = source
            .split_once("async fn poll_once")
            .expect("poll")
            .1
            .split_once("async fn advance")
            .expect("advance")
            .0;
        assert!(poll.contains("disconnect_if_poll_snapshot(poll_snapshot)"));
    }

    #[test]
    fn teardown_budget_supervises_the_complete_media_lane_continuation() {
        let source = include_str!("cast_qt.rs");
        let helper = source
            .split_once("async fn run_media_teardown_bounded")
            .expect("bounded media teardown helper")
            .1
            .split_once("async fn run_blocking_teardown")
            .expect("blocking teardown helper")
            .0;
        let fence = helper
            .find("teardown_pending.fetch_add")
            .expect("physical fence");
        let spawn = helper.find("tokio::spawn(future)").expect("owned task");
        let budget = helper
            .find("timeout(BLOCKING_TEARDOWN_BUDGET, &mut task)")
            .expect("caller budget");
        assert!(fence < spawn && spawn < budget);
        let timed_out = helper
            .split_once("Err(_) =>")
            .expect("timeout supervisor")
            .1;
        assert!(timed_out.contains("tokio::spawn(async move"));
        assert!(timed_out.contains("task.await"));
        assert!(!timed_out.contains("task.abort"));

        let teardown = source
            .split_once("async fn teardown_renderer_exact")
            .expect("renderer teardown")
            .1
            .split_once("async fn publish_disconnected_state")
            .expect("disconnect publication")
            .0;
        assert!(teardown.contains("run_media_teardown_bounded(\"renderer\""));
        let gate = teardown.find("media_command_gate").expect("media lane");
        let registry = teardown.find("stream_cancel").expect("registry cleanup");
        let physical = teardown
            .find("teardown_detached_renderer(renderer)")
            .expect("physical renderer cleanup");
        assert!(gate < registry && registry < physical);

        let shutdown = source
            .split_once("pub(crate) async fn shutdown")
            .expect("terminal Cast shutdown")
            .1
            .split_once("// ---- State push to the UI")
            .expect("state publication")
            .0;
        assert!(shutdown.contains("run_media_teardown_bounded(\"shutdown\""));
        let gate = shutdown
            .find("media_command_gate")
            .expect("shutdown media lane");
        let server = shutdown
            .find("media_server.take")
            .expect("server ownership");
        let teardown = shutdown.find("tokio::join!").expect("parallel teardown");
        assert!(gate < server && server < teardown);
    }

    #[test]
    fn late_safe_teardown_resumes_only_its_exact_qconnect_restore() {
        let source = include_str!("cast_qt.rs");
        let supervisor = source
            .split_once("fn supervise_late_qconnect_restore")
            .expect("late restore supervisor")
            .1
            .split_once("async fn restore_latched_qconnect")
            .expect("restore implementation")
            .0;
        assert!(supervisor.contains("transition_is_current(transition_epoch)"));
        assert!(supervisor.contains("teardown_unsafe.load"));
        assert!(supervisor.contains("teardown_pending.load"));
        assert!(supervisor.contains("qconnect_restore_token == Some(restore_token)"));
        assert!(supervisor.contains("restore_latched_qconnect(transition_epoch).await"));

        let completion = source
            .split_once("async fn complete_disconnect")
            .expect("disconnect completion")
            .1
            .split_once("async fn teardown_renderer")
            .expect("teardown entry")
            .0;
        let relatch = completion
            .find("qconnect_restore_token = Some(token)")
            .expect("restore relatch");
        let supervise = completion
            .find("supervise_late_qconnect_restore(restore_epoch, token)")
            .expect("late recovery handoff");
        assert!(relatch < supervise);
    }

    #[test]
    fn consumed_qconnect_restore_is_replaced_even_without_a_runtime() {
        let source = include_str!("cast_qt.rs");
        let body = source
            .split_once("async fn suspend_qconnect_if_on")
            .expect("Cast QConnect suspend seam")
            .1
            .split_once("async fn restore_latched_qconnect")
            .expect("Cast QConnect restore seam")
            .0;

        assert!(body.contains("carried_restore.is_some() && qc.has_enabled_intent()"));
        assert!(body.contains("if !qconnect_running && !consumed_restore_in_flight"));
        assert!(body.contains("qc.disconnect_for_cast(transition_epoch).await"));
        assert!(!body.contains("qconnect_restore_token = None"));
    }

    #[test]
    fn qconnect_queue_rows_remain_castable_after_disconnect() {
        for source in ["qobuz", "qobuz_connect_remote"] {
            let track: QueueTrack = serde_json::from_value(serde_json::json!({
                "id": 128659874_u64,
                "title": "Burning Ambition",
                "artist": "Iron Maiden",
                "album": "Best Of The B-Sides",
                "duration_secs": 162_u64,
                "artwork_url": null,
                "bit_depth": 16_u32,
                "sample_rate": 44100.0,
                "album_id": null,
                "artist_id": null,
                "source": source
            }))
            .expect("queue track");
            assert!(uses_qobuz_cast_pipeline(&track), "{source}");
        }
    }
}
